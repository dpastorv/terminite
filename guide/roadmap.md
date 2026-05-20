# Roadmap

The map of what terminite is and what it's becoming. Honest, opinionated,
written for the human-AI pair to read together. Move things up and down as we
go; promote from `friction-log.md`; cross out what's shipped.

Three phases. Each one earns the right to the next.

- **Phase 1 — Earn the terminal**: be a terminal a careful person would
  actually live in. No tricks yet.
- **Phase 2 — Be the pair's terminal**: the differentiator. Command/output as
  blocks, lifecycle events, modules, the AI as a co-user.
- **Phase 3 — Be lovable to the wider world**: distribution, theming, the
  story we ship with.

Status tags inline:

- `[done]` — shipped.
- `[core]` — must-have to call this a real terminal (Phase 1) or a real pair
  surface (Phase 2). Blocks the next phase.
- `[next]` — should land soon, not blocking but the friction is real.
- `[later]` — real terminals have it; we don't need it yet to be honest.
- `[skip-for-now]` — explicitly deferred; tracked so we don't forget.

---

## Shipped (the floor we stand on)

- [done] Window, surface, GPU pipeline (wgpu) + text (glyphon + cosmic-text).
- [done] alacritty_terminal as the VT engine; PTY spawn with `TERM=xterm-256color`,
  `COLORTERM=truecolor`, 10,000-line scrollback.
- [done] Event-driven render loop (~0% idle CPU).
- [done] Foreground/background color, bold, italic, dim, inverse, hidden.
- [done] Underline, double underline, strikeout decorations.
- [done] Wide chars, wide-char spacers, zero-width combining marks.
- [done] Cursor (block, padded, on top of selection/bg).
- [done] Mouse-wheel scroll, pixel-smooth with sub-line shift.
- [done] Click-drag selection, drag-scroll, jitter filter, copy on release.
- [done] Cmd+C / Cmd+V (via arboard).
- [done] Shift+PageUp / PageDown.
- [done] Keyboard input → PTY (arrows, function keys, Ctrl-*, named keys).
- [done] CPR / device queries / clipboard responses via `Event::PtyWrite`.
- [done] Window resize → grid resize; physical-pixel sizing (no double scale).
- [done] Per-frame Snapshot pipeline (one term lock; bg + text + deco + cursor).
- [done] Session-logger script (`tools/log_session.py`).

---

## Phase 1 — Earn the terminal

The bar: a careful developer in 2026 could use terminite as their daily
driver without resenting it. Not yet *lovable* — just *correct*.

Phase 1 grew when we honestly asked "what does a 2026 developer expect day
one?" Tabs and splits joined the floor. Hyperlinks, find, working directory,
config, and image protocol followed. Phase 1 is bigger than it was; that's
right — the floor has moved over the last few years and we're meeting it,
not skipping it.

### Shell-app integration

- [core] **Window title** — handle `Event::Title` (OSC 0/1/2). Right now the
  dock entry and window list stay "terminite" forever, regardless of the
  shell, ssh session, or in-progress command.
- [core] **Bracketed paste** — set `TermMode::BRACKETED_PASTE` correctly and
  wrap pasted bytes in `\e[200~ ... \e[201~` when the mode is enabled.
  Without this, pasting multi-line content into zsh or bash mid-prompt
  *executes each line*. Security/correctness floor.
- [core] **Mouse reporting** — translate winit mouse events into the
  protocol bytes (X10, SGR-mode at minimum). vim, htop, less, lazygit, fzf,
  tmux all expect clicks and scroll to be reported when they enable the
  mode.
- [core] **Hyperlinks (OSC 8)** — Cmd-click an explicit hyperlink range.
  Modern shells and tools (eza, lazygit, npm, cargo, gh) emit these
  everywhere; clicking them is the 2026 default.
- [core] **Working directory tracking (OSC 7)** — required for "new tab
  inherits cwd" (most-reached-for tab affordance) and useful on its own
  (show cwd in tab title, jump-to-folder from menu).
