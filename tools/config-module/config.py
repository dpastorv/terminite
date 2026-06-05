#!/usr/bin/env python3
"""terminite module: Config.

Inline view + edit of terminite's settings. The host owns the
schema + the live values; this module just renders rows and
forwards edits back through the wire.

Wire (this module → host):
  set_text          render the body
  config_request    ask host for the current schema + values
  config_set        write one key (host re-fires `config` after)

Wire (host → this module):
  init              once at startup
  input             keystrokes
  config            list of keys + current values (schema snapshot)
  click             single/double click in the body
  close             tear down

Modes:
  browse            arrows nav, Enter to edit current key
  edit_text         typing a new string / number value
  edit_bool         no typing — Enter toggles instantly (handled
                    inline in browse mode)
  edit_enum         Enter cycles through the next known option

Honest limit: the editable surface covers what's in the schema —
enum value lists are hard-coded for bell_style. Adding new fields
to Config means extending both the schema (already required) and
the optional enum-options map here.
"""

import json
import os
import sys
from typing import Dict, List, Optional, Tuple, Any

HEADER_LINES = 2  # title + blank
GUTTER_PAD = 2

ENUM_OPTIONS: Dict[str, List[str]] = {
    "bell_style": ["visual", "none"],
}


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def log(message):
    send({"kind": "log", "message": message})


def coerce(kind: str, raw: str) -> Optional[Any]:
    """Parse a typed value out of a text input. Returns None for
    "doesn't fit" — the module shows an error and stays in edit
    mode so the user can fix the typo without losing their place."""
    raw = raw.strip()
    if kind in ("float",):
        try:
            return float(raw)
        except ValueError:
            return None
    if kind == "int":
        try:
            return int(raw)
        except ValueError:
            return None
    if kind == "bool":
        if raw.lower() in ("true", "1", "yes", "on"):
            return True
        if raw.lower() in ("false", "0", "no", "off"):
            return False
        return None
    # string + enum
    return raw


