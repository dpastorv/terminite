# Phase 1 Completion Plan

Everything between today (post-Bundle 1) and Phase 2. Written so we can
align on the *shape* before any keystroke of code. Stakes are now
real-usage — Daniel will do client work in this terminal once it crosses
the line.

## Order of operations (re-ordered around the client-work deadline)

The roadmap's natural order is 2 → 3 → 4 → 5. Given the new stakes I'd
re-sequence: **2 → 4 (partial) → 3 → 5**. Reasons:

- Tabs (Bundle 2) is the single biggest unlock for real daily use. No
  argument it's first.
- Find, right-click, config file, and (cheap) hyperlinks (Bundle 4
  partial) are quality-of-life items that bite within an hour of real
  use. They land cheaply on top of Bundle 2's per-tab refactor.
- Splits (Bundle 3) is structural and valuable but not blocking — tmux
  inside terminite is a real fallback for the split case.
- Image protocol (Bundle 5) is the most-complex remaining item and the
  one that arguably belongs to Phase 2 anyway (an image is a *block*).
  Defer until after Daniel is using terminite live and we can see whether
  he actually reaches for `imgcat` / `yazi`.

The `[next]` items get folded in opportunistically — each one is
small enough to ride in whichever bundle naturally touches its code path.

---

## Bundle 2 — Tabs + OSC 7 working directory

The architectural shift. After this bundle, every input event, every
render frame, and every snapshot is "active-tab aware."

### Per-tab state

A new `Tab` struct in `renderer.rs` (or a new `tab.rs`) owns everything
that's currently a per-window field but is conceptually per-shell:

```rust
struct Tab {
    live_term: LiveTerm,
    pixel_offset: f32,
    selection: Option<Selection>,
    dragging: bool,
    last_drag_mouse_pos: (f32, f32),
    last_click: Option<(Instant, (i32, usize), u8)>,
    last_text_runs: Vec<(String, SpanStyle)>,
    autoscroll_dir: Option<i32>,
    title: String,
    cwd: Option<String>,
    id: TabId,  // monotonic; survives reordering
}
```

`Renderer.live_term` → `Renderer.tabs: Vec<Tab>`, plus
`Renderer.active: usize`.

State that stays at the Renderer level (one per window): GPU resources,
font_system, atlas, text_renderer, viewport, swash_cache, rects_below /
rects_above, text_buffer (single buffer; content is swapped on tab
switch via `set_rich_text`), mouse_pos, modifiers, focused, start_time,
bell_flash_until, next_*_deadline, rss_kill_bytes, clipboard, window,
surface, surface_config, device, queue, instance.

### Routing the Notifier per tab

This is the trickiest sub-problem. Right now `Notifier` carries a single
`EventLoopProxy<UserEvent>`; `UserEvent::SetTitle(String)` and
`UserEvent::Bell` don't say *which tab* they came from. Solutions:

1. Add `TabId` to the relevant `UserEvent` variants:
   `SetTitle(TabId, String)`, `Bell(TabId)`.
2. The Notifier holds a `tab_id: TabId` so it knows what to stamp.
3. `user_event` matches and routes to the right `Tab`.

Wakeup stays untagged — a wakeup is just "redraw."

### Tab bar UI

A horizontal strip ~LINE_HEIGHT + 8px tall above the text area.
`TAB_BAR_HEIGHT = 44.0` (physical px on Retina). `TEXT_TOP` becomes
`TAB_BAR_HEIGHT + ORIG_TEXT_TOP` (or we just reuse the existing TEXT_TOP
constant by shifting the bar offset; cleaner: introduce
`CONTENT_TOP = TAB_BAR_HEIGHT + 24.0` and use it where TEXT_TOP is used
today).

Per tab:
- Background rect (slightly lighter when active).
- Title text (truncated with ellipsis if needed).
- Small `×` close affordance on the right of the tab; hit-tested
  separately.
- Underline (cursor-color) under the active tab.

Tab widths: equal-split across the available width up to a max
(~200px); when N tabs would exceed the bar width, shrink uniformly with
a min width (~80px) and clip beyond that.

### Mouse routing

```
y < TAB_BAR_HEIGHT  → tab-bar hit test (switch / close)
y ≥ TAB_BAR_HEIGHT  → active tab's text area as today
```

`cell_at_1indexed` and `pixel_to_absolute` subtract `CONTENT_TOP`
instead of TEXT_TOP.

### Keyboard shortcuts

