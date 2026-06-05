# terminite

**A terminal for the human–AI pair.**

Modern terminals are beautifully built for a person typing commands. None of them
are built for the *other* user now sitting at the terminal — the AI. terminite is.
It's a real GPU-rendered terminal — panes, tabs, scrollback, selection, find — and
it's a **shared room**: the human and one or more AI CLIs (Claude, Codex, Kimi,
Qwen, Antigravity) are *present to each other* at one surface, and can coordinate.

It's a personal project, built first for its owner and released to share the vision.

> **Status — early, and honest about it.** The hard part — the room — is built and
> validated: agents across five vendors see each other, talk, and coordinate (a
> file collision was resolved live, not in theory). The terminal around it is
> usable and stable. But a terminal earns its v1 by being *lived in*, and that
> stretch is just beginning. macOS is the dogfooded target; Linux likely works but
> isn't exercised yet. Expect rough edges, file them, watch them get fixed.

---

## Quick start

Requirements: macOS, the Rust toolchain (`cargo`, via [rustup](https://rustup.rs)),
and Xcode Command Line Tools (`xcode-select --install`).

```sh
cargo run --release          # build + launch
cargo install --path .       # put `terminite` on PATH (the window is also its own CLI)
./tools/build-app.sh         # bundle a macOS .app for Spotlight / Dock
```

The single `terminite` binary is both the window and the CLI — there are no
runtime dependencies on `python`/`jq`/`socat`.

## The room — connect your AI CLIs

terminite doesn't host the agents; each CLI installs a thin **faculty** into
*itself* — a skill + an MCP server — that teaches it about the room and connects
it. One command per vendor:

```sh
terminite install claude-terminite     # also: codex / kimi / qwen / agy
```

Now a plain `claude` (or `codex`, …) started in a terminite pane joins the room as
a colored presence, sees who else is here, streams its tool-calls so others can
watch it *work*, and can claim a file before editing it so two agents don't
clobber each other. Coordination is **human-led** — you drive by moving between
panes; terminite is the surface that makes the work visible. Nothing is forced:
agents work in parallel by default and only coordinate on a real collision.

## Configure

Settings live in a hot-reloaded config — edit it, click back into terminite, it
applies. Two ways in:

- **The Config pane** — open it from a pane's selector; navigate with `↑/↓`, edit
  on the row with `Enter`. The friendly path.
- **`terminite config`** — prints the file path and every key with its docs and
  default. The file (`~/.config/terminite/config.toml`) is also written
  self-documenting on first run.

Live keys: `Cmd +`/`Cmd -`/`Cmd 0` and `Cmd`+wheel to zoom, `Cmd`+`Shift`+`F` to
cycle the five bundled fonts. Colors (background, foreground, cursor, selection,
focus tint) are configurable hex and apply live.

## Modules

terminite has a small extension surface — modules render in a pane and talk to the
host over a simple wire. Bundled:

- **Config** — the settings pane above.
- **Nav / Preview / Edit** — a native file-navigation trio (a file list that
  publishes focus events; a previewer and an editor that react to them).
- **debug** — an observability pane for working on terminite itself.

Drop a new module into `~/.terminite/modules/` and it's picked up live.

## How it's built

Rust, rendering on `wgpu` / `winit` / `glyphon` (cosmic-text). The terminal core
is a **vendored, lightly modified `alacritty_terminal`** — the modifications
surface OSC 7 (working directory) and OSC 133 (shell-integration / command blocks)
to the model. Fonts (JetBrains Mono, Fira Code, DM Mono, PT Mono, Roboto Mono) are
bundled, so box-drawing and fallback are consistent on any machine.

## Docs

Deeper context lives in [`guide/`](guide/) — the [vision](guide/vision.md), the
[lounge thesis](guide/lounge-thesis.md) (where the room is reaching), the
[architecture](guide/architecture.md), and [history.md](guide/history.md) (the AI
partner's session-by-session log).

## License

Apache-2.0 — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). Includes a vendored,
modified `alacritty_terminal` (Apache-2.0) and bundles OFL-licensed fonts; both are
attributed in `NOTICE`.

---

*Built by the human–AI pair — Daniel Pastor and Claude (Anthropic).*
