//! The Renderer: assembles backgrounds, decorations, text, the cursor, and
//! selection highlights into a single frame. Two `RectRenderer` instances
//! sandwich the text — one draws *below* (backgrounds + selection + cursor),
//! one draws *above* (decorations).

use std::sync::Arc;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use glyphon::{
    Attrs, Buffer, Cache, Color, ColorMode, Family, FontSystem, Metrics, Resolution, Shaping,
    Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use winit::event::{MouseButton, MouseScrollDelta};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, Window};

use crate::blocks::BlockStore;
use crate::config::{BellStyle, Config, Padding};
use crate::images::{self, Action};
use crate::palette::color_to_floats;
use crate::rect::{RectInstance, RectRenderer};
use crate::term::{CursorShapeKind, DecorationKind, LiveTerm, ModeFlags, Snapshot, SpanStyle, TermScroll};
use crate::texture::{TextureImage, TextureInstance, TextureRenderer};
use crate::{TabId, UserEvent};

// The Renderer impl is split across these submodules (same type, multiple
// impl blocks). Each child sees this module's private items via `use super::*`.
mod config;
mod input;
mod io;
mod modules;
mod overlays;
mod panes;
mod proto;
mod render;
mod render_pane;
mod tabs;

// Supporting types and helper fns live in their topical submodules; re-export
// them here so the rest of the renderer tree can name them unqualified via
// `use super::*`.
use input::*;
use overlays::*;
use panes::*;
use render::*;
use tabs::*;

/// Re-check interval for a held PTY-floor message's gate (unfocused + idle)
/// while it waits. Only ticks while one is undelivered (see `has_pending_pty_work`).
const PTY_RETRY: std::time::Duration = std::time::Duration::from_millis(1500);
/// Gap between typing a floor message's text and sending its Enter. TUIs read a
/// fast text+Enter burst as a *paste*, where the trailing newline is buffered as
/// content, not a submit — so the message lands in the prompt but never fires
/// (the relay's "submit gap": text appeared, Daniel had to press Enter). A
/// distinct, delayed Enter is seen as a real keystroke and submits. Well outside
/// any paste-coalescing window, imperceptible to a watching human.
const PTY_SUBMIT_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

/// A floor injection waiting on its delayed Enter. Carries what
/// `room_message_cancel` needs to *unsend* in this window: the typed char
/// count (to backspace-erase the line) and the message ids it covered.
pub(super) struct FloorSubmit {
    pub deadline: Instant,
    pub pane: u64,
    pub typed_chars: usize,
    pub msg_ids: Vec<u64>,
}

const UNDERLINE_THICKNESS: f32 = 1.5;
/// Max distinct glyph buffers cached for the per-cell render path before a
/// wholesale clear — bounds memory (system-impact discipline).
const GLYPH_CACHE_CAP: usize = 4096;

/// Config RGB (sRGB) → wgpu clear colour. The surface is a plain (non-sRGB)
/// Unorm target, so the value is stored raw and shown as-is — pass the authored
/// sRGB color straight through, no linearization.
fn rgb_to_clear((r, g, b): (u8, u8, u8)) -> wgpu::Color {
    wgpu::Color { r: r as f64 / 255.0, g: g as f64 / 255.0, b: b as f64 / 255.0, a: 1.0 }
}

/// Config RGBA (sRGB + alpha) → a rect colour `[f32; 4]`. The rect shader passes
/// RGB straight to the Unorm target (no linearization); alpha is raw.
fn rgba_to_floats((r, g, b, a): (u8, u8, u8, u8)) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0]
}

/// Scale each edge of a `Padding` by the HiDPI ui factor.
fn scale_padding(p: Padding, ui: f32) -> Padding {
    Padding {
        left: p.left * ui,
        right: p.right * ui,
        top: p.top * ui,
        bottom: p.bottom * ui,
    }
}
const DOUBLE_UNDERLINE_GAP: f32 = 2.0;
const STRIKEOUT_THICKNESS: f32 = 1.5;

const CURSOR_PAD_X: f32 = 1.0;
const CURSOR_PAD_Y: f32 = 1.0;
// Cursor + selection colours are config-driven (see Renderer::cursor_color /
// selection_color); defaults live in config.rs.

/// Underline drawn beneath OSC 8 hyperlink ranges.
const LINK_UNDERLINE_COLOR: [f32; 4] = [0.40, 0.60, 0.95, 0.85];

/// Find-match highlight (all matches) and the current-match accent.
const FIND_MATCH_COLOR: [f32; 4] = [0.85, 0.75, 0.20, 0.40];
const FIND_CURRENT_COLOR: [f32; 4] = [1.0, 200.0 / 255.0, 80.0 / 255.0, 0.65];

/// Bell flash: a soft warm overlay drawn above everything for a fraction of
/// a second on `\a`.
const BELL_COLOR: [f32; 4] = [1.0, 0.9, 0.5, 0.18];
const BELL_DURATION: Duration = Duration::from_millis(120);

/// Multi-click window: a second/third click within this duration at the same
/// cell triggers word / line selection.
const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// Cursor blink period: full cycle in ms (half on, half off).
const CURSOR_BLINK_PERIOD_MS: u64 = 530;

/// Tick rate for auto-scroll while drag-selecting past the viewport edge.
const AUTOSCROLL_TICK_MS: u64 = 33;
/// Pixel margin past the viewport edge that triggers auto-scroll.
const AUTOSCROLL_MARGIN_PX: f32 = 8.0;

