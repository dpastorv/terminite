#!/usr/bin/env python3
"""terminite module: Nav — the browse half of the Files module.

Native file navigator. Keyboard-only, designed to feel fast in a
narrow sidebar pane.

Modes:
  normal    arrows / Enter / o (open) / R (reveal) / fs commands / etc.
  filter    after `/` — typing narrows the list live
  input     after n / N / r — type a name (new file / new folder / rename)
  confirm   y/n prompt: external open / reveal, or a delete warning
  help      after `?` — a key legend; any key dismisses

Filesystem commands (normal mode, all scoped to the current dir):
  n  new file        N  new folder        r  rename selected
  d  delete selected (Del key too) — confirms, loud for non-empty folders

Wire (this half → dispatcher / host):
  set_text   body + scroll_to_line to keep cursor on screen
  log        diagnostics
  returns    ("open", path) up to the Files dispatcher on Enter over a file

Wire (host → this half, via the dispatcher):
  input      keystrokes (raw bytes as a string)
  cwd        a shell pane reported a new cwd via OSC 7
"""

import json
import os
import shutil
import subprocess
import sys
import time
from typing import List, Optional, Tuple

MAX_NAME = 64
PAGE = 20  # lines per page-up/page-down
# Soft cap on directory entries we render. A `node_modules` or a
# git repo's `.git/objects` can hold 100k+ entries; rendering every
# one of those every keystroke flattens the host. Hit the cap →
# show the first N + a truncation footer; use `/` to filter to
# what you actually want.
MAX_ENTRIES = 2000

# Display lines reserved at the top before entries start:
#   line 0: cwd
#   line 1: status (count + selection info / filter prompt / confirm prompt)
#   line 2: blank separator
HEADER_LINES = 3


from wire import send, log, del_word_back, WORD_BACKSPACE  # shared host wire


def fmt_size(n):
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if n < 1024 or unit == "TB":
            if unit == "B":
                return f"{n} B"
            return f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} TB"


def fmt_mtime(ts):
    t = time.localtime(ts)
    now = time.localtime()
    if t.tm_year == now.tm_year:
        return time.strftime("%b %d %H:%M", t)
    return time.strftime("%b %d %Y", t)


def entry_indicator(full_path, name):
    """Single-char suffix per entry — borrowed from `ls -F`."""
    try:
        if os.path.islink(full_path):
            return "@"
        if os.path.isdir(full_path):
            return "/"
        if os.access(full_path, os.X_OK):
            return "*"
    except OSError:
        pass
    return ""