class Config:
    def __init__(self):
        self.keys: List[Dict[str, Any]] = []
        self.config_path: Optional[str] = None
        self.idx = 0
        self.mode = "browse"  # browse | edit_text
        self.edit_buffer = ""
        self.message = "(loading…)"

    # --- wire-driven state ------------------------------------------------

    def on_config(self, payload: dict):
        """Host pushed a fresh schema + values snapshot."""
        self.keys = payload.get("keys", [])
        self.config_path = payload.get("config_path")
        if self.idx >= len(self.keys):
            self.idx = max(0, len(self.keys) - 1)
        # Drop edit mode on a fresh snapshot — the host may have
        # rejected a `set` and the value didn't change.
        if self.mode == "edit_text":
            self.mode = "browse"
            self.edit_buffer = ""

    # --- rendering --------------------------------------------------------

    def status(self) -> str:
        path = self.config_path or "(no config path)"
        return f"terminite config — {path}"

    def prompt(self) -> str:
        if self.mode == "browse":
            if not self.keys:
                return self.message
            key = self.keys[self.idx]
            warn = "" if key.get("hot_reload") else "   restart-only"
            return f"↑/↓ navigate · Enter to edit · {key['name']}: {key['kind']}{warn}"
        if self.mode == "edit_text":
            key = self.keys[self.idx]
            # The value is now typed inline on the row itself; the header just
            # holds the hint so you're never editing somewhere you can't see.
            return f"editing {key['name']} — Enter to commit · Esc to cancel"
        return ""

    def render(self):
        out = [self.status(), self.prompt(), ""]
        gutter: List[str] = ["", "", ""]
        # Two-column-ish: name on the left, value on the right.
        name_w = max((len(k["name"]) for k in self.keys), default=20)
        for i, key in enumerate(self.keys):
            current = key.get("current")
            default = key.get("default")
            if current is None:
                disp_val = "—"
            elif isinstance(current, bool):
                disp_val = "true" if current else "false"
            else:
                disp_val = str(current)
            modified = current != default
            # ‣ marker on modified rows so you can scan what's been
            # tuned vs left at default.
            mod_mark = "‣ " if modified else "  "
            doc = key.get("doc", "")
            if self.mode == "edit_text" and i == self.idx:
                # Type the value right on its row — you always see what you're
                # changing, instead of editing in a header far from the row.
                line = f"{mod_mark}{key['name'].ljust(name_w)}  {self.edit_buffer}_"
            else:
                line = f"{mod_mark}{key['name'].ljust(name_w)}  {disp_val}"
                if doc:
                    line += f"   — {doc}"
            out.append(line)
            gutter.append(str(i + 1).rjust(3))
        if not self.keys:
            out.append("  (no config keys — host didn't send a snapshot)")
            gutter.append("")
        cursor_line = HEADER_LINES + 1 + self.idx  # 0=status, 1=prompt, 2=blank, 3+=rows
        send({
            "kind": "set_text",
            "body": "\n".join(out),
            "scroll_to_line": cursor_line,
            "gutter": gutter,
            # Highlight the current row in both modes — so the row you're
            # editing is the row that's lit.
            "highlight_line": cursor_line,
        })

    # --- input ------------------------------------------------------------

    def move(self, delta: int):
        if not self.keys:
            return
        self.idx = max(0, min(len(self.keys) - 1, self.idx + delta))
        self.render()

    def begin_edit(self):
        if not self.keys:
            return
        key = self.keys[self.idx]
        kind = key["kind"]
        # Bools toggle instantly. Enums cycle to next option.
        # Floats / ints / strings open a text prompt.
        if kind == "bool":
            current = bool(key.get("current", False))
            self._send_set(key["name"], not current)
            return
        if kind == "enum":
            opts = ENUM_OPTIONS.get(key["name"], [])
            if not opts:
                self.message = f"no enum options registered for {key['name']}"
                self.render()
                return
            current = str(key.get("current", opts[0]))
            try:
                next_idx = (opts.index(current) + 1) % len(opts)
            except ValueError:
                next_idx = 0
            self._send_set(key["name"], opts[next_idx])
            return
        # Text edit — seed buffer with current value.
        current = key.get("current", "")
        self.edit_buffer = "" if current is None else str(current)
        self.mode = "edit_text"
        self.render()

    def commit_edit(self):
        key = self.keys[self.idx]
        raw = self.edit_buffer
        value = coerce(key["kind"], raw)
        if value is None:
            self.message = f"invalid {key['kind']} value: {raw!r}"
            self.mode = "browse"
            self.edit_buffer = ""
            self.render()
            return
        self._send_set(key["name"], value)
        # Host re-fires `config` event after the set — we'll drop
        # edit mode there. Don't pre-clear here in case the set
        # bounced (rejected).

    def cancel_edit(self):
        self.mode = "browse"
        self.edit_buffer = ""
        self.render()

    def _send_set(self, name: str, value: Any):
        send({"kind": "config_set", "name": name, "value": value})

    def handle_input(self, raw: str):
        if not raw:
            return
        if self.mode == "edit_text":
            self._edit_keystroke(raw)
            return
        # Browse mode.
        if raw == "\r" or raw == "\n":
            self.begin_edit()
            return
        if raw.startswith("\x1b") and len(raw) > 1:
            if raw.endswith("[A"):
                self.move(-1)
            elif raw.endswith("[B"):
                self.move(1)
            elif raw.endswith("[H") or raw.endswith("OH"):
                self.idx = 0
                self.render()
            elif raw.endswith("[F") or raw.endswith("OF"):
                if self.keys:
                    self.idx = len(self.keys) - 1
                    self.render()
            elif raw.endswith("[5~"):
                self.move(-10)
            elif raw.endswith("[6~"):
                self.move(10)
            return

    def _edit_keystroke(self, raw: str):
        if raw == "\x1b":
            self.cancel_edit()
            return
        if raw == "\r" or raw == "\n":
            self.commit_edit()
            return
        if raw == "\x7f" or raw == "\b":
            self.edit_buffer = self.edit_buffer[:-1]
            self.render()
            return
        if raw.startswith("\x1b"):
            return  # ignore arrow keys etc. inside text-edit
        if all(ord(c) >= 32 for c in raw):
            self.edit_buffer += raw
            self.render()


def main():
    cfg = Config()
    cfg.render()
    # Ask the host for the schema + values up front. Host responds
    # with a `config` event we'll handle in the loop below.
    send({"kind": "config_request"})
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
            log("config: init")
            # init may arrive before our config_request — re-ask to
            # be safe (host dedupes).
            send({"kind": "config_request"})
        elif kind == "config":
            cfg.on_config(cmd)
            cfg.render()
        elif kind == "input":
            cfg.handle_input(cmd.get("bytes", ""))
        elif kind == "click":
            line_no = int(cmd.get("line", 0))
            # rows start at HEADER_LINES + 1 (status, prompt, blank)
            row_start = HEADER_LINES + 1
            if line_no >= row_start and cfg.keys:
                target = line_no - row_start
                if 0 <= target < len(cfg.keys):
                    cfg.idx = target
                    count = int(cmd.get("count", 1))
                    if count >= 2:
                        cfg.begin_edit()
                    else:
                        cfg.render()
        elif kind == "close":
            break
        # focus / cwd events ignored — config module is host-state-only.


if __name__ == "__main__":
    main()