/// Tab bar height in physical px. Active and inactive tabs share this row;
/// Default tab-bar height; the live value lives on `Renderer` so it
/// can be configured. The constant remains as the fallback the free
/// `pane_grid` / `compute_grid_size` fns take as a parameter.
/// Minimum width of a tab in the tab bar. When more tabs are open than the
/// bar can fit at full width, they shrink uniformly down to this floor.
const TAB_ACTIVE_BG: [f32; 4] = [0.10, 0.10, 0.13, 1.0];
const TAB_INACTIVE_BG: [f32; 4] = [0.06, 0.06, 0.08, 1.0];
const TAB_ACTIVE_UNDERLINE: [f32; 4] =
    [1.0, 200.0 / 255.0, 80.0 / 255.0, 1.0];
const TAB_SEPARATOR: [f32; 4] = [0.16, 0.16, 0.20, 1.0];

/// Font size for tab titles, smaller than content text so they fit in the
/// bar nicely.
/// Ratio used to derive tab line height from `tab_font_size`. Matches
/// the prior hardcoded 26 / 18.
const TAB_LINE_RATIO: f32 = 26.0 / 18.0;
/// The display scale the config's pixel dimensions were tuned for — a 2x
/// Retina panel (where every default was authored). At runtime we multiply
/// every physical metric by `scale_factor / REFERENCE_SCALE`, so the UI is
/// the same perceptual size on a 1x monitor as on Retina: on Retina this is
/// x1.0 (identical to before, no config change), on a 1x external it's x0.5.
const REFERENCE_SCALE: f32 = 2.0;
/// Horizontal inset from the tab's left edge to where the title text starts.
const TAB_LABEL_INSET: f32 = 18.0;
/// Right-edge space reserved for the `×` close affordance.
const TAB_CLOSE_WIDTH: f32 = 32.0;

// Modal dialog (in-window). Centered card with Cancel/Confirm buttons.
const MODAL_BG_DIM: [f32; 4] = [0.0, 0.0, 0.0, 0.55];
const MODAL_CARD_BG: [f32; 4] = [0.10, 0.10, 0.13, 1.0];
const MODAL_CARD_BORDER: [f32; 4] = [0.18, 0.18, 0.24, 1.0];
const MODAL_BTN_BG: [f32; 4] = [0.06, 0.06, 0.08, 1.0];
const MODAL_BTN_CONFIRM_BG: [f32; 4] = [0.55, 0.15, 0.15, 1.0];
const MODAL_CARD_W: f32 = 520.0;
const MODAL_CARD_H: f32 = 220.0;
const MODAL_BTN_H: f32 = 56.0;
const MODAL_BTN_W: f32 = 140.0;
const MODAL_FONT_SIZE: f32 = 22.0;
const MODAL_LINE_H: f32 = 32.0;

/// Memory kill-switch. If peak RSS ever crosses this, terminite exits before
/// the OS does it for us (the 2026-05-20 incident took the whole Mac down at
/// ~76 GB). Override with `TERMINITE_RSS_LIMIT_GB`; set to `0` to disable.
const DEFAULT_RSS_LIMIT_GB: u64 = 4;

/// Selection coordinates are stored in alacritty's *absolute* `Line`
/// coordinate (viewport row minus the current display_offset). That way the
/// selection tracks the underlying grid content as the user scrolls — a
/// viewport-relative store would leave the highlight glued to fixed rows that
/// then show different content.
#[derive(Clone, Copy, PartialEq)]
struct Selection {
    anchor_line: i32,
    anchor_col: usize,
    head_line: i32,
    head_col: usize,
}

impl Selection {
    fn from_anchor(line: i32, col: usize) -> Self {
        Self {
            anchor_line: line,
            anchor_col: col,
            head_line: line,
            head_col: col,
        }
    }

    fn extend_to(&mut self, line: i32, col: usize) {
        self.head_line = line;
        self.head_col = col;
    }

    /// Return start <= end lexicographically.
    fn normalized(&self) -> ((i32, usize), (i32, usize)) {
        let a = (self.anchor_line, self.anchor_col);
        let h = (self.head_line, self.head_col);
        if a <= h {
            (a, h)
        } else {
            (h, a)
        }
    }

    fn is_empty(&self) -> bool {
        self.anchor_line == self.head_line && self.anchor_col == self.head_col
    }
}

/// Orientation of a split. `Vertical` puts children side by side (a vertical
/// divider); `Horizontal` stacks them (a horizontal divider).
#[derive(Copy, Clone, PartialEq)]
pub enum SplitDir {
    Vertical,
    Horizontal,
}

/// Look up `[r, g, b, a]` from the palette at the given index. Out-of-
/// range falls back to the "none" entry — defensive against stale data.
fn palette_color(idx: u8) -> [f32; 4] {
    COLOR_PALETTE
        .get(idx as usize)
        .map(|(_, c)| *c)
        .unwrap_or([0.0, 0.0, 0.0, 0.0])
}

/// Human-readable name for the palette entry — used in the menu item.
fn palette_name(idx: u8) -> &'static str {
    COLOR_PALETTE
        .get(idx as usize)
        .map(|(n, _)| *n)
        .unwrap_or("none")
}
/// Width reserved at the left of each pane's tab bar for the content-
/// kind selector — Blender-style: leftmost element in the area
/// header. Clicking it opens a popover with the available kinds.
/// Scales with the tab font so labels like `Welcome ▾` don't clip
/// the dropdown arrow at larger fonts.
fn kind_selector_w(tab_font_size: f32) -> f32 {
    // Roughly: 9 monospace cells (longest label + arrow) + insets,
    // floored at 110 so small fonts still feel clickable.
    (tab_font_size * 6.5 + 36.0).max(110.0)
}

