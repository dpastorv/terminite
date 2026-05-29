//! The Renderer: assembles backgrounds, decorations, text, the cursor, and
//! selection highlights into a single frame. Two `RectRenderer` instances
//! sandwich the text — one draws *below* (backgrounds + selection + cursor),
//! one draws *above* (decorations).

use std::sync::Arc;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, Style,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use winit::event::{MouseButton, MouseScrollDelta};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, Window};

use crate::blocks::BlockStore;
use crate::config::{BellStyle, Config, Padding};
use crate::images::{self, Action};
use crate::palette::{color_to_floats, DEFAULT_FG};
use crate::rect::{RectInstance, RectRenderer};
use crate::term::{CursorShapeKind, DecorationKind, LiveTerm, ModeFlags, Snapshot, SpanStyle, TermScroll};
use crate::texture::{TextureImage, TextureInstance, TextureRenderer};
use crate::{TabId, UserEvent, BACKGROUND};

// The Renderer impl is split across these submodules (same type, multiple
// impl blocks). Each child sees this module's private items via `use super::*`.
mod acp;
mod config;
mod input;
mod io;
mod modules;
mod overlays;
mod panes;
mod proto;
mod render;

const UNDERLINE_THICKNESS: f32 = 1.5;
const DOUBLE_UNDERLINE_GAP: f32 = 2.0;
const STRIKEOUT_THICKNESS: f32 = 1.5;

const CURSOR_PAD_X: f32 = 1.0;
const CURSOR_PAD_Y: f32 = 1.0;
const CURSOR_COLOR: [f32; 4] = [1.0, 200.0 / 255.0, 80.0 / 255.0, 180.0 / 255.0];

/// Translucent steel-blue selection highlight.
const SELECTION_COLOR: [f32; 4] = [0.32, 0.46, 0.75, 0.35];

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

/// A pane's rectangle in physical pixels (top-left origin).
#[derive(Clone, Copy)]
struct PaneRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// One shell — a PTY plus its title and view state. The unit you tab
/// between inside a pane. (Pre-inversion this was `Pane`; the window now
/// owns the pane tree directly and each pane leaf owns a `Vec<Tab>`.)
struct Tab {
    id: TabId,
    title: String,
    /// Tab-bar label buffer; rebuilt whenever the displayed title changes.
    title_buffer: Buffer,
    /// Shell-set title (OSC 0/1/2) — when present, wins over the auto title.
    shell_title: Option<String>,
    /// Last auto-title we computed; rebuild the buffer only on changes.
    last_auto_title: String,
    live_term: LiveTerm,
    /// This tab's own cosmic-text content buffer.
    text_buffer: Buffer,
    /// Grid size this tab's PTY is currently sized to.
    cols: usize,
    rows: usize,
    pixel_offset: f32,
    selection: Option<Selection>,
    dragging: bool,
    last_drag_mouse_pos: (f32, f32),
    last_click: Option<(Instant, (i32, usize), u8)>,
    last_text_runs: Vec<(String, SpanStyle)>,
    /// Whether `text_buffer` currently holds `last_text_runs`'s content.
    buffer_dirty: bool,
    autoscroll_dir: Option<i32>,
    /// Most recent image from a Kitty `a=T` (transmit+display). v1 shows
    /// one image per tab at the pane's top-left, scaled to fit. Dropped
    /// when the tab drops (closes the GPU texture too).
    image: Option<TextureImage>,
    /// Set when an animated image (multi-frame GIF) is showing. Holds
    /// pre-uploaded per-frame textures + timing; the render path picks
    /// the current frame and the wakeup scheduler keeps the loop
    /// ticking. Mutually exclusive with `image` in practice — set_image
    /// clears both before installing one or the other.
    animation: Option<TabAnimation>,
    /// Per-tab block Model — populated from OSC 133 marks. Phase 2's
    /// shared coordinate system (`B7`) for the pair lives here.
    blocks: BlockStore,
    /// Which content type this tab is currently showing. Shell is the
    /// default and the only one with a live PTY behind it; other
    /// kinds suppress the shell render path and substitute their
    /// own. Bundle 6 step 1 — the dropdown stays inside built-ins.
    kind: TabContentKind,
    /// Lazily-shaped buffer for non-shell content (e.g., Welcome).
    /// `None` when the kind is Shell or the buffer hasn't been built
    /// for the current size. Rebuilt on resize.
    content_buffer: Option<Buffer>,
    /// Live *data* module session — module talks JSON via stdio,
    /// pushes `set_text` frames. None when not in a data module.
    module_session: Option<crate::modules::ModuleSession>,
    /// Cached body a data module last asked us to render.
    last_module_body: String,
    /// Live *tty* module — a second LiveTerm pointed at the module's
    /// binary instead of the user's shell. Rendered via the same
    /// vte/alacritty path as shells; input flows here for the
    /// duration the pane shows the module. None when not in a TTY
    /// module.
    module_pty: Option<LiveTerm>,
    /// Palette index for the tab's color band. `0` is none. Set via
    /// the right-click "Tab color" item; cycles through the palette.
    color_idx: u8,
    /// Vertical scroll offset (pixels) for data-module content. Reset
    /// to 0 whenever the body changes (unless `scroll_to_line` was
    /// supplied). Clamped against laid-out content height in the
    /// render path. Only data modules use this; shells have their
    /// own scrollback and TTY modules drive their own buffer.
    module_scroll_y: f32,
    /// "Please ensure this 0-indexed source line is visible after the
    /// next render" — set by `SetText { scroll_to_line: Some(N) }`,
    /// consumed (and cleared) by `render_non_shell_pane` once it
    /// knows the laid-out content height. Lets nav keep its cursor
    /// on screen as the user moves it.
    pending_ensure_visible: Option<u32>,
    /// Host-rendered cursor position for a data module (Editor). The
    /// render path draws a block cursor at (line, col) in the body
    /// using the same color + blink as a shell cursor. Stays `None`
    /// for modules with no cursor (Preview, Nav, …).
    module_cursor: Option<(u32, u32)>,
    /// Per-source-line gutter labels — empty string = no label for
    /// that line. Editor sends "1", "2", … for content lines and
    /// "" for header/prompt/blank. Rendered host-side at the y of
    /// each line's *first* layout run only, in a dim color, to the
    /// left of content. Content shifts right by the widest label's
    /// width when present.
    module_gutter: Option<Vec<String>>,
    /// Lazily-shaped buffer for the gutter labels themselves —
    /// rebuilt whenever `module_gutter` changes. Holds the joined
    /// gutter strings; the render path places it per first-run y.
    /// `None` when there's no gutter to render.
    gutter_buffer: Option<Buffer>,
    /// 0-indexed source line painted with a subtle background rect
    /// — Nav's selection row, Editor's cursor row. Spans all wrap
    /// segments of that line for continuous highlight.
    module_highlight_line: Option<u32>,
    /// Syntect language token (e.g. "rs", "py") for this body, or
    /// `None` for plain rendering. Editor sends a value derived
    /// from the file extension; Nav / Preview leave it `None`.
    module_language: Option<String>,
    /// Cached per-source-line color spans from syntect. Recomputed
    /// on body or language change; reused otherwise so steady-state
    /// cursor moves stay cheap.
    module_highlights: Option<crate::highlight::LineSpans>,
    /// Last `publish_focus` path this tab's module saw — persisted
    /// in the layout file so Editor reopens the same file on
    /// restore. Updated whenever the host sends a focus event to
    /// this tab's module session.
    last_focused_path: Option<String>,
    /// Multi-click bookkeeping for data-module panes — mirrors the
    /// shell-tab last_click pattern with body coordinates. Reset
    /// (or rolled over) by `dispatch_data_module_click`.
    last_module_click: Option<(Instant, u32, u32, u8)>,
    /// Live ACP session when this tab is hosting an Agent (kind ==
    /// Agent). Holds the subprocess, the turn list, and the
    /// composing draft. None when the tab is a Shell / Welcome /
    /// Module / TTY module.
    acp_session: Option<crate::acp::AcpSession>,
}

/// Hard cap on a single `set_text` body. A 16 MB body is already past
/// what glyphon can shape interactively; anything larger is almost
/// certainly a runaway module. We log + drop the message rather than
/// rebuild the content buffer for it.
const MAX_MODULE_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Per-tab animation state for multi-frame images (GIFs). Frames are
/// uploaded to the GPU once at decode time; the render path picks the
/// current one off the cumulative-delay table without copying.
///
/// Bounded by [`crate::images::MAX_ANIMATED_BYTES`] and
/// [`crate::images::MAX_ANIMATED_FRAMES`] upstream — by the time we
/// allocate textures the frame list is already capped.
struct TabAnimation {
    /// Frame textures in playback order. `frames.len() == cumulative.len()`.
    frames: Vec<TextureImage>,
    /// Display dimensions (max width/height across frames). Frames in
    /// a GIF can technically vary in size; we render every frame
    /// scaled into the same envelope so the pane doesn't jitter.
    width: u32,
    height: u32,
    /// `cumulative[i]` is the ms timestamp at which frame `i` *ends*.
    /// Lookup is a partition_point against `elapsed % total_ms`.
    cumulative: Vec<u64>,
    /// Total loop length in ms (== `cumulative.last()`).
    total_ms: u64,
    /// Wall-clock origin for the running loop. Stays fixed; the
    /// render path reads `started_at.elapsed()` for the position.
    started_at: Instant,
}

