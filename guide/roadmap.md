# Roadmap

The map of what terminite is and what it's becoming. Honest, opinionated,
written for the human-AI pair to read together. Promote items from
`friction-log.md` and `opinions.md` when the felt cost is real; cross out what
ships.

Four phases. Each one earns the right to the next.

- **Phase 1 — Earn the terminal**: be a terminal a careful person would
  actually live in. *Shipped.*
- **Phase 2 — Be the pair's terminal**: blocks as shared coordinates between
  human and AI. *Shipped.*
- **Phase 3 — Be lovable to live in**: layouts, modules, configuration, the
  personal-usable thread. *Shipped.*
- **Phase 4 — Be the lounge**: terminite as a shared room for N actors with
  shared vocabulary. *In progress.* See [lounge-thesis.md](lounge-thesis.md)
  for the destination.

Status tags inline:

- `[done]` — shipped.
- `[in-progress]` — actively being built.
- `[next]` — should land soon, real friction.
- `[core]` — must-have to call the phase complete; blocks the next phase.
- `[later]` — real terminals / lounges have it; we don't need it yet.
- `[skip-for-now]` — explicitly deferred; tracked so we don't forget.

This document is the *current state*. The individual phase plans
(`phase1-plan.md`, `phase2-plan.md`, `phase3-plan.md`) are kept for the
project record but are superseded by this map.

---

## Phase 1 — Earn the terminal *(shipped)*

A terminal a careful person actually lives in.

**Foundations**
- [done] Window, surface, GPU pipeline (wgpu) + text (glyphon + cosmic-text)
- [done] alacritty_terminal as the VT engine; PTY with `TERM=xterm-256color`,
  `COLORTERM=truecolor`, configurable scrollback
- [done] Event-driven render loop (~0% idle CPU)
- [done] Per-frame snapshot pipeline (single term lock)
- [done] Foreground/background color, bold, italic, dim, inverse, hidden
- [done] Underline, double underline, strikeout decorations
- [done] Wide chars, wide-char spacers, zero-width combining marks
- [done] Cursor shapes from CSI 0–6 q (block, beam, underline, hollow); cursor blink
- [done] Mouse-wheel scroll, pixel-smooth with sub-line shift
- [done] Click-drag selection; double-click word / triple-click line; auto-scroll past edge
- [done] Copy on selection-end; Cmd+C / Cmd+V via arboard
- [done] Shift+PageUp / PageDown
- [done] Keyboard input → PTY with xterm modifier sequences (Shift/Alt/Ctrl + arrows)
- [done] IME / dead-key input
- [done] Bracketed paste
- [done] Mouse reporting (X10 + SGR + button + drag + motion)
- [done] Window title from OSC 0/1/2
- [done] Working directory tracking via OSC 7
- [done] Bell — visual flash with debouncing; `bell_style = "none"` to silence
- [done] Hyperlinks (OSC 8) — Cmd-click to open
- [done] Find (Cmd+F) — incremental scrollback search with match highlight
- [done] Right-click context menu (copy / paste / open link / select all)
- [done] Window resize → grid resize; physical-pixel sizing
- [done] Cmd+Q quit + close routing
- [done] Drop window — terminite shuts down cleanly

**Tabs & splits (Blender model)**
- [done] Tabs (Cmd+T, Cmd+W, Cmd+1–9, Cmd+Shift+] / [)
- [done] Splits (Cmd+D vertical, Cmd+Shift+D horizontal, Cmd+Opt+arrows for focus)
- [done] Drag every border between panes to resize; uniform pane handles
- [done] Top-right corner-handle gesture for split direction

**Config**
- [done] TOML config at `~/.config/terminite/config.toml`
- [done] Hot-reload on window focus
- [done] Numeric field clamping (no unbounded grid allocation)
- [done] Bell style (visual/none), cursor blink, scrollback, font_family/size, padding,
  gutter, tab font/size/bar height, line height

**Image protocol**
- [done] Kitty graphics protocol (transmit + display); per-pane image as wgpu texture

**Crash safety + observability**
- [done] Panic hook → crash dump under `~/.terminite/log/crashes/`
- [done] Hand-rolled structured logging with size-rotation
- [done] RSS kill-switch (`TERMINITE_RSS_LIMIT_GB`)

---

## Phase 2 — Be the pair's terminal *(shipped)*

