#!/usr/bin/env python3
"""terminite module: Preview.

Read-only viewer. Renders whatever was most recently focused by any
other module on the same host. The host pipes `focus` events to us:

    {"kind":"focus","path":"/abs/path"}

Dispatch is by extension:
  - PNG / JPG / GIF / WEBP / BMP  →  set_image (host decodes + uploads)
  - MD / Markdown                 →  prettified text (headings, lists, code)
  - HTML / HTM                    →  text with tags stripped + entities decoded
  - Code, configs, plain text     →  raw text body
  - Everything else               →  "preview not yet supported" message

Real rich rendering for MD/HTML (bold/italic/code spans, headings at
different sizes) would need a styling extension on the module
protocol — currently `set_text` is one color, one weight. That's a
later bundle; for now MD/HTML render as honest plaintext with light
visual polish.

Wire (module → host):
    {"kind":"set_text",  "body":"…"}
    {"kind":"set_image", "path":"/abs/file.png"}
"""

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from html.parser import HTMLParser
from html import unescape

MAX_BYTES = 1_000_000
SNIFF_BYTES = 4096

TEXT_EXTS = {
    "txt", "rst", "log",
    "py", "rs", "js", "jsx", "ts", "tsx", "go", "rb", "php", "java",
    "c", "h", "cc", "cpp", "hpp", "cs", "swift", "kt", "scala", "lua",
    "sh", "bash", "zsh", "fish", "ps1",
    "json", "toml", "yaml", "yml", "ini", "cfg", "conf", "env",
    "css", "scss", "sass",
    "sql", "graphql", "proto",
    "gitignore", "gitattributes", "editorconfig", "dockerignore",
    "csv", "tsv", "xml",
}

MD_EXTS = {"md", "markdown", "mdown", "mkd"}
HTML_EXTS = {"html", "htm", "xhtml"}

# Host can decode these via the `image` crate (jpeg/gif/webp/bmp/png).
IMAGE_EXTS = {"png", "jpg", "jpeg", "gif", "webp", "bmp"}

# Acknowledged-but-not-yet types — render a friendly placeholder so
# the user knows why their click didn't preview.
IMAGE_TODO = {"svg", "tiff", "tif", "ico", "avif", "heic", "heif"}

VIDEO_EXTS = {"mp4", "mov", "mkv", "webm", "avi", "m4v"}

NOT_YET = {
    "pdf": "PDF",
    "doc": "Word document", "docx": "Word document",
    "xls": "spreadsheet", "xlsx": "spreadsheet",
    "ppt": "slide deck", "pptx": "slide deck",
    "mp3": "audio", "wav": "audio", "flac": "audio", "m4a": "audio", "ogg": "audio",
    "zip": "archive", "tar": "archive", "gz": "archive", "tgz": "archive", "bz2": "archive",
    "7z": "archive", "rar": "archive",
}

# qlmanage is the macOS-built-in Quick Look thumbnailer — same engine
# Finder uses for file previews. It handles most video formats and
# many others (psd, sketch, …) we might add later. We cache the
# generated PNG keyed by abs path + mtime so we don't re-thumbnail
# the same file on repeated focus.
QLMANAGE = shutil.which("qlmanage")
THUMB_CACHE = os.path.join(tempfile.gettempdir(), "terminite-preview-thumbs")
THUMB_SIZE = 1200  # px on the longer edge — wide enough for any pane


# --- wire helpers -----------------------------------------------------------

def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def log(message):
    send({"kind": "log", "message": message})


def text(body):
    send({"kind": "set_text", "body": body})


# --- placeholders -----------------------------------------------------------

def render_idle():
    text(
        "Preview\n"
        "\n"
        "Open a Nav pane and press Enter on a file —\n"
        "this pane will mirror what's selected."
    )


def render_unsupported(path, label, ext):
    name = os.path.basename(path) or path
    body = (
        f"{path}\n\n"
        f"Preview not yet supported for {label} files (.{ext}).\n"
        f"\n"
        f"Adding support is a deliberate add-as-we-need-it path.\n"
        f"See TEXT_EXTS / IMAGE_EXTS / NOT_YET in preview.py."
    )
    text(body)