impl TabAnimation {
    fn current_index(&self) -> usize {
        if self.total_ms == 0 || self.frames.is_empty() {
            return 0;
        }
        let elapsed = self.started_at.elapsed().as_millis() as u64 % self.total_ms;
        self.cumulative
            .partition_point(|c| *c <= elapsed)
            .min(self.frames.len() - 1)
    }

    fn current_frame(&self) -> &TextureImage {
        &self.frames[self.current_index()]
    }

    /// Wall-clock instant when the next frame should appear. The main
    /// loop uses this for `ControlFlow::WaitUntil` so we wake exactly
    /// at the frame boundary, no per-tick polling.
    fn next_wakeup(&self) -> Option<Instant> {
        if self.total_ms == 0 || self.frames.is_empty() {
            return None;
        }
        let total_elapsed = self.started_at.elapsed().as_millis() as u64;
        let phase = total_elapsed % self.total_ms;
        let loops = total_elapsed / self.total_ms;
        let idx = self.cumulative.partition_point(|c| *c <= phase);
        let boundary = *self.cumulative.get(idx).unwrap_or(&self.total_ms);
        let absolute_ms = loops * self.total_ms + boundary;
        let offset_ms = absolute_ms.saturating_sub(total_elapsed);
        Some(Instant::now() + Duration::from_millis(offset_ms))
    }
}

/// What a tab currently shows. `Shell` has a live PTY behind it;
/// `Welcome` is a built-in static card; `Module(id)` is an
/// externally-registered module (step 2a: placeholder render only;
/// step 2b spawns the process and wires IPC); `Agent(name)` is an
/// ACP-hosted AI agent rendered as a structured chat surface.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum TabContentKind {
    Shell,
    Welcome,
    Module(String),
    Agent(String),
}

impl TabContentKind {
    /// Stable string key for label-buffer lookup. Built-ins get
    /// hard-coded strings; modules use their id.
    fn key(&self) -> &str {
        match self {
            TabContentKind::Shell => "shell",
            TabContentKind::Welcome => "welcome",
            TabContentKind::Module(id) => id.as_str(),
            TabContentKind::Agent(name) => name.as_str(),
        }
    }
}

impl Tab {
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: TabId,
        title: String,
        title_buffer: Buffer,
        live_term: LiveTerm,
        text_buffer: Buffer,
        cols: usize,
        rows: usize,
    ) -> Self {
        Self {
            id,
            title,
            title_buffer,
            shell_title: None,
            last_auto_title: String::new(),
            live_term,
            text_buffer,
            cols,
            rows,
            pixel_offset: 0.0,
            selection: None,
            dragging: false,
            last_drag_mouse_pos: (0.0, 0.0),
            last_click: None,
            last_text_runs: Vec::new(),
            buffer_dirty: true,
            autoscroll_dir: None,
            image: None,
            animation: None,
            blocks: BlockStore::new(),
            kind: TabContentKind::Shell,
            content_buffer: None,
            module_session: None,
            last_module_body: String::new(),
            module_pty: None,
            color_idx: 0,
            module_scroll_y: 0.0,
            pending_ensure_visible: None,
            module_cursor: None,
            module_gutter: None,
            gutter_buffer: None,
            module_highlight_line: None,
            module_language: None,
            module_highlights: None,
            last_focused_path: None,
            last_module_click: None,
            acp_session: None,
        }
    }

    /// The LiveTerm that should drive snapshot / input for this tab.
    /// TTY modules supply their own; everything else uses the tab's
    /// permanent shell. Preserves "non-destructive switch" — the
    /// shell stays alive in `live_term` even while a TTY module is
    /// running.
    fn active_term(&self) -> &LiveTerm {
        match (&self.kind, self.module_pty.as_ref()) {
            (TabContentKind::Module(_), Some(pty)) => pty,
            _ => &self.live_term,
        }
    }

    fn active_term_mut(&mut self) -> &mut LiveTerm {
        if matches!(self.kind, TabContentKind::Module(_)) && self.module_pty.is_some() {
            self.module_pty.as_mut().unwrap()
        } else {
            &mut self.live_term
        }
    }
}

/// A leaf of the window's pane tree — a self-contained workspace with its
/// own tab bar. Every leaf is equal; this is the Blender area model.
struct Pane {
    tabs: Vec<Tab>,
    active_tab: usize,
    /// Background palette index for the pane's content area. `0` is
    /// none (transparent). Set via the right-click "Pane bg" item.
    bg_idx: u8,
    /// Multiplier on the global `font_size` for this pane's content.
    /// `1.0` is the default; cycled via the right-click "Pane scale"
    /// item through `PANE_SCALE_PRESETS`. Buffer metrics + the pane's
    /// grid are rebuilt when this changes.
    font_scale: f32,
}

/// Available pane-scale presets — cycled through by the right-click
/// menu item. Default 100% is first so a freshly-set pane reads the
/// "off" state cleanly.
const PANE_SCALE_PRESETS: &[f32] = &[1.0, 0.8, 0.65, 1.25, 1.5];

/// Per-pane render metrics — the global config scaled by the pane's
/// `font_scale`. Returned by `pane_metrics`; callers that used to
/// read `self.font_size` / `self.cell_advance` / `self.line_height`
/// pull from here when rendering a specific pane.
#[derive(Copy, Clone)]
struct PaneMetrics {
    font_size: f32,
    cell_advance: f32,
    line_height: f32,
}

fn next_pane_scale(current: f32) -> f32 {
    let idx = PANE_SCALE_PRESETS
        .iter()
        .position(|s| (s - current).abs() < 0.01)
        .unwrap_or(0);
    PANE_SCALE_PRESETS[(idx + 1) % PANE_SCALE_PRESETS.len()]
}

impl Pane {
    fn single(tab: Tab) -> Self {
        Self {
            tabs: vec![tab],
            active_tab: 0,
            bg_idx: 0,
            font_scale: 1.0,
        }
    }

    fn active_tab_ref(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }
}

/// Identifies one pane (leaf) within a tab's pane tree. Monotonic.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct PaneId(u64);

/// Orientation of a split. `Vertical` puts children side by side (a vertical
/// divider); `Horizontal` stacks them (a horizontal divider).
#[derive(Copy, Clone, PartialEq)]
pub enum SplitDir {
    Vertical,
    Horizontal,
}