- [core] **IME / dead-key input** — winit exposes IME events; today we drop
  them. Anyone typing accented characters (or any non-ASCII input method)
  can't.
- [core] **Verify alternate screen** — vim, less, htop swap to the alt
  buffer. alacritty_terminal handles the mode internally, but we have to
  confirm our snapshot draws the right grid in both modes.
- [next] **Reflow on resize** — alacritty's grid reflows by default; verify
  visually with long lines + window drag.
- [next] **Tab character handling** — make sure `\t` lands on a real tab
  stop after width changes.
- [next] **OSC 52 clipboard write** — let remote apps set the local
  clipboard. alacritty_terminal exposes it via events; we just route.

### Selection

- [core] **Double-click word selection, triple-click line selection**. The
  most-reached-for shortcut in any terminal; absent feels broken.
- [core] **Auto-scroll while dragging past the viewport edge** — natural
  complement to drag-scroll. Drag past top/bottom while holding the button
  → viewport scrolls and selection extends.
- [later] **Selection by regex / column / block (Cmd+Option drag)**.

### Surface affordances

- [core] **Bell** — at minimum, a one-frame background flash on `\a`. Hold
  the option of "audible" for later. Silent bells violate apps that depend
  on them (cd completion in some shells, IRC clients).
- [core] **Cursor shape from CSI 0–6 q** — apps switch cursor between
  block / bar / underline. zsh's vi mode relies on it.
- [core] **Cursor blink** — configurable, on by default. Matches 2026
  baseline.
- [core] **Right-click context menu** — Copy, Paste, Open Link, Select
  All. Lightweight macOS-native menu.
- [later] **URL autodetection** — heuristic regex over the grid, hover
  underline, click to open. (Distinct from explicit OSC 8 above.)

### Tabs & splits

- [core] **Tabs** — Cmd+T new, Cmd+W close, Cmd+1–9 jump, Cmd+Shift+[/]
  move, Cmd+Shift+T reopen-closed. One `LiveTerm` per tab; tab bar UI;
  focus routing; new tab inherits cwd (via OSC 7). Architecturally the
  biggest single item on the list — most data structures grow a "current
  tab" dimension.
- [core] **Splits** — Cmd+D vertical split, Cmd+Shift+D horizontal,
  Cmd+Opt+arrow to switch focus, drag-the-divider resize, per-pane PTY,
  per-pane scrollback. Pairs with tabs in the modern terminal — tmux
  users live in splits and ssh-bouncing devs too. In Phase 2 we may
  rethink the *unit*: a "split" might be re-anchored to a block thread.

### Find & images

- [core] **Find (Cmd+F)** — incremental search across the scrollback, with
  match highlights and next/prev. Becomes the *seed* for Phase 2's
  block-aware find.
- [core] **Image protocol** — Kitty graphics protocol at minimum; ideally
  iTerm2 inline images and Sixel too. Decode and render via the wgpu
  texture pipeline. In Phase 2 the rendered image becomes an addressable
  block (referenceable, augmentable, point-at-able like any other).

### Config

- [core] **Minimal config file** — TOML at `~/.config/terminite/config.toml`,
  hot-reload on save. v1 fields (small on purpose; each one is a future
  commitment): `font_family`, `font_size`, `theme`, `padding`,
  `cursor_blink`, `bell_style` (visual/audible/none).

### Window & system

- [core] **Quit on Cmd+Q / window close** — currently we exit on
  `CloseRequested` but should also confirm Cmd+Q routes correctly on macOS.
- [next] **Restore window position/size on launch** (per-app state, not a
  config file flag).
- [next] **Focus events** — react to focus loss (dim cursor, stop blink) and
  emit DEC focus reporting (`\e[?1004h`) when the app asks.
- [later] **Drag-and-drop files** — drop a file onto terminite → paste its
  shell-quoted path.