class Nav:
    def __init__(self):
        self.cwd = os.getcwd()
        self.idx = 0
        self.show_hidden = False
        self.filter = ""
        self.mode = "normal"  # normal | filter | confirm
        self.confirm: Optional[Tuple[str, str]] = None  # (action, path)
        self.last_shell_cwd: Optional[str] = None
        self.all_entries: List[str] = []   # before filter
        self.entries: List[str] = []        # after filter
        # Transient one-shot feedback ("created foo", "delete failed: …")
        # shown in the status line for a single render, cleared on the
        # next normal-mode keystroke.
        self.notice = ""
        # One-shot "look here" flag — set after a create / rename so the
        # new row gets the theme's attention (green) wash for one render.
        self.attention = False
        # Name-entry state (new file / new folder / rename).
        self.input_action: Optional[str] = None  # new_file | new_folder | rename
        self.input_buf = ""
        self.input_rename_from: Optional[str] = None
        self.refresh()

    # --- listing ----------------------------------------------------------

    def refresh(self):
        try:
            names = os.listdir(self.cwd)
        except OSError as e:
            log(f"nav: listdir failed: {e}")
            names = []
        if not self.show_hidden:
            names = [n for n in names if not n.startswith(".")]
        # Directories first (case-insensitive name sort within each group).
        names.sort(key=lambda n: (
            not self._is_dir(os.path.join(self.cwd, n)),
            n.lower(),
        ))
        # Cap rendered entries so a giant directory doesn't drive the
        # host's reshape time off a cliff. Filter (`/`) bypasses the
        # cap by matching against the full pre-cap list.
        self.entries_truncated = len(names) > MAX_ENTRIES
        if self.entries_truncated:
            names = names[:MAX_ENTRIES]
        base = [] if self.cwd == "/" else [".."]
        self.all_entries = base + names
        self.apply_filter()

    def _is_dir(self, path):
        try:
            return os.path.isdir(path)
        except OSError:
            return False

    def apply_filter(self):
        if not self.filter:
            self.entries = list(self.all_entries)
        else:
            needle = self.filter.lower()
            self.entries = [n for n in self.all_entries if needle in n.lower()]
        if self.idx >= len(self.entries):
            self.idx = max(0, len(self.entries) - 1)

    # --- rendering --------------------------------------------------------

    def status_line(self):
        if self.mode == "input":
            label = {
                "new_file": "new file",
                "new_folder": "new folder",
                "rename": "rename to",
            }.get(self.input_action, "name")
            return f"{label}: {self.input_buf}_   Enter = ok, Esc = cancel"
        if self.mode == "filter":
            n = len(self.entries)
            suffix = "no matches" if n == 0 else f"{n} match" + ("es" if n != 1 else "")
            return f"filter: {self.filter}_   ({suffix})  Esc = cancel"
        if self.mode == "confirm" and self.confirm:
            return self._confirm_prompt()
        if self.notice:
            return self.notice
        # Normal mode: cursor position + selection metadata.
        if not self.entries:
            base = "(empty)"
        else:
            sel = self.entries[self.idx]
            full = self._selected_full_path()
            meta = ""
            try:
                st = os.lstat(full)
                if os.path.isdir(full) and not os.path.islink(full):
                    try:
                        cnt = len(os.listdir(full))
                        meta = f"  {cnt} items"
                    except OSError:
                        meta = ""
                else:
                    meta = f"  {fmt_size(st.st_size)}  {fmt_mtime(st.st_mtime)}"
            except OSError:
                meta = ""
            base = f"{self.idx + 1}/{len(self.entries)}  {sel}{meta}"
        shell = ""
        if self.last_shell_cwd and self.last_shell_cwd != self.cwd:
            shell = f"    [s = sync to shell: {self.last_shell_cwd}]"
        return base + shell + "    ? = keys"

    def _confirm_prompt(self):
        action, path = self.confirm
        name = os.path.basename(path) or path
        if action == "open":
            return f"Open {name} in default app?  (y / n)"
        if action == "reveal":
            return f"Reveal {name} in Finder?  (y / n)"
        if action == "delete":
            try:
                is_dir = os.path.isdir(path) and not os.path.islink(path)
            except OSError:
                is_dir = False
            if is_dir:
                try:
                    cnt = len(os.listdir(path))
                except OSError:
                    cnt = 0
                if cnt:
                    return (
                        f"Delete {name}/ and ALL {cnt} item(s) inside? "
                        "Cannot be undone.  (y / n)"
                    )
                return f"Delete empty folder {name}/?  (y / n)"
            return f"Delete {name}? Cannot be undone.  (y / n)"
        return f"{action} {name}?  (y / n)"

    def render(self):
        if self.mode == "help":
            self.render_help()
            return
        # Leading badge marks which Files sub-mode owns the pane — the
        # navigator. NAV wears the neutral foreground; the editor wears
        # the theme's yellow ● EDIT. The badge text is colored via a span.
        badge = "● NAV"
        lines = [f"{badge}   {self.cwd}", self.status_line(), ""]
        for i, name in enumerate(self.entries):
            full = self._full_path(name)
            marker = "▸" if i == self.idx else " "
            indicator = entry_indicator(full, name)
            display = name if len(name) <= MAX_NAME else name[: MAX_NAME - 1] + "…"
            lines.append(f"{marker} {display}{indicator}")
        if not self.entries:
            lines.append("  (empty)")
        if getattr(self, "entries_truncated", False) and not self.filter:
            lines.append("")
            lines.append(f"  … listing truncated at {MAX_ENTRIES} entries — use / to filter")
        # Keep the cursor row visible; status & cwd are pinned above and
        # scroll off the top as needed.
        cursor_line = HEADER_LINES + self.idx if self.entries else 0
        # Recolor the selection band by intent: red over a row about to
        # be deleted, green over a freshly created / renamed row, else
        # the default amber. The host owns the actual theme colors.
        accent = None
        if self.mode == "confirm" and self.confirm and self.confirm[0] == "delete":
            accent = "danger"
        elif self.attention:
            accent = "new"
        send({
            "kind": "set_text",
            "body": "\n".join(lines),
            "scroll_to_line": cursor_line,
            # Subtle background on the selected entry — same band the
            # host uses for the editor cursor row, so the highlight
            # treatment is consistent across modules.
            "highlight_line": cursor_line if self.entries else None,
            "highlight_accent": accent,
            # Color the ● NAV badge (bullet + word) with the neutral fg.
            "spans": [
                {"line": 0, "start": 0, "end": len(badge.encode()), "accent": "fg"}
            ],
        })

    def render_help(self):
        body = "\n".join([
            "▌ NAV — keys",
            "",
            "  ↑ ↓         move             Enter / →   open / enter dir",
            "  ← / Bksp    up a directory   /           filter the list",
            "  Home / End  first / last     .           toggle hidden",
            "  o           open in app      R           reveal in Finder",
            "  s           sync to shell's cwd",
            "",
            "  n  new file       N  new folder",
            "  r  rename         d / Del  delete (asks first)",
            "",
            "  ?  this help      any key  dismiss",
        ])
        send({"kind": "set_text", "body": body, "highlight_line": None})

    # --- filesystem ops ---------------------------------------------------

    def enter_input(self, action):
        """Open the name-entry line for new_file / new_folder / rename.
        Rename pre-fills the selected name so an edit is a few keystrokes."""
        self.mode = "input"
        self.input_action = action
        if action == "rename" and self.entries and self.entries[self.idx] != "..":
            self.input_buf = self.entries[self.idx]
            self.input_rename_from = self.entries[self.idx]
        else:
            self.input_buf = ""
            self.input_rename_from = None
        self.render()

    def exit_input(self, commit):
        action = self.input_action
        name = self.input_buf.strip()
        rename_from = self.input_rename_from
        self.mode = "normal"
        self.input_action = None
        self.input_buf = ""
        self.input_rename_from = None
        if commit and name:
            self._run_fs_op(action, name, rename_from)
        self.render()

    def input_keystroke(self, raw):
        if raw == "\x1b":
            self.exit_input(commit=False)
            return
        if raw == "\r" or raw == "\n":
            self.exit_input(commit=True)
            return
        if raw == WORD_BACKSPACE:
            self.input_buf = del_word_back(self.input_buf)
            self.render()
            return
        if raw == "\x7f" or raw == "\b":
            self.input_buf = self.input_buf[:-1]
            self.render()
            return
        if raw.startswith("\x1b"):
            return  # ignore arrows / control sequences while typing a name
        if all(ord(c) >= 32 for c in raw):
            self.input_buf += raw
            self.render()

    def _bad_name(self, name):
        """Reject names that would escape the current dir or break listing.
        We deliberately keep ops single-directory — no path separators."""
        if not name or name in (".", ".."):
            return "invalid name"
        if "/" in name or "\x00" in name:
            return "name can't contain /"
        return None

    def _select_name(self, name):
        """Put the cursor on `name` after a refresh, if it's visible."""
        if name in self.entries:
            self.idx = self.entries.index(name)

    def _run_fs_op(self, action, name, rename_from):
        err = self._bad_name(name)
        if err:
            self.notice = err
            return
        dest = os.path.join(self.cwd, name)
        if action == "new_file":
            if os.path.exists(dest):
                self.notice = f"{name} already exists"
                return
            try:
                open(dest, "x").close()  # x: fail if it raced into existence
            except OSError as e:
                self.notice = f"create failed: {e}"
                return
            self.notice = f"created {name}"
        elif action == "new_folder":
            if os.path.exists(dest):
                self.notice = f"{name} already exists"
                return
            try:
                os.mkdir(dest)
            except OSError as e:
                self.notice = f"mkdir failed: {e}"
                return
            self.notice = f"created {name}/"
        elif action == "rename":
            if not rename_from or name == rename_from:
                return
            if os.path.exists(dest):
                self.notice = f"{name} already exists"
                return
            try:
                os.rename(os.path.join(self.cwd, rename_from), dest)
            except OSError as e:
                self.notice = f"rename failed: {e}"
                return
            self.notice = f"renamed → {name}"
        else:
            return
        # Success → draw the eye to the new row for one render.
        self.attention = True
        self.refresh()
        self._select_name(name)

    def _request_delete(self):
        """d / Del → arm the delete confirm for the selected entry."""
        if not self.entries:
            return
        if self.entries[self.idx] == "..":
            self.notice = "can't delete .."
            self.render()
            return
        self.confirm = ("delete", self._selected_full_path())
        self.mode = "confirm"
        self.render()

    def _do_delete(self, path):
        name = os.path.basename(path) or path
        prev_idx = self.idx
        try:
            if os.path.isdir(path) and not os.path.islink(path):
                shutil.rmtree(path)
            else:
                os.remove(path)
        except OSError as e:
            self.notice = f"delete failed: {e}"
            self.refresh()
            return
        self.notice = f"deleted {name}"
        self.refresh()
        if self.entries:
            self.idx = max(0, min(prev_idx, len(self.entries) - 1))

    def enter_help(self):
        self.mode = "help"
        self.render()

    def exit_help(self):
        self.mode = "normal"
        self.render()

    # --- helpers ----------------------------------------------------------

    def _full_path(self, name):
        if name == "..":
            return os.path.dirname(self.cwd) or "/"
        return os.path.join(self.cwd, name)

    def _selected_full_path(self):
        if not self.entries:
            return self.cwd
        return self._full_path(self.entries[self.idx])

    # --- navigation -------------------------------------------------------

    def move(self, delta):
        if not self.entries:
            return
        self.idx = max(0, min(len(self.entries) - 1, self.idx + delta))
        self.render()

    def jump(self, where):
        if not self.entries:
            return
        self.idx = 0 if where == "home" else len(self.entries) - 1
        self.render()

    def activate(self):
        if not self.entries:
            return
        name = self.entries[self.idx]
        full = self._full_path(name)
        if name == "..":
            self.cwd = os.path.dirname(self.cwd) or "/"
            self.idx = 0
            self.filter = ""
            self.refresh()
            self.render()
            return
        try:
            is_dir = os.path.isdir(full)
        except OSError as e:
            log(f"nav: stat failed: {e}")
            return
        if is_dir:
            self.cwd = full
            self.idx = 0
            self.filter = ""
            self.refresh()
            self.render()
        else:
            # In the unified Files module the dispatcher routes the open by
            # type (image → preview, else → editor) — return the signal
            # instead of publishing a cross-module focus event.
            return ("open", full)

    def go_up(self):
        if self.cwd == "/":
            return
        self.cwd = os.path.dirname(self.cwd) or "/"
        self.idx = 0
        self.filter = ""
        self.refresh()
        self.render()

    def cd_to(self, path):
        """Used by `cwd` events (shell sync) and `s` keypress."""
        if not os.path.isdir(path):
            log(f"nav: cd_to ignored non-dir {path}")
            return
        self.cwd = path
        self.idx = 0
        self.filter = ""
        self.refresh()
        self.render()

    # --- type-to-jump -----------------------------------------------------

    def jump_to_letter(self, ch):
        if not self.entries:
            return
        ch = ch.lower()
        start = (self.idx + 1) % len(self.entries)
        # Search from after current cursor, wrap around, find first
        # entry whose first non-dot character matches.
        for offset in range(len(self.entries)):
            i = (start + offset) % len(self.entries)
            name = self.entries[i]
            core = name.lstrip(".")
            if core and core[0].lower() == ch:
                self.idx = i
                self.render()
                return

    # --- modes ------------------------------------------------------------

    def enter_filter_mode(self):
        self.mode = "filter"
        self.filter = ""
        self.apply_filter()
        self.render()

    def exit_filter_mode(self, keep_filter=False):
        self.mode = "normal"
        if not keep_filter:
            self.filter = ""
            self.apply_filter()
        self.render()

    def filter_keystroke(self, raw):
        if raw == "\x1b":  # Esc — drop the filter
            self.exit_filter_mode(keep_filter=False)
            return
        if raw == "\r" or raw == "\n":
            # Enter from filter — keep the filter, drop back to normal
            # mode so arrow keys / Enter work on the narrowed set.
            self.exit_filter_mode(keep_filter=True)
            return
        if raw == WORD_BACKSPACE:
            self.filter = del_word_back(self.filter)
            self.apply_filter()
            self.render()
            return
        if raw == "\x7f" or raw == "\b":
            self.filter = self.filter[:-1]
            self.apply_filter()
            self.render()
            return
        if raw.startswith("\x1b"):
            # Arrow keys etc inside filter mode: leave the filter alone
            # but pass-through navigation. Lets users type-then-arrow.
            self._navigate(raw)
            return
        # Printable char (assume single char; pasted multi-char goes in
        # as-is too — handy if the user pastes a search string).
        if all(ord(c) >= 32 for c in raw):
            self.filter += raw
            self.apply_filter()
            self.render()

    def enter_confirm(self, action):
        if not self.entries:
            return
        self.confirm = (action, self._selected_full_path())
        self.mode = "confirm"
        self.render()

    def exit_confirm(self, run):
        action_path = self.confirm
        self.confirm = None
        self.mode = "normal"
        if run and action_path:
            action, path = action_path
            if action == "delete":
                self._do_delete(path)
            else:
                self._run_external(action, path)
        self.render()

    def _run_external(self, action, path):
        try:
            if action == "open":
                subprocess.Popen(["open", path])
                log(f"nav: open {path}")
            elif action == "reveal":
                subprocess.Popen(["open", "-R", path])
                log(f"nav: reveal {path}")
        except OSError as e:
            log(f"nav: external launch failed: {e}")

    # --- input dispatch ---------------------------------------------------

    def _navigate(self, raw):
        if raw.endswith("[A"):
            self.move(-1)
        elif raw.endswith("[B"):
            self.move(1)
        elif raw.endswith("[D"):
            self.go_up()
        elif raw.endswith("[C"):
            self.activate()
        elif raw.endswith("[H") or raw.endswith("OH"):
            self.jump("home")
        elif raw.endswith("[F") or raw.endswith("OF"):
            self.jump("end")
        elif raw.endswith("[5~"):
            self.move(-PAGE)
        elif raw.endswith("[6~"):
            self.move(PAGE)

    def handle_input(self, raw):
        if not raw:
            return
        if self.mode == "input":
            self.input_keystroke(raw)
            return
        if self.mode == "help":
            self.exit_help()
            return
        if self.mode == "filter":
            self.filter_keystroke(raw)
            return
        if self.mode == "confirm":
            if raw in ("y", "Y"):
                self.exit_confirm(run=True)
            elif raw in ("n", "N", "\x1b", "\r"):
                self.exit_confirm(run=False)
            return
        # Normal mode — clear any one-shot feedback from the prior action.
        self.notice = ""
        self.attention = False
        if raw == "\r" or raw == "\n":
            # Propagate the open-signal (if a file) up to the dispatcher.
            return self.activate()
        if raw == "\x7f" or raw == "\b":
            self.go_up()
            return
        if raw.startswith("\x1b") and len(raw) > 1:
            if raw.endswith("[3~"):       # Delete key → delete selected
                self._request_delete()
            else:
                self._navigate(raw)
            return
        # Single printable bindings.
        if raw == "/":
            self.enter_filter_mode()
            return
        if raw == ".":
            self.show_hidden = not self.show_hidden
            self.refresh()
            self.render()
            return
        if raw == "s":
            if self.last_shell_cwd and self.last_shell_cwd != self.cwd:
                self.cd_to(self.last_shell_cwd)
            return
        if raw == "o":
            self.enter_confirm("open")
            return
        if raw == "R":
            self.enter_confirm("reveal")
            return
        # Filesystem commands. These shadow type-to-jump for n / r / d
        # (same trade the o / s bindings already make) — mnemonic wins.
        if raw == "n":
            self.enter_input("new_file")
            return
        if raw == "N":
            self.enter_input("new_folder")
            return
        if raw == "r":
            if self.entries and self.entries[self.idx] != "..":
                self.enter_input("rename")
            return
        if raw == "d":
            self._request_delete()
            return
        if raw == "?":
            self.enter_help()
            return
        # Anything else printable → type-to-jump on first letter.
        if len(raw) == 1 and raw.isalnum():
            self.jump_to_letter(raw)
        return None

    def handle_click(self, line, count):
        """Body click selects an entry; double-click activates it (cd into a
        folder, or return an ("open", path) signal for a file). Header rows and
        out-of-range clicks are ignored."""
        if line < HEADER_LINES or not self.entries:
            return None
        idx = line - HEADER_LINES
        if not (0 <= idx < len(self.entries)):
            return None
        self.idx = idx
        if count >= 2:
            return self.activate()
        self.render()
        return None
