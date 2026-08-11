#!/usr/bin/env python3
"""terminite module: Voice — talk to the app itself, in a pane.

A chat surface over `terminite ask`. Each submitted question spawns one
short-lived `ask` process (the model loads, answers, exits — nothing stays
resident; the pane is only a window onto that call). The answer streams into
the transcript as it generates.

Modes:
  idle      typing edits the input line; Enter submits
  thinking  a question is in flight — the answer streams in live;
            Ctrl+C cancels it; typing still edits the next question

Keys:
  Enter   ask (when input is non-empty and nothing is in flight)
  Esc     clear the input line
  Ctrl+L  clear the whole transcript
  Ctrl+C  cancel the in-flight answer

Wire (this module → host):
  set_text   full-frame render: header, transcript, prompt line
  log        diagnostics

Wire (host → this module):
  init       once at startup
  input      keystrokes (raw byte sequences as strings)
  close      shut down (kills any in-flight ask)

Bounds: transcript capped (oldest turns dropped), per-answer byte cap,
input-line length cap. The module holds no model state at all — every
question is a fresh, process-scoped `terminite ask`.
"""

import json
import os
import shutil
import subprocess
import sys
import textwrap
import threading

WRAP = 76            # no resize events on the module wire — fixed wrap
MAX_INPUT = 2000     # chars in the input line
MAX_ANSWER = 64 * 1024   # bytes per answer (ask's own cap is far lower)
MAX_TRANSCRIPT = 600     # rendered lines kept; oldest dropped past this

PROMPT = "you ❯ "
VOICE = "terminite ❯ "


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def find_terminite():
    """The binary that hosts us. Env override first, then the app bundle
    (PATH is minimal when the app launched from Dock/Spotlight), then PATH."""
    for cand in (
        os.environ.get("TERMINITE_BIN", ""),
        "/Applications/Terminite.app/Contents/MacOS/terminite",
        os.path.expanduser("~/Applications/Terminite.app/Contents/MacOS/terminite"),
    ):
        if cand and os.access(cand, os.X_OK):
            return cand
    return shutil.which("terminite")


def wrap(text, indent=""):
    """Wrap a block of text, preserving blank lines between paragraphs."""
    out = []
    for para in text.split("\n"):
        if not para.strip():
            out.append("")
            continue
        out.extend(
            textwrap.wrap(
                para, WRAP, initial_indent=indent, subsequent_indent=indent
            )
            or [""]
        )
    return out


class Voice:
    def __init__(self):
        self.lock = threading.Lock()
        self.bin = find_terminite()
        self.transcript = []      # rendered lines of finished turns
        self.answer_bytes = b""   # the in-flight answer, raw
        self.pending_q = ""       # the in-flight question (shown while busy)
        self.input_buf = ""
        self.busy = False
        self.proc = None
        self.status = "" if self.bin else "terminite binary not found — is the app installed?"

    # ── rendering ──────────────────────────────────────────────────────

    def render(self):
        lines = ["◆ Voice — terminite's own; local, offline, CPU-only", ""]
        lines += self.transcript
        if self.busy:
            lines += wrap(PROMPT + self.pending_q)
            answer = self.answer_bytes.decode("utf-8", errors="replace").strip()
            if answer:
                lines += wrap(VOICE + answer)
            else:
                lines += [VOICE + "…"]
            lines.append("")
        if self.status:
            lines += ["· " + self.status, ""]
        state = "thinking — Ctrl+C cancels" if self.busy else "Enter ask · Esc clear · Ctrl+L clear chat"
        lines += ["❯ " + self.input_buf + "▏", "  " + state]
        send({"kind": "set_text", "body": "\n".join(lines), "scroll_to_line": len(lines) - 1})

    # ── input ──────────────────────────────────────────────────────────

    def handle_input(self, data):
        if not data:
            return
        if data.startswith("\x1b") and len(data) > 1:
            return  # arrows / function keys — no cursor movement in lean tier
        with self.lock:
            if data in ("\r", "\n"):
                q = self.input_buf.strip()
                if q and not self.busy:
                    self.input_buf = ""
                    self.status = ""
                    self.submit(q)
            elif data in ("\x7f", "\x08"):
                self.input_buf = self.input_buf[:-1]
            elif data == "\x1b":
                self.input_buf = ""
            elif data == "\x0c":  # Ctrl+L
                self.transcript = []
                self.status = ""
            elif data == "\x03":  # Ctrl+C
                if self.proc is not None:
                    try:
                        self.proc.kill()
                    except OSError:
                        pass
                    self.status = "cancelled"
            else:
                clean = "".join(c for c in data if c.isprintable() or c == " ")
                if clean and len(self.input_buf) < MAX_INPUT:
                    self.input_buf += clean
            self.render()

    # ── asking ─────────────────────────────────────────────────────────

    def submit(self, q):
        """Called with the lock held. Spawns the one-shot ask + its reader."""
        if not self.bin:
            self.status = "terminite binary not found — is the app installed?"
            return
        self.busy = True
        self.pending_q = q
        self.answer_bytes = b""
        try:
            self.proc = subprocess.Popen(
                [self.bin, "ask", q],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except OSError as e:
            self.busy = False
            self.proc = None
            self.status = f"can't run ask: {e}"
            return
        threading.Thread(target=self._reader, args=(self.proc,), daemon=True).start()

    def _reader(self, proc):
        """Stream the answer in; finish the turn when the process exits."""
        fd = proc.stdout.fileno()
        while True:
            try:
                chunk = os.read(fd, 4096)
            except OSError:
                break
            if not chunk:
                break
            with self.lock:
                if len(self.answer_bytes) < MAX_ANSWER:
                    self.answer_bytes += chunk
                self.render()
        code = proc.wait()
        err = b""
        if proc.stderr is not None:
            try:
                err = proc.stderr.read() or b""
            except OSError:
                pass
        with self.lock:
            answer = self.answer_bytes.decode("utf-8", errors="replace").strip()
            self.transcript += wrap(PROMPT + self.pending_q)
            if answer:
                self.transcript += wrap(VOICE + answer)
            if code != 0:
                detail = err.decode("utf-8", errors="replace").strip()
                if detail and self.status != "cancelled":
                    self.transcript += wrap("· " + detail)
            self.transcript.append("")
            if len(self.transcript) > MAX_TRANSCRIPT:
                self.transcript = self.transcript[-MAX_TRANSCRIPT:]
            self.busy = False
            self.proc = None
            self.pending_q = ""
            self.answer_bytes = b""
            self.render()


def main():
    app = Voice()
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
        elif kind == "init":
            pass
        elif kind == "close":
            break
    if app.proc is not None:
        try:
            app.proc.kill()
        except OSError:
            pass


if __name__ == "__main__":
    main()