/// A binary tree of panes. Every leaf is a shell; every split divides its
/// rect between two children by `ratio`.
enum PaneNode {
    Leaf { id: PaneId, pane: Pane },
    Split {
        dir: SplitDir,
        /// Fraction of the parent rect given to `first`.
        ratio: f32,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

/// Pixel gap between split panes — also the divider's hit/draw thickness.
const DIVIDER_THICKNESS: f32 = 6.0;

impl PaneNode {
    /// Recursively assign pixel rects to every leaf for a given outer rect.
    fn layout(&self, rect: PaneRect, out: &mut Vec<(PaneId, PaneRect)>) {
        match self {
            PaneNode::Leaf { id, .. } => out.push((*id, rect)),
            PaneNode::Split { dir, ratio, first, second } => {
                let (r1, r2) = split_rect(rect, *dir, *ratio);
                first.layout(r1, out);
                second.layout(r2, out);
            }
        }
    }

    /// Consume the tree and return a new one where the leaf with `target`
    /// has been replaced by a Split at `ratio`: the old leaf becomes
    /// `first`, a fresh leaf `(new_id, new_pane)` becomes `second`.
    fn into_split(
        self,
        target: PaneId,
        dir: SplitDir,
        new_id: PaneId,
        new_pane: Pane,
        ratio: f32,
    ) -> PaneNode {
        match self {
            PaneNode::Leaf { id, pane } if id == target => PaneNode::Split {
                dir,
                ratio,
                first: Box::new(PaneNode::Leaf { id, pane }),
                second: Box::new(PaneNode::Leaf { id: new_id, pane: new_pane }),
            },
            leaf @ PaneNode::Leaf { .. } => leaf,
            PaneNode::Split { dir: d, ratio: r, first, second } => {
                if first.find(target).is_some() {
                    PaneNode::Split {
                        dir: d,
                        ratio: r,
                        first: Box::new(
                            first.into_split(target, dir, new_id, new_pane, ratio),
                        ),
                        second,
                    }
                } else {
                    PaneNode::Split {
                        dir: d,
                        ratio: r,
                        first,
                        second: Box::new(
                            second.into_split(target, dir, new_id, new_pane, ratio),
                        ),
                    }
                }
            }
        }
    }

    /// Consume the tree and return one with the `target` leaf removed —
    /// its sibling subtree takes the parent's place. Returns `None` if the
    /// tree was a single leaf == target (i.e. nothing left).
    fn into_closed(self, target: PaneId) -> Option<PaneNode> {
        match self {
            PaneNode::Leaf { id, .. } if id == target => None,
            leaf @ PaneNode::Leaf { .. } => Some(leaf),
            PaneNode::Split { dir, ratio, first, second } => {
                let first_has = first.find(target).is_some();
                let second_has = second.find(target).is_some();
                if first_has {
                    match first.into_closed(target) {
                        Some(f) => Some(PaneNode::Split {
                            dir,
                            ratio,
                            first: Box::new(f),
                            second,
                        }),
                        None => Some(*second),
                    }
                } else if second_has {
                    match second.into_closed(target) {
                        Some(s) => Some(PaneNode::Split {
                            dir,
                            ratio,
                            first,
                            second: Box::new(s),
                        }),
                        None => Some(*first),
                    }
                } else {
                    Some(PaneNode::Split { dir, ratio, first, second })
                }
            }
        }
    }

    /// Walk to the `Split` whose immediate layout produced `divider`-th
    /// boundary and adjust its ratio. Used by divider drag (Stage D); for
    /// now only `find`/`layout` are exercised.
    fn find(&self, target: PaneId) -> Option<&Pane> {
        match self {
            PaneNode::Leaf { id, pane } => (*id == target).then_some(pane),
            PaneNode::Split { first, second, .. } => {
                first.find(target).or_else(|| second.find(target))
            }
        }
    }

    fn find_mut(&mut self, target: PaneId) -> Option<&mut Pane> {
        match self {
            PaneNode::Leaf { id, pane } => (*id == target).then_some(pane),
            PaneNode::Split { first, second, .. } => {
                if let Some(p) = first.find_mut(target) {
                    return Some(p);
                }
                second.find_mut(target)
            }
        }
    }

    fn first_leaf_id(&self) -> PaneId {
        match self {
            PaneNode::Leaf { id, .. } => *id,
            PaneNode::Split { first, .. } => first.first_leaf_id(),
        }
    }

    fn leaf_count(&self) -> usize {
        match self {
            PaneNode::Leaf { .. } => 1,
            PaneNode::Split { first, second, .. } => {
                first.leaf_count() + second.leaf_count()
            }
        }
    }

    /// Collect a mutable reference to every tab in every pane of the tree.
    fn all_tabs_mut<'a>(&'a mut self, out: &mut Vec<&'a mut Tab>) {
        match self {
            PaneNode::Leaf { pane, .. } => {
                for t in pane.tabs.iter_mut() {
                    out.push(t);
                }
            }
            PaneNode::Split { first, second, .. } => {
                first.all_tabs_mut(out);
                second.all_tabs_mut(out);
            }
        }
    }

    /// Immutable variant of `all_tabs_mut` — read-only walk of every
    /// tab in the tree.
    fn all_tabs<'a>(&'a self, out: &mut Vec<&'a Tab>) {
        match self {
            PaneNode::Leaf { pane, .. } => {
                for t in pane.tabs.iter() {
                    out.push(t);
                }
            }
            PaneNode::Split { first, second, .. } => {
                first.all_tabs(out);
                second.all_tabs(out);
            }
        }
    }

    /// Collect every leaf `Pane` in the tree. Used when we need
    /// pane-level state (font_scale, bg) rather than per-tab data.
    fn all_panes<'a>(&'a self, out: &mut Vec<&'a Pane>) {
        match self {
            PaneNode::Leaf { pane, .. } => out.push(pane),
            PaneNode::Split { first, second, .. } => {
                first.all_panes(out);
                second.all_panes(out);
            }
        }
    }


    /// Find the split divider under a point. Returns the path to the owning
    /// `Split`, that split's outer rect, and its orientation.
    fn divider_at(
        &self,
        rect: PaneRect,
        x: f32,
        y: f32,
    ) -> Option<(Vec<usize>, PaneRect, SplitDir)> {
        let PaneNode::Split { dir, ratio, first, second } = self else {
            return None;
        };
        let (r1, r2) = split_rect(rect, *dir, *ratio);
        let m = DIVIDER_HIT_MARGIN;
        let self_hit = match dir {
            SplitDir::Vertical => {
                let gx = r1.x + r1.w;
                x >= gx - m
                    && x <= gx + DIVIDER_THICKNESS + m
                    && y >= rect.y
                    && y <= rect.y + rect.h
            }
            SplitDir::Horizontal => {
                let gy = r1.y + r1.h;
                y >= gy - m
                    && y <= gy + DIVIDER_THICKNESS + m
                    && x >= rect.x
                    && x <= rect.x + rect.w
            }
        };
        if self_hit {
            return Some((Vec::new(), rect, *dir));
        }
        if let Some((mut p, sr, sd)) = first.divider_at(r1, x, y) {
            p.insert(0, 0);
            return Some((p, sr, sd));
        }
        if let Some((mut p, sr, sd)) = second.divider_at(r2, x, y) {
            p.insert(0, 1);
            return Some((p, sr, sd));
        }
        None
    }

    /// Mutable reference to the ratio of the `Split` at `path`.
    fn split_ratio_at_mut(&mut self, path: &[usize]) -> Option<&mut f32> {
        match self {
            PaneNode::Leaf { .. } => None,
            PaneNode::Split { ratio, first, second, .. } => match path.split_first() {
                None => Some(ratio),
                Some((&0, rest)) => first.split_ratio_at_mut(rest),
                Some((_, rest)) => second.split_ratio_at_mut(rest),
            },
        }
    }
}

/// Colour of the seam drawn in a split's divider gap.
const DIVIDER_COLOR: [f32; 4] = [0.20, 0.20, 0.26, 1.0];

/// Extra grab margin each side of a divider — the seam is thin, so the
/// hit zone is widened for comfortable dragging.
const DIVIDER_HIT_MARGIN: f32 = 5.0;

/// Smallest a pane is allowed to be dragged to (tab bar + a row + padding).
const MIN_PANE: f32 = 140.0;

/// Clamp a split ratio so neither child shrinks below `MIN_PANE`.
fn clamp_ratio(ratio: f32, span: f32) -> f32 {
    let usable = (span - DIVIDER_THICKNESS).max(1.0);
    let min_frac = (MIN_PANE / usable).min(0.45);
    ratio.clamp(min_frac, 1.0 - min_frac)
}

/// Hit-box size of the corner split handle (top-right of every pane).
const SPLIT_HANDLE_SIZE: f32 = 18.0;
/// Leg length of the triangular grip drawn in that corner.
const SPLIT_GRIP: f32 = 14.0;
/// Resting colour of the split grip.
const SPLIT_HANDLE_COLOR: [f32; 4] = [0.34, 0.34, 0.40, 1.0];
/// Minimum drag distance before a corner gesture commits.
const SPLIT_GESTURE_THRESHOLD: f32 = 24.0;
/// Translucent wash over a pane the corner gesture would remove.
const REMOVE_PREVIEW_COLOR: [f32; 4] = [0.55, 0.16, 0.16, 0.38];

/// Draw the corner split grip — a small right triangle flush to a pane's
/// top-right corner (a "peel"), approximated by 1px-tall steps.
fn push_split_grip(out: &mut Vec<RectInstance>, pane: PaneRect, color: [f32; 4]) {
    let corner_x = pane.x + pane.w;
    let steps = SPLIT_GRIP as usize;
    for i in 0..steps {
        let w = SPLIT_GRIP - i as f32;
        out.push(RectInstance {
            rect: [corner_x - w, pane.y + i as f32, w, 1.0],
            color,
        });
    }
}

/// True if a point is inside a pane's corner split-handle hit box.
fn in_split_handle(pane: PaneRect, x: f32, y: f32) -> bool {
    x >= pane.x + pane.w - SPLIT_HANDLE_SIZE
        && x <= pane.x + pane.w
        && y >= pane.y
        && y <= pane.y + SPLIT_HANDLE_SIZE
}

/// What a committed corner-handle gesture does.
#[derive(Clone, Copy)]
enum GestureOutcome {
    Split(SplitDir),
    Remove,
}