Blocks as shared coordinates. Vendored VT fork. Module surface.

**The Model**
- [done] OSC 133 (A/B/C/D) dispatch via vendored alacritty_terminal + vte
- [done] Block model: command/output as numbered units (B1, B2, B3, …)
- [done] Block addressability + cap (`MAX_BLOCKS_PER_TAB`)
- [done] Lifecycle events: `block_opened`, `block_closed`
- [done] Block lookup, slicing, export to markdown

**Proto socket — JSON-RPC over Unix socket**
- [done] `~/.terminite/socket` mode 0600
- [done] Verbs: `list_tabs`, `list_blocks`, `get_block`, `subscribe`, `set_tag`,
  `remove_tag`, `cursor_at`, `cursor_clear`, `export_tab`, `stats`,
  `list_modules`, `reload_modules`
- [done] CLI wrapping all verbs (`terminite tabs`, `blocks`, `cursor`, `tag`, etc.)
- [done] Subscriber broadcast (block events stream to a subscribed client)
- [core] **Multi-client proto** — currently single-client; the lounge needs N. See Phase 4.

**The pair as co-users**
- [done] AI cursor highlight on block label (warm amber) — `cursor_at` verb
- [done] Block tags as durable labels (visible color tint on label)
- [done] Block-aware selection (Cmd-click inside a block selects it + copies)
- [next] **Tag *text* in the gutter** — currently tags only tint; the label string
  doesn't render. Surface the tag alongside the block ID. Promoted by
  opinions.md and the 2026-05-29 MCP round-trip dogfood ("how do I see the
  tag?").

**Modules — out of process**
- [done] Module protocol — manifest + JSON-line stdio
- [done] Five first-party modules: **nav** (file navigator), **preview** (multi-
  format renderer), **editor** (line buffer with selection, find, save_as,
  syntect), **config** (host config UI), **debug** (proto subscription pane)
- [done] Module wire: set_text, set_image, publish_focus, log, config_request,
  config_set, with peer events for focus/cwd/click/cursor
- [done] Module manifest discovery from `~/.terminite/modules/`
- [done] Module CRUD verbs (`terminite module add/remove/reload`)
- [done] fs-watch on `~/.terminite/modules/` — drop a module folder, dropdown updates
- [done] "Open modules folder" entry in the kind dropdown
- [later] **Module SDK** — once we know the protocol's true shape

**Block-as-AI-turn (the workflow gap)**
- [core] **Activities layer** — when a human runs an AI agent (`claude`, `aider`)
  interactively, the entire session collapses to one open block. The activity
  model — per-tool-call granularity, with stable coordinate ranges — is named
  in lounge-thesis.md but not yet implemented. *See Phase 4.*

---

## Phase 3 — Be lovable to live in *(shipped)*

The personal-usable thread that makes terminite a daily-driver.

**Layout persistence**
- [done] Pane tree + tab kinds + per-pane chrome + shell cwds + module focused-paths
  serialize to `~/.terminite/state/last.json` atomically
- [done] Auto-save on structural change; auto-restore on startup
- [done] Layout caps: `MAX_LAYOUT_BYTES` (256 KB) + `MAX_LAYOUT_PANES` (256)

**Syntax highlighting**
- [done] syntect (pure-Rust fancy-regex; no onig dep) with bundled grammars (50+ langs)
- [done] One Dark + One Light (Zed-aligned palette) bundled via `include_str!`
- [done] Editor module sends a `language` hint; host runs syntect, renders rich-text
- [later] **Theme-choice config knob** — the light variant is bundled; toggle pending

**Config pane**
- [done] Native module that calls `config_request` / `config_set` over the
  module wire; toml_edit preserves comments and formatting
- [done] Hot-reload after a set: live-applies hot-reload-eligible fields
- [done] Config schema introspection (`config::schema()`) — known keys, types,
  defaults, docs

**Shell-integration last mile**
- [done] `terminite shell-init [--install]` writes the OSC 133 snippet to
  `~/.zshrc` / `~/.bashrc` idempotently between marker comments
- [done] Documented in getting-started.md

**Distribution starter**
- [done] `tools/build-app.sh` builds a macOS .app bundle (signed icon, Info.plist)
- [next] **Codesigning + notarization** — Gatekeeper-clean install
- [next] **DMG packaging**
- [later] **Homebrew cask** — `brew install --cask terminite`
- [later] **Auto-update**
- [later] **Linux build** (Wayland first; PTY + clipboard layers need work)
- [later] **Windows build** (winit + wgpu run; PTY is ConPTY — different beast)

---

## Phase 4 — Be the lounge *(in progress)*

The destination from [lounge-thesis.md](lounge-thesis.md). terminite as a
shared room for N actors with shared vocabulary. Built in bricks, ordered so
each lights up usable value.

### Brick 1: MCP server *(shipped)*

- [done] `terminite mcp` — JSON-RPC 2.0 over stdio; handshake (initialize,
  notifications/initialized, ping, tools/list, tools/call)
- [done] 11 tools with onboarding-flavored descriptions (tabs_list,
  blocks_list, block_get, cursor_move, cursor_clear, tag_add, tag_remove,
  block_export, modules_list, modules_reload, stats)
- [done] Tool descriptions written so the *palette is the primer* — fresh AI
  sessions don't need a primer file; they see the tools and know how to participate
- [done] Stateless bridge to the proto socket; clean errors when terminite isn't running
- [done] User docs for Claude Desktop / Claude Code / Cursor MCP config
- [done] End-to-end verified (round-trip: cursor + tag actually mutated B1)
- [next] **Multi-AI smoketest** — exercise the MCP server from a non-Claude AI
  (Cursor, Windsurf) for real validation, not just Claude. Owed since 2026-05-27.

### Brick 2: ACP client — host agents in panes *(next)*

- [core] **`Agent` kind in the dropdown** — alongside Shell, Welcome, Module
- [core] **ACP protocol** — JSON-RPC 2.0 over stdio (we have the wire from
  MCP; share infrastructure). Verbs: initialize, newSession, prompt, cancel,
  resumeSession, closeSession, listSessions
- [core] **`FileSystemHandler`** — terminite implements file read/write the
  agent calls into, with permission gating
- [core] **`TerminalHandler`** — terminite implements shell execution the agent
  calls into
- [core] **Chat UI rendering** inside an Agent pane — message bubbles,
  streaming text, tool-call cards, syntax-highlighted code via the existing
  syntect path. This is real renderer work (probably 1000+ lines)
- [core] **Permission dialog system** — yes/no modal for sensitive operations
- [core] **Agent discovery** — what ACP-speaking agents are on the user's
  machine (Hermes, OpenClaw, etc.)
- [next] **Streaming response handling** — token-by-token render
- [next] **Error recovery** — agent crashes, mid-session drops
- [next] **Multi-AI smoketest** — host a non-Claude ACP agent (Hermes, OpenClaw)
  and verify the chat surface renders properly

### Brick 3: Activities layer *(next)*

The conceptual gap between Phase 2 blocks (shell commands) and the lounge.

- [core] **Activity model** — per-AI-action coordinates (tool call, file edit,
  prompt, save). Stable IDs (`act-42`), addressable like blocks
- [core] **Activity emission from ACP host pane** — when the hosted agent calls
  a tool, terminite spawns an activity with a stable coordinate
- [core] **Gutter rendering** that distinguishes shell blocks (`B7`) from agent
  activities (`act-42`). Nested style maybe; design TBD
- [next] **Activity tags + cursor** — same partnership signals as blocks
- [next] **Activity-as-block-equivalent in the proto + MCP** — extend the verbs
  symmetrically (`activities_list`, `activity_get`, `cursor_move_to_activity`)

### Brick 4: Multi-client + multi-actor proto *(core for the lounge)*

The proto is currently single-client. The lounge requires N.

- [core] **Concurrent subscriptions** — multiplex block / activity / cursor events to all connected clients
- [core] **Actor identification** — every connection registers with an `actor_id`,
  display label, and color
- [core] **Multi-cursor presence** — `BlockStore::cursor` becomes
  `actor_cursors: HashMap<ActorId, BlockId>`; renderer paints N cursors in N
  colors
- [core] **Per-actor tag spaces** — tags carry their author; the gutter shows
  whose mark is whose
- [next] **Conflict / turn semantics** — soft turn-claim (an actor moves their
  cursor to B7, communicating "I'm on it"); explicit handoff (tag a block
  `to:codex`)
- [later] **CRDT for editor pane concurrent edits** — when two ACP agents edit
  the same file. Real work; design pending real use.

### Brick 5: Tests + structural resilience

Promoted from opinions.md (Qwen, Kimi). The single biggest risk-mitigation.

- [next] **PTY round-trip integration test** — spawn, write, read snapshot,
  assert grid state. Closes the regression gap on every Phase 1 change.
- [next] **Proto wire-format compatibility test** — v1 client vs current
  server, golden responses
- [next] **Latency benchmark** — `time cat /usr/share/dict/words` style
- [next] **`BlockStore` edge-case tests** — eviction, cursor pinning, tag
  collisions, cap behavior
- [next] **Snapshot / geometry tests** — cell↔pixel survive resize, scroll
  offset math
- [next] **CI pipeline** — `cargo test` + `cargo clippy` + `tools/build-app.sh`
  on every push. Adds GitHub Actions.

### Brick 6: Render decomposition

Promoted from opinions.md (Kimi). `src/renderer.rs` is ~6,900 lines covering
GPU pipeline, text atlas, tab bar, pane tree, selection, find, context menus,
modals, cursor blink, auto-scroll, bell, input routing, image rendering, proto
dispatch.

- [next] **Plan a split** before the next surface feature lands. The renderer
  owns wgpu + atlas + draw-list primitives; higher-level chrome lives in
  separate modules. No today refactor; a deliberate split when we next touch
  it for activities / multi-actor cursors / chat panes.

---

## Phase 5 — Lovable to the wider world *(later)*

Distribution and polish for "released to share the vision."

### Theming
- [done] One Dark (default) + One Light bundled
- [next] **Theme-choice config** + drop-in `.tmTheme` from a user folder
- [later] **Built-in themes** — Solarized, Tomorrow Night, Gruvbox, Tokyo Night
- [later] **Font ligatures toggle** (cosmic-text supports them; surface doesn't yet)
- [later] **Background opacity / blur** (macOS NSVisualEffectView equivalent)

### Distribution
- [next] **Codesigning + notarization** + DMG
- [later] **Homebrew cask**
- [later] **Sparkle auto-update**
- [later] **Linux build** (Wayland; X11)
- [later] **Crash reporting** (opt-in)

### Story & community
- [next] **Top-level project README** (above guide/)
- [next] **Project website** — vision page, screenshots, install button
- [next] **Contribution guide** — what to send, how to log friction
- [next] **License decision** — track in decisions.md
- [later] **Module SDK** — once the protocol's true shape is known
- [later] **Public friction log** — a living artifact for the wider audience

---

## Cross-cutting principles

Carry these forward when promoting items into the next push:

- **Event-driven over polling**, always. If a render loop ticks without cause,
  that's a bug. See friction-log.md 2026-05-20 entry.
- **System-impact pass** before every commit. Three machine crashes earned
  this gate. See memory `feedback-system-impact-pass.md`.
- **Bigger per-turn chunks, aligned up front**. Plan the push, get a
  green-light, execute the bundle in one commit train.
- **Friction-log + opinions.md are inputs, this map is the output**. Promote
  items here when the felt cost is real.
- **Honest reversals belong in `decisions.md`**, not buried in commits.
- **Two users, one surface** — generalized to *N actors, one room*. Every
  Phase 4 design choice should make sense for the human, for a hosted ACP
  agent, and for a subscribed MCP client simultaneously.
- **Mature libs over DIY for recognition**. See memory
  `feedback-mature-libs.md`.
- **Don't assume the AI will or can**. The vocabulary lives in the protocol
  layer (MCP tool descriptions, eventually ACP capabilities), not in
  documentation files written to user projects.

---

## Convergent gaps named by the outside review *(opinions.md, 2026-05-29)*

Five outside AIs independently reviewed the project. They converged on the
same five issues, which are now Phase 4 bricks above:

1. **MCP server as the next move** — *shipped* (`5ddf2ed`)
2. **Activities layer** — Brick 3 above
3. **Docs out of sync with code** — *this update*
4. **Tests thin for the codebase size** — Brick 5 above
5. **Multi-client proto** — Brick 4 above

Plus two further structural observations:

6. **`renderer.rs` monolith risk** — Brick 6 above
7. **Wasm modules for zero-dep distribution** (Gemini) — *parked.* The Python
  module pattern works today; the real friction is dep footprint at
  distribution time, not architecture. Re-examine when distribution gets serious.