/// Where `render_pane` placed one pane's content + tab-bar text — handed
/// back to `render` so it can build the `TextArea`s after every pane's
/// buffers are refreshed (phase 2 needs immutable borrows).
struct PaneDraw {
    pid: PaneId,
    /// Active tab's content text placement.
    text_left: f32,
    text_top: f32,
    bounds: TextBounds,
    /// One slot per tab in this pane's tab bar.
    tabs: Vec<TabLabelSlot>,
}

/// Placement for one tab's label + close glyph in a pane's tab bar.
struct TabLabelSlot {
    index: usize,
    is_active: bool,
    label_left: f32,
    label_bounds: TextBounds,
    close_left: f32,
    close_bounds: TextBounds,
    text_top: f32,
}

/// An in-progress divider drag: which `Split` (by path), its outer rect at
/// drag start, and its orientation.
struct DividerDrag {
    path: Vec<usize>,
    outer: PaneRect,
    dir: SplitDir,
}

/// An in-progress corner-handle drag that will split `pid` on release.
struct SplitGesture {
    pid: PaneId,
    start: (f32, f32),
}

pub struct Renderer {
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,

    font_system: FontSystem,
    /// Single-glyph shaped buffers for the per-cell render path, keyed by
    /// (grapheme, bold, italic, font_size bits). A glyph shaped in isolation has
    /// no inter-glyph kerning, so placing it at `col * cell_advance` tiles
    /// box-drawing perfectly while Advanced shaping keeps fallback. Bounded —
    /// cleared wholesale on overflow (a blunt but allocation-safe cap).
    glyph_cache: std::collections::HashMap<(String, bool, bool, u32), Buffer>,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,

    rects_below: RectRenderer,
    rects_above: RectRenderer,
    /// Tab bar rendered with a separate scissor zone above the text area.
    rects_tab_bar: RectRenderer,
    /// Modal overlay/card rendered on top of everything else.
    rects_modal: RectRenderer,
    /// Second text pipeline for tab bar labels — same atlas, separate prepare/
    /// render cycle so it can use a different scissor than the content text.
    tab_text_renderer: TextRenderer,
    /// Third text pipeline used exclusively for the in-window modal so its
    /// text can be drawn ON TOP of the modal background. (We can't share
    /// tab_text_renderer here — a second prepare would clobber the tab
    /// labels' vertex buffer before they render.)
    modal_text_renderer: TextRenderer,
    /// Textured-quad pipeline for displayed images (Kitty graphics).
    texture_renderer: TextureRenderer,
    /// Shared buffer for the `×` close glyph; reused via multiple TextAreas
    /// (one per tab) at different positions.
    close_buffer: Buffer,
    /// Label buffers for the kind-selector dropdown, keyed by the
    /// kind's `key()`. Built at startup for built-ins + every
    /// discovered module; reusable across all panes since the labels
    /// are stable per kind.
    kind_label_buffers: std::collections::HashMap<String, Buffer>,
    /// Registered modules — extension surface inhabitants.
    /// Step 2a: discovered at startup, surfaced in the dropdown +
    /// `module list` CLI. Step 2b spawns the binaries.
    modules: crate::modules::Registry,

    /// Layout metrics — derived once at startup from the config and a font
    /// measurement. font_size / family / padding are startup-applied; the
    /// config's live values may differ after a focus-reload.
    cell_advance: f32,
    line_height: f32,
    /// Window background (the wgpu clear) — from config, hot-reloadable.
    bg_color: wgpu::Color,
    /// Faint tint over the focused pane's content — from config, hot-reloadable.
    focus_tint: [f32; 4],
    /// Cursor + selection colours — from config, hot-reloadable.
    cursor_color: [f32; 4],
    selection_color: [f32; 4],
    pad: Padding,
    /// Block-label inset from the pane's left edge. Label sits in the
    /// strip `[pane.x + gutter_left, pane.x + pad.left]`.
    gutter_left: f32,
    /// Space between the block label's right edge and the line content.
    gutter_gap: f32,
    /// Cursor / tag highlight rect insets — tunable via the live config
    /// so the box can be dialed in for whatever font / line_height
    /// the user is running.
    highlight_pad_x: f32,
    highlight_pad_y: f32,
    highlight_offset_y: f32,
    /// Tab-label width range — tabs in a pane's bar shrink uniformly
    /// from `tab_max_width` down to `tab_min_width` as more tabs open.
    /// Live-tunable via config so the bar can be dialed in.
    tab_min_width: f32,
    tab_max_width: f32,
    /// Chrome font size for tab labels + close glyph + block labels.
    /// Startup-applied (the title buffers are pre-shaped at this size).
    tab_font_size: f32,
    /// Line height derived from `tab_font_size` (font * ratio). Cached
    /// so the render loop doesn't recompute.
    tab_line_h: f32,
    /// Height of the per-pane tab-bar strip in pixels. Threads through
    /// the grid math and the chrome layout.
    tab_bar_height: f32,
    /// Physical raster font size — `base_font_size * (scale_factor / REFERENCE_SCALE)`.
    font_size: f32,
    /// The user's chosen font size before HiDPI scaling (config default or a
    /// zoom). Zoom + persistence operate on this; `font_size` derives from it,
    /// so the zoom you set survives a move to a different-density monitor.
    base_font_size: f32,
    /// The window's current display scale factor (physical px per logical px).
    /// Re-read on resize; a change re-derives every scaled metric.
    scale_factor: f32,
    font_family: String,
    grid_cols: usize,
    grid_rows: usize,

