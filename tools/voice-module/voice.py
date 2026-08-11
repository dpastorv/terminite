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

Slash commands (chat-native — `?` and chords collide with real questions):
  /help    the key legend + what this pane is
  /clear   clear the chat (same as Ctrl+L)
  /soul    print the voice's active identity (`terminite voice soul`)
  /models  voice + model status (`terminite voice status`)

An empty chat shows a welcome block with the same instructions, so the
pane explains itself on first open.

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

KEY_LEGEND = [
    "  Enter    ask                  Esc      clear the input line",
    "  Ctrl+L   clear the chat       Ctrl+C   cancel an answer",
    "  /help    these instructions   /soul    who I am",
    "  /models  models + status      /clear   clear the chat",
]

WELCOME = [
    "I'm terminite's own voice — a small model living inside the app.",
    "Local, offline, CPU-only, private. Nothing runs between questions:",
    "each answer loads the model fresh, speaks, and goes back to sleep,",
    "so this pane costs nothing while it sits open.",
    "",
    "Ask me about your config, who's in the room, or how my features",
    "work. The first words of an answer take a few seconds — that's the",
    "model waking up.",
    "",
] + KEY_LEGEND


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
        if not self.transcript and not self.busy:
            lines += WELCOME + [""]
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
        state = "thinking — Ctrl+C cancels" if self.busy else "Enter ask · /help for keys"
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
                    if q.startswith("/"):
                        self.command(q)
                    else:
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

    # ── slash commands ─────────────────────────────────────────────────
    # Local utility only — a command never reaches the model and never
    # touches anything beyond terminite's own read-only CLI verbs.

    def command(self, q):
        """Called with the lock held. `q` starts with '/'."""
        cmd = q.split()[0].lower()
        if cmd == "/clear":
            self.transcript = []
        elif cmd == "/help":
            self.transcript += [PROMPT + q, ""] + KEY_LEGEND + [""]
        elif cmd in ("/soul", "/models"):
            self.transcript += [PROMPT + q, ""]
            verb = ["voice", "soul"] if cmd == "/soul" else ["voice", "status"]
            out = self.run_verb(verb)
            self.transcript += wrap(out, indent="  ") + [""]
        else:
            self.status = f"unknown command {cmd} — /help lists them"
        if len(self.transcript) > MAX_TRANSCRIPT:
            self.transcript = self.transcript[-MAX_TRANSCRIPT:]

    def run_verb(self, verb):
        """One fast, read-only `terminite <verb>` call; output as text."""
        if not self.bin:
            return "terminite binary not found — is the app installed?"
        try:
            r = subprocess.run(
                [self.bin] + verb, capture_output=True, timeout=10
            )
        except (OSError, subprocess.TimeoutExpired) as e:
            return f"can't run terminite {' '.join(verb)}: {e}"
        text = (r.stdout or b"").decode("utf-8", errors="replace").strip()
        if not text:
            text = (r.stderr or b"").decode("utf-8", errors="replace").strip()
        return text or "(no output)"

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
