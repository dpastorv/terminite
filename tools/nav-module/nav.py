#!/usr/bin/env python3
"""terminite module: Nav.

Native file navigator. Keyboard-only, designed to feel fast in a
narrow sidebar pane.

Modes:
  normal    arrow keys / Enter / `o` (open) / `R` (reveal) / etc.
  filter    after `/` — typing narrows the list live
  confirm   after `o` or `R` — y/n prompt before running an external

Wire (this module → host):
  set_text   body + optional scroll_to_line to keep cursor on screen
  set_image  unused
  log        diagnostics
  publish_focus  when the user Enters on a file (Preview / Edit react)

Wire (host → this module):
  init       once at startup
  input      keystrokes (raw bytes as a string)
  focus      another module published — Nav ignores
  cwd        a shell pane reported a new cwd via OSC 7
"""

import json
import os
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


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def log(message):
    send({"kind": "log", "message": message})


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
        if self.mode == "filter":
            n = len(self.entries)
            suffix = "no matches" if n == 0 else f"{n} match" + ("es" if n != 1 else "")
            return f"filter: {self.filter}_   ({suffix})  Esc = cancel"
        if self.mode == "confirm" and self.confirm:
            action, path = self.confirm
            name = os.path.basename(path) or path
            prompt = {
                "open": f"Open {name} in default app?",
                "reveal": f"Reveal {name} in Finder?",
            }.get(action, f"{action} {name}?")
            return f"{prompt}  (y / n)"
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
        return base + shell

    def render(self):
        lines = [self.cwd, self.status_line(), ""]
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
        send({
            "kind": "set_text",
            "body": "\n".join(lines),
            "scroll_to_line": cursor_line,
            # Subtle background on the selected entry — same band the
            # host uses for the editor cursor row, so the highlight
            # treatment is consistent across modules.
            "highlight_line": cursor_line if self.entries else None,
        })

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
            log(f"nav: focus {full}")
            send({"kind": "publish_focus", "path": full})

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
        if self.mode == "filter":
            self.filter_keystroke(raw)
            return
        if self.mode == "confirm":
            if raw in ("y", "Y"):
                self.exit_confirm(run=True)
            elif raw in ("n", "N", "\x1b", "\r"):
                self.exit_confirm(run=False)
            return
        # Normal mode.
        if raw == "\r" or raw == "\n":
            self.activate()
            return
        if raw == "\x7f" or raw == "\b":
            self.go_up()
            return
        if raw.startswith("\x1b") and len(raw) > 1:
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
        # Anything else printable → type-to-jump on first letter.
        if len(raw) == 1 and raw.isalnum():
            self.jump_to_letter(raw)


def main():
    nav = Nav()
    nav.render()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            cmd = json.loads(line)
        except json.JSONDecodeError:
            continue
        kind = cmd.get("kind", "")
        if kind == "init":
            log("nav: init")
        elif kind == "input":
            nav.handle_input(cmd.get("bytes", ""))
        elif kind == "cwd":
            path = cmd.get("path", "")
            if path:
                nav.last_shell_cwd = path
                nav.render()  # update status hint
        elif kind == "focus":
            pass
        elif kind == "click":
            # Body coords → entry index. Header takes HEADER_LINES
            # rows; clicks above that, or past the last entry, are
            # ignored. Doubles as "click to activate" on a second
            # click is a future polish — v1 just selects.
            line = int(cmd.get("line", 0))
            if line >= HEADER_LINES and nav.entries:
                idx = line - HEADER_LINES
                if 0 <= idx < len(nav.entries):
                    nav.idx = idx
                    nav.render()
        elif kind == "close":
            break


if __name__ == "__main__":
    main()