- [later] **Zoom (Cmd+=/Cmd+-)** — runtime font_size bump (config takes
  care of static sizing for v1).

---

## Phase 2 — Be the pair's terminal

> Phase 2 turns the terminal from a stream into a Model. Terminal with a
> DOM. Chess with words.
>
> Every command + its output becomes a block — named, addressable, with
> lifecycle events the pair can react to. On any block, three things become
> possible: **reference** it (name it, point at it by ID), **augment** it
> (annotate, transform, slice, ask), and **point** at it (highlight, share
> attention). Both of us — human and AI — share that Model as common
> ground. We refer to the same objects, by the same names, in the same
> coordinate system. The AI stops reconstructing structure from heuristics;
> the human stops re-grounding every reference from scratch.
>
> Out-of-process modules grow the surface from both sides. Either user can
> write one.
>
> The thesis is small and load-bearing: when the substrate is structured,
> the relationship is natural. Phase 2 is the work of making it so.

### The Model — the central idea

- [core] **Command/output as structured blocks**. Use OSC 133 (FinalTerm /
  iTerm2 shell integration) as the input signal — most shells already emit
  it with a small shellrc snippet. Each block has a stable ID, a start/end
  cell range, a command, an exit code, and a timestamp.
- [core] **Lifecycle events** — `started`, `output_added(range)`, `finished
  (exit_code, duration)`. Modules listen for these; no polling.
- [core] **Block addressability** — every block gets a short ID (e.g.
  `B7-3`). The pair can name what they're looking at — *"that error in B7,
  line 3"* — and both refer to the same cells.
- [core] **Sliceable access** — modules can ask the Model for `B7.head(50)`,
  `B7.tail(N)`, `B7.last_n_lines`, without dumping the whole stream into a
  small context window.

### The pair as co-users

- [core] **AI presence on the surface** — a second cursor / highlight color
  for the AI partner. When the AI references a range, the human sees it
  light up. When the human selects, the AI sees the same selection.
- [next] **Block-aware selection** — selecting a block highlights the whole
  block, not just the cells; copying yields the command + output, not raw.
- [next] **Conversation as first-class artifact** — the session itself is
  navigable, exportable, sharable. Promotes `tools/log_session.py` into a
  native panel instead of a script.

### Modules — out of process

- [core] **Module protocol** — out-of-process modules speak a small JSON
  protocol over stdio. The Model is read-only to a module unless it
  declares write intent. Process isolation is the security model.
- [core] **First module: AI partner panel** — a side panel that holds the
  conversation, sees blocks by ID, can ask the Model for slices, can post
  bytes back to a shell. Provider-agnostic (Claude first; Kimi, GPT,
  Gemini, Ollama next).
- [next] **Second module: command palette** — fuzzy search over blocks and
  commands across the session.
- [next] **Third module: notes / annotations** — attach a note to a block;
  notes travel with the session export.
- [later] **Module SDK** — once the protocol is stable, ship a small
  library so others can write modules in any language.

### Multi-AI

- [next] **Provider abstraction** — swap Claude for Kimi or local Ollama
  without restarting. The relationship is with *the pair*, not with one
  vendor.
- [later] **Per-block AI assignment** — different blocks can be reviewed
  by different partners.

---

## Phase 3 — Be lovable to the wider world

Phase 2 makes terminite different. Phase 3 makes it adoptable.

### Theming

