#!/usr/bin/env python3
"""terminite module: Edit.

A v1 line-buffer editor. Loads the file pointed to by the most recent
`focus` event from another module, then accepts keystrokes:

  Arrows         move cursor
  Home / End     start / end of line
  PgUp / PgDn    half-page jump
  Backspace      delete prev char (merges lines at col 0)
  Enter          split line at cursor
  Ctrl+S         save
  Printable      insert at cursor

The cursor is shown by injecting a visible bar character (│) at the
cursor column on the active line before rendering. That sidesteps the
fact that data-module bodies are static text — no terminal cursor.

Refusal modes:
  - Files >  MAX_BYTES open as read-only (status line warns)
  - Binary files render a placeholder and don't load into the buffer

This is intentionally minimal. No undo, no syntax, no selection. The
point of the module is to prove the cross-pane focus wire works end-
to-end with a real editing surface; richer behavior comes later.
"""

import json
import os
import sys

MAX_BYTES = 1_000_000
PAGE_LINES = 16
STATUS_BAR_LINES = 2
CURSOR = "│"


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def log(message):
    send({"kind": "log", "message": message})


class Editor:
    def __init__(self):
        self.path = None
        self.lines = [""]
        self.row = 0
        self.col = 0
        self.dirty = False
        self.readonly = False
        self.message = "(no file — Nav → Enter to load one)"

    def load(self, path):
        self.path = path
        self.row = 0
        self.col = 0
        self.dirty = False
        self.readonly = False
        if not os.path.exists(path):
            self.lines = [""]
            self.message = f"new file: {path}"
            return
        try:
            size = os.path.getsize(path)
        except OSError as e:
            self.lines = [f"(error: {e})"]
            self.readonly = True
            self.message = "read error"
            return
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
        self.lines = text.split("\n") if text else [""]
        if not self.message.startswith("file > ") and not self.message.startswith("decoded"):
            self.message = f"loaded {len(self.lines)} lines"

    def save(self):
        if self.path is None:
            self.message = "no file to save"
            return
        if self.readonly:
            self.message = "read-only"
            return
        try:
            with open(self.path, "w", encoding="utf-8") as f:
                f.write("\n".join(self.lines))
        except OSError as e:
            self.message = f"save failed: {e}"
            return
        self.dirty = False
        self.message = f"saved {self.path}"

    # --- cursor movement --------------------------------------------------
    def clamp_col(self):
        self.col = max(0, min(self.col, len(self.lines[self.row])))

    def move(self, drow, dcol):
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

    def home(self):
        self.col = 0

    def end(self):
        self.col = len(self.lines[self.row])

    def page(self, delta):
        self.row = max(0, min(len(self.lines) - 1, self.row + delta))
        self.clamp_col()

    # --- editing ----------------------------------------------------------
    def insert(self, ch):
        if self.readonly:
            return
        line = self.lines[self.row]
        self.lines[self.row] = line[: self.col] + ch + line[self.col :]
        self.col += len(ch)
        self.dirty = True

    def backspace(self):
        if self.readonly:
            return
        if self.col > 0:
            line = self.lines[self.row]
            self.lines[self.row] = line[: self.col - 1] + line[self.col :]
            self.col -= 1
            self.dirty = True
        elif self.row > 0:
            prev = self.lines[self.row - 1]
            curr = self.lines.pop(self.row)
            self.row -= 1
            self.col = len(prev)
            self.lines[self.row] = prev + curr
            self.dirty = True

    def newline(self):
        if self.readonly:
            return
        line = self.lines[self.row]
        self.lines[self.row] = line[: self.col]
        self.lines.insert(self.row + 1, line[self.col :])
        self.row += 1
        self.col = 0
        self.dirty = True

    # --- rendering --------------------------------------------------------
    def render(self):
        out = []
        path = self.path or "(no file)"
        dirty = "*" if self.dirty else " "
        ro = " [RO]" if self.readonly else ""
        header = f"{dirty} {path}{ro}    {self.row + 1}:{self.col + 1}"
        out.append(header)
        out.append("")  # spacer
        for i, line in enumerate(self.lines):
            if i == self.row:
                rendered = line[: self.col] + CURSOR + line[self.col :]
            else:
                rendered = line
            out.append(rendered)
        out.append("")
        out.append(f"— {self.message}    (Ctrl+S save)")
        send({"kind": "set_text", "body": "\n".join(out)})


# --- input parsing ------------------------------------------------------------

def handle_input(ed: Editor, raw: str):
    if not raw:
        return
    # Multi-byte escapes first.
    if raw.endswith("[A"):
        ed.move(-1, 0); ed.render(); return
    if raw.endswith("[B"):
        ed.move(1, 0); ed.render(); return
    if raw.endswith("[C"):
        ed.move(0, 1); ed.render(); return
    if raw.endswith("[D"):
        ed.move(0, -1); ed.render(); return
    if raw.endswith("[H") or raw.endswith("OH"):
        ed.home(); ed.render(); return
    if raw.endswith("[F") or raw.endswith("OF"):
        ed.end(); ed.render(); return
    if raw.endswith("[5~"):
        ed.page(-PAGE_LINES); ed.render(); return
    if raw.endswith("[6~"):
        ed.page(PAGE_LINES); ed.render(); return
    # Single bytes.
    if raw == "\r" or raw == "\n":
        ed.newline(); ed.render(); return
    if raw == "\x7f" or raw == "\b":
        ed.backspace(); ed.render(); return
    if raw == "\x13":  # Ctrl+S
        ed.save(); ed.render(); return
    if raw == "\x1b":
        # Bare Esc — ignore.
        return
    # Plain printable text (could be multi-codepoint paste).
    if all(ord(c) >= 32 or c == "\t" for c in raw):
        ed.insert(raw)
        ed.render()


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
            handle_input(ed, cmd.get("bytes", ""))
        elif kind == "init":
            log("editor: init")
        elif kind == "close":
            break


if __name__ == "__main__":
    main()