/// Resolve a corner-drag delta: drag *into* the pane (down → stack,
/// left → side by side) splits it; drag back *out* (up / right) removes it.
/// `None` until the drag passes the commit threshold.
fn gesture_outcome(dx: f32, dy: f32) -> Option<GestureOutcome> {
    if dx.hypot(dy) < SPLIT_GESTURE_THRESHOLD {
        return None;
    }
    Some(if dy.abs() > dx.abs() {
        if dy > 0.0 {
            GestureOutcome::Split(SplitDir::Horizontal)
        } else {
            GestureOutcome::Remove
        }
    } else if dx < 0.0 {
        GestureOutcome::Split(SplitDir::Vertical)
    } else {
        GestureOutcome::Remove
    })
}

/// Ratio for a cursor-placed split — where the divider lands inside `pane`,
/// clamped so neither side falls below `MIN_PANE`.
fn split_ratio_from_cursor(pane: PaneRect, dir: SplitDir, cx: f32, cy: f32) -> f32 {
    let (raw, span) = match dir {
        SplitDir::Vertical => {
            ((cx - pane.x) / (pane.w - DIVIDER_THICKNESS).max(1.0), pane.w)
        }
        SplitDir::Horizontal => {
            ((cy - pane.y) / (pane.h - DIVIDER_THICKNESS).max(1.0), pane.h)
        }
    };
    clamp_ratio(raw, span)
}

/// Hard ceilings on the cell grid. No real terminal approaches these; they
/// exist so a degenerate font size, window size, or scrollback can't drive
/// a `Term` allocation (`cols × scrollback × Cell`) into OOM territory. The
/// per-frame RSS kill switch cannot catch a single runaway allocation, so
/// the grid must be bounded at the source.
const MAX_GRID_COLS: usize = 600;
const MAX_GRID_ROWS: usize = 400;
/// Cap on the rolling frame-time window used by the stats verb.
const FRAME_TIMER_CAP: usize = 120;

/// Shared palette for the per-tab color band + per-pane background tint.
/// Index 0 is "none" (transparent, the off state). Colors borrow from
/// the One Dark family already in `src/palette.rs` so a colored pane
/// reads as part of terminite's existing visual language.
const COLOR_PALETTE: &[(&str, [f32; 4])] = &[
    ("none",    [0.0, 0.0, 0.0, 0.0]),
    ("red",     [224.0 / 255.0, 108.0 / 255.0, 117.0 / 255.0, 1.0]),
    ("yellow",  [229.0 / 255.0, 192.0 / 255.0, 123.0 / 255.0, 1.0]),
    ("green",   [152.0 / 255.0, 195.0 / 255.0, 121.0 / 255.0, 1.0]),
    ("blue",    [ 97.0 / 255.0, 175.0 / 255.0, 239.0 / 255.0, 1.0]),
    ("magenta", [198.0 / 255.0, 120.0 / 255.0, 221.0 / 255.0, 1.0]),
    ("cyan",    [ 86.0 / 255.0, 182.0 / 255.0, 194.0 / 255.0, 1.0]),
];