def render_unknown(path, ext):
    body = (
        f"{path}\n\n"
        f"Preview not available — unknown format (.{ext}).\n"
        f"\n"
        f"Open as text manually if you think it should be readable."
    )
    text(body)


def render_image_todo(path, ext):
    body = (
        f"{path}\n\n"
        f"{ext.upper()} preview not yet supported.\n"
        f"\n"
        f"Host currently decodes png/jpg/gif/webp/bmp.\n"
        f"SVG needs a vector renderer; HEIC/AVIF need separate decoders."
    )
    text(body)


# --- text reading -----------------------------------------------------------

def looks_binary(blob):
    return b"\x00" in blob


def read_text(path):
    """Read file as UTF-8 text, capped at MAX_BYTES. Returns
    (text, size, truncated) or None on read error / binary file."""
    try:
        size = os.path.getsize(path)
    except OSError as e:
        text(f"{path}\n\n(error: {e})")
        return None
    try:
        with open(path, "rb") as f:
            head = f.read(min(size, MAX_BYTES))
    except OSError as e:
        text(f"{path}\n\n(error: {e})")
        return None
    if looks_binary(head[:SNIFF_BYTES]):
        text(f"{path}\n\n(binary file — {size} bytes)\n\nUse a hex viewer to inspect.")
        return None
    try:
        decoded = head.decode("utf-8")
    except UnicodeDecodeError:
        decoded = head.decode("utf-8", errors="replace")
    return decoded, size, size > MAX_BYTES


def render_text(path):
    got = read_text(path)
    if got is None:
        return
    body, size, truncated = got
    tail = f"\n\n… (truncated at {MAX_BYTES} bytes — file is {size})" if truncated else ""
    text(f"{path}\n\n{body}{tail}")


# --- markdown prettification ------------------------------------------------
# Plain text, single color/weight. We can't bold or shrink fonts so
# we use case + underline + box drawing to signal structure visually.

H1 = re.compile(r"^# +(.+?)\s*$")
H2 = re.compile(r"^## +(.+?)\s*$")
H3 = re.compile(r"^### +(.+?)\s*$")
H4 = re.compile(r"^####+ +(.+?)\s*$")
BULLET = re.compile(r"^(\s*)[-*+] +(.+)$")
BLOCKQUOTE = re.compile(r"^> ?(.*)$")
LINK = re.compile(r"\[([^\]]+)\]\(([^)]+)\)")
IMAGE_INLINE = re.compile(r"!\[([^\]]*)\]\(([^)]+)\)")
BOLD = re.compile(r"\*\*(.+?)\*\*")
EMPH_STAR = re.compile(r"(?<!\*)\*(?!\s)(.+?)(?<!\s)\*(?!\*)")
EMPH_UNDER = re.compile(r"(?<!_)_(?!\s)(.+?)(?<!\s)_(?!_)")
HR = re.compile(r"^(-{3,}|\*{3,}|_{3,})\s*$")


def prettify_inline(line):
    """Inline span cleanup — links, images, bold/italic markers."""
    line = IMAGE_INLINE.sub(lambda m: f"[image: {m.group(1) or m.group(2)}]", line)
    line = LINK.sub(lambda m: f"{m.group(1)} → {m.group(2)}", line)
    line = BOLD.sub(lambda m: m.group(1).upper(), line)
    line = EMPH_STAR.sub(lambda m: m.group(1), line)
    line = EMPH_UNDER.sub(lambda m: m.group(1), line)
    return line


def prettify_md(src):
    """Light prettification — headings, lists, blockquotes, fenced
    code. Keeps the body recognizably markdown but easier to scan."""
    out = []
    in_fence = False
    for raw in src.split("\n"):
        if raw.startswith("```"):
            if not in_fence:
                out.append("─" * 60)
                in_fence = True
            else:
                out.append("─" * 60)
                in_fence = False
            continue
        if in_fence:
            out.append("  " + raw)
            continue
        if HR.match(raw):
            out.append("─" * 60)
            continue
        m = H1.match(raw)
        if m:
            title = prettify_inline(m.group(1)).upper()
            out.append(title)
            out.append("=" * len(title))
            continue
        m = H2.match(raw)
        if m:
            title = prettify_inline(m.group(1)).upper()
            out.append(title)
            out.append("─" * len(title))
            continue
        m = H3.match(raw)
        if m:
            out.append(prettify_inline(m.group(1)).upper())
            continue
        m = H4.match(raw)
        if m:
            out.append("  " + prettify_inline(m.group(1)))
            continue
        m = BULLET.match(raw)
        if m:
            indent = m.group(1)
            out.append(f"{indent}• {prettify_inline(m.group(2))}")
            continue
        m = BLOCKQUOTE.match(raw)
        if m:
            out.append(f"│ {prettify_inline(m.group(1))}")
            continue
        out.append(prettify_inline(raw))
    return "\n".join(out)


