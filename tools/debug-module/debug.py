#!/usr/bin/env python3
"""terminite module: Debug.

A live view of terminite's internal state, rendered inside terminite
itself via the same module framework user modules use.

Two channels run in parallel:

1. **Host channel** (stdin/stdout, line-delimited JSON) — the
   pane-content protocol. We send `set_text` frames; the host renders.
2. **Proto channel** (Unix socket `~/.terminite/socket`) — the
   read-side of terminite's introspection API. We poll the `stats`
   verb and read whatever else we'd like.

The point: a module that uses *both* channels is what closes Phase 2.
The framework can host terminite's own observability; the protocol
surface is read-able to a module that's also a pane.

Refresh interval: 1 second. The proto's `stats` verb is a single-
snapshot read with a small sorted-frames computation; cheap.
"""

from __future__ import annotations

import json
import os
import socket
import sys
import threading
import time

SOCKET_PATH = os.environ.get("TERMINITE_SOCKET") or os.path.expanduser(
    "~/.terminite/socket"
)
REFRESH_SEC = 1.0


def send(msg: dict) -> None:
    """Push one frame to the host."""
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def fetch_stats() -> dict:
    """One round trip to terminite's proto socket. Short timeout — a
    stalled host shouldn't freeze the debug pane."""
    try:
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(1.0)
        s.connect(SOCKET_PATH)
        s.send(b'{"id":1,"method":"stats"}\n')
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(64 * 1024)
            if not chunk:
                break
            buf += chunk
        s.close()
        first_line = buf.split(b"\n", 1)[0]
        return json.loads(first_line.decode())
    except Exception as e:
        return {"error": str(e)}


def fmt_bytes(n: int | None) -> str:
    if not n:
        return "?"
    if n >= 1024 * 1024 * 1024:
        return f"{n / (1024 ** 3):.2f} GB"
    if n >= 1024 * 1024:
        return f"{n / (1024 ** 2):.1f} MB"
    if n >= 1024:
        return f"{n / 1024:.1f} KB"
    return f"{n} B"


def format_stats(stats: dict) -> str:
    if "error" in stats:
        return (
            "terminite debug — can't reach the proto socket\n"
            f"  {SOCKET_PATH}\n"
            f"  error: {stats['error']}\n\n"
            "is terminite running? (this module talks back to its host\n"
            "via the same socket any other proto client uses.)"
        )

    version = stats.get("version", "?")
    rss = fmt_bytes(stats.get("peak_rss_bytes"))
    frame = stats.get("frame", {}) or {}
    frames = frame.get("frames_observed", 0)
    avg = frame.get("avg_ms", 0.0)
    p99 = frame.get("p99_ms", 0.0)
    fmax = frame.get("max_ms", 0.0)
    samples = frame.get("recent_samples", 0)
    sub = "yes" if stats.get("subscriber_connected") else "no"

    lines = [
        f"terminite v{version} — debug",
        "─" * 40,
        f"  peak rss:         {rss}",
        f"  frames observed:  {frames}",
        f"  frame avg / p99 / max:",
        f"    {avg:.2f} / {p99:.2f} / {fmax:.2f} ms   (n={samples})",
        f"  proto subscriber: {sub}",
        "",
        "tabs:",
    ]
    for t in stats.get("tabs", []) or []:
        lines.append(
            f"  #{t['tab_id']}  {t['cols']}x{t['rows']}  "
            f"blocks={t['blocks']}  "
            f"open={t['open_block'] if t.get('open_block') is not None else '-'}  "
            f"cursor={t['cursor_block'] if t.get('cursor_block') is not None else '-'}  "
            f"image={'yes' if t['has_image'] else 'no'}"
        )
        title = t.get("title", "")
        if title:
            lines.append(f"     title: {title}")
    if not stats.get("tabs"):
        lines.append("  (no tabs)")
    lines.append("")
    lines.append(f"polling every {REFRESH_SEC:.1f}s · pick another kind from the dropdown to leave")
    return "\n".join(lines)


def stdin_drainer() -> None:
    """We don't act on host input in this v1 — but a parent pipe that
    nobody reads from blocks the host writer. Drain quietly."""
    for _ in sys.stdin:
        pass


def main() -> None:
    threading.Thread(target=stdin_drainer, daemon=True).start()
    # First frame immediate so the user isn't staring at the
    # placeholder while we wait for the first poll.
    send({"kind": "set_text", "body": format_stats(fetch_stats())})
    while True:
        time.sleep(REFRESH_SEC)
        send({"kind": "set_text", "body": format_stats(fetch_stats())})


if __name__ == "__main__":
    main()