/// Cycle the palette index forward by one, wrapping.
fn next_color_idx(idx: u8) -> u8 {
    ((idx as usize + 1) % COLOR_PALETTE.len()) as u8
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

/// Body text for each non-shell content kind. Modules render a
/// placeholder until step 2b lands process spawning + IPC.
fn non_shell_body(
    kind: &TabContentKind,
    registry: &crate::modules::Registry,
) -> String {
    match kind {
        TabContentKind::Shell => String::new(),
        TabContentKind::Welcome => "\
welcome to terminite — a terminal for the human + AI pair.

each pane runs a shell (Shell) or some other kind of inhabitant.
the leftmost dropdown in this pane's tab bar switches between them.
this pane is showing the Welcome inhabitant — read-only, static.
pick Shell from the dropdown to drop into a real shell.

two halves of the pair share one surface here. blocks (B1, B2, …)
in the left gutter are command + output units the pair can name.
the AI partner connects to ~/.terminite/socket and gets the same
coordinates you do. see guide/getting-started.md for more."
            .to_string(),
        TabContentKind::Module(id) => match registry.find(id) {
            Some(m) => format!(
                "module: {}  (v{})\nbinary: {}\nwaiting for the module to send its first frame…",
                m.name,
                m.version,
                m.binary.display(),
            ),
            None => format!(
                "module '{id}' is no longer registered.\npick a different kind from the dropdown."
            ),
        },
        TabContentKind::Agent(name) => format!(
            "agent: {name}\n\nspawning agent process…\nwaiting for the initialize handshake."
        ),
    }
}

// ── Proto helpers ────────────────────────────────────────────────────────

fn block_to_info(b: &crate::blocks::Block) -> crate::proto::BlockInfo {
    crate::proto::BlockInfo {
        id: b.id,
        exit_code: b.exit_code,
        prompt_line: b.prompt_line,
        command_end_line: b.command_end_line,
        output_start_line: b.output_start_line,
        output_end_line: b.output_end_line,
        tags: b.tags.clone(),
    }
}

/// Extract the command text for a block, converting from session-absolute
/// line coordinates back to alacritty's `Line` frame using the current
/// `history_size`. Returns `None` if the block lacks the marks needed to
/// bracket a range (e.g. `C` arrived without `A`).
fn block_command_text(tab: &Tab, block: &crate::blocks::Block) -> Option<String> {
    let start_abs = block.prompt_line?;
    // Prefer the explicit command-end mark; fall back to output-start.
    let end_abs = block.command_end_line.or(block.output_start_line)?;
    if end_abs < start_abs {
        return None;
    }
    let (_, history) = tab.live_term.offset_and_history();
    let start_line = start_abs - history as i32;
    let end_line = end_abs - history as i32;
    let max_col = tab.cols.saturating_sub(1);
    Some(
        tab.live_term
            .extract_text((start_line, 0), (end_line, max_col)),
    )
}

/// Extract the output text for a closed block. An open block returns
/// `None` — the AI should wait for the `block_closed` event, then ask.
///
/// Shells fire `D` *after* the trailing newline of the last output line,
/// so `output_end_line` is the row the cursor advanced to — which the
/// next prompt then takes. Trim that off; otherwise every block's
/// `.output` leaks the next block's `demo$ ...` line. Same trim Bundle 4
/// applies to Cmd-click block selection.
fn block_output_text(tab: &Tab, block: &crate::blocks::Block) -> Option<String> {
    let start_abs = block.output_start_line?;
    let end_abs = block.output_end_line?;
    if end_abs <= start_abs {
        // C and D fired on the same row — the command produced no
        // output rows before finishing. Empty string, not `None` —
        // callers want to know "block has no output," not "data
        // unavailable."
        return Some(String::new());
    }
    let (_, history) = tab.live_term.offset_and_history();
    let start_line = start_abs - history as i32;
    // `end_abs - 1` excludes the row the cursor moved to after the last
    // output newline — that row belongs to whatever comes next.
    let end_line = (end_abs - 1) - history as i32;
    let max_col = tab.cols.saturating_sub(1);
    Some(
        tab.live_term
            .extract_text((start_line, 0), (end_line, max_col)),
    )
}

/// Grid (cols, rows) that fits inside a pane's pixel rect. Each pane carves
/// its own `self.tab_bar_height` strip off the top, then the per-edge padding.
fn pane_grid(
    rect: PaneRect,
    cell_advance: f32,
    line_height: f32,
    pad: Padding,
    tab_bar_height: f32,
) -> (usize, usize) {
    let avail_w = (rect.w - pad.left - pad.right).max(cell_advance);
    let avail_h =
        (rect.h - tab_bar_height - pad.top - pad.bottom).max(line_height);
    let cols = (avail_w / cell_advance).floor().max(1.0) as usize;
    let rows = (avail_h / line_height).floor().max(1.0) as usize;
    (cols.min(MAX_GRID_COLS), rows.min(MAX_GRID_ROWS))
}

/// Geometry of each tab inside a pane's tab bar: `(x_start, width, is_active)`.
///
/// Widths are per-tab dynamic: each tab's *ideal* width is its measured
/// title width plus the chrome insets (label inset + close-glyph
/// reservation), clamped to `[min_width, max_width]`. If the sum fits in
/// the available bar, each tab gets exactly its ideal. If not, every
/// tab shrinks proportionally; nothing drops below `min_width` even
/// then. With enough tabs this can overflow — accept that, the user
/// either closes some or lives with clipping.
fn pane_tab_layout(
    rect: PaneRect,
    title_widths: &[f32],
    active: usize,
    min_width: f32,
    max_width: f32,
    kind_selector_w: f32,
) -> Vec<(f32, f32, bool)> {
    let n = title_widths.len();
    if n == 0 {
        return Vec::new();
    }
    // Reserve the left edge for the kind-selector dropdown (Blender-
    // style area-type picker) and the top-right corner for the split
    // handle.
    let avail = (rect.w - SPLIT_HANDLE_SIZE - kind_selector_w).max(min_width);
    let chrome = TAB_LABEL_INSET + TAB_CLOSE_WIDTH;
    let ideal: Vec<f32> = title_widths
        .iter()
        .map(|w| (w + chrome).clamp(min_width, max_width))
        .collect();
    let total: f32 = ideal.iter().sum();
    let widths: Vec<f32> = if total <= avail {
        ideal
    } else {
        let factor = avail / total;
        ideal.iter().map(|w| (w * factor).max(min_width)).collect()
    };
    let mut x = rect.x + kind_selector_w;
    let mut out = Vec::with_capacity(n);
    for (i, w) in widths.into_iter().enumerate() {
        out.push((x, w, i == active));
        x += w;
    }
    out
}

/// Render-shaped width of a chrome buffer's first line. Used to size
/// tabs to their actual title text rather than equal share.
fn measure_title_width(buf: &Buffer) -> f32 {
    buf.layout_runs().next().map(|r| r.line_w).unwrap_or(0.0)
}

/// Walk the pane tree, emitting one rect per split divider gap.
fn collect_dividers(node: &PaneNode, rect: PaneRect, out: &mut Vec<RectInstance>) {
    if let PaneNode::Split { dir, ratio, first, second } = node {
        let (r1, r2) = split_rect(rect, *dir, *ratio);
        let gap = match dir {
            SplitDir::Vertical => [r1.x + r1.w, rect.y, DIVIDER_THICKNESS, rect.h],
            SplitDir::Horizontal => [rect.x, r1.y + r1.h, rect.w, DIVIDER_THICKNESS],
        };
        out.push(RectInstance { rect: gap, color: DIVIDER_COLOR });
        collect_dividers(first, r1, out);
        collect_dividers(second, r2, out);
    }
}

fn split_rect(r: PaneRect, dir: SplitDir, ratio: f32) -> (PaneRect, PaneRect) {
    let d = DIVIDER_THICKNESS;
    match dir {
        SplitDir::Vertical => {
            let first_w = ((r.w - d) * ratio).max(0.0);
            let second_w = (r.w - d - first_w).max(0.0);
            (
                PaneRect { x: r.x, y: r.y, w: first_w, h: r.h },
                PaneRect {
                    x: r.x + first_w + d,
                    y: r.y,
                    w: second_w,
                    h: r.h,
                },
            )
        }
        SplitDir::Horizontal => {
            let first_h = ((r.h - d) * ratio).max(0.0);
            let second_h = (r.h - d - first_h).max(0.0);
            (
                PaneRect { x: r.x, y: r.y, w: r.w, h: first_h },
                PaneRect {
                    x: r.x,
                    y: r.y + first_h + d,
                    w: r.w,
                    h: second_h,
                },
            )
        }
    }
}

/// What the user is being asked to confirm. Generalized so we can reuse the
/// same modal for future yes/no decisions.
#[derive(Debug)]
enum ModalAction {
    CloseTab,
    ClosePane,
}

/// An action invoked from the right-click context menu.
enum MenuAction {
    Copy,
    Paste,
    OpenLink(String),
    SelectAll,
    /// Switch the active tab of the menu's pane to a different content
    /// kind. Bundle 6 step 1 — the dropdown surface.
    SetTabKind {
        pane: PaneId,
        kind: TabContentKind,
    },
    /// Advance the active tab's color band one step through the palette.
    CycleTabColor,
    /// Advance the active pane's background tint one step.
    CyclePaneBg,
    /// Cycle the active pane's font scale (100% / 80% / 65% / 125% / 150%).
    /// Triggers a per-pane buffer-metrics rebuild + grid resize.
    CyclePaneScale,
    /// Reveal `~/.terminite/modules/` in Finder so the user can
    /// drop a new module in. fs-watch picks it up automatically;
    /// no CLI dance needed.
    OpenModulesFolder,
}

/// One row in the context menu.
struct MenuItem {
    label_buf: Buffer,
    action: MenuAction,
    enabled: bool,
}

/// Right-click context menu — a small overlay anchored at the cursor.
struct ContextMenu {
    x: f32,
    y: f32,
    items: Vec<MenuItem>,
    /// Index of the item under the cursor, for hover highlight.
    hovered: Option<usize>,
}

const MENU_WIDTH: f32 = 320.0;
const MENU_ITEM_H: f32 = 40.0;
const MENU_BG: [f32; 4] = [0.12, 0.12, 0.15, 1.0];
const MENU_BORDER: [f32; 4] = [0.20, 0.20, 0.26, 1.0];
const MENU_HOVER_BG: [f32; 4] = [0.22, 0.30, 0.46, 1.0];

// Find bar — a floating box at the top-right of the content area.
const FIND_BAR_W: f32 = 420.0;
const FIND_BAR_H: f32 = 48.0;
const FIND_BAR_MARGIN: f32 = 16.0;
const FIND_BAR_BG: [f32; 4] = [0.12, 0.12, 0.15, 1.0];
const FIND_BAR_BORDER: [f32; 4] = [0.20, 0.20, 0.26, 1.0];

/// In-progress incremental search over the active tab's scrollback.
struct FindState {
    query: String,
    /// Text buffer for the find bar (`⌕ query    N/M`), rebuilt on change.
    bar_buf: Buffer,
    /// Absolute `(line, col_start, col_end)` matches, top-to-bottom.
    matches: Vec<(i32, usize, usize)>,
    /// Index of the current (accented) match.
    current: usize,
}

/// In-window modal dialog. Built when the user attempts to do something
/// destructive while a non-trivial process is running.
struct Modal {
    action: ModalAction,
    title_buf: Buffer,
    body_buf: Buffer,
    cancel_buf: Buffer,
    confirm_buf: Buffer,
    /// Hit boxes computed at layout time (origin x, y, w, h). Live for the
    /// frame; updated each render.
    cancel_rect: (f32, f32, f32, f32),
    confirm_rect: (f32, f32, f32, f32),
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
    font_size: f32,
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

    // Deadlines surfaced via `next_wakeup()` to the main loop's
    // `ControlFlow::WaitUntil(...)`. We used to spawn a fresh OS thread per
    // bell / blink / autoscroll tick; with `\a`-spam the spawn rate outran
    // the kernel's thread-destruction rate and pinned the machine (the
    // 2026-05-20 watchdog panic). Deadlines drive everything now.
    next_blink_deadline: Option<Instant>,
    next_autoscroll_deadline: Option<Instant>,

    /// Peak-RSS kill-switch threshold in bytes; `0` disables. Checked once
    /// per frame in `render()`.
    rss_kill_bytes: u64,

    /// User config, reloaded when the window regains focus.
    config: Config,

    /// Held so new tabs can construct their `LiveTerm` with a Notifier
    /// pointing back at this event loop.
    proxy: EventLoopProxy<UserEvent>,

    /// The active proto-subscription writer, if any. v1 = single client;
    /// the slot holds the channel of the connection that most recently
    /// called `subscribe`. Events are sent here; a `try_send` failure
    /// (channel full or disconnected) clears the slot.
    proto_subscriber: Option<std::sync::mpsc::SyncSender<crate::proto::OutMessage>>,

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

        let format = wgpu::TextureFormat::Bgra8UnormSrgb;
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

        // Layout metrics from the config, locked for this run. line_height
        // derives from font_size; cell_advance is measured from the font.
        let config = Config::load();
        let font_size = config.font_size;
        let font_family = config.font_family.clone();
        let line_height = (font_size * LINE_H_RATIO * config.line_height).round();
        let pad = config.padding;
        let gutter_left = config.gutter_left;
        let gutter_gap = config.gutter_gap;
        let highlight_pad_x = config.highlight_pad_x;
        let highlight_pad_y = config.highlight_pad_y;
        let highlight_offset_y = config.highlight_offset_y;
        let tab_min_width = config.tab_min_width;
        let tab_max_width = config.tab_max_width;
        let tab_font_size = config.tab_font_size;
        let tab_line_h = (tab_font_size * TAB_LINE_RATIO).round();
        let tab_bar_height = config.tab_bar_height;
        let cell_advance = measure_cell_advance(&mut font_system, font_size, &font_family);

        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
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
        for preset in crate::acp::presets() {
            add_label(
                &mut font_system,
                preset.display_name,
                preset.display_name,
            );
        }

        let mut renderer = Self {
            instance,
            surface,
            surface_config,
            device,
            queue,
            font_system,
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
            next_blink_deadline: None,
            next_autoscroll_deadline: None,
            rss_kill_bytes: rss_kill_threshold_bytes(),
            config,
            proxy,
            proto_subscriber: None,
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

    pub fn new_tab(&mut self) {
        // Inherit the active tab's shell cwd into the new shell.
        // Inherit the active tab's shell cwd into the new shell.
        let cwd = self.active_tab_ref().live_term.current_dir();
        let id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        // The new tab joins the active pane, sized to that pane's rect.
        let rect = self.active_pane_rect();
        let (cols, rows) = pane_grid(rect, self.cell_advance, self.line_height, self.pad, self.tab_bar_height);
        let live_term = LiveTerm::new(
            cols,
            rows,
            self.cell_advance,
            self.line_height,
            self.proxy.clone(),
            id,
            cwd,
            self.config.scrollback,
        );
        let title = "terminite".to_string();
        let title_buf = make_title_buffer(
            &mut self.font_system,
            &title,
            self.tab_font_size,
            self.tab_line_h,
            self.tab_max_width,
        );
        let text_buf = make_content_buffer(
            &mut self.font_system,
            self.cell_advance,
            self.line_height,
            self.font_size,
            &self.font_family,
            rect.w,
            rect.h,
        );
        let tab = Tab::new(id, title, title_buf, live_term, text_buf, cols, rows);
        let pane = self.active_pane_mut();
        pane.tabs.push(tab);
        pane.active_tab = pane.tabs.len() - 1;
        self.sync_active_grid();
        self.window.set_title(&self.active_tab_ref().title);
        self.window.request_redraw();
        self.persist_layout();
    }

    /// Request closing the active tab. If a non-shell process is in the
    /// foreground, opens an in-window modal — the caller observes `false`
    /// (didn't close) and the actual close happens when the user confirms.
    /// Otherwise closes immediately. Returns true if the window should
    /// exit (no tabs remain).
    pub fn close_active_tab(&mut self) -> bool {
        if self.modal.is_some() {
            return false;
        }
        let live = &self.active_tab_mut().live_term;
        if live.has_active_process() {
            let proc_name = live
                .foreground_pid()
                .and_then(proc_name_of)
                .unwrap_or_else(|| "A process".to_string());
            let title = "Close tab?".to_string();
            let body = format!("{proc_name} is running in this tab.");
            self.open_modal(ModalAction::CloseTab, title, body, "Cancel", "Close");
            return false;
        }
        self.do_close_active_tab()
    }

    /// Close the active tab. If it was the pane's last tab the pane closes
    /// too; if that was the window's last pane, returns true (window exits).
    fn do_close_active_tab(&mut self) -> bool {
        let pane = self.active_pane_mut();
        if pane.tabs.len() > 1 {
            let idx = pane.active_tab;
            pane.tabs.remove(idx);
            if pane.active_tab >= pane.tabs.len() {
                pane.active_tab = pane.tabs.len() - 1;
            }
            self.sync_active_grid();
            self.window.set_title(&self.active_tab_ref().title);
            self.window.request_redraw();
            self.persist_layout();
            return false;
        }
        // Last tab in this pane — close the pane itself.
        // close_active_pane already persists on its own.
        self.close_active_pane()
    }

    /// Switch the active pane to one of its tabs by index.
    pub fn switch_to_tab(&mut self, idx: usize) {
        let pane = self.active_pane_mut();
        if idx >= pane.tabs.len() || idx == pane.active_tab {
            return;
        }
        // Drop the prior tab's selection + drag state — same reason
        // we clear them on a pane switch. Otherwise a stale highlight
        // (and worse, a silent "your Cmd+C did nothing, clipboard
        // kept tab N's text") survives the switch.
        {
            let prior = pane.active_tab_mut();
            prior.selection = None;
            prior.dragging = false;
        }
        pane.active_tab = idx;
        self.sync_active_grid();
        self.window.set_title(&self.active_tab_ref().title);
        self.window.request_redraw();
    }

    pub fn next_tab(&mut self) {
        let pane = self.active_pane_ref();
        if pane.tabs.len() <= 1 {
            return;
        }
        let idx = (pane.active_tab + 1) % pane.tabs.len();
        self.switch_to_tab(idx);
    }

    pub fn prev_tab(&mut self) {
        let pane = self.active_pane_ref();
        if pane.tabs.len() <= 1 {
            return;
        }
        let idx = if pane.active_tab == 0 {
            pane.tabs.len() - 1
        } else {
            pane.active_tab - 1
        };
        self.switch_to_tab(idx);
    }

    /// Compute the modal's card + button rectangles for the current surface
    /// size. Also updates the cached hit-boxes on the open modal so mouse
    /// clicks resolve to the correct button.
    fn build_modal_rects(&mut self) -> Vec<RectInstance> {
        let modal = match self.modal.as_mut() {
            Some(m) => m,
            None => return Vec::new(),
        };
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let card_x = (surface_w - MODAL_CARD_W) * 0.5;
        let card_y = (surface_h - MODAL_CARD_H) * 0.5;
        let btn_y = card_y + MODAL_CARD_H - MODAL_BTN_H - 18.0;
        let gap = 16.0;
        let confirm_x = card_x + MODAL_CARD_W - MODAL_BTN_W - 18.0;
        let cancel_x = confirm_x - MODAL_BTN_W - gap;
        modal.cancel_rect = (cancel_x, btn_y, MODAL_BTN_W, MODAL_BTN_H);
        modal.confirm_rect = (confirm_x, btn_y, MODAL_BTN_W, MODAL_BTN_H);

        let border = 1.5;
        vec![
            // Dim full-surface overlay.
            RectInstance {
                rect: [0.0, 0.0, surface_w, surface_h],
                color: MODAL_BG_DIM,
            },
            // Card border (drawn slightly larger; card bg covers the interior).
            RectInstance {
                rect: [
                    card_x - border,
                    card_y - border,
                    MODAL_CARD_W + 2.0 * border,
                    MODAL_CARD_H + 2.0 * border,
                ],
                color: MODAL_CARD_BORDER,
            },
            RectInstance {
                rect: [card_x, card_y, MODAL_CARD_W, MODAL_CARD_H],
                color: MODAL_CARD_BG,
            },
            // Cancel button.
            RectInstance {
                rect: [cancel_x, btn_y, MODAL_BTN_W, MODAL_BTN_H],
                color: MODAL_BTN_BG,
            },
            // Confirm button (warm red — destructive emphasis).
            RectInstance {
                rect: [confirm_x, btn_y, MODAL_BTN_W, MODAL_BTN_H],
                color: MODAL_BTN_CONFIRM_BG,
            },
        ]
    }

    /// Emit one pane's tab-bar rects into `out`, and return a label slot per
    /// tab for the text pass. `rect` is the pane's full rect; the bar fills
    /// its top `self.tab_bar_height`. `is_active_pane` gates the gold underline so
    /// exactly one tab bar in the window marks where keystrokes go.
    fn build_pane_tab_bar(
        &self,
        pid: PaneId,
        rect: PaneRect,
        is_active_pane: bool,
        out: &mut Vec<RectInstance>,
    ) -> Vec<TabLabelSlot> {
        let pane = self.root_ref().find(pid).expect("pane present");
        let title_widths: Vec<f32> = pane
            .tabs
            .iter()
            .map(|t| measure_title_width(&t.title_buffer))
            .collect();
        let ksw = kind_selector_w(self.config.tab_font_size);
        let layout = pane_tab_layout(
            rect,
            &title_widths,
            pane.active_tab,
            self.tab_min_width,
            self.tab_max_width,
            ksw,
        );
        let bar_top = rect.y;
        // Bar background across the pane's width.
        out.push(RectInstance {
            rect: [rect.x, bar_top, rect.w, self.tab_bar_height],
            color: TAB_INACTIVE_BG,
        });
        // Kind selector — the leftmost element in the bar (Blender area
        // header model). Same bg as inactive tabs, with a separator on
        // its right edge. Click → opens a popover with available
        // kinds. The label text is emitted in render's phase 2.
        out.push(RectInstance {
            rect: [
                rect.x + ksw - 1.0,
                bar_top + 6.0,
                1.0,
                self.tab_bar_height - 12.0,
            ],
            color: TAB_SEPARATOR,
        });
        let text_top = bar_top + (self.tab_bar_height - self.tab_line_h) / 2.0;
        let mut slots = Vec::with_capacity(layout.len());
        for (i, (x, w, is_active)) in layout.iter().enumerate() {
            let (x, w, is_active) = (*x, *w, *is_active);
            out.push(RectInstance {
                rect: [x, bar_top, w, self.tab_bar_height],
                color: if is_active { TAB_ACTIVE_BG } else { TAB_INACTIVE_BG },
            });
            out.push(RectInstance {
                rect: [x + w - 1.0, bar_top + 6.0, 1.0, self.tab_bar_height - 12.0],
                color: TAB_SEPARATOR,
            });
            if is_active {
                // Gold underline only in the focused pane; a dim seam marks
                // the active tab of an unfocused pane.
                out.push(RectInstance {
                    rect: [x + 6.0, bar_top + self.tab_bar_height - 3.0, w - 12.0, 3.0],
                    color: if is_active_pane {
                        TAB_ACTIVE_UNDERLINE
                    } else {
                        TAB_SEPARATOR
                    },
                });
            }
            // Per-tab color band — a thin strip at the top of the tab
            // slot, so it sits above the active-tab underline at the
            // bottom and doesn't fight it. Drawn only when the tab
            // has a non-`none` color picked.
            let tab = &pane.tabs[i];
            if tab.color_idx != 0 {
                out.push(RectInstance {
                    rect: [x + 6.0, bar_top + 2.0, w - 12.0, 3.0],
                    color: palette_color(tab.color_idx),
                });
            }
            let label_left = x + TAB_LABEL_INSET;
            let label_right = (x + w - TAB_CLOSE_WIDTH).max(label_left);
            let close_left = x + w - TAB_CLOSE_WIDTH + 8.0;
            slots.push(TabLabelSlot {
                index: i,
                is_active,
                label_left,
                label_bounds: TextBounds {
                    left: label_left as i32,
                    top: bar_top as i32,
                    right: label_right as i32,
                    bottom: (bar_top + self.tab_bar_height) as i32,
                },
                close_left,
                close_bounds: TextBounds {
                    left: close_left as i32,
                    top: bar_top as i32,
                    right: (x + w) as i32,
                    bottom: (bar_top + self.tab_bar_height) as i32,
                },
                text_top,
            });
        }
        // Bottom border between the bar and the content.
        out.push(RectInstance {
            rect: [rect.x, bar_top + self.tab_bar_height, rect.w, 1.0],
            color: TAB_SEPARATOR,
        });
        // Corner split handle — a "peel" triangle; drag it to split (or,
        // dragged back out, to remove) this pane.
        let grip_active = self.split_gesture.as_ref().map(|g| g.pid) == Some(pid);
        push_split_grip(
            out,
            rect,
            if grip_active {
                TAB_ACTIVE_UNDERLINE
            } else {
                SPLIT_HANDLE_COLOR
            },
        );
        slots
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
        [
            self.bell_flash_until,
            self.next_blink_deadline,
            self.next_autoscroll_deadline,
            earliest_anim,
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

    pub fn ime_preedit(&mut self, text: String) {
        self.preedit = text;
        self.window.request_redraw();
    }

    pub fn ime_commit(&mut self, text: String) {
        self.preedit.clear();
        if !text.is_empty() {
            self.active_tab_mut().active_term().write(text.into_bytes());
        }
        self.window.request_redraw();
    }

    /// 1-indexed (col, row) inside the visible viewport, for mouse-reporting
    /// protocols. Returns `None` if the pointer is outside the text area.
    fn cell_at_1indexed(&self, x: f32, y: f32) -> Option<(u32, u32)> {
        let metrics = self.active_pane_metrics();
        let pad = self.pad;
        let line_height = metrics.line_height;
        let apr = self.active_pane_rect();
        let left = apr.x + pad.left;
        let top = apr.y + self.tab_bar_height + pad.top;
        if x < left {
            return None;
        }
        // pixel_offset correction so the reported cell is the one the user
        // visually clicked on, not the natural-grid cell.
        let row_f = (y - top - self.active_tab_ref().pixel_offset) / line_height;
        if row_f < 0.0 {
            return None;
        }
        let col = ((x - left) / metrics.cell_advance) as u32 + 1;
        let row = row_f as u32 + 1;
        if col as usize > self.grid_cols || row as usize > self.grid_rows {
            return None;
        }
        Some((col, row))
    }

}

// ── Mouse reporting helpers ──────────────────────────────────────────────

/// What kind of mouse event we're reporting. The button is needed for
/// press/release; motion/wheel use a synthetic encoding.
#[derive(Clone, Copy)]
enum MouseEvent {
    Press(MouseButton),
    Release(MouseButton),
    Motion,
    WheelUp,
    WheelDown,
}

/// Encode a mouse event in the format the foreground app has asked for.
/// SGR (1006) preferred; X10 used as fallback. Modifiers add the standard
/// shift / alt / ctrl bits. Returns `None` if reporting isn't enabled.
fn encode_mouse_report(
    mode: &ModeFlags,
    event: MouseEvent,
    modifiers: ModifiersState,
    col: u32,
    row: u32,
) -> Option<Vec<u8>> {
    if !mode.mouse_report_click && !mode.mouse_drag && !mode.mouse_motion {
        return None;
    }

    // Base button code.
    let (mut btn, is_release) = match event {
        MouseEvent::Press(b) => (button_code(b)?, false),
        MouseEvent::Release(b) => (button_code(b)?, true),
        MouseEvent::Motion => (32, false),         // motion modifier on no button
        MouseEvent::WheelUp => (64, false),
        MouseEvent::WheelDown => (65, false),
    };

    if modifiers.shift_key() {
        btn |= 4;
    }
    if modifiers.alt_key() {
        btn |= 8;
    }
    if modifiers.control_key() {
        btn |= 16;
    }

    if mode.sgr_mouse {
        let suffix = if is_release { 'm' } else { 'M' };
        Some(format!("\x1b[<{};{};{}{}", btn, col, row, suffix).into_bytes())
    } else {
        // X10: \e[M{btn+32}{col+32}{row+32}. Release uses button 3 (no info
        // about which button was released).
        let btn = if is_release { 3 } else { btn };
        let clamp = |v: u32| (v.min(223)) as u8;
        let mut out = Vec::with_capacity(6);
        out.extend_from_slice(b"\x1b[M");
        out.push((btn as u8).saturating_add(32));
        out.push(clamp(col).saturating_add(32));
        out.push(clamp(row).saturating_add(32));
        Some(out)
    }
}

fn button_code(button: MouseButton) -> Option<u32> {
    Some(match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        _ => return None,
    })
}

/// Line height as a multiple of font size — the pre-config ratio (36 at
/// font size 28). Derives `line_height` from the configured `font_size`.
const LINE_H_RATIO: f32 = 36.0 / 28.0;

/// The cosmic-text font family for a config `font_family` string — empty
/// means terminite's built-in monospace default.
fn font_family(name: &str) -> Family<'_> {
    if name.is_empty() {
        Family::Monospace
    } else {
        Family::Name(name)
    }
}

/// Build a content `Buffer` for a pane — monospace, one-cell glyph advance,
/// sized to the pane's pixel rect.
#[allow(clippy::too_many_arguments)]
fn make_content_buffer(
    font_system: &mut FontSystem,
    cell_advance: f32,
    line_height: f32,
    font_size: f32,
    family: &str,
    w: f32,
    h: f32,
) -> Buffer {
    let mut buf = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buf.set_size(font_system, Some(w.max(1.0)), Some(h.max(1.0)));
    buf.set_monospace_width(font_system, Some(cell_advance));
    buf.set_text(
        font_system,
        "",
        &Attrs::new().family(font_family(family)),
        Shaping::Advanced,
        None,
    );
    buf.shape_until_scroll(font_system, false);
    buf
}

/// Build a `Buffer` for modal-card text at a larger font size.
fn make_modal_buffer(font_system: &mut FontSystem, text: &str) -> Buffer {
    let metrics = Metrics::new(MODAL_FONT_SIZE, MODAL_LINE_H);
    let mut buf = Buffer::new(font_system, metrics);
    buf.set_size(font_system, Some(MODAL_CARD_W), Some(MODAL_LINE_H * 3.0));
    let attrs = Attrs::new().family(Family::Monospace);
    buf.set_text(font_system, text, &attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(font_system, false);
    buf
}

/// Build a small cosmic-text `Buffer` holding a tab title. Sized to the
/// maximum tab width so titles never wrap; bounds at render-time clip to the
/// actual tab area.
/// Convert a live `PaneNode` into the persistable `LayoutNode` —
/// snapshot all the data worth replaying, skip the live state
/// (PTYs, buffers, undo stacks). Called by `persist_layout` on
/// every structural change.
fn snapshot_node(node: &PaneNode) -> crate::layout::LayoutNode {
    match node {
        PaneNode::Leaf { pane, .. } => crate::layout::LayoutNode::Pane(snapshot_pane(pane)),
        PaneNode::Split { dir, ratio, first, second } => crate::layout::LayoutNode::Split {
            dir: match dir {
                SplitDir::Vertical => crate::layout::LayoutSplitDir::Vertical,
                SplitDir::Horizontal => crate::layout::LayoutSplitDir::Horizontal,
            },
            ratio: *ratio,
            first: Box::new(snapshot_node(first)),
            second: Box::new(snapshot_node(second)),
        },
    }
}

fn snapshot_pane(pane: &Pane) -> crate::layout::LayoutPane {
    crate::layout::LayoutPane {
        tabs: pane.tabs.iter().map(snapshot_tab).collect(),
        active_tab: pane.active_tab,
        bg_idx: pane.bg_idx,
        font_scale: pane.font_scale,
    }
}

fn snapshot_tab(tab: &Tab) -> crate::layout::LayoutTab {
    let kind = match &tab.kind {
        TabContentKind::Shell => crate::layout::LayoutTabKind::Shell,
        TabContentKind::Welcome => crate::layout::LayoutTabKind::Welcome,
        TabContentKind::Module(id) => crate::layout::LayoutTabKind::Module { id: id.clone() },
        TabContentKind::Agent(name) => crate::layout::LayoutTabKind::Agent { name: name.clone() },
    };
    // Capture cwd from the shell side. TTY modules with a PTY also
    // have a current_dir but it's the module process's cwd — not
    // useful to restore. Only persist for plain shells.
    let cwd = if matches!(tab.kind, TabContentKind::Shell) {
        tab.live_term
            .current_dir()
            .map(|p| p.to_string_lossy().to_string())
    } else {
        None
    };
    crate::layout::LayoutTab {
        kind,
        title: tab.title.clone(),
        color_idx: tab.color_idx,
        cwd,
        focused_path: tab.last_focused_path.clone(),
    }
}

/// Walk the tree to find the path (0/1 sequence) to the leaf with
/// the given id. `Some(vec![])` if the target is the root leaf.
/// `None` if the target doesn't exist in this tree.
fn path_to(node: &PaneNode, target: PaneId) -> Option<Vec<u8>> {
    match node {
        PaneNode::Leaf { id, .. } => {
            if *id == target { Some(Vec::new()) } else { None }
        }
        PaneNode::Split { first, second, .. } => {
            if let Some(mut p) = path_to(first, target) {
                p.insert(0, 0);
                Some(p)
            } else if let Some(mut p) = path_to(second, target) {
                p.insert(0, 1);
                Some(p)
            } else {
                None
            }
        }
    }
}

/// Resolve a path back to a PaneId in a freshly-built tree. Used
/// during restore to figure out which leaf to make active.
fn pane_id_at_path(node: &PaneNode, path: &[u8]) -> Option<PaneId> {
    if path.is_empty() {
        if let PaneNode::Leaf { id, .. } = node {
            return Some(*id);
        }
        return None;
    }
    if let PaneNode::Split { first, second, .. } = node {
        let (head, rest) = (path[0], &path[1..]);
        let child = if head == 0 { first.as_ref() } else { second.as_ref() };
        return pane_id_at_path(child, rest);
    }
    None
}

/// Cap for ACP-driven fs reads — same shape as the editor's load
/// limit. Keeps a hostile agent asking for /dev/zero from OOMing
/// the host.
const ACP_FS_MAX_BYTES: u64 = 4 * 1024 * 1024;

fn read_text_for_agent(
    path: &str,
    line: Option<u32>,
    limit: Option<u32>,
) -> Result<String, String> {
    use std::io::Read;
    let meta = std::fs::metadata(path).map_err(|e| format!("stat {path}: {e}"))?;
    if meta.len() > ACP_FS_MAX_BYTES {
        return Err(format!(
            "{path}: {} bytes > cap {}",
            meta.len(),
            ACP_FS_MAX_BYTES
        ));
    }
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mut buf = String::with_capacity(meta.len() as usize);
    f.read_to_string(&mut buf).map_err(|e| format!("read {path}: {e}"))?;
    if line.is_none() && limit.is_none() {
        return Ok(buf);
    }
    let start = line.unwrap_or(1).saturating_sub(1) as usize;
    let mut out = String::new();
    for (i, l) in buf.lines().enumerate().skip(start) {
        if let Some(n) = limit {
            if (i - start) as u32 >= n {
                break;
            }
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(l);
    }
    Ok(out)
}

fn write_text_for_agent(path: &str, content: &str) -> Result<(), String> {
    if content.len() as u64 > ACP_FS_MAX_BYTES {
        return Err(format!(
            "{path}: write payload {} bytes > cap {}",
            content.len(),
            ACP_FS_MAX_BYTES
        ));
    }
    let path_buf = std::path::PathBuf::from(path);
    if let Some(parent) = path_buf.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir parent: {e}"))?;
    }
    std::fs::write(&path_buf, content).map_err(|e| format!("write {path}: {e}"))
}

/// Render an ACP session's turn list as a plain-text body the
/// existing non-shell content_buffer path can shape. Each turn gets
/// a `─── role ───` divider; tool calls show inline with status;
/// a trailing `> draft` line shows what the user is composing.
fn render_acp_body(session: &crate::acp::AcpSession) -> String {
    use crate::acp::{ToolCallStatus, Turn};
    let mut out = String::new();
    let agent_label = "Agent";
    for turn in &session.turns {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        match turn {
            Turn::User { text } => {
                out.push_str("─── You ───\n");
                out.push_str(text);
            }
            Turn::Assistant { text, tool_calls, streaming } => {
                let header = if *streaming {
                    format!("─── {agent_label} (streaming…) ───")
                } else {
                    format!("─── {agent_label} ───")
                };
                out.push_str(&header);
                out.push('\n');
                if !text.is_empty() {
                    out.push_str(text);
                }
                for tc in tool_calls {
                    let marker = match tc.status {
                        ToolCallStatus::Pending => "○",
                        ToolCallStatus::InProgress => "◐",
                        ToolCallStatus::Completed => "●",
                        ToolCallStatus::Failed => "✗",
                    };
                    out.push_str(&format!(
                        "\n\n  {marker} [{}] {}",
                        tc.kind, tc.title
                    ));
                    if let Some(o) = &tc.output {
                        for line in o.lines() {
                            out.push_str("\n    ");
                            out.push_str(line);
                        }
                    }
                }
            }
        }
    }
    // Pending permission prompt — inline.
    if let Some(prompt) = &session.pending_permission {
        out.push_str("\n\n─── Permission requested ───\n");
        out.push_str(&prompt.title);
        out.push('\n');
        for (i, opt) in prompt.options.iter().enumerate() {
            let key: &str = match opt.kind.as_str() {
                "allow_once" => "a",
                "allow_always" => "A",
                "reject_once" => "r",
                "reject_always" => "R",
                _ => match i {
                    0 => "1",
                    1 => "2",
                    2 => "3",
                    _ => "4",
                },
            };
            out.push_str(&format!("\n  [{key}] {}", opt.name));
        }
    }
    // Composing draft at the bottom — what the user is typing.
    out.push_str("\n\n> ");
    out.push_str(&session.draft);
    out.push('_'); // cursor marker
    out
}

fn make_title_buffer(
    font_system: &mut FontSystem,
    title: &str,
    font_size: f32,
    line_h: f32,
    max_w: f32,
) -> Buffer {
    let metrics = Metrics::new(font_size, line_h);
    let mut buf = Buffer::new(font_system, metrics);
    // The buffer is sized to twice the max tab width so long titles
    // don't get pre-wrapped — the tab's `TextBounds` clips at display.
    buf.set_size(font_system, Some(max_w * 2.0), Some(line_h));
    let attrs = Attrs::new().family(Family::Monospace);
    buf.set_text(font_system, title, &attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(font_system, false);
    buf
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

fn compute_grid_size(
    physical_width: f32,
    physical_height: f32,
    cell_advance: f32,
    line_height: f32,
    pad: Padding,
    tab_bar_height: f32,
) -> (usize, usize) {
    // Full window as a single pane: one tab-bar strip plus per-edge pads.
    let available_w = (physical_width - pad.left - pad.right).max(cell_advance);
    let available_h =
        (physical_height - tab_bar_height - pad.top - pad.bottom).max(line_height);
    let cols = ((available_w / cell_advance) as usize).clamp(2, MAX_GRID_COLS);
    let rows = ((available_h / line_height) as usize).clamp(2, MAX_GRID_ROWS);
    (cols, rows)
}

/// Measure the one-cell advance width of the configured font at the
/// configured size, by shaping an `M` and reading its glyph advance.
fn measure_cell_advance(font_system: &mut FontSystem, font_size: f32, family: &str) -> f32 {
    let line_height = font_size * LINE_H_RATIO;
    let mut probe = Buffer::new(font_system, Metrics::new(font_size, line_height));
    probe.set_size(font_system, Some(1000.0), Some(line_height * 2.0));
    probe.set_text(
        font_system,
        "M",
        &Attrs::new().family(font_family(family)),
        Shaping::Advanced,
        None,
    );
    probe.shape_until_scroll(font_system, false);
    probe
        .layout_runs()
        .next()
        .and_then(|run| run.glyphs.first())
        .map(|glyph| glyph.w)
        .unwrap_or(font_size * 0.6)
        // Floor it: a degenerate measurement must never explode the grid.
        .max(2.0)
}

// ── Memory kill-switch ────────────────────────────────────────────────────

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