def render_md(path):
    got = read_text(path)
    if got is None:
        return
    body, size, truncated = got
    pretty = prettify_md(body)
    tail = f"\n\n… (truncated at {MAX_BYTES} bytes — file is {size})" if truncated else ""
    text(f"{path}\n\n{pretty}{tail}")


# --- html stripping ---------------------------------------------------------

class HtmlToText(HTMLParser):
    """Crude HTML→text. Drops scripts/styles entirely; turns block
    tags into newlines; <a> shows 'text → href' like our MD pass.
    Not a renderer — just enough to read prose."""

    BLOCK_TAGS = {
        "p", "div", "section", "article", "header", "footer", "main", "nav",
        "h1", "h2", "h3", "h4", "h5", "h6",
        "ul", "ol", "li", "tr", "td", "th", "table",
        "blockquote", "pre", "hr", "form", "fieldset",
    }
    HEADING_TAGS = {"h1", "h2", "h3", "h4", "h5", "h6"}
    SKIP_TAGS = {"script", "style", "noscript", "template"}

    def __init__(self):
        super().__init__()
        self.buf = []
        self.skip_depth = 0
        self.tag_stack = []
        self.pending_href = None

    def handle_starttag(self, tag, attrs):
        self.tag_stack.append(tag)
        if tag in self.SKIP_TAGS:
            self.skip_depth += 1
            return
        if self.skip_depth:
            return
        if tag == "br":
            self.buf.append("\n")
            return
        if tag == "li":
            self.buf.append("\n• ")
            return
        if tag == "hr":
            self.buf.append("\n" + ("─" * 60) + "\n")
            return
        if tag == "a":
            for k, v in attrs:
                if k == "href":
                    self.pending_href = v
                    break
        if tag == "img":
            alt = ""
            src = ""
            for k, v in attrs:
                if k == "alt":
                    alt = v
                elif k == "src":
                    src = v
            self.buf.append(f"[image: {alt or src}]")
            return
        if tag in self.HEADING_TAGS:
            self.buf.append("\n\n")
            return
        if tag in self.BLOCK_TAGS:
            self.buf.append("\n")

    def handle_endtag(self, tag):
        if self.tag_stack and self.tag_stack[-1] == tag:
            self.tag_stack.pop()
        if tag in self.SKIP_TAGS:
            if self.skip_depth:
                self.skip_depth -= 1
            return
        if self.skip_depth:
            return
        if tag == "a" and self.pending_href:
            self.buf.append(f" → {self.pending_href}")
            self.pending_href = None
        if tag in self.HEADING_TAGS:
            self.buf.append("\n")
        elif tag in self.BLOCK_TAGS:
            self.buf.append("\n")

    def handle_data(self, data):
        if self.skip_depth:
            return
        # In heading context, uppercase to signal hierarchy visually.
        if any(t in self.HEADING_TAGS for t in self.tag_stack):
            self.buf.append(data.upper())
        else:
            self.buf.append(data)

    def handle_entityref(self, name):
        if not self.skip_depth:
            self.buf.append(unescape(f"&{name};"))

    def handle_charref(self, name):
        if not self.skip_depth:
            self.buf.append(unescape(f"&#{name};"))

    def text(self):
        raw = "".join(self.buf)
        # Collapse runs of 3+ newlines to 2.
        return re.sub(r"\n{3,}", "\n\n", raw).strip()


