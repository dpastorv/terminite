#!/usr/bin/env python3
"""terminite module: Hello.

Proves the Bundle 6 step-2b wire end to end.

Protocol — line-delimited JSON, both directions:

  terminite → us (stdin)
    {"kind":"init",  "tab_id":N}
    {"kind":"input", "bytes":"…"}
    {"kind":"close"}

  us → terminite (stdout)
    {"kind":"set_text", "body":"…"}
    {"kind":"log",      "message":"…"}

We keep a small buffer of recent keystrokes and render them back into
the pane. Pure echo — no shell, no PTY. The point is to show that
*anything* can be a pane.
"""

import json
import sys

# Most-recent keystrokes, capped so a long session doesn't blow memory.
BUF_CAP = 240
buf = []


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


HEADER = "hello from a module 👋\n\nType in this pane — your input echoes here.\nPick another kind from the dropdown to leave."


def render():
    joined = "".join(buf)
    body = HEADER + "\n\n— —\n\n" + joined
    send({"kind": "set_text", "body": body})


def main():
    # First frame so the user sees something immediately, before they
    # type anything.
    render()
    for line in sys.stdin:
        try:
            cmd = json.loads(line.strip())
        except json.JSONDecodeError:
            continue
        kind = cmd.get("kind", "")
        if kind == "init":
            send({"kind": "log", "message": "hello: init"})
        elif kind == "input":
            buf.append(cmd.get("bytes", ""))
            joined = "".join(buf)
            if len(joined) > BUF_CAP:
                joined = joined[-BUF_CAP:]
                buf[:] = [joined]
            render()
        elif kind == "close":
            send({"kind": "log", "message": "hello: bye"})
            break


if __name__ == "__main__":
    main()