(The config *file* is in Phase 1; this section is about what's *in* it.)

- [next] **Built-in themes** — One Dark (current), Solarized, Tomorrow,
  Gruvbox, plus a light theme.
- [later] **Font ligatures toggle** (cosmic-text supports them; we just
  need the surface).
- [later] **Background opacity / blur** (macOS NSVisualEffectView equivalent).

### Distribution

- [core for shipping] **macOS notarized DMG** + Homebrew cask.
- [core for shipping] **Auto-update** (Sparkle on macOS, simple JSON feed).
- [next] **Linux build** — Wayland first, then X11. winit and wgpu both
  support these; we just need to test and package.
- [next] **Crash reporting (opt-in)** — without it we don't hear when it
  breaks for real users.
- [later] **Windows build**. winit + wgpu run there, but the PTY layer is
  different (ConPTY); deferable.

### Story & community

- [next] **Project website** — vision page, screenshots, install button,
  short demo video.
- [next] **Contribution guide** — what to send, how to log friction.
- [next] **License decision** — track in `decisions.md`. Probably
  permissive (Apache 2.0 or MIT) given the goal of being adopted broadly.
- [later] **A public friction log** — `friction-log.md` becomes a living
  doc fed by both AI and human users in the field.

### Performance & quality

- [next] **Frame-stats overlay** — toggle to see snapshot ms, render ms,
  GPU submit ms. Lets us catch regressions before users do.
- [next] **Benchmark harness** — `time cat /usr/share/dict/words` style
  tests against Terminal.app / Alacritty / Ghostty as baselines.
- [later] **Background tab/window throttling** — render at 1fps when
  unfocused.

### Window management

(Tabs and splits moved to Phase 1 — they're 2026 table-stakes. What stays
here is the rest.)

- [later] **Multiple windows from the menu** — File → New Window.
- [later] **Sessions / profiles** — saved sets of tabs/splits with cwd and
  command per pane.
- [later] **Tab/split reordering across windows** (drag a tab between
  windows).

---

## Cross-cutting principles

Carry these forward when promoting items into the next push:

- **Event-driven over polling**, always. If a render loop ticks without
  cause, that's a bug. See `friction-log.md` 2026-05-20 entry.
- **Bigger per-turn chunks, aligned up front**. Plan the push, get
  green-light, execute the bundle in one commit train.
- **Friction-log is the input, this map is the output**. Promote items
  here when the felt cost is real.
- **Honest reversals belong in `decisions.md`**, not buried in commits.
- **Two users, one surface**. Every Phase 2 design choice should make
  sense for both the human and the AI partner reading it.

---

## Phase 1 execution plan

Phase 1 is too large for a single push. Five bundles, in order. Each one
lands cleanly on `main` and leaves terminite usable.

### Bundle 1 — Correctness floor (small, independent items)

Closes the most-felt gaps. None of these items depends on the others;
they all earn their keep on their own.

1. Window title (OSC 0/1/2).
2. Bracketed paste (mode + wrap on write).
3. Mouse reporting (X10 + SGR).
4. Bell (one-frame background flash).
5. Cursor shape (CSI 0–6 q) + cursor blink.
6. Double-click word / triple-click line selection.
7. Auto-scroll while drag-selecting past viewport edge.
8. IME / dead-key input.
9. Alt-screen verify (probably no code, just confirm vim/less/htop work).
10. Cmd+Q routing.

### Bundle 2 — Tabs

Architecturally the biggest item. Restructures the renderer to own a
`Vec<LiveTerm>` indexed by active tab, with focus routing, tab bar UI,
and new-tab-inherits-cwd via OSC 7. This bundle has OSC 7 in it because
the two land naturally together.

### Bundle 3 — Splits

Per-pane PTY, per-pane scrollback, per-pane focus. Adds a split-divider
UI and Cmd+Opt+arrow focus motion. Touches almost as much as tabs.

### Bundle 4 — Discoverability polish

- Hyperlinks (OSC 8) + Cmd-click to open.
- Find (Cmd+F) — incremental scrollback search.
- Right-click context menu (copy / paste / open link / select all).
- Minimal config file (TOML, hot-reload, 6 fields).

### Bundle 5 — Image protocol

Kitty graphics protocol first, iTerm2 inline next, Sixel last. Decode +
upload as wgpu textures; render under or over text. Phase 2 will treat
each image as a block.

---

After Bundle 5 we're at the line. Crossing it lands us in Phase 2 — the
part this whole thing is *for*.
