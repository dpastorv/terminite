#!/usr/bin/env python3
"""terminite module: Preview.

Read-only viewer. Renders whatever was most recently focused by any
other module on the same host. The host pipes `focus` events to us:

    {"kind":"focus","path":"/abs/path"}

For files we show the first MAX_LINES of text (binary heuristic just
checks for NUL in the first 4 KiB). For directories we show a
listing. Errors get rendered into the pane too — silent failure makes
the pair-debugging harder than it needs to be.
"""

import json
import os
import sys

MAX_LINES = 400
MAX_BYTES = 256 * 1024
SNIFF_BYTES = 4096


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def log(message):
    send({"kind": "log", "message": message})


def render_idle():
    body = (
        "Preview\n"
        "\n"
        "Open a Nav pane and press Enter on a file —\n"
        "this pane will mirror what's selected."
    )
    send({"kind": "set_text", "body": body})


def render_dir(path):
    try:
        names = sorted(os.listdir(path))
    except OSError as e:
        send({"kind": "set_text", "body": f"{path}\n\n(error: {e})"})
        return
    header = f"{path}/\n\n"
    body = header + "\n".join(names) if names else header + "(empty)"
    send({"kind": "set_text", "body": body})


def looks_binary(blob: bytes) -> bool:
    return b"\x00" in blob


def render_file(path):
    try:
        size = os.path.getsize(path)
    except OSError as e:
        send({"kind": "set_text", "body": f"{path}\n\n(error: {e})"})
        return
    try:
        with open(path, "rb") as f:
            head = f.read(min(size, MAX_BYTES))
    except OSError as e:
        send({"kind": "set_text", "body": f"{path}\n\n(error: {e})"})
        return
    sniff = head[:SNIFF_BYTES]
    if looks_binary(sniff):
        send({"kind": "set_text", "body": f"{path}\n\n(binary file — {size} bytes)"})
        return
    try:
        text = head.decode("utf-8")
    except UnicodeDecodeError:
        text = head.decode("utf-8", errors="replace")
    lines = text.split("\n")
    truncated = len(lines) > MAX_LINES or size > MAX_BYTES
    shown = "\n".join(lines[:MAX_LINES])
    tail = f"\n\n… ({len(lines) - MAX_LINES} more lines)" if truncated else ""
    body = f"{path}\n\n{shown}{tail}"
    send({"kind": "set_text", "body": body})


def render(path):
    if not os.path.exists(path):
        send({"kind": "set_text", "body": f"{path}\n\n(not found)"})
        return
    if os.path.isdir(path):
        render_dir(path)
    else:
        render_file(path)


def main():
    render_idle()
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
                log(f"preview: focus {path}")
                render(path)
        elif kind == "init":
            log("preview: init")
        elif kind == "close":
            break


if __name__ == "__main__":
    main()