def render_html(path):
    got = read_text(path)
    if got is None:
        return
    body, size, truncated = got
    parser = HtmlToText()
    try:
        parser.feed(body)
    except Exception as e:  # malformed HTML — don't crash the pane
        log(f"preview: html parse error: {e}")
        text(f"{path}\n\n(HTML parse error — showing raw source)\n\n{body}")
        return
    stripped = parser.text()
    tail = f"\n\n… (truncated at {MAX_BYTES} bytes — file is {size})" if truncated else ""
    text(f"{path}\n\n{stripped}{tail}")


# --- video thumbnail via macOS qlmanage -------------------------------------

def thumb_path_for(path):
    """Stable cache filename keyed by abs path + mtime + size."""
    try:
        st = os.stat(path)
    except OSError:
        return None
    key = f"{os.path.abspath(path)}:{st.st_mtime_ns}:{st.st_size}".encode("utf-8")
    digest = hashlib.sha256(key).hexdigest()[:16]
    return os.path.join(THUMB_CACHE, f"{digest}.png")


def make_video_thumb(path):
    """Generate a first-frame thumbnail PNG with macOS Quick Look.
    Returns the cached PNG path on success, None otherwise."""
    if QLMANAGE is None:
        return None
    cached = thumb_path_for(path)
    if cached is None:
        return None
    if os.path.exists(cached):
        return cached
    try:
        os.makedirs(THUMB_CACHE, exist_ok=True)
    except OSError as e:
        log(f"preview: thumb cache mkdir failed: {e}")
        return None
    # qlmanage emits to <outdir>/<basename>.png; we move/rename to
    # our content-addressed cache path so subsequent focuses hit cache
    # without re-running qlmanage.
    with tempfile.TemporaryDirectory(prefix="terminite-ql-") as workdir:
        try:
            res = subprocess.run(
                [QLMANAGE, "-t", "-s", str(THUMB_SIZE), "-o", workdir, path],
                capture_output=True,
                timeout=10,
            )
        except subprocess.TimeoutExpired:
            log(f"preview: qlmanage timed out for {path}")
            return None
        except OSError as e:
            log(f"preview: qlmanage spawn failed: {e}")
            return None
        if res.returncode != 0:
            log(f"preview: qlmanage exit {res.returncode} for {path}")
            return None
        # qlmanage names the output <basename>.png — find the first PNG it left.
        candidates = [f for f in os.listdir(workdir) if f.endswith(".png")]
        if not candidates:
            log(f"preview: qlmanage produced no png for {path}")
            return None
        produced = os.path.join(workdir, candidates[0])
        try:
            shutil.copyfile(produced, cached)
        except OSError as e:
            log(f"preview: thumb copy failed: {e}")
            return None
    return cached


def render_video(path, ext):
    thumb = make_video_thumb(path)
    if thumb is not None:
        send({"kind": "set_image", "path": thumb})
        return
    # Fallback: no qlmanage, or it failed for this format.
    body = (
        f"{path}\n\n"
        f"Video preview ({ext.upper()}): first-frame thumbnail unavailable.\n"
        f"\n"
        f"Inline playback isn't supported in v1 — a real player would\n"
        f"need a decoder + frame timing + audio out. For now, open\n"
        f"externally with `open '{path}'`."
    )
    text(body)


# --- dispatch ---------------------------------------------------------------

def ext_of(path):
    base = os.path.basename(path)
    if base.startswith("."):
        return base[1:].lower()
    _, dot, ext = base.rpartition(".")
    return ext.lower() if dot else ""


def render(path):
    if not os.path.exists(path):
        text(f"{path}\n\n(not found)")
        return
    if os.path.isdir(path):
        text(f"{path}\n\n(directory — open in Nav to browse)")
        return
    ext = ext_of(path)
    if ext in IMAGE_EXTS:
        log(f"preview: image {path}")
        send({"kind": "set_image", "path": os.path.abspath(path)})
        return
    if ext in IMAGE_TODO:
        render_image_todo(path, ext)
        return
    if ext in MD_EXTS:
        render_md(path)
        return
    if ext in HTML_EXTS:
        render_html(path)
        return
    if ext in VIDEO_EXTS:
        log(f"preview: video {path}")
        render_video(path, ext)
        return
    if ext in TEXT_EXTS or ext == "":
        render_text(path)
        return
    if ext in NOT_YET:
        render_unsupported(path, NOT_YET[ext], ext)
        return
    render_unknown(path, ext)


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
