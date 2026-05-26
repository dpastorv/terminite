#!/usr/bin/env python3
"""terminite module: Edit.

Lean text editor. Loads the file pointed to by the most recent
`focus` event from another module, then accepts keystrokes.

Modes:
  normal     typing inserts; Ctrl/Opt shortcuts run commands
  find       after `/` or Ctrl+F — live search, n/N for next/prev
  save_as    after Ctrl+S on an untitled buffer — prompt for path
  confirm    y/n prompt for destructive / risky actions

Selection rendering is bracket-style: `❮` at the anchor, `❯` at the
head. Honest about being plaintext — real highlighted selection wants
the styled-text wire extension we deferred.

System clipboard uses macOS `pbcopy` / `pbpaste` via subprocess. The
host's Cmd+C / Cmd+V copy the *host* selection (the pane's text),
which is what you want for shells; the editor's selection uses
Ctrl+C / Ctrl+X / Ctrl+V so the two surfaces don't compete.

Wire (this module → host):
  set_text   body + scroll_to_line to keep cursor on screen
  log        diagnostics

Wire (host → this module):
  init       once at startup
  input      keystrokes (raw byte sequences as strings)
  focus      another module published — load that file
  cwd        a shell pane reported a new cwd (used for save-as default)
"""

import hashlib
import json
import os
import re
import subprocess
import sys
import time
from typing import List, Optional, Tuple

MAX_BYTES = 1_000_000
PAGE_LINES = 16
GUTTER_PAD = 2  # spaces between line number and content
# Total bytes of buffered undo snapshots. Each snapshot is a copy of
# the full lines list, so 16 MB caps memory use across pathological
# edit storms without ever blowing past the host's per-module body
# cap (also 16 MB). Older snapshots drop off the front of the ring
# once the budget is breached.
UNDO_BYTE_BUDGET = 16 * 1024 * 1024
# Kill buffer caps — protect against unintentional Ctrl+K spam on a
# huge file leaving us holding the entire file in memory.
KILL_BUFFER_LINE_CAP = 5000
KILL_BUFFER_BYTE_CAP = 4 * 1024 * 1024
SEL_OPEN = "❮"
SEL_CLOSE = "❯"


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def log(message):
    send({"kind": "log", "message": message})


# Words are runs of [\w-] then runs of non-whitespace non-word. The
# word-jump skips whitespace in between, lands at word boundaries.
WORD_RE = re.compile(r"[A-Za-z0-9_]+|[^\sA-Za-z0-9_]+")


def fmt_size(n):
    for unit in ("B", "KB", "MB"):
        if n < 1024 or unit == "MB":
            return f"{n} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} MB"


def detect_eol(blob: bytes) -> str:
    # Inspect the first chunk; pick majority style.
    head = blob[:8192]
    crlf = head.count(b"\r\n")
    lf_only = head.count(b"\n") - crlf
    return "CRLF" if crlf > lf_only else "LF"


def system_clipboard_copy(text: str) -> bool:
    try:
        p = subprocess.Popen(["pbcopy"], stdin=subprocess.PIPE)
        p.communicate(text.encode("utf-8"), timeout=2)
        return p.returncode == 0
    except (OSError, subprocess.TimeoutExpired) as e:
        log(f"editor: pbcopy failed: {e}")
        return False


def system_clipboard_paste() -> Optional[str]:
    try:
        res = subprocess.run(["pbpaste"], capture_output=True, timeout=2)
        if res.returncode != 0:
            return None
        return res.stdout.decode("utf-8", errors="replace")
    except (OSError, subprocess.TimeoutExpired) as e:
        log(f"editor: pbpaste failed: {e}")
        return None


