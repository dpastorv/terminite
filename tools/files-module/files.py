#!/usr/bin/env python3
"""terminite module: Files — browse, preview, and edit in one pane.

A file navigator. Enter on a folder cds in; Enter on an image opens an inline
PREVIEW; Enter on anything else opens the EDITOR. Esc backs out of preview/edit
to the list (the editor guards unsaved changes). One module replacing the
former Nav / Preview / Edit trio.

Built for quick edits with your AI partner sharing the surface — not a full IDE.

Modes: browse | preview | edit. The dispatcher routes input to the active
component and owns the mode transitions; the components (browse.Nav, edit.Editor)
render themselves on the shared wire.
"""
import json
import os
import sys

from wire import send, log
from browse import Nav as Browser
from edit import Editor

# Image types the host can decode + display via `set_image`. Everything else
# (text, code, binary) opens in the editor, which handles binaries read-only.
IMAGE_EXTS = {"png", "jpg", "jpeg", "gif", "webp", "bmp"}


def is_image(path):
    _, dot, ext = os.path.basename(path).rpartition(".")
    return bool(dot) and ext.lower() in IMAGE_EXTS


class Files:
    def __init__(self):
        self.mode = "browse"        # browse | preview | edit
        self.browser = Browser()
        self.editor = Editor()
        self.preview_path = None
        self.back_confirm = False   # the dirty-edit "discard?" guard

    # --- rendering --------------------------------------------------------

    def render(self):
        if self.mode == "browse":
            self.browser.render()
        elif self.mode == "preview":
            send({"kind": "set_image", "path": self.preview_path})
        elif self.mode == "edit":
            if self.back_confirm:
                self.render_back_confirm()
            else:
                self.editor.render()

    def render_back_confirm(self):
        name = os.path.basename(self.editor.path or "(untitled)")
        send({
            "kind": "set_text",
            "body": (
                f"  {name} has unsaved changes.\n\n"
                "    s        save and go back\n"
                "    d        discard and go back\n"
                "    c / Esc  keep editing\n"
            ),
            "highlight_line": None,
        })

    # --- mode transitions -------------------------------------------------

    def open_path(self, path):
        """Route an opened file: images → preview, everything else → editor."""
        if is_image(path):
            self.preview_path = path
            self.mode = "preview"
        else:
            self.editor.load(path)
            self.back_confirm = False
            self.mode = "edit"
        self.render()

    def back_to_browse(self):
        self.mode = "browse"
        self.preview_path = None
        self.back_confirm = False
        # set_text (the browser body) replaces any image the host was showing.
        self.browser.render()

    # --- input ------------------------------------------------------------

    def handle_input(self, raw):
        if not raw:
            return
        if self.mode == "browse":
            sig = self.browser.handle_input(raw)
            if sig and sig[0] == "open":
                self.open_path(sig[1])
            return
        if self.mode == "preview":
            if raw == "\x1b":          # Esc closes the preview
                self.back_to_browse()
            return
        # edit mode
        if self.back_confirm:
            self._back_confirm_key(raw)
            return
        # Esc backs out — but ONLY from the editor's normal mode, so Esc still
        # cancels the editor's own find / save-as / confirm sub-modes.
        if raw == "\x1b" and self.editor.mode == "normal":
            if self.editor.dirty:
                self.back_confirm = True
                self.render_back_confirm()
            else:
                self.back_to_browse()
            return
        self.editor.handle_input(raw)

    def _back_confirm_key(self, raw):
        if raw in ("s", "S"):
            self.editor.save()
            if not self.editor.dirty:        # saved cleanly
                self.back_to_browse()
            else:                            # save bounced (e.g. external change)
                self.back_confirm = False    # hand control back to the editor
                self.editor.render()
        elif raw in ("d", "D"):
            self.back_to_browse()
        else:                                # c / Esc / anything → keep editing
            self.back_confirm = False
            self.editor.render()

    def handle_click(self, line, col, count):
        if self.mode == "browse":
            sig = self.browser.handle_click(line, count)
            if sig and sig[0] == "open":
                self.open_path(sig[1])
        elif self.mode == "edit" and not self.back_confirm:
            # Same body-coord mapping the editor uses: 3 header lines, col in
            # content cells (the host already subtracted the gutter).
            if line >= 3 and self.editor.lines:
                self.editor.row = min(line - 3, len(self.editor.lines) - 1)
                self.editor.col = max(
                    0, min(len(self.editor.lines[self.editor.row]), col)
                )
                self.editor.clear_selection()
                self.editor.render()


def main():
    app = Files()
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
        if kind == "init":
            log("files: init")
        elif kind == "input":
            app.handle_input(cmd.get("bytes", ""))
        elif kind == "cwd":
            path = cmd.get("path", "")
            # Keep both components' shell-cwd hint fresh.
            if path:
                app.browser.last_shell_cwd = path
            app.editor.last_shell_cwd = path or None
            if app.mode == "browse":
                app.browser.render()
        elif kind == "focus":
            # An external focus event (kept for compatibility) → open it.
            path = cmd.get("path", "")
            if path:
                app.open_path(path)
        elif kind == "click":
            app.handle_click(
                int(cmd.get("line", 0)),
                int(cmd.get("col", 0)),
                int(cmd.get("count", 1)),
            )
        elif kind == "close":
            break


if __name__ == "__main__":
    main()