    /// Bundled syntect grammars + theme. Loaded once at startup;
    /// every highlight call against it is read-only. ~10 MB resident
    /// — bought back any time the editor wants to colorize a body.
    highlight_store: crate::highlight::HighlightStore,
    /// fs-watch on `~/.terminite/modules/`. Holds the live notify
    /// watcher; dropping it stops the watch. `None` when the dir
    /// can't be located or the watcher fails to spawn — terminite
    /// still works, you just need the `module reload` CLI verb to
    /// pick up changes in that case.
    #[allow(dead_code)]
    modules_watcher: Option<crate::modules_watch::ModulesWatcher>,

    /// The window's pane tree. Every leaf is a `Pane` (a workspace with its
    /// own tab bar). `Option` only so split / close can `take()` and rebuild
    /// — always `Some` between operations.
    root: Option<PaneNode>,
    /// The focused pane; keyboard / mouse / wheel input routes to its active
    /// tab.
    active_pane: PaneId,
    /// Monotonic counter for new TabId allocation.
    next_tab_id: u64,
    /// Monotonic counter for new PaneId allocation.
    next_pane_id: u64,

    // Shared mouse / system state. Mouse position is window-relative.
    mouse_pos: (f32, f32),
    clipboard: Option<Clipboard>,
    /// When Some, a split divider is being dragged to resize.
    divider_drag: Option<DividerDrag>,
    /// When Some, a corner handle is being dragged to split a pane.
    split_gesture: Option<SplitGesture>,
    /// Last cursor icon set on the window — set only on change.
    cursor_icon: CursorIcon,

    /// Visual bell deadline; `Some(t)` means draw a flash overlay until `t`.
    bell_flash_until: Option<Instant>,
    /// Whether the window has keyboard focus — gates cursor blink.
    focused: bool,
    /// Renderer start time; cursor-blink phase is computed from elapsed.
    start_time: Instant,
    /// Last time auto-titles were refreshed — throttles the per-tab
    /// `proc_*` syscalls off the hot render path.
    last_title_refresh: Instant,
    /// In-flight IME preedit text rendered near the cursor.
    preedit: String,
    /// When Some, an in-window confirmation modal is up. While it's set,
    /// keyboard / mouse routing goes to the modal first; the PTY and tabs
    /// don't receive input.
    modal: Option<Modal>,
    /// When Some, the right-click context menu is up.
    context_menu: Option<ContextMenu>,
    /// When Some, the find bar is open and keyboard input edits the query.
    find: Option<FindState>,
    /// When Some, the command palette is open and keyboard input filters it.
    palette: Option<PaletteState>,

    // Deadlines surfaced via `next_wakeup()` to the main loop's
    // `ControlFlow::WaitUntil(...)`. We used to spawn a fresh OS thread per
    // bell / blink / autoscroll tick; with `\a`-spam the spawn rate outran
    // the kernel's thread-destruction rate and pinned the machine (the
    // 2026-05-20 watchdog panic). Deadlines drive everything now.
    next_blink_deadline: Option<Instant>,
    next_autoscroll_deadline: Option<Instant>,

    /// Backoff deadline for re-attempting a frame after the surface failed
    /// to deliver a texture (timeout / outdated / lost). Common during a
    /// display-mode switch — alt-tabbing out of a fullscreen game on a
    /// high-refresh external display renegotiates resolution+refresh, and
    /// the surface is perpetually invalid until it settles. The failure
    /// path used to call `request_redraw()` immediately, spinning render +
    /// surface-reconfigure at full CPU while the OS compositor was already
    /// fighting the bad mode — a livelock that could amplify a recoverable
    /// glitch into a system-wide stall (the 2026-06-03 alt-tab freeze).
    /// Routing the retry through this deadline + `WaitUntil` caps the
    /// cadence; `surface_retry_count` drives an exponential backoff and
    /// resets the instant a frame succeeds, so recovery stays immediate.
    next_surface_retry_deadline: Option<Instant>,
    surface_retry_count: u32,

    /// Peak-RSS kill-switch threshold in bytes; `0` disables. Checked once
    /// per frame in `render()`.
    rss_kill_bytes: u64,

    /// User config, reloaded when the window regains focus.
    config: Config,

    /// Held so new tabs can construct their `LiveTerm` with a Notifier
    /// pointing back at this event loop.
    proxy: EventLoopProxy<UserEvent>,

    /// The active proto-subscription writer, if any. v1 = single client;
    /// the slot holds `(conn_id, channel)` of the connection that most
    /// recently called `subscribe`. Conn-bound so only that connection's
    /// disconnect (or a send failure) clears it — an unrelated one-shot
    /// command connecting/disconnecting must not wipe a live subscriber.
    proto_subscriber: Option<(u64, std::sync::mpsc::SyncSender<crate::proto::OutMessage>)>,

    /// The comms base's push channels — one per subscribed actor. The faculty's
    /// receiver calls `room_subscribe {actor}` and terminite pushes directed
    /// messages for that actor down this writer as they arrive (delivery, not
    /// poll). Keyed by actor slug → (conn_id, writer); conn_id is for cleanup on
    /// disconnect. This is the comms base's substrate.
    room_subscribers:
        std::collections::HashMap<String, (u64, std::sync::mpsc::SyncSender<crate::proto::OutMessage>)>,