class Editor:
    def __init__(self):
        # Buffer state
        self.path: Optional[str] = None
        self.lines: List[str] = [""]
        self.row = 0
        self.col = 0
        self.dirty = False
        self.readonly = False
        self.eol = "LF"
        self.message = "(no file — Nav → Enter to load one)"

        # External-change detection
        self.loaded_mtime: Optional[float] = None
        self.loaded_hash: Optional[str] = None

        # Cwd from latest shell event — used to default save-as dir
        self.last_shell_cwd: Optional[str] = None

        # Mode + prompts
        self.mode = "normal"  # normal | find | save_as | confirm
        self.confirm: Optional[Tuple[str, dict]] = None  # (kind, payload)
        self.prompt_text = ""

        # Selection — sel_anchor None means no selection. The head is
        # always (self.row, self.col).
        self.sel_anchor: Optional[Tuple[int, int]] = None

        # Find state
        self.find_query = ""
        self.find_matches: List[Tuple[int, int, int]] = []  # (row, col_start, col_end)
        self.find_idx = 0
        self.pre_find_cursor: Optional[Tuple[int, int]] = None

        # Undo / redo: snapshots of (lines, row, col, dirty)
        self.undo: List[Tuple[List[str], int, int, bool]] = []
        self.redo: List[Tuple[List[str], int, int, bool]] = []

        # Line cut buffer — Ctrl+K stacks consecutive cuts so
        # Ctrl+Y restores them in order.
        self.kill_buffer: List[str] = []
        self.last_op_was_kill = False

    # --- snapshots --------------------------------------------------------

    def snapshot(self):
        return ([line for line in self.lines], self.row, self.col, self.dirty)

    def restore(self, snap):
        lines, row, col, dirty = snap
        self.lines = [line for line in lines]
        self.row = row
        self.col = col
        self.dirty = dirty
        self.sel_anchor = None

    def push_undo(self):
        self.undo.append(self.snapshot())
        # Byte-budgeted eviction. `_undo_bytes` is conservative — sum
        # of len(line) across snapshots; Python overhead is real but
        # bounded by the same line count.
        bytes_used = sum(
            sum(len(line) for line in snap[0]) for snap in self.undo
        )
        while bytes_used > UNDO_BYTE_BUDGET and len(self.undo) > 1:
            dropped = self.undo.pop(0)
            bytes_used -= sum(len(line) for line in dropped[0])
        self.redo.clear()
        self.last_op_was_kill = False

    def do_undo(self):
        if not self.undo:
            self.message = "nothing to undo"
            return
        self.redo.append(self.snapshot())
        self.restore(self.undo.pop())
        self.message = "undo"

    def do_redo(self):
        if not self.redo:
            self.message = "nothing to redo"
            return
        self.undo.append(self.snapshot())
        self.restore(self.redo.pop())
        self.message = "redo"

    # --- load / save ------------------------------------------------------

    def load(self, path):
        self.path = path
        self.row = 0
        self.col = 0
        self.dirty = False
        self.readonly = False
        self.sel_anchor = None
        self.undo.clear()
        self.redo.clear()
        self.kill_buffer = []
        self.last_op_was_kill = False
        self.loaded_mtime = None
        self.loaded_hash = None
        if not os.path.exists(path):
            self.lines = [""]
            self.eol = "LF"
            self.message = f"new file: {path}"
            return
        try:
            st = os.stat(path)
        except OSError as e:
            self.lines = [f"(error: {e})"]
            self.readonly = True
            self.message = "read error"
            return
        size = st.st_size
        if size > MAX_BYTES:
            self.readonly = True
            self.message = f"file > {MAX_BYTES} bytes — read-only"
        try:
            with open(path, "rb") as f:
                blob = f.read(min(size, MAX_BYTES))
        except OSError as e:
            self.lines = [f"(error: {e})"]
            self.readonly = True
            self.message = "read error"
            return
        if b"\x00" in blob[:4096]:
            self.lines = [f"(binary file — {size} bytes)"]
            self.readonly = True
            self.message = "binary — read-only"
            return
        try:
            text = blob.decode("utf-8")
        except UnicodeDecodeError:
            text = blob.decode("utf-8", errors="replace")
            self.message = "decoded with replacements"
        self.eol = detect_eol(blob)
        # Normalize to LF for internal storage; serialize back on save.
        text = text.replace("\r\n", "\n")
        self.lines = text.split("\n") if text else [""]
        self.loaded_mtime = st.st_mtime
        self.loaded_hash = hashlib.sha256(blob).hexdigest()
        if not self.message.startswith("file > ") and not self.message.startswith("decoded"):
            self.message = f"loaded {len(self.lines)} lines ({self.eol})"

    def serialize(self) -> str:
        sep = "\r\n" if self.eol == "CRLF" else "\n"
        return sep.join(self.lines)

    def save(self, force_overwrite=False):
        if self.path is None:
            # Untitled buffer — prompt for a path.
            self.enter_save_as_prompt()
            return
        if self.readonly:
            self.message = "read-only"
            return
        # External-change check — if the on-disk file's hash differs
        # from what we loaded, somebody else wrote to it. Confirm
        # before we clobber.
        if os.path.exists(self.path) and not force_overwrite:
            try:
                with open(self.path, "rb") as f:
                    head = f.read(MAX_BYTES)
                disk_hash = hashlib.sha256(head).hexdigest()
            except OSError:
                disk_hash = None
            if disk_hash is not None and self.loaded_hash is not None and disk_hash != self.loaded_hash:
                self.confirm = ("overwrite_changed", {})
                self.mode = "confirm"
                return
        try:
            with open(self.path, "w", encoding="utf-8") as f:
                f.write(self.serialize())
            st = os.stat(self.path)
            self.loaded_mtime = st.st_mtime
            self.loaded_hash = hashlib.sha256(
                self.serialize().encode("utf-8")
            ).hexdigest()
        except OSError as e:
            self.message = f"save failed: {e}"
            return
        self.dirty = False
        self.message = f"saved {self.path}"

    # --- selection helpers ------------------------------------------------

    def clear_selection(self):
        self.sel_anchor = None

    def begin_selection_if_absent(self):
        if self.sel_anchor is None:
            self.sel_anchor = (self.row, self.col)

    def selection_range(self):
        """Returns ((start_row, start_col), (end_row, end_col)) or None."""
        if self.sel_anchor is None:
            return None
        a = self.sel_anchor
        b = (self.row, self.col)
        return (a, b) if a <= b else (b, a)

    def selection_text(self) -> Optional[str]:
        r = self.selection_range()
        if r is None:
            return None
        (sr, sc), (er, ec) = r
        if sr == er:
            return self.lines[sr][sc:ec]
        parts = [self.lines[sr][sc:]]
        for i in range(sr + 1, er):
            parts.append(self.lines[i])
        parts.append(self.lines[er][:ec])
        return "\n".join(parts)

    def delete_selection(self):
        """Mutates lines; leaves cursor at the deleted region's start."""
        r = self.selection_range()
        if r is None:
            return
        (sr, sc), (er, ec) = r
        before = self.lines[sr][:sc]
        after = self.lines[er][ec:]
        del self.lines[sr:er + 1]
        self.lines.insert(sr, before + after)
        self.row = sr
        self.col = sc
        self.sel_anchor = None
        self.dirty = True

    # --- cursor movement --------------------------------------------------

    def clamp_col(self):
        self.col = max(0, min(self.col, len(self.lines[self.row])))

    def move(self, drow, dcol, extend_selection=False):
        if extend_selection:
            self.begin_selection_if_absent()
        else:
            self.clear_selection()
        self.row = max(0, min(len(self.lines) - 1, self.row + drow))
        if dcol != 0:
            self.col += dcol
            if self.col < 0:
                if self.row > 0:
                    self.row -= 1
                    self.col = len(self.lines[self.row])
                else:
                    self.col = 0
            elif self.col > len(self.lines[self.row]):
                if self.row < len(self.lines) - 1:
                    self.row += 1
                    self.col = 0
                else:
                    self.col = len(self.lines[self.row])
        else:
            self.clamp_col()

    def home(self, extend=False):
        if extend:
            self.begin_selection_if_absent()
        else:
            self.clear_selection()
        self.col = 0

    def end(self, extend=False):
        if extend:
            self.begin_selection_if_absent()
        else:
            self.clear_selection()
        self.col = len(self.lines[self.row])

    def page(self, delta, extend=False):
        if extend:
            self.begin_selection_if_absent()
        else:
            self.clear_selection()
        self.row = max(0, min(len(self.lines) - 1, self.row + delta))
        self.clamp_col()

    def word_left(self, extend=False):
        if extend:
            self.begin_selection_if_absent()
        else:
            self.clear_selection()
        line = self.lines[self.row]
        if self.col == 0:
            if self.row > 0:
                self.row -= 1
                self.col = len(self.lines[self.row])
            return
        # Find the last word-match that ends before col.
        target = 0
        for m in WORD_RE.finditer(line):
            if m.end() < self.col:
                target = m.start()
            elif m.start() < self.col:
                target = m.start()
                break
        self.col = target

    def word_right(self, extend=False):
        if extend:
            self.begin_selection_if_absent()
        else:
            self.clear_selection()
        line = self.lines[self.row]
        if self.col >= len(line):
            if self.row < len(self.lines) - 1:
                self.row += 1
                self.col = 0
            return
        for m in WORD_RE.finditer(line):
            if m.end() > self.col:
                self.col = m.end()
                return
        self.col = len(line)

    # --- editing primitives ----------------------------------------------

    def insert(self, ch: str):
        if self.readonly:
            return
        if self.sel_anchor is not None:
            self.push_undo()
            self.delete_selection()
        else:
            self.push_undo()
        # Multi-char inserts can contain newlines (paste path).
        if "\n" in ch or "\r" in ch:
            ch = ch.replace("\r\n", "\n").replace("\r", "\n")
            head = self.lines[self.row][:self.col]
            tail = self.lines[self.row][self.col:]
            parts = (head + ch + tail).split("\n")
            self.lines[self.row:self.row + 1] = parts
            self.row += len(parts) - 1
            self.col = len(parts[-1]) - len(tail)
        else:
            line = self.lines[self.row]
            self.lines[self.row] = line[:self.col] + ch + line[self.col:]
            self.col += len(ch)
        self.dirty = True

    def backspace(self):
        if self.readonly:
            return
        if self.sel_anchor is not None:
            self.push_undo()
            self.delete_selection()
            return
        if self.col > 0:
            self.push_undo()
            line = self.lines[self.row]
            self.lines[self.row] = line[:self.col - 1] + line[self.col:]
            self.col -= 1
            self.dirty = True
        elif self.row > 0:
            self.push_undo()
            prev = self.lines[self.row - 1]
            curr = self.lines.pop(self.row)
            self.row -= 1
            self.col = len(prev)
            self.lines[self.row] = prev + curr
            self.dirty = True

    def delete_forward(self):
        if self.readonly:
            return
        if self.sel_anchor is not None:
            self.push_undo()
            self.delete_selection()
            return
        line = self.lines[self.row]
        if self.col < len(line):
            self.push_undo()
            self.lines[self.row] = line[:self.col] + line[self.col + 1:]
            self.dirty = True
        elif self.row < len(self.lines) - 1:
            self.push_undo()
            nxt = self.lines.pop(self.row + 1)
            self.lines[self.row] = line + nxt
            self.dirty = True

    def newline(self):
        if self.readonly:
            return
        if self.sel_anchor is not None:
            self.push_undo()
            self.delete_selection()
        else:
            self.push_undo()
        line = self.lines[self.row]
        # Auto-indent: carry over the previous line's leading
        # whitespace. Open-brace lines could also get a tab of extra
        # indent — not in v2; predictable wins over magic.
        indent_match = re.match(r"^[ \t]*", line[:self.col])
        indent = indent_match.group(0) if indent_match else ""
        self.lines[self.row] = line[:self.col]
        self.lines.insert(self.row + 1, indent + line[self.col:])
        self.row += 1
        self.col = len(indent)
        self.dirty = True

    # --- line ops ---------------------------------------------------------

    def cut_line(self):
        if self.readonly or not self.lines:
            return
        if not self.last_op_was_kill:
            self.kill_buffer = []
        self.push_undo()
        self.last_op_was_kill = True
        # Cut current line; if it's the last line, leave one empty
        # line behind so the buffer is never empty.
        line = self.lines[self.row]
        # Respect kill-buffer caps so Ctrl+K-holding a giant file
        # can't accumulate the whole thing in memory.
        if len(self.kill_buffer) >= KILL_BUFFER_LINE_CAP or sum(
            len(s) for s in self.kill_buffer
        ) + len(line) > KILL_BUFFER_BYTE_CAP:
            self.message = "kill buffer full — paste then cut more"
            return
        self.kill_buffer.append(line)
        if len(self.lines) > 1:
            del self.lines[self.row]
            if self.row >= len(self.lines):
                self.row = len(self.lines) - 1
        else:
            self.lines[0] = ""
        self.col = 0
        self.dirty = True
        self.message = f"cut {len(self.kill_buffer)} line(s)"

    def yank(self):
        if self.readonly or not self.kill_buffer:
            return
        self.push_undo()
        for i, line in enumerate(self.kill_buffer):
            self.lines.insert(self.row + i, line)
        self.row += len(self.kill_buffer) - 1
        self.col = 0
        self.dirty = True
        self.message = f"yanked {len(self.kill_buffer)} line(s)"

    def duplicate_line(self):
        if self.readonly:
            return
        self.push_undo()
        self.lines.insert(self.row + 1, self.lines[self.row])
        self.row += 1
        self.dirty = True

    def indent_line(self):
        if self.readonly:
            return
        self.push_undo()
        r = self.selection_range()
        rows = range(r[0][0], r[1][0] + 1) if r else [self.row]
        for row in rows:
            self.lines[row] = "    " + self.lines[row]
        self.col += 4
        self.dirty = True

    def dedent_line(self):
        if self.readonly:
            return
        self.push_undo()
        r = self.selection_range()
        rows = range(r[0][0], r[1][0] + 1) if r else [self.row]
        removed_on_cursor = 0
        for row in rows:
            line = self.lines[row]
            removed = 0
            while removed < 4 and removed < len(line) and line[removed] in " \t":
                removed += 1
            self.lines[row] = line[removed:]
            if row == self.row:
                removed_on_cursor = removed
        self.col = max(0, self.col - removed_on_cursor)
        self.dirty = True

    # --- clipboard --------------------------------------------------------

    def copy_selection(self):
        text = self.selection_text()
        if text is None:
            return
        if system_clipboard_copy(text):
            self.message = f"copied {len(text)} chars"

    def cut_selection(self):
        text = self.selection_text()
        if text is None:
            return
        if not system_clipboard_copy(text):
            return
        self.push_undo()
        self.delete_selection()
        self.message = f"cut {len(text)} chars"

    def paste(self):
        text = system_clipboard_paste()
        if text is None or not text:
            return
        self.insert(text)
        self.message = "pasted"

    # --- find -------------------------------------------------------------

    def enter_find(self):
        self.mode = "find"
        self.find_query = ""
        self.find_matches = []
        self.find_idx = 0
        self.pre_find_cursor = (self.row, self.col)

    def exit_find(self, keep=True):
        self.mode = "normal"
        if not keep and self.pre_find_cursor:
            self.row, self.col = self.pre_find_cursor
        self.pre_find_cursor = None

    def rebuild_find_matches(self):
        self.find_matches = []
        if not self.find_query:
            return
        needle = self.find_query
        for r, line in enumerate(self.lines):
            start = 0
            while True:
                idx = line.find(needle, start)
                if idx < 0:
                    break
                self.find_matches.append((r, idx, idx + len(needle)))
                start = idx + max(1, len(needle))

    def jump_to_match(self):
        if not self.find_matches:
            return
        self.find_idx %= len(self.find_matches)
        r, sc, _ = self.find_matches[self.find_idx]
        self.row = r
        self.col = sc

    def find_next(self, delta=1):
        if not self.find_matches:
            return
        self.find_idx = (self.find_idx + delta) % len(self.find_matches)
        self.jump_to_match()

    # --- save-as prompt ---------------------------------------------------

    def enter_save_as_prompt(self):
        self.mode = "save_as"
        base = self.last_shell_cwd or os.getcwd()
        self.prompt_text = base.rstrip("/") + "/"

    def exit_save_as(self, commit=True):
        path = self.prompt_text
        self.mode = "normal"
        self.prompt_text = ""
        if commit and path.strip():
            self.path = path
            self.save()

    # --- rendering --------------------------------------------------------

    def status_line(self):
        path = self.path or "(no file)"
        dirty = "●" if self.dirty else "○"
        ro = " [RO]" if self.readonly else ""
        cursor_pos = f"{self.row + 1}:{self.col + 1}"
        line_count = len(self.lines)
        progress = (self.row + 1) * 100 // max(1, line_count)
        sel_info = ""
        r = self.selection_range()
        if r:
            (sr, sc), (er, ec) = r
            if sr == er:
                sel_info = f"  sel:{ec - sc}"
            else:
                sel_info = f"  sel:{er - sr + 1}L"
        return f"{dirty} {path}{ro}   {cursor_pos} / {line_count}L ({progress}%)   {self.eol}{sel_info}"

    def prompt_line(self):
        if self.mode == "find":
            n = len(self.find_matches)
            if n == 0:
                stat = "no matches" if self.find_query else "type to search"
            else:
                stat = f"{self.find_idx + 1}/{n}"
            return f"find: {self.find_query}_   ({stat})   Enter = commit, Esc = cancel"
        if self.mode == "save_as":
            return f"save as: {self.prompt_text}_   Enter = save, Esc = cancel"
        if self.mode == "confirm" and self.confirm:
            kind, _ = self.confirm
            prompts = {
                "overwrite_changed": "File changed on disk since loaded. Overwrite anyway? (y/n)",
            }
            return prompts.get(kind, f"confirm {kind}? (y/n)")
        return self.message

    def render(self):
        out = []
        out.append(self.status_line())
        out.append(self.prompt_line())
        out.append("")  # spacer
        # Line-number gutter width sized to total line count. The
        # gutter is part of the body; the cursor's col field has to
        # add the gutter width so the host draws the block over the
        # right cell.
        gutter_w = max(2, len(str(len(self.lines))))
        gutter_total = gutter_w + GUTTER_PAD
        sel = self.selection_range()
        # Track how many bracket characters were inserted *before*
        # the cursor on the cursor row, so we can shift the host
        # cursor col to land on the actual character (not the bracket).
        cursor_col_shift = 0
        for i, line in enumerate(self.lines):
            content = line
            if sel:
                (sr, sc), (er, ec) = sel
                if i == sr and i == er:
                    content = content[:sc] + SEL_OPEN + content[sc:ec] + SEL_CLOSE + content[ec:]
                    if i == self.row:
                        if self.col >= ec:
                            cursor_col_shift += 2
                        elif self.col >= sc:
                            cursor_col_shift += 1
                elif i == sr:
                    content = content[:sc] + SEL_OPEN + content[sc:]
                    if i == self.row and self.col >= sc:
                        cursor_col_shift += 1
                elif i == er:
                    content = content[:ec] + SEL_CLOSE + content[ec:]
                    if i == self.row and self.col >= ec:
                        cursor_col_shift += 1
                elif sr < i < er and i == self.row:
                    content = SEL_OPEN + content + SEL_CLOSE
                    cursor_col_shift += 1
            num = str(i + 1).rjust(gutter_w)
            out.append(f"{num}{' ' * GUTTER_PAD}{content}")
        # Cursor's source line in the body = 3 header lines + i.
        cursor_line = 3 + self.row
        cursor_col = gutter_total + self.col + cursor_col_shift
        msg = {
            "kind": "set_text",
            "body": "\n".join(out),
            "scroll_to_line": cursor_line,
            "cursor": {"line": cursor_line, "col": cursor_col},
            # Dim the line-number gutter so it reads as chrome.
            "dim_left_cols": gutter_total,
            # Subtle highlight on the cursor row — only when no
            # selection is active (selection brackets carry the
            # visual; a band would compete).
            "highlight_line": cursor_line if sel is None else None,
        }
        send(msg)

    # --- input dispatch ---------------------------------------------------

    def handle_input(self, raw: str):
        if not raw:
            return
        if self.mode == "find":
            self._find_keystroke(raw)
            self.render()
            return
        if self.mode == "save_as":
            self._save_as_keystroke(raw)
            self.render()
            return
        if self.mode == "confirm":
            self._confirm_keystroke(raw)
            self.render()
            return
        # Normal mode.
        self._normal_keystroke(raw)
        self.render()

    def _find_keystroke(self, raw):
        if raw == "\x1b":
            self.exit_find(keep=False)
            return
        if raw == "\r" or raw == "\n":
            self.exit_find(keep=True)
            return
        if raw == "\x7f" or raw == "\b":
            self.find_query = self.find_query[:-1]
            self.rebuild_find_matches()
            self.jump_to_match()
            return
        if raw.startswith("\x1b"):
            # Pass-through nav inside find — just skip.
            return
        if all(ord(c) >= 32 for c in raw):
            self.find_query += raw
            self.rebuild_find_matches()
            self.find_idx = 0
            self.jump_to_match()

    def _save_as_keystroke(self, raw):
        if raw == "\x1b":
            self.exit_save_as(commit=False)
            return
        if raw == "\r" or raw == "\n":
            self.exit_save_as(commit=True)
            return
        if raw == "\x7f" or raw == "\b":
            self.prompt_text = self.prompt_text[:-1]
            return
        if raw.startswith("\x1b"):
            return
        if all(ord(c) >= 32 for c in raw):
            self.prompt_text += raw

    def _confirm_keystroke(self, raw):
        if not self.confirm:
            self.mode = "normal"
            return
        kind, _ = self.confirm
        if raw in ("y", "Y"):
            self.mode = "normal"
            self.confirm = None
            if kind == "overwrite_changed":
                self.save(force_overwrite=True)
        elif raw in ("n", "N", "\x1b"):
            self.mode = "normal"
            self.confirm = None
            self.message = "cancelled"

    def _normal_keystroke(self, raw):
        # Multi-byte escape sequences (arrows, modifier+arrows, …).
        if raw.startswith("\x1b") and len(raw) > 1:
            self._handle_escape(raw)
            self.last_op_was_kill = False
            return
        # Single-byte control characters.
        if raw == "\r" or raw == "\n":
            self.newline()
        elif raw == "\x7f" or raw == "\b":
            self.backspace()
        elif raw == "\t":
            self.indent_line() if self.sel_anchor else self.insert("    ")
        elif raw == "\x13":  # Ctrl+S
            self.save()
        elif raw == "\x1a":  # Ctrl+Z
            self.do_undo()
        elif raw == "\x12":  # Ctrl+R
            self.do_redo()
        elif raw == "\x0b":  # Ctrl+K
            self.cut_line()
            return  # don't reset last_op_was_kill
        elif raw == "\x19":  # Ctrl+Y
            self.yank()
        elif raw == "\x04":  # Ctrl+D
            self.duplicate_line()
        elif raw == "\x03":  # Ctrl+C
            self.copy_selection()
        elif raw == "\x18":  # Ctrl+X
            self.cut_selection()
        elif raw == "\x16":  # Ctrl+V
            self.paste()
        elif raw == "\x06":  # Ctrl+F
            self.enter_find()
        elif raw == "\x07":  # Ctrl+G — next match
            if self.find_matches:
                self.find_next(1)
        elif raw == "/":
            self.enter_find()
        elif raw == "n" and self.find_matches:
            # n / N work in normal mode only if a find session left matches.
            self.find_next(1)
        elif raw == "N" and self.find_matches:
            self.find_next(-1)
        elif all(ord(c) >= 32 or c == "\t" for c in raw):
            self.insert(raw)
        self.last_op_was_kill = False

    def _handle_escape(self, raw):
        # xterm modifier-encoded sequences look like \x1b[1;NX where
        # N is the modifier (2=shift, 3=alt, 4=shift+alt, ...).
        m = re.match(r"^\x1b\[1;(\d+)([A-Za-z])$", raw)
        if m:
            mod = int(m.group(1))
            letter = m.group(2)
            shift = (mod - 1) & 1
            alt = (mod - 1) & 2
            if letter == "A":
                self.move(-1, 0, extend_selection=bool(shift))
            elif letter == "B":
                self.move(1, 0, extend_selection=bool(shift))
            elif letter == "C":
                if alt:
                    self.word_right(extend=bool(shift))
                else:
                    self.move(0, 1, extend_selection=bool(shift))
            elif letter == "D":
                if alt:
                    self.word_left(extend=bool(shift))
                else:
                    self.move(0, -1, extend_selection=bool(shift))
            elif letter == "H":
                self.home(extend=bool(shift))
            elif letter == "F":
                self.end(extend=bool(shift))
            return
        # Unmodified named keys.
        if raw.endswith("[A"):
            self.move(-1, 0)
        elif raw.endswith("[B"):
            self.move(1, 0)
        elif raw.endswith("[C"):
            self.move(0, 1)
        elif raw.endswith("[D"):
            self.move(0, -1)
        elif raw.endswith("[H") or raw.endswith("OH"):
            self.home()
        elif raw.endswith("[F") or raw.endswith("OF"):
            self.end()
        elif raw.endswith("[3~"):
            self.delete_forward()
        elif raw.endswith("[5~"):
            self.page(-PAGE_LINES)
        elif raw.endswith("[6~"):
            self.page(PAGE_LINES)
        elif raw == "\x1b[Z":
            self.dedent_line()


def main():
    ed = Editor()
    ed.render()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            cmd = json.loads(line)
        except json.JSONDecodeError:
            continue
        kind = cmd.get("kind", "")
        if kind == "focus":
            path = cmd.get("path", "")
            if path:
                log(f"editor: load {path}")
                ed.load(path)
                ed.render()
        elif kind == "input":
            ed.handle_input(cmd.get("bytes", ""))
        elif kind == "cwd":
            ed.last_shell_cwd = cmd.get("path", None)
        elif kind == "click":
            # Body coords → editor coords. Body has 3 header lines
            # before the content; ignore clicks in the header. Col
            # is in body cells, which include the gutter + pad —
            # subtract those to land on a real content column.
            line = int(cmd.get("line", 0))
            col = int(cmd.get("col", 0))
            if line >= 3 and ed.lines:
                gw = max(2, len(str(len(ed.lines)))) + GUTTER_PAD
                ed.row = min(line - 3, len(ed.lines) - 1)
                ed.col = max(0, min(len(ed.lines[ed.row]), col - gw))
                ed.clear_selection()
                ed.render()
        elif kind == "init":
            log("editor: init")
        elif kind == "close":
            break


if __name__ == "__main__":
    main()
