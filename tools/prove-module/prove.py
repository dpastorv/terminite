#!/usr/bin/env python3
"""terminite module: Prove — the room's open prove-list, in a pane.

Reads PROVE.md from the current project (searching up from the cwd) and renders
it navigable: each task with its answered-status (○ open / ✓ N answers), Enter
to read the question and the answers logged against it, Esc back, r to reload.

The point (Daniel, 2026-06-09): proving terminite shouldn't wait on one human's
hours — ship the open questions and let the next room answer them BY USING the
room. The one rule, carried from the experiment: answers count only as a
byproduct of REAL WORK, never a staged test (a test built to prove a thing is
exactly the false positive that killed ad-boards). Answers land in PROVE.md as
you work; this pane is how the room sees what's still open.
"""
import json
import os
import sys


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def find_prove(start):
    """Search up from `start` for a PROVE.md — so the pane works from any
    subdirectory of a project that ships one."""
    d = os.path.abspath(start)
    for _ in range(40):
        p = os.path.join(d, "PROVE.md")
        if os.path.isfile(p):
            return p
        parent = os.path.dirname(d)
        if parent == d:
            break
        d = parent
    return None


def parse_tasks(text):
    """Split PROVE.md on `## ` headers, keep only the prove-TASKS — a task is a
    section with an `Answers:` slot (the intro sections don't have one). The
    answer count is the number of logged `verdict:` lines under it."""
    sections = []
    cur = None
    for line in text.splitlines():
        if line.startswith("## "):
            if cur:
                sections.append(cur)
            cur = {"title": line[3:].strip(), "lines": []}
        elif cur is not None:
            cur["lines"].append(line)
    if cur:
        sections.append(cur)
    tasks = []
    for s in sections:
        if not any("answers:" in ln.lower() for ln in s["lines"]):
            continue  # an intro section, not a prove-task
        s["text"] = "\n".join(s["lines"]).strip("\n")
        s["answers"] = sum(
            1 for ln in s["lines"] if ln.strip().lower().startswith("verdict:")
        )
        tasks.append(s)
    return tasks


class Prove:
    def __init__(self):
        self.cwd = os.getcwd()
        self.path = None
        self.tasks = []
        self.idx = 0
        self.mode = "list"  # list | detail
        self.load()

    def load(self):
        self.path = find_prove(self.cwd)
        self.tasks = []
        if self.path:
            try:
                with open(self.path, encoding="utf-8", errors="replace") as f:
                    self.tasks = parse_tasks(f.read())
            except OSError:
                self.tasks = []
        if self.idx >= len(self.tasks):
            self.idx = max(0, len(self.tasks) - 1)

    # --- rendering --------------------------------------------------------

    def render(self):
        if self.mode == "detail" and self.tasks:
            self.render_detail()
            return
        loc = self.path or f"(no PROVE.md found above {self.cwd})"
        lines = [f"● PROVE   {loc}", ""]
        if not self.tasks:
            lines.append("  No prove-list here. A project ships PROVE.md to ask the room")
            lines.append("  what it hasn't proven yet — answered only by real work, not tests.")
        for i, t in enumerate(self.tasks):
            marker = "▸" if i == self.idx else " "
            n = t["answers"]
            tag = f"✓ {n}" if n else "○ open"
            lines.append(f"{marker} [{tag:>6}]  {t['title']}")
        if self.tasks:
            open_n = sum(1 for t in self.tasks if not t["answers"])
            lines.append("")
            lines.append(f"  {open_n}/{len(self.tasks)} open   ·   ↑↓ move · Enter read · r reload")
            lines.append("  answers go in PROVE.md as a byproduct of real work — never a staged test")
        cursor_line = 2 + self.idx if self.tasks else None
        send({
            "kind": "set_text",
            "body": "\n".join(lines),
            "highlight_line": cursor_line,
            "scroll_to_line": cursor_line,
        })

    def render_detail(self):
        t = self.tasks[self.idx]
        n = t["answers"]
        tag = f"✓ {n} answer(s)" if n else "○ open — unanswered"
        body = f"● PROVE — {t['title']}   [{tag}]\n\n{t['text']}\n\n  Esc back · r reload"
        send({"kind": "set_text", "body": body, "highlight_line": None})

    # --- input ------------------------------------------------------------

    def handle_input(self, raw):
        if not raw:
            return
        if self.mode == "detail":
            if raw == "\x1b":
                self.mode = "list"
                self.render()
            elif raw == "r":
                self.load()
                self.render()
            return
        # list mode
        if raw == "\r" or raw == "\n":
            if self.tasks:
                self.mode = "detail"
                self.render()
        elif raw == "r":
            self.load()
            self.render()
        elif raw.endswith("[A"):  # up
            if self.tasks:
                self.idx = max(0, self.idx - 1)
                self.render()
        elif raw.endswith("[B"):  # down
            if self.tasks:
                self.idx = min(len(self.tasks) - 1, self.idx + 1)
                self.render()

    def handle_click(self, line, count):
        # body line 2 is the first task (header + blank above)
        idx = line - 2
        if 0 <= idx < len(self.tasks):
            self.idx = idx
            if count >= 2:
                self.mode = "detail"
            self.render()


def main():
    app = Prove()
    app.render()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            cmd = json.loads(line)
        except json.JSONDecodeError:
            continue
        kind = cmd.get("kind", "")
        if kind == "input":
            app.handle_input(cmd.get("bytes", ""))
        elif kind == "click":
            app.handle_click(int(cmd.get("line", 0)), int(cmd.get("count", 1)))
        elif kind == "cwd":
            p = cmd.get("path", "")
            if p:
                app.cwd = p
                app.load()
                app.render()
        elif kind == "init":
            pass
        elif kind == "close":
            break


if __name__ == "__main__":
    main()