- **Cmd+T**: `new_tab()` — spawn a fresh `LiveTerm` at current
  `(cols, rows)`, inheriting cwd from the active tab.
- **Cmd+W**: `close_tab(active)` — drop the `LiveTerm` (its PTY thread
  cleans itself via Drop); if tabs becomes empty, exit the app.
- **Cmd+1**…**Cmd+9**: jump to that index (clamped to existing tabs).
- **Cmd+Shift+]** / **Cmd+Shift+[**: next / previous (wraps).

### OSC 7

alacritty's `Term` exposes the parsed current directory via
`term.grid().current_dir()` or `term.current_dir()` (I'll confirm at
implementation time — if alacritty doesn't surface it, we intercept
the OSC 7 directly in the Notifier path before alacritty consumes it).

Per render frame, read the active tab's `live_term.cwd()` and update
`tab.cwd`. On new-tab, pass `tab.cwd` to `tty::Options.working_directory`.

### Title routing

`Event::Title` was previously routed to the window via
`UserEvent::SetTitle`. Now it sets `tab.title` for the *active* tab's
Notifier-tagged TabId. The window title shows the active tab's title.

### Files touched

- `term.rs`: Notifier carries `tab_id: TabId`. (Possibly: expose
  `current_dir()` accessor on LiveTerm.)
- `main.rs`: UserEvent variants gain `TabId`. Keyboard shortcut handlers
  for Cmd+T/W/1–9/Shift+[/].
- `renderer.rs`: Tab struct, tabs Vec, active index. Tab bar rendering.
  Routing in mouse_*, mouse_wheel, render. Per-tab snapshot.

### Risks / unknowns

- **PTY shutdown order**: closing a tab must terminate that LiveTerm's
  I/O thread cleanly. Verify Drop semantics; if alacritty's event_loop
  doesn't have a clean shutdown, send `Msg::Shutdown` and join briefly.
- **text_buffer per tab vs one shared**: shared is simpler (swap rich
  text on switch); but if the swap is slow, we may want per-tab buffers
  later.
- **OSC 7 source path**: confirm alacritty exposes it.

### Exit criteria

- Cmd+T opens a new shell in the same cwd as the active one.
- Switching tabs is instant; per-tab scroll/selection state is
  preserved.
- Closing the last tab exits.
- Tab bar reflects the shell's title (`OSC 0`) and re-renders on cwd
  changes.

---

## Bundle 4 (partial) — Find, hyperlinks, right-click, config

Lands after Bundle 2 because it relies on the per-tab refactor (find is
per-tab; right-click uses per-tab selection; hyperlinks need per-tab
cwd for `open` context).

### Find (Cmd+F)

A search bar overlay above the text area (below the tab bar):

- Cmd+F opens, focus moves to a single-line text input.
- Each keystroke updates the query and re-runs the search synchronously
  over the active tab's scrollback (alacritty grid lines `-history..rows`).
- Matches stored as `Vec<(abs_line, col_start, col_end)>`; rendered as
  pale-yellow highlight rects (same pipeline as selection).
- Enter / Shift+Enter cycle next / previous match. Current match drawn
  brighter; viewport auto-scrolls to bring it into view.
- Esc closes the search bar; matches and current-match highlight clear.

Implementation: a `FindState` field on `Tab`. Render code adds the find
overlay above the text. Input events while find is active route to the
find input instead of the PTY.

### Hyperlinks (OSC 8)

alacritty already parses OSC 8 and stores hyperlink data per-cell
(`cell.hyperlink()` returns `Option<Hyperlink>` with `uri`, etc.). Two
behaviors:

1. **Render**: cells with a hyperlink get a thin underline in cursor
   color (or the cell's fg) — visible affordance.
2. **Cmd-click**: on left-button down with `super_key`, hit-test the
   cell; if it has a hyperlink, spawn `open <uri>` (macOS) via
   `std::process::Command`. Don't enter selection mode.

(URL autodetection — heuristic regex over the grid for raw URLs — stays
`[later]`.)

### Right-click context menu

A small custom-drawn overlay (no NSMenu; keeps it cross-platform-ready):

- Right mouse button down (only when mouse reporting isn't active):
  show a rect of items at the cursor position.
- Items: **Copy** (disabled if no selection), **Paste**, **Open Link**
  (only if hovering an OSC 8 cell), **Select All**.
- Hit-test on next mouse event. Click outside dismisses.
- Esc dismisses.

Implementation: a new `ContextMenu` struct with origin + items + hover
index. Render adds a top-most overlay. Mouse events check menu state
first.

### Minimal config file

Path: `~/.config/terminite/config.toml`. Create if missing on first
launch. Fields (v1):

```toml
font_family = "monospace"
font_size = 28.0
padding = 24.0
cursor_blink = true
bell_style = "visual"  # "visual" | "audible" | "none"
theme = "one-dark"      # built-in name
```

- Read on startup; `Renderer::new` consumes a `Config`.
- Hot-reload via the `notify` crate watching the file; on change, re-read
  and apply (font_size triggers reshape; padding triggers grid resize;
  theme triggers palette swap; cursor_blink / bell_style toggle flags).
- Invalid values fall back to defaults with a stderr warning.

### Files touched

- `main.rs`: load config at startup; pass to Renderer.
- `renderer.rs`: FindState, ContextMenu, config-driven fields, hot-reload
  hook.
- `term.rs`: nothing (find iterates the grid externally).
- `Cargo.toml`: `serde`, `toml`, `notify`.
- New: `src/config.rs`.

### Risks / unknowns

- Find's match-rendering across pixel-smooth scroll: matches in
  absolute Line coordinates, rendered by translating to viewport via
  display_offset (same shape as selection). Free if selection works.
- Hot-reload's notify crate adds dependencies; might be heavyweight for
  what's a tiny file. Alternative: re-read on window-focus only.
- macOS `open` will hand control to the system; if a user clicks a
  malicious-looking link, we did our part by showing the URL on hover
  (a hover-tooltip is a follow-up; for v1, no tooltip — Cmd-click only).

### Exit criteria

- Cmd+F finds and cycles matches; Esc clears.
- Cmd-clicking an OSC 8 hyperlink in a `gh pr view` / `cargo build` /
  `npm install` output opens the URL.
- Right-click on a selection copies; on a hyperlink offers Open Link.
- Editing `~/.config/terminite/config.toml` (bell_style especially)
  takes effect within a second.

---

## Bundle 3 — Splits

Adds in-window panes within a tab. Bundle 2 left each tab owning a
single `LiveTerm`; Bundle 3 either (a) refactors `Tab` to own a
`PaneTree`, or (b) embraces tmux as the splits story and skips this
bundle.

I'll plan for (a). Reasoning: panes-with-distinct-blocks are essential
in Phase 2 (a pane is a natural unit for the Model's block surface), and
shipping splits in Phase 1 gives us a real test of the per-pane data
model before Phase 2 leans on it.

### PaneTree

```rust
enum PaneTree {
    Leaf(Pane),
    Split { dir: SplitDir, ratio: f32, a: Box<PaneTree>, b: Box<PaneTree> },
}

enum SplitDir { Horizontal, Vertical }
```

Each `Pane` carries the per-pane state that used to be per-tab (the
`Tab` struct contents minus `title` / `cwd` / `id` — title stays at the
tab level; cwd becomes per-pane).

### Focus

Each tab gets `active_pane: PaneId`. Walking the tree finds the
active leaf. Cmd+Opt+arrows navigate by geometric direction (closest
leaf in that direction from the active leaf's rect).

### Layout

Each frame, walk the tree producing `(PaneId, Rect)` pairs. Render each
pane at its rect, scissored to that rect. The PTY for each pane is
resized to the cells that fit in its rect.

### Dividers

Drawn as thin (2px) rects between sibling subtrees. Mouse-draggable:
mouse_down on a divider initiates a drag that updates the parent
Split's `ratio` until release.

### Keyboard shortcuts

- **Cmd+D**: vertical split (new pane to the right).
- **Cmd+Shift+D**: horizontal split (new pane below).
- **Cmd+Opt+↑/↓/←/→**: focus by direction.
- **Cmd+W** (when split): close active pane → restructure tree;
  if pane is the only one in its tab, behave as today (close tab).

### Selection across panes

Out of scope; selection is per-pane. Defer cross-pane selection.

### Files touched

- `renderer.rs`: PaneTree, layout walker, scissor-per-pane rendering,
  focus state, divider rendering and drag.
- `tab.rs` (new from Bundle 2): Tab owns pane_tree + active_pane.
- `main.rs`: new shortcuts; mouse events route to the pane under the
  cursor.

### Risks / unknowns

- Per-pane resize on every tree edit means N PTYs see resize at once;
  shouldn't be hot, but worth measuring.
- Each pane needs its own `text_buffer` *or* we render N times into one
  shared buffer with rich text per pane. Probably one buffer per pane
  is simplest — atlas usage grows but bounded by visible pane count.
- Focus by direction across non-rectangular splits is a small geometry
  problem; pick "centroid distance in the chosen direction" heuristic.

### Exit criteria

- Cmd+D creates a working split with its own PTY.
- Resizing dividers updates both panes' cell dimensions and the PTYs
  reflow correctly.
- Cmd+Opt+arrow moves focus along the expected axis.

---

## Bundle 5 — Image protocol (Kitty graphics minimum)

Most complex and most-deferrable. Scope down to the Kitty protocol for
v1; iTerm2 inline and Sixel can land as follow-ups in Phase 2 alongside
images-as-blocks.

### Kitty graphics protocol — what it is

Apps send `\e_Gf=100,a=T,...;<base64-png-data>\e\\` (and chunked variants
for large payloads). Parameters control format, action, placement,
sizing, deletion. The terminal decodes the payload, uploads to GPU,
renders at the given (cell-aligned) position.

### Implementation shape

1. **Intercept**: alacritty doesn't speak the Kitty protocol natively;
   we'd need to filter incoming bytes before alacritty's parser, route
   `\e_G…\e\\` sequences to our handler, and pass everything else
   through. Either patch alacritty (vendored) or interpose at the PTY
   read path.
2. **Decode**: parse params + base64 payload. For PNG, decode via the
   `png` crate.
3. **Upload**: `wgpu::Texture` per image; store in an `ImageStore`
   keyed by image-id.
4. **Place**: each image has a position (cell coordinates, optional
   pixel offsets) and a size (cells). Placed images get drawn as
   textured quads in a new render pass between text and decorations.
5. **Delete**: act `d` removes images by id or range.

### Risks / unknowns

- Interposing on the PTY read is invasive. Two options:
  - Wrap the PTY reader so we filter sequences before alacritty sees
    them.
  - Add a fork of alacritty_terminal with a callback hook for
    application protocols. Heavier maintenance.
- Memory: a 4K image is ~30MB raw; we need an eviction policy or apps
  that lean on images heavily will balloon memory.
- The RSS kill-switch (4 GB default from Bundle 1 fix) is a backstop,
  but we want images to behave even at scale.

### Exit criteria

- `kitty +icat some.png` displays the image.
- `yazi`'s image preview works for PNGs.
- Memory stays bounded with repeated image displays (an eviction policy
  or per-id replacement).

---

## [next] items folded in

These ride in adjacent bundles instead of getting their own:

- **Reflow on resize** (verify): test during Bundle 2 work as soon as
  tabs are resized. If alacritty's reflow works, free; otherwise file
  a friction-log entry and patch.
- **Tab character handling** (verify): test in Bundle 2 with
  `printf 'a\tb'`; fix in place if broken.
- **OSC 52 clipboard write**: lands in Bundle 4; trivial route from
  `Event::ClipboardStore` to `arboard`.
- **Restore window position**: in Bundle 4 (alongside config file —
  natural fit, since restored geometry is essentially per-window state).
- **Alt-screen verify**: tested every bundle when we run vim/htop;
  expect zero code change.

---

## Phase 1 exit criteria

We cross the line — and Daniel starts client work — when all of:

- All `[core]` items in `roadmap.md` Phase 1 are shipped.
- Daniel has used terminite as his primary terminal for 3+ days of
  ordinary developer activity (editing, building, running tests, git,
  ssh, package managers) without a `[P0]` incident.
- vim, htop, less, lazygit, yazi all behave correctly when run inside.
- Tabs, splits, and find feel native (sub-100ms responsiveness on
  every interaction).
- No memory pressure beyond ~250MB resident in normal use.
- The RSS kill-switch never fires during normal use.

When all six hold, Phase 1 closes and we begin Phase 2 — the part
this whole thing is *for*.

---

## Sequencing summary

```
Bundle 2   tabs + OSC 7                    ──┐
                                              │   ↓ unblocks daily use
Bundle 4   find / hyperlinks / right-click /  │
           config                             │
                                              │   ↓ unblocks "feels native"
Bundle 3   splits                             │
                                              │   ↓ unblocks "Phase 2 is feasible"
Bundle 5   Kitty graphics                     │
                                              │
                                              ▼
                                       Phase 2 — the Model
```

I'll execute one bundle per push, aligned with this plan, unless something
in the live work tells us to re-order.