    /// Per-actor queue of directed messages that have been recorded but not yet
    /// CONSUMED (the receiver hasn't `room_ack`'d them). Drives catch-up: when a
    /// receiver subscribes, terminite re-delivers everything pending for it, so
    /// a message sent while the agent was away (or before its receiver attached)
    /// still arrives. Per-message, not a cursor — a message is a declared intent
    /// and is accounted individually. Capped per actor (`PENDING_CAP`).
    pending: std::collections::HashMap<String, Vec<u64>>,

    /// The delivery fate of each directed message (`message_id → MsgState`), so a
    /// sender can be told whether its message was processed, not just logged —
    /// R1's missing receipt. The layer already drives every transition (queue →
    /// deliver/floor-type → read/give-up); this just records it where the sender
    /// can read it. Bounded (`MESSAGE_STATE_CAP`, oldest ids evicted).
    message_state: std::collections::HashMap<u64, crate::proto::MsgState>,

    /// Loop-guard: recent delivery timestamps per actor (a small sliding window).
    /// Caps the rate terminite will push to any one actor, so two idle agents
    /// can't bounce messages into a runaway — the real danger isn't the chatter,
    /// it's unbounded resource use ([[feedback-system-impact-pass]]). Over-cap
    /// deliveries stay pending and catch up once the rate cools. Bounded: each
    /// deque is pruned to the window, never larger than `DELIVERY_MAX`.
    delivery_log: std::collections::HashMap<String, std::collections::VecDeque<u64>>,

    /// Stall watch: a directed message was delivered to this actor and it has
    /// gone silent — `(deadline, retries used)`. If the actor produces no
    /// activity by the deadline, terminite RE-DELIVERS (the base owns progress,
    /// so a stalled turn — a 529, or a weaker model freezing — recovers without
    /// a smart peer noticing). Cleared the moment the actor acts. Bounded:
    /// keyed by actor, retries capped, re-delivery rate-limited by the loop-guard.
    delivery_watch: std::collections::HashMap<String, (Instant, u8)>,

    /// File waiters: who asked for a path that was already held (`path → waiter
    /// slugs`). The instant the file frees (release, or claim expiry), terminite
    /// pushes each waiter a "file free" message through the comms base — they see
    /// the salt set down instead of polling. Cleared when notified; bounded.
    file_waiters: std::collections::HashMap<String, Vec<String>>,

    /// Last time each actor *did* something (emitted or made a tool call). The
    /// PTY floor's idle signal: an actor silent for `PTY_IDLE` is treated as
    /// sitting at its prompt, so terminite can safely type a held room message
    /// into its pane. A coarse cross-vendor proxy (agy has a precise idle hook).
    last_activity: std::collections::HashMap<String, Instant>,

    /// Last time the HUMAN typed into each pane (by tab id). The PTY floor gates
    /// on this, not on focus: you can *watch* a wake land on the pane you're
    /// looking at (focused but not typing), while the floor still never stomps a
    /// pane you're actively typing in.
    last_human_input: std::collections::HashMap<u64, Instant>,

    /// Self-declared interruption status per actor (slug → state + when set):
    /// `"busy"` = in a long process, DON'T inject; `"available"` = at my prompt,
    /// safe to deliver now. Absent ⇒ fall back to the silence heuristic. The
    /// precise signal only the agent has — so the room never assaults a peer
    /// mid-task. TTL-bounded so a forgotten `busy` doesn't block forever.
    actor_status: std::collections::HashMap<String, (String, Instant)>,

    /// The fast lane (slug → when entered). An actor in here gave *standing
    /// consent* to be driven: the PTY floor delivers promptly instead of only
    /// when idle. A separate axis from `actor_status` so the momentary brake
    /// (`busy`) composes with it — an auto actor can still protect an atomic
    /// step. TTL-bounded; a forgotten lane self-limits (it respects `busy`, and
    /// a gone actor has no pane to inject).
    actor_auto: std::collections::HashMap<String, Instant>,

    /// HALTED actors — benched by the human (the cancel ladder's reversible
    /// middle rung). A halted actor is ejected from room participation: no
    /// delivery reaches it, and its room actions (emit, tool-call, claim) are
    /// refused, until the human `room_release`s it. Unlike `busy` it does NOT
    /// expire — it's a deliberate hold the human lifts. (Local shell action is
    /// out of the room's reach; that's the human's KILL escalation.)
    quarantined: std::collections::HashSet<String>,

    /// Pending floor-message Enters. The text is typed immediately; the Enter
    /// follows a beat later so the TUI doesn't swallow it as paste content.
    /// Drained in `flush_pty_submits` on the WaitUntil tick. Each entry also
    /// carries what `room_message_cancel` needs to *unsend* in this window:
    /// the typed char count (to backspace-erase) and the message ids covered.
    /// Bounded by in-flight injections (≤ pending cap), one short Vec.
    pty_submit_queue: Vec<FloorSubmit>,

    /// The room's activity stream — workspace-global (not per-tab),
    /// because cross-pane visibility is the whole point. The lounge's
    /// substrate.
    activities: crate::activities::ActivityStore,
    /// Who is *present* in the room right now (attendance), keyed by proto
    /// connection id. Workspace-global like `activities`. Host-assigned
    /// color per actor; presence ends when the connection drops.
    roster: crate::presence::Roster,
    /// Advisory file claims — "who is working in which file right now", so
    /// residents co-edit without clobbering. Workspace-global; TTL-bounded.
    file_claims: crate::fileclaims::FileClaims,

