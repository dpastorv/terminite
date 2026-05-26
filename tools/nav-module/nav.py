#!/usr/bin/env python3
"""terminite module: Nav.

Native file navigator. List view of the current directory, arrow keys
to move the selection cursor, Enter to act on the highlighted entry.

When Enter lands on a directory we descend into it. When it lands on a
file we publish a `focus` event:

    {"kind":"publish_focus","path":"/abs/path/to/file"}

The host broadcasts that event to every *other* live module — Preview
and Editor sit downstream of it without ever talking to Nav directly.

Keys:
  Up / Down     move selection
  Enter         enter directory  /  publish focus on file
  Left / Bksp   parent directory
  Home / End    jump to first / last entry
"""

import json
import os
import sys

HEADER_LINES = 2  # cwd line + blank
MAX_NAME = 64


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def log(message):
    send({"kind": "log", "message": message})


class Nav:
    def __init__(self):
        self.cwd = os.getcwd()
        self.idx = 0
        self.entries = []
        self.refresh()

    def refresh(self):
        try:
            names = sorted(os.listdir(self.cwd), key=lambda n: (not os.path.isdir(os.path.join(self.cwd, n)), n.lower()))
        except OSError as e:
            log(f"nav: listdir failed: {e}")
            names = []
        # Always offer ".." unless we're at filesystem root
        entries = []
        if self.cwd != "/":
            entries.append("..")
        entries.extend(names)
        self.entries = entries
        if self.idx >= len(self.entries):
            self.idx = max(0, len(self.entries) - 1)

    def render(self):
        lines = [self.cwd, ""]
        for i, name in enumerate(self.entries):
            full = os.path.join(self.cwd, name) if name != ".." else os.path.dirname(self.cwd)
            try:
                is_dir = os.path.isdir(full)
            except OSError:
                is_dir = False
            marker = ">" if i == self.idx else " "
            suffix = "/" if is_dir else ""
            display = name if len(name) <= MAX_NAME else name[: MAX_NAME - 1] + "…"
            lines.append(f"{marker} {display}{suffix}")
        if not self.entries:
            lines.append("  (empty)")
        send({"kind": "set_text", "body": "\n".join(lines)})

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
        if name == "..":
            self.cwd = os.path.dirname(self.cwd) or "/"
            self.idx = 0
            self.refresh()
            self.render()
            return
        full = os.path.join(self.cwd, name)
        try:
            is_dir = os.path.isdir(full)
        except OSError as e:
            log(f"nav: stat failed: {e}")
            return
        if is_dir:
            self.cwd = full
            self.idx = 0
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
        self.refresh()
        self.render()


def handle_input(nav, raw):
    # Arrow keys + most named keys arrive as escape sequences. We
    # match on suffixes so a partial first byte doesn't break us.
    if raw == "\r" or raw == "\n":
        nav.activate()
        return
    if raw == "\x7f" or raw == "\b":
        nav.go_up()
        return
    if raw.endswith("[A"):
        nav.move(-1)
    elif raw.endswith("[B"):
        nav.move(1)
    elif raw.endswith("[D"):
        nav.go_up()
    elif raw.endswith("[C"):
        nav.activate()
    elif raw.endswith("[H") or raw.endswith("OH"):
        nav.jump("home")
    elif raw.endswith("[F") or raw.endswith("OF"):
        nav.jump("end")
    elif raw in ("k", "K"):
        nav.move(-1)
    elif raw in ("j", "J"):
        nav.move(1)
    elif raw in ("h", "H"):
        nav.go_up()
    elif raw in ("l", "L"):
        nav.activate()
    elif raw in ("g",):
        nav.jump("home")
    elif raw in ("G",):
        nav.jump("end")


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
            handle_input(nav, cmd.get("bytes", ""))
        elif kind == "focus":
            # Another module published — Nav itself doesn't react,
            # but we could highlight the path here if desired.
            pass
        elif kind == "close":
            break


if __name__ == "__main__":
    main()