    /// Recent frame timings in milliseconds — rolling window for the
    /// stats verb. Capped at `FRAME_TIMER_CAP` samples.
    frame_samples: std::collections::VecDeque<f32>,
    /// Wall-clock of the last completed frame, for delta computation.
    last_frame_end: Option<Instant>,
    /// Monotonic count of frames since startup.
    frame_count: u64,

    pub window: Arc<Window>,
}

impl Renderer {
    pub async fn new(window: Arc<Window>, proxy: EventLoopProxy<UserEvent>) -> Self {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);

        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .expect("terminite: failed to create the surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("terminite: no suitable GPU adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("terminite device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .expect("terminite: failed to acquire the GPU device");

        // Degrade, don't die. wgpu's default uncaptured-error handler is a hard
        // panic, so a GPU out-of-memory — frequently *external* pressure (another
        // GPU-heavy app saturating VRAM, e.g. a game) — would take the whole
        // terminal down with it (the 2026-06-03 wgpu OOM crash). Catch it and log
        // instead: the failing frame's GPU work drops and rendering recovers once
        // memory frees. Surface timeout/loss is already handled in the render
        // path; this covers device-level allocation failures.
        device.on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| match error {
            wgpu::Error::OutOfMemory { .. } => crate::logging::error(
                "wgpu: out of GPU memory — dropping this frame instead of panicking \
                 (likely external GPU pressure)",
            ),
            other => crate::logging::error(&format!("wgpu uncaptured error: {other}")),
        }));

        // Non-sRGB surface on purpose. With an sRGB target the blend
        // hardware composites glyph coverage in *linear* space, which
        // makes light-on-dark text look thin and gray. A plain Unorm
        // target + glyphon's `ColorMode::Web` blends coverage in sRGB
        // space (the "gamma-incorrect" path browsers and Ghostty use) so
        // text reads heavier and sharper. Everything that draws here
        // (rects, images, the clear color) outputs sRGB values directly
        // to match — no shader linearization.
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let mut font_system = FontSystem::new();
        // Embed terminite's own fixed-pitch fonts so a configured family always
        // resolves, regardless of the host's installed fonts.
        crate::fonts::load_bundled(&mut font_system);

        // Layout metrics from the config, locked for this run. line_height
        // derives from font_size; cell_advance is measured from the font.
        let config = Config::load();
        // HiDPI: scale every physical dimension by the display's scale relative
        // to the 2x reference the config was tuned for (see REFERENCE_SCALE).
        let scale_factor = window.scale_factor() as f32;
        let ui = scale_factor / REFERENCE_SCALE;
        let base_font_size = config.font_size;
        let font_size = (base_font_size * ui).round().max(1.0);
        let font_family = config.font_family.clone();
        let line_height = (font_size * LINE_H_RATIO * config.line_height).round();
        let bg_color = rgb_to_clear(config.background);
        let focus_tint = rgba_to_floats(config.focus_tint);
        let cursor_color = rgba_to_floats(config.cursor_color);
        let selection_color = rgba_to_floats(config.selection_color);
        let pad = scale_padding(config.padding, ui);
        let gutter_left = config.gutter_left * ui;
        let gutter_gap = config.gutter_gap * ui;
        let highlight_pad_x = config.highlight_pad_x * ui;
        let highlight_pad_y = config.highlight_pad_y * ui;
        let highlight_offset_y = config.highlight_offset_y * ui;
        let tab_min_width = config.tab_min_width * ui;
        let tab_max_width = config.tab_max_width * ui;
        let tab_font_size = (config.tab_font_size * ui).round().max(1.0);
        let tab_line_h = (tab_font_size * TAB_LINE_RATIO).round();
        let tab_bar_height = (config.tab_bar_height * ui).round();
        let cell_advance = measure_cell_advance(&mut font_system, font_size, &font_family);

        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas =
            TextAtlas::with_color_mode(&device, &queue, &cache, format, ColorMode::Web);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        let rects_below = RectRenderer::new(&device, format, "below");
        let rects_above = RectRenderer::new(&device, format, "above");
        let rects_tab_bar = RectRenderer::new(&device, format, "tab_bar");
        let rects_modal = RectRenderer::new(&device, format, "modal");
        let tab_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let modal_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let texture_renderer = TextureRenderer::new(&device, format);

        // winit's PhysicalSize is already in physical pixels — earlier code
        // multiplied by scale_factor a second time, so the grid math thought
        // the surface was 2x taller than it actually was on Retina, and rows
        // past visible got snapshotted into the buffer but rendered off the
        // bottom of the window.
        let physical_width = width as f32;
        let physical_height = height as f32;

        let (cols, rows) = compute_grid_size(
            physical_width,
            physical_height,
            cell_advance,
            line_height,
            pad,
            tab_bar_height,
        );
        let first_tab_id = TabId(0);
        let live_term = LiveTerm::new(
            cols,
            rows,
            cell_advance,
            line_height,
            proxy.clone(),
            first_tab_id,
            None,
            config.scrollback,
        );

        // Clipboard is optional; it's possible the platform refuses to give us
        // one (sandboxing, missing service). Copy/paste then become no-ops.
        let clipboard = Clipboard::new().ok();

        let first_title = "terminite".to_string();
        let first_title_buf = make_title_buffer(
            &mut font_system,
            &first_title,
            tab_font_size,
            tab_line_h,
            tab_max_width,
        );
        let first_text_buf = make_content_buffer(
            &mut font_system,
            cell_advance,
            line_height,
            font_size,
            &font_family,
            physical_width,
            physical_height,
        );
        let first_tab = Tab::new(
            first_tab_id,
            first_title,
            first_title_buf,
            live_term,
            first_text_buf,
            cols,
            rows,
        );
        let root = PaneNode::Leaf {
            id: PaneId(0),
            pane: Pane::single(first_tab),
        };
        let close_buffer = make_title_buffer(
            &mut font_system,
            "×",
            tab_font_size,
            tab_line_h,
            tab_max_width,
        );
        let modules = crate::modules::Registry::discover();
        let mut kind_label_buffers: std::collections::HashMap<String, Buffer> =
            std::collections::HashMap::new();
        let ksw_init = kind_selector_w(tab_font_size);
        let mut add_label = |fs: &mut FontSystem, key: &str, name: &str| {
            kind_label_buffers.insert(
                key.to_string(),
                make_title_buffer(
                    fs,
                    &format!("{name} ▾"),
                    tab_font_size,
                    tab_line_h,
                    ksw_init,
                ),
            );
        };
        add_label(&mut font_system, "shell", "Shell");
        add_label(&mut font_system, "welcome", "Welcome");
        for m in modules.list() {
            add_label(&mut font_system, &m.id, &m.name);
        }

        let mut renderer = Self {
            instance,
            surface,
            surface_config,
            device,
            queue,
            font_system,
            glyph_cache: std::collections::HashMap::new(),
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            rects_below,
            rects_above,
            rects_tab_bar,
            rects_modal,
            tab_text_renderer,
            modal_text_renderer,
            texture_renderer,
            close_buffer,
            kind_label_buffers,
            modules,
            cell_advance,
            line_height,
            bg_color,
            focus_tint,
            cursor_color,
            selection_color,
            pad,
            gutter_left,
            gutter_gap,
            highlight_pad_x,
            highlight_pad_y,
            highlight_offset_y,
            tab_min_width,
            tab_max_width,
            tab_font_size,
            tab_line_h,
            tab_bar_height,
            font_size,
            base_font_size,
            scale_factor,
            font_family,
            grid_cols: cols,
            grid_rows: rows,
            highlight_store: crate::highlight::HighlightStore::load(),
            modules_watcher: crate::modules::modules_dir()
                .and_then(|dir| crate::modules_watch::start(dir, proxy.clone())),
            root: Some(root),
            active_pane: PaneId(0),
            next_tab_id: 1,
            next_pane_id: 1,
            mouse_pos: (0.0, 0.0),
            clipboard,
            divider_drag: None,
            split_gesture: None,
            cursor_icon: CursorIcon::Default,
            bell_flash_until: None,
            focused: true,
            start_time: Instant::now(),
            last_title_refresh: Instant::now(),
            preedit: String::new(),
            modal: None,
            context_menu: None,
            find: None,
            palette: None,
            next_blink_deadline: None,
            next_autoscroll_deadline: None,
            next_surface_retry_deadline: None,
            surface_retry_count: 0,
            rss_kill_bytes: rss_kill_threshold_bytes(),
            config,
            proxy,
            proto_subscriber: None,
            room_subscribers: std::collections::HashMap::new(),
            pending: std::collections::HashMap::new(),
            message_state: std::collections::HashMap::new(),
            quarantined: std::collections::HashSet::new(),
            delivery_log: std::collections::HashMap::new(),
            delivery_watch: std::collections::HashMap::new(),
            file_waiters: std::collections::HashMap::new(),
            last_activity: std::collections::HashMap::new(),
            last_human_input: std::collections::HashMap::new(),
            actor_status: std::collections::HashMap::new(),
            actor_auto: std::collections::HashMap::new(),
            pty_submit_queue: Vec::new(),
            activities: crate::activities::ActivityStore::new(),
            roster: crate::presence::Roster::new(),
            file_claims: crate::fileclaims::FileClaims::new(),
            frame_samples: std::collections::VecDeque::with_capacity(FRAME_TIMER_CAP),
            last_frame_end: None,
            frame_count: 0,
            window,
        };
        // Size the first pane's buffers/grid to the laid-out pane rect
        // (the constructor built them at full surface size).
        renderer.relayout();
        renderer.sync_active_grid();
        renderer
    }

    // ── Pane tree accessors ───────────────────────────────────────────────

    fn root_ref(&self) -> &PaneNode {
        self.root.as_ref().expect("pane tree present")
    }

    fn root_mut(&mut self) -> &mut PaneNode {
        self.root.as_mut().expect("pane tree present")
    }

    fn active_pane_ref(&self) -> &Pane {
        self.root_ref()
            .find(self.active_pane)
            .expect("active pane present in tree")
    }

    fn active_pane_mut(&mut self) -> &mut Pane {
        let id = self.active_pane;
        self.root_mut()
            .find_mut(id)
            .expect("active pane present in tree")
    }

    fn active_tab_ref(&self) -> &Tab {
        self.active_pane_ref().active_tab_ref()
    }

    fn active_tab_mut(&mut self) -> &mut Tab {
        self.active_pane_mut().active_tab_mut()
    }

    /// The active tab of a specific pane.
    fn pane_tab_mut(&mut self, pid: PaneId) -> &mut Tab {
        self.root
            .as_mut()
            .expect("pane tree present")
            .find_mut(pid)
            .expect("pane present")
            .active_tab_mut()
    }

    /// Pixel rect of every pane leaf, filling the whole window.
    fn pane_layout(&self) -> Vec<(PaneId, PaneRect)> {
        let mut v = Vec::new();
        self.root_ref().layout(self.content_rect(), &mut v);
        v
    }

    /// The pane leaf (and its rect) under a window-relative point.
    fn pane_at(&self, x: f32, y: f32) -> Option<(PaneId, PaneRect)> {
        self.pane_layout()
            .into_iter()
            .find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
    }

    /// Earliest pending deadline the main loop should wake on
    /// (`ControlFlow::WaitUntil`). `None` = sleep until the next real event.
    pub fn next_wakeup(&self) -> Option<Instant> {
        // Collect deadlines from every tab's animation state (GIFs in
        // any visible — or hidden — pane). Walking every tab is fine:
        // animations are rare, and the alternative (tracking the
        // earliest globally) adds bookkeeping for negligible savings.
        let mut earliest_anim: Option<Instant> = None;
        if let Some(root) = self.root.as_ref() {
            let mut tabs: Vec<&Tab> = Vec::new();
            root.all_tabs(&mut tabs);
            for tab in tabs {
                if let Some(anim) = tab.animation.as_ref() {
                    if let Some(when) = anim.next_wakeup() {
                        earliest_anim = Some(
                            earliest_anim.map(|e| e.min(when)).unwrap_or(when),
                        );
                    }
                }
            }
        }
        let earliest_stall = self.delivery_watch.values().map(|(d, _)| *d).min();
        // While a PTY-floor message waits for its gate (unfocused + idle), tick
        // so we re-check; no tick when nothing's held.
        let pty_tick = self
            .has_pending_pty_work()
            .then(|| Instant::now() + PTY_RETRY);
        // A deferred floor Enter waiting to fire.
        let pty_submit = self.pty_submit_queue.iter().map(|s| s.deadline).min();
        [
            self.bell_flash_until,
            self.next_blink_deadline,
            self.next_autoscroll_deadline,
            self.next_surface_retry_deadline,
            earliest_anim,
            earliest_stall,
            pty_tick,
            pty_submit,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn focus_changed(&mut self, focused: bool) {
        self.focused = focused;
        // Re-read the config on focus-gain — edit it in another window,
        // switch back, and it applies. cursor_blink + bell_style take
        // effect immediately; padding / gutter_left / line_height apply
        // here via `apply_live_layout`; scrollback applies to tabs
        // opened afterward; font_size / font_family are startup-only.
        if focused {
            self.config = Config::load();
            self.apply_live_layout();
        }
        // Optionally emit DEC focus reporting when an app asked for it.
        let mode = self.active_tab_mut().live_term.mode_flags();
        if mode.focus_in_out {
            let seq: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
            self.active_tab_mut().live_term.write(seq.to_vec());
        }
        self.window.request_redraw();
    }
}

/// Resolve a display name for a PID — same logic the tab titles use, so
/// the modal body matches what the user sees in the bar.
fn proc_name_of(pid: i32) -> Option<String> {
    crate::term::process_display_name(pid)
}

/// Open a URI with the platform handler. macOS: `open <uri>`. The
/// launcher exits in milliseconds (it dispatches to the registered app
/// and quits), but we still need to reap it so it doesn't sit as a
/// zombie in the process table until terminite exits. One short-lived
/// thread per URI click, bounded by user click rate.
fn open_uri(uri: &str) {
    // Only handle the obvious safe schemes — never shell-anything-arbitrary.
    let ok = uri.starts_with("http://")
        || uri.starts_with("https://")
        || uri.starts_with("file://")
        || uri.starts_with("mailto:");
    if !ok {
        return;
    }
    #[cfg(target_os = "macos")]
    let spawn = std::process::Command::new("open").arg(uri).spawn();
    #[cfg(not(target_os = "macos"))]
    let spawn = std::process::Command::new("xdg-open").arg(uri).spawn();
    if let Ok(mut child) = spawn {
        std::thread::Builder::new()
            .name("terminite-uri-reap".into())
            .spawn(move || {
                let _ = child.wait();
            })
            .ok();
    }
}

fn rss_kill_threshold_bytes() -> u64 {
    let gb = std::env::var("TERMINITE_RSS_LIMIT_GB")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_RSS_LIMIT_GB);
    gb.saturating_mul(1024 * 1024 * 1024)
}

/// Peak resident set size since the process started, in bytes. Peak-not-
/// current is intentional: if we ever crossed the limit, we want to bail
/// even after the spike subsides — recovery from a runaway is not the
/// kill-switch's job.
fn process_rss_peak_bytes() -> Option<u64> {
    use std::mem::MaybeUninit;
    let mut usage = MaybeUninit::<libc::rusage>::uninit();
    let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if ret != 0 {
        return None;
    }
    // macOS reports `ru_maxrss` in bytes; Linux in kilobytes.
    let raw = unsafe { usage.assume_init() }.ru_maxrss as u64;
    #[cfg(target_os = "linux")]
    let raw = raw.saturating_mul(1024);
    Some(raw)
}

fn check_rss_kill_switch(limit_bytes: u64) {
    if limit_bytes == 0 {
        return;
    }
    if let Some(rss) = process_rss_peak_bytes()
        && rss > limit_bytes
    {
        let gib = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
        eprintln!(
            "terminite: peak RSS {:.2} GiB exceeded kill-switch limit {:.2} GiB — exiting \
             to protect the system. Override with TERMINITE_RSS_LIMIT_GB (=0 disables).",
            gib(rss),
            gib(limit_bytes),
        );
        std::process::exit(2);
    }
}
