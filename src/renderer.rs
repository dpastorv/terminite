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
    /// Number of leading body columns rendered in a dim color
    /// (line-number gutter). `0` / `None` = no dim region.
    module_dim_cols: Option<u32>,
    /// 0-indexed source line painted with a subtle background rect
    /// — Nav's selection row, Editor's cursor row.
    module_highlight_line: Option<u32>,
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
/// step 2b spawns the process and wires IPC).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum TabContentKind {
    Shell,
    Welcome,
    Module(String),
}

impl TabContentKind {
    /// Stable string key for label-buffer lookup. Built-ins get
    /// hard-coded strings; modules use their id.
    fn key(&self) -> &str {
        match self {
            TabContentKind::Shell => "shell",
            TabContentKind::Welcome => "welcome",
            TabContentKind::Module(id) => id.as_str(),
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
            module_dim_cols: None,
            module_highlight_line: None,
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

const MENU_WIDTH: f32 = 240.0;
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
            return false;
        }
        // Last tab in this pane — close the pane itself.
        self.close_active_pane()
    }

    /// True while an in-window modal is up — callers (main.rs) should route
    /// keyboard / mouse input to the modal handlers below.
    pub fn has_modal(&self) -> bool {
        self.modal.is_some()
    }

    pub fn modal_cancel(&mut self) {
        self.modal = None;
        self.window.request_redraw();
    }

    /// Confirm the open modal. Returns true if the window should exit.
    pub fn modal_confirm(&mut self) -> bool {
        let Some(modal) = self.modal.take() else { return false };
        self.window.request_redraw();
        match modal.action {
            ModalAction::CloseTab => self.do_close_active_tab(),
            ModalAction::ClosePane => {
                // close_active_pane is guarded against the last pane, so it
                // never signals an exit here.
                let _ = self.close_active_pane();
                false
            }
        }
    }

    /// Mouse click while the modal is up. Returns true if the window should
    /// exit (confirm hit on the last tab).
    pub fn modal_click(&mut self, x: f32, y: f32) -> bool {
        let Some(modal) = self.modal.as_ref() else { return false };
        let in_rect = |r: (f32, f32, f32, f32)| {
            x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
        };
        if in_rect(modal.confirm_rect) {
            return self.modal_confirm();
        }
        if in_rect(modal.cancel_rect) {
            self.modal_cancel();
        }
        false
    }

    /// Open the right-click context menu at a pixel position. Items depend
    /// on context: Copy is enabled only with a selection, Open Link only
    /// appears when the click landed on an OSC 8 hyperlink.
    fn open_context_menu(&mut self, x: f32, y: f32) {
        let (line, col) = self.pixel_to_absolute(x, y);
        let link = self.active_tab_mut().live_term.hyperlink_at(line, col);
        let has_selection = self
            .active_tab_ref()
            .selection
            .map(|s| !s.is_empty())
            .unwrap_or(false);

        let mut items: Vec<MenuItem> = Vec::new();
        items.push(MenuItem {
            label_buf: make_modal_buffer(&mut self.font_system, "Copy"),
            action: MenuAction::Copy,
            enabled: has_selection,
        });
        items.push(MenuItem {
            label_buf: make_modal_buffer(&mut self.font_system, "Paste"),
            action: MenuAction::Paste,
            enabled: true,
        });
        if let Some(uri) = link {
            items.push(MenuItem {
                label_buf: make_modal_buffer(&mut self.font_system, "Open Link"),
                action: MenuAction::OpenLink(uri),
                enabled: true,
            });
        }
        items.push(MenuItem {
            label_buf: make_modal_buffer(&mut self.font_system, "Select All"),
            action: MenuAction::SelectAll,
            enabled: true,
        });

        // Color items — apply to the active tab + active pane. Each
        // click cycles one step through the shared palette. The label
        // shows the *current* setting so the user knows what state
        // they're in before clicking.
        let tab_color_name =
            palette_name(self.active_tab_ref().color_idx);
        let pane_bg_name =
            palette_name(self.active_pane_ref().bg_idx);
        items.push(MenuItem {
            label_buf: make_modal_buffer(
                &mut self.font_system,
                &format!("Tab color: {tab_color_name}"),
            ),
            action: MenuAction::CycleTabColor,
            enabled: true,
        });
        items.push(MenuItem {
            label_buf: make_modal_buffer(
                &mut self.font_system,
                &format!("Pane bg: {pane_bg_name}"),
            ),
            action: MenuAction::CyclePaneBg,
            enabled: true,
        });
        let pane_scale_pct = (self.active_pane_ref().font_scale * 100.0).round() as i32;
        items.push(MenuItem {
            label_buf: make_modal_buffer(
                &mut self.font_system,
                &format!("Pane scale: {pane_scale_pct}%"),
            ),
            action: MenuAction::CyclePaneScale,
            enabled: true,
        });

        // Keep the menu fully on-screen.
        let h = items.len() as f32 * MENU_ITEM_H;
        let mx = x
            .min(self.surface_config.width as f32 - MENU_WIDTH - 4.0)
            .max(0.0);
        let my = y
            .min(self.surface_config.height as f32 - h - 4.0)
            .max(0.0);
        self.context_menu = Some(ContextMenu {
            x: mx,
            y: my,
            items,
            hovered: None,
        });
        self.window.request_redraw();
    }

    /// Open the kind-selector dropdown for one pane. Anchored at the
    /// bottom-left of that pane's selector slot, so it falls open like
    /// a normal dropdown rather than appearing where the cursor was.
    fn open_kind_dropdown(&mut self, pid: PaneId, prect: PaneRect) {
        let current = self
            .root_ref()
            .find(pid)
            .map(|p| p.active_tab_ref().kind.clone())
            .unwrap_or(TabContentKind::Shell);

        // Menu: built-ins first, then every discovered module in
        // registry order.
        let mut entries: Vec<(TabContentKind, String)> = vec![
            (TabContentKind::Shell, "Shell".to_string()),
            (TabContentKind::Welcome, "Welcome".to_string()),
        ];
        for m in self.modules.list() {
            entries.push((TabContentKind::Module(m.id.clone()), m.name.clone()));
        }

        let items: Vec<MenuItem> = entries
            .into_iter()
            .map(|(kind, name)| {
                let label = if kind == current {
                    format!("• {name}")
                } else {
                    format!("  {name}")
                };
                MenuItem {
                    label_buf: make_modal_buffer(&mut self.font_system, &label),
                    action: MenuAction::SetTabKind { pane: pid, kind },
                    enabled: true,
                }
            })
            .collect();
        let h = items.len() as f32 * MENU_ITEM_H;
        let mx = prect.x.max(0.0);
        let my = (prect.y + self.tab_bar_height)
            .min(self.surface_config.height as f32 - h - 4.0)
            .max(0.0);
        self.context_menu = Some(ContextMenu {
            x: mx,
            y: my,
            items,
            hovered: None,
        });
        self.window.request_redraw();
    }

    pub fn has_context_menu(&self) -> bool {
        self.context_menu.is_some()
    }

    pub fn dismiss_context_menu(&mut self) {
        self.context_menu = None;
        self.window.request_redraw();
    }

    /// Item index under a pixel position, or None if outside the menu.
    fn context_menu_at(&self, x: f32, y: f32) -> Option<usize> {
        let menu = self.context_menu.as_ref()?;
        if x < menu.x || x >= menu.x + MENU_WIDTH || y < menu.y {
            return None;
        }
        let idx = ((y - menu.y) / MENU_ITEM_H) as usize;
        (idx < menu.items.len()).then_some(idx)
    }

    /// Resolve a click while the menu is up: run the hit item's action (if
    /// enabled), then dismiss. A click anywhere just dismisses.
    fn context_menu_click(&mut self, x: f32, y: f32) {
        let hit = self.context_menu_at(x, y);
        let Some(menu) = self.context_menu.take() else { return };
        self.window.request_redraw();
        let Some(idx) = hit else { return };
        if !menu.items[idx].enabled {
            return;
        }
        match &menu.items[idx].action {
            MenuAction::Copy => self.copy_selection(),
            MenuAction::Paste => self.paste(),
            MenuAction::OpenLink(uri) => open_uri(uri),
            MenuAction::SelectAll => {
                let ((sl, sc), (el, ec)) =
                    self.active_tab_mut().live_term.whole_buffer();
                self.active_tab_mut().selection = Some(Selection {
                    anchor_line: sl,
                    anchor_col: sc,
                    head_line: el,
                    head_col: ec,
                });
                self.copy_selection();
            }
            MenuAction::SetTabKind { pane, kind } => {
                let pane = *pane;
                let kind = kind.clone();
                self.set_tab_kind(pane, kind);
            }
            MenuAction::CycleTabColor => {
                let tab = self.active_tab_mut();
                tab.color_idx = next_color_idx(tab.color_idx);
                self.window.request_redraw();
            }
            MenuAction::CyclePaneBg => {
                let pane = self.active_pane_mut();
                pane.bg_idx = next_color_idx(pane.bg_idx);
                self.window.request_redraw();
            }
            MenuAction::CyclePaneScale => {
                let pid = self.active_pane;
                let current = self
                    .root_ref()
                    .find(pid)
                    .map(|p| p.font_scale)
                    .unwrap_or(1.0);
                let next = next_pane_scale(current);
                self.apply_pane_scale(pid, next);
            }
        }
    }

    /// Build the rect instances for the context menu (background, border,
    /// hovered-item highlight).
    fn build_menu_rects(&self) -> Vec<RectInstance> {
        let Some(menu) = self.context_menu.as_ref() else {
            return Vec::new();
        };
        let h = menu.items.len() as f32 * MENU_ITEM_H;
        let border = 1.0;
        let mut rects = vec![
            RectInstance {
                rect: [
                    menu.x - border,
                    menu.y - border,
                    MENU_WIDTH + 2.0 * border,
                    h + 2.0 * border,
                ],
                color: MENU_BORDER,
            },
            RectInstance {
                rect: [menu.x, menu.y, MENU_WIDTH, h],
                color: MENU_BG,
            },
        ];
        if let Some(hov) = menu.hovered {
            if menu.items[hov].enabled {
                rects.push(RectInstance {
                    rect: [
                        menu.x,
                        menu.y + hov as f32 * MENU_ITEM_H,
                        MENU_WIDTH,
                        MENU_ITEM_H,
                    ],
                    color: MENU_HOVER_BG,
                });
            }
        }
        rects
    }

    // ── Find ──────────────────────────────────────────────────────────────

    pub fn has_find(&self) -> bool {
        self.find.is_some()
    }

    pub fn open_find(&mut self) {
        let bar_buf = make_modal_buffer(&mut self.font_system, "Find:");
        self.find = Some(FindState {
            query: String::new(),
            bar_buf,
            matches: Vec::new(),
            current: 0,
        });
        self.window.request_redraw();
    }

    pub fn close_find(&mut self) {
        self.find = None;
        self.window.request_redraw();
    }

    pub fn find_input(&mut self, ch: char) {
        if let Some(find) = self.find.as_mut() {
            find.query.push(ch);
        }
        self.rerun_search();
    }

    pub fn find_backspace(&mut self) {
        if let Some(find) = self.find.as_mut() {
            find.query.pop();
        }
        self.rerun_search();
    }

    pub fn find_next(&mut self) {
        if let Some(find) = self.find.as_mut() {
            if !find.matches.is_empty() {
                find.current = (find.current + 1) % find.matches.len();
            }
        }
        self.rebuild_find_bar();
        self.scroll_to_current_match();
        self.window.request_redraw();
    }

    pub fn find_prev(&mut self) {
        if let Some(find) = self.find.as_mut() {
            if !find.matches.is_empty() {
                find.current = if find.current == 0 {
                    find.matches.len() - 1
                } else {
                    find.current - 1
                };
            }
        }
        self.rebuild_find_bar();
        self.scroll_to_current_match();
        self.window.request_redraw();
    }

    /// Re-run the search for the current query and reset to the first match.
    fn rerun_search(&mut self) {
        let query = match self.find.as_ref() {
            Some(f) => f.query.clone(),
            None => return,
        };
        let matches = self.active_tab_mut().live_term.search(&query);
        if let Some(find) = self.find.as_mut() {
            find.matches = matches;
            find.current = 0;
        }
        self.rebuild_find_bar();
        self.scroll_to_current_match();
        self.window.request_redraw();
    }

    fn rebuild_find_bar(&mut self) {
        let text = match self.find.as_ref() {
            Some(f) if f.query.is_empty() => "Find:".to_string(),
            Some(f) if f.matches.is_empty() => {
                format!("Find: {}   no matches", f.query)
            }
            Some(f) => {
                format!("Find: {}   {}/{}", f.query, f.current + 1, f.matches.len())
            }
            None => return,
        };
        let buf = make_modal_buffer(&mut self.font_system, &text);
        if let Some(find) = self.find.as_mut() {
            find.bar_buf = buf;
        }
    }

    fn scroll_to_current_match(&mut self) {
        let target = self
            .find
            .as_ref()
            .and_then(|f| f.matches.get(f.current).copied());
        if let Some((line, _, _)) = target {
            let rows = self.grid_rows;
            self.active_tab_mut()
                .live_term
                .scroll_to_line(line, rows);
        }
    }

    fn open_modal(
        &mut self,
        action: ModalAction,
        title: String,
        body: String,
        cancel: &str,
        confirm: &str,
    ) {
        let title_buf = make_modal_buffer(&mut self.font_system, &title);
        let body_buf = make_modal_buffer(&mut self.font_system, &body);
        let cancel_buf = make_modal_buffer(&mut self.font_system, cancel);
        let confirm_buf = make_modal_buffer(&mut self.font_system, confirm);
        self.modal = Some(Modal {
            action,
            title_buf,
            body_buf,
            cancel_buf,
            confirm_buf,
            cancel_rect: (0.0, 0.0, 0.0, 0.0),
            confirm_rect: (0.0, 0.0, 0.0, 0.0),
        });
        self.window.request_redraw();
    }

    /// Switch the active pane to one of its tabs by index.
    pub fn switch_to_tab(&mut self, idx: usize) {
        let pane = self.active_pane_mut();
        if idx >= pane.tabs.len() || idx == pane.active_tab {
            return;
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

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        self.relayout();
        self.sync_active_grid();
    }

    /// The whole window — the rect the pane tree fills. Each pane carves its
    /// own tab bar off the top of its slice.
    fn content_rect(&self) -> PaneRect {
        PaneRect {
            x: 0.0,
            y: 0.0,
            w: self.surface_config.width as f32,
            h: self.surface_config.height as f32,
        }
    }

    /// Pixel rect of the active pane.
    fn active_pane_rect(&self) -> PaneRect {
        let active = self.active_pane;
        self.pane_layout()
            .into_iter()
            .find(|(id, _)| *id == active)
            .map(|(_, r)| r)
            .unwrap_or_else(|| self.content_rect())
    }

    /// Recompute every pane's pixel rect and resize every tab's PTY / buffer
    /// to fit. Background tabs are kept accurate too — shells resize on the
    /// SIGWINCH alacritty sends, so they must stay correct for the switch.
    /// Per-pane render metrics, computed by scaling the global values
    /// by `pane.font_scale`. Approximate — actual cell advance depends
    /// on font shaping at the target size — but close enough for v1.
    fn pane_metrics(&self, pid: PaneId) -> PaneMetrics {
        let scale = self
            .root_ref()
            .find(pid)
            .map(|p| p.font_scale)
            .unwrap_or(1.0);
        PaneMetrics {
            font_size: self.font_size * scale,
            cell_advance: self.cell_advance * scale,
            line_height: (self.line_height * scale).round().max(1.0),
        }
    }

    fn active_pane_metrics(&self) -> PaneMetrics {
        self.pane_metrics(self.active_pane)
    }

    /// Find the `font_scale` of whatever pane currently owns `tab_id`,
    /// or 1.0 if the tab vanished. Used when shaping block labels so
    /// they're sized to the pane's content from the moment they're
    /// created.
    fn scale_for_tab(&self, tab_id: TabId) -> f32 {
        let mut leaves: Vec<&Pane> = Vec::new();
        self.root_ref().all_panes(&mut leaves);
        for p in leaves {
            if p.tabs.iter().any(|t| t.id == tab_id) {
                return p.font_scale;
            }
        }
        1.0
    }

    /// Set one pane's font scale and rebuild its tab buffers + grid.
    /// Cheap when nothing changed.
    fn apply_pane_scale(&mut self, pid: PaneId, scale: f32) {
        // Short-circuit on no-op.
        let changed = self
            .root
            .as_mut()
            .and_then(|n| n.find_mut(pid))
            .map(|p| {
                let diff = (p.font_scale - scale).abs() > 0.01;
                if diff {
                    p.font_scale = scale;
                }
                diff
            })
            .unwrap_or(false);
        if !changed {
            return;
        }
        let metrics = self.pane_metrics(pid);
        let font_metrics = Metrics::new(metrics.font_size, metrics.line_height);
        // Block labels are content-anchored, so they scale too. Use
        // `LABEL_LINE_H * scale` rather than `metrics.line_height` —
        // the label has its own line-height ratio independent of the
        // content's `line_height` multiplier.
        let scale = metrics.font_size / self.font_size;
        let label_font_size = crate::blocks::LABEL_FONT_SIZE * scale;
        let label_line_h = (crate::blocks::LABEL_LINE_H * scale).max(1.0);
        if let Some(p) = self.root.as_mut().and_then(|n| n.find_mut(pid)) {
            for tab in p.tabs.iter_mut() {
                tab.text_buffer.set_metrics(&mut self.font_system, font_metrics);
                tab.content_buffer = None;
                tab.buffer_dirty = true;
                tab.last_text_runs.clear();
                tab.blocks.rescale_labels(
                    &mut self.font_system,
                    label_font_size,
                    label_line_h,
                );
            }
        }
        self.relayout();
        self.sync_active_grid();
        self.window.request_redraw();
    }

    fn relayout(&mut self) {
        for (pid, rect) in self.pane_layout() {
            let metrics = self.pane_metrics(pid);
            let (cols, rows) = pane_grid(
                rect,
                metrics.cell_advance,
                metrics.line_height,
                self.pad,
                self.tab_bar_height,
            );
            let content_h = (rect.h - self.tab_bar_height).max(1.0);
            let pane = self
                .root
                .as_mut()
                .expect("pane tree present")
                .find_mut(pid)
                .expect("laid-out pane present");
            for tab in pane.tabs.iter_mut() {
                tab.text_buffer.set_size(
                    &mut self.font_system,
                    Some(rect.w.max(1.0)),
                    Some(content_h),
                );
                if tab.cols != cols || tab.rows != rows {
                    // Resize the shell *and* any active TTY module —
                    // both need to react to pane geometry changes.
                    tab.live_term.resize(cols, rows);
                    if let Some(pty) = tab.module_pty.as_ref() {
                        pty.resize(cols, rows);
                    }
                    tab.cols = cols;
                    tab.rows = rows;
                    // A resize invalidates the snapshot cache and selection.
                    tab.last_text_runs.clear();
                    tab.buffer_dirty = true;
                    tab.selection = None;
                }
            }
        }
    }

    /// Apply the layout-affecting config knobs (per-edge padding,
    /// `gutter_left`, `line_height` multiplier) to the running window.
    /// Called from `focus_changed` after `Config::load`, so the tuning
    /// loop is: edit `~/.config/terminite/config.toml` in a side pane,
    /// click back into terminite, see the values apply.
    ///
    /// Only line_height needs per-tab work — it lives in each buffer's
    /// `Metrics`, so we touch every tab to update the metrics and mark
    /// the snapshot dirty. Padding / gutter_left are positional and
    /// propagate to the next frame automatically; `relayout` recomputes
    /// the grid with the new pad on top.
    fn apply_live_layout(&mut self) {
        let new_line_height =
            (self.font_size * LINE_H_RATIO * self.config.line_height).round();
        let line_height_changed = (new_line_height - self.line_height).abs() > f32::EPSILON;
        let pad_or_gutter_changed = self.pad != self.config.padding
            || self.gutter_left != self.config.gutter_left
            || self.gutter_gap != self.config.gutter_gap
            || self.highlight_pad_x != self.config.highlight_pad_x
            || self.highlight_pad_y != self.config.highlight_pad_y
            || self.highlight_offset_y != self.config.highlight_offset_y
            || self.tab_min_width != self.config.tab_min_width
            || self.tab_max_width != self.config.tab_max_width;
        if !line_height_changed && !pad_or_gutter_changed {
            return;
        }

        self.pad = self.config.padding;
        self.gutter_left = self.config.gutter_left;
        self.gutter_gap = self.config.gutter_gap;
        self.highlight_pad_x = self.config.highlight_pad_x;
        self.highlight_pad_y = self.config.highlight_pad_y;
        self.highlight_offset_y = self.config.highlight_offset_y;
        self.tab_min_width = self.config.tab_min_width;
        self.tab_max_width = self.config.tab_max_width;
        self.line_height = new_line_height;

        if line_height_changed {
            let metrics = Metrics::new(self.font_size, new_line_height);
            let mut tabs: Vec<&mut Tab> = Vec::new();
            self.root
                .as_mut()
                .expect("pane tree present")
                .all_tabs_mut(&mut tabs);
            for tab in tabs {
                tab.text_buffer.set_metrics(&mut self.font_system, metrics);
                tab.buffer_dirty = true;
            }
        }

        self.relayout();
        self.sync_active_grid();
        self.window.request_redraw();
    }

    /// Mirror the active tab's grid into `grid_cols` / `grid_rows`, which the
    /// mouse / autoscroll paths read.
    fn sync_active_grid(&mut self) {
        let t = self.active_tab_ref();
        let (cols, rows) = (t.cols, t.rows);
        self.grid_cols = cols;
        self.grid_rows = rows;
    }

    /// Split the active pane in two at `ratio`; the new pane (one fresh tab)
    /// is focused.
    pub fn split_active(&mut self, dir: SplitDir, ratio: f32) {
        let target = self.active_pane;
        let cwd = self.active_tab_ref().live_term.current_dir();
        let tab_id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        let new_pid = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        // Provisional size; `relayout` immediately corrects it.
        let live = LiveTerm::new(
            self.grid_cols.max(1),
            self.grid_rows.max(1),
            self.cell_advance,
            self.line_height,
            self.proxy.clone(),
            tab_id,
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
        let buf = make_content_buffer(
            &mut self.font_system,
            self.cell_advance,
            self.line_height,
            self.font_size,
            &self.font_family,
            100.0,
            100.0,
        );
        let new_tab = Tab::new(
            tab_id,
            title,
            title_buf,
            live,
            buf,
            self.grid_cols.max(1),
            self.grid_rows.max(1),
        );
        let root = self.root.take().expect("pane tree present");
        self.root = Some(root.into_split(target, dir, new_pid, Pane::single(new_tab), ratio));
        self.active_pane = new_pid;
        self.relayout();
        self.sync_active_grid();
        self.window.request_redraw();
    }

    /// Close the active pane. Returns true if it was the window's last pane
    /// (the window should then exit).
    pub fn close_active_pane(&mut self) -> bool {
        if self.root_ref().leaf_count() <= 1 {
            return true;
        }
        let target = self.active_pane;
        let root = self.root.take().expect("pane tree present");
        let new_root = root.into_closed(target).expect("more than one leaf remains");
        self.active_pane = new_root.first_leaf_id();
        self.root = Some(new_root);
        self.relayout();
        self.sync_active_grid();
        self.window.set_title(&self.active_tab_ref().title);
        self.window.request_redraw();
        false
    }

    /// Close the active pane, but if any of its tabs has a non-shell process
    /// running, open a confirmation modal first (the corner-drag remove
    /// path — it takes the whole pane and every tab in it).
    fn request_close_active_pane(&mut self) {
        if self.modal.is_some() || self.root_ref().leaf_count() <= 1 {
            return;
        }
        let busy = self
            .active_pane_ref()
            .tabs
            .iter()
            .any(|t| t.live_term.has_active_process());
        if !busy {
            let _ = self.close_active_pane();
            return;
        }
        let pane = self.active_pane_ref();
        let name = pane
            .tabs
            .iter()
            .find(|t| t.live_term.has_active_process())
            .and_then(|t| t.live_term.foreground_pid())
            .and_then(proc_name_of)
            .unwrap_or_else(|| "A process".to_string());
        let tab_count = pane.tabs.len();
        let body = if tab_count > 1 {
            format!("{name} is running in this pane ({tab_count} tabs).")
        } else {
            format!("{name} is running in this pane.")
        };
        self.open_modal(
            ModalAction::ClosePane,
            "Close pane?".to_string(),
            body,
            "Cancel",
            "Close",
        );
    }

    /// Make a pane the active one.
    fn focus_pane(&mut self, pid: PaneId) {
        if self.active_pane != pid {
            // Drop any selection the *prior* pane's active tab still
            // holds — keeping it across a pane switch reads as
            // "stale highlight in the pane I just left." Each pane
            // re-selects on its own click.
            let prior = self.active_pane;
            if let Some(p) = self.root.as_mut().and_then(|n| n.find_mut(prior)) {
                let tab = p.active_tab_mut();
                tab.selection = None;
                tab.dragging = false;
            }
            self.active_pane = pid;
            self.sync_active_grid();
            self.window.set_title(&self.active_tab_ref().title);
            // Hot-reload also fires on in-window pane focus — editing the
            // config in a side pane and clicking back into a shell pane is
            // the natural tuning loop, and the window focus event doesn't
            // fire for that. `apply_live_layout` early-returns when
            // nothing changed, so the per-click cost is one ~1.7 KB read.
            self.config = Config::load();
            self.apply_live_layout();
            self.window.request_redraw();
        }
    }

    /// Move keyboard focus to the neighbouring pane in a direction. `dx` /
    /// `dy` are -1 / 0 / +1; we probe just past the active pane's edge (in
    /// the divider gap's far side) and focus whatever pane lands there.
    pub fn focus_dir(&mut self, dx: f32, dy: f32) {
        let a = self.active_pane_rect();
        let past = DIVIDER_THICKNESS + 1.0;
        let probe_x = if dx > 0.0 {
            a.x + a.w + past
        } else if dx < 0.0 {
            a.x - past
        } else {
            a.x + a.w / 2.0
        };
        let probe_y = if dy > 0.0 {
            a.y + a.h + past
        } else if dy < 0.0 {
            a.y - past
        } else {
            a.y + a.h / 2.0
        };
        if let Some((pid, _)) = self.pane_at(probe_x, probe_y) {
            self.focus_pane(pid);
        }
    }

    /// Make the pane under a window-relative point the active one. Returns
    /// true if a pane was hit.
    fn focus_pane_at(&mut self, x: f32, y: f32) -> bool {
        if let Some((pid, _)) = self.pane_at(x, y) {
            self.focus_pane(pid);
            true
        } else {
            false
        }
    }

    /// Handle a left-click inside pane `pid`'s tab-bar strip: switch to the
    /// clicked tab, or close it if the × close-zone was hit.
    fn tab_bar_click(&mut self, pid: PaneId, prect: PaneRect) {
        let ksw = kind_selector_w(self.config.tab_font_size);
        // Kind-selector hit first — leftmost zone of the bar.
        if self.mouse_pos.0 < prect.x + ksw {
            self.open_kind_dropdown(pid, prect);
            return;
        }
        let (title_widths, active) = {
            let pane = self.root_ref().find(pid).expect("pane present");
            let widths: Vec<f32> = pane
                .tabs
                .iter()
                .map(|t| measure_title_width(&t.title_buffer))
                .collect();
            (widths, pane.active_tab)
        };
        let layout = pane_tab_layout(prect, &title_widths, active, self.tab_min_width, self.tab_max_width, ksw);
        let mut hit: Option<(usize, f32, f32)> = None;
        for (i, (tx, tw, _)) in layout.iter().enumerate() {
            if self.mouse_pos.0 >= *tx && self.mouse_pos.0 < *tx + *tw {
                hit = Some((i, *tx, *tw));
                break;
            }
        }
        let Some((i, tx, tw)) = hit else { return };
        if self.mouse_pos.0 >= tx + tw - TAB_CLOSE_WIDTH {
            // Don't let a stray × click close the window — the very last tab
            // of the very last pane stays put; Cmd+W is the deliberate path.
            let last = self.root_ref().leaf_count() == 1 && title_widths.len() == 1;
            if !last {
                self.active_pane_mut().active_tab = i;
                self.close_active_tab();
            }
        } else {
            self.switch_to_tab(i);
        }
        self.window.request_redraw();
    }

    // ── Mouse / keyboard input routing ────────────────────────────────────

    /// Convert a mouse pixel position into an absolute (Line, Column) using
    /// the current display_offset. Used for both selection start and extend.
    /// Look up the block whose row range contains a clicked selection-abs
    /// line. Returns the block's selection-coordinate range
    /// `((start_line, 0), (end_line, last_col))` — ready to drop into a
    /// `Selection`. Translates between the block store's session-absolute
    /// coordinates (history + cursor at fire time) and the selection
    /// model's `vl - display_offset` convention.
    fn block_at_selection_line(&self, sel_line: i32) -> Option<((i32, usize), (i32, usize))> {
        let tab = self.active_tab_ref();
        let (_, history) = tab.live_term.offset_and_history();
        let history = history as i32;
        let session_abs = sel_line + history;
        let last_col = tab.cols.saturating_sub(1);

        // Iterates each block's start + raw-end pair once.
        let bounds = |block: &crate::blocks::Block| -> Option<(i32, i32, i32)> {
            let start = block.prompt_line.or(block.output_start_line)?;
            let raw_end = block
                .output_end_line
                .or(block.command_end_line)
                .or(block.prompt_line)?;
            let end = if raw_end > start { raw_end - 1 } else { raw_end };
            Some((start, end, raw_end))
        };

        // Pass 1 — unambiguous match. The trimmed range stops one row
        // above `output_end_line`, which is where the NEXT block's
        // prompt sits. So clicking on a row that visually says "demo$
        // echo hi" finds the echo-hi block (its trimmed range starts
        // at its prompt_line) rather than the prior false-block (which
        // claims that row only via its trailing-prompt overlap).
        for block in tab.blocks.iter() {
            if let Some((start, end, _)) = bounds(block) {
                if start <= session_abs && session_abs <= end {
                    return Some(((start - history, 0), (end - history, last_col)));
                }
            }
        }

        // Pass 2 — fall back to the raw range. Picks up clicks on the
        // trailing-prompt row of a no-output block that has no
        // following block yet (the open final block before the next
        // prompt fires).
        for block in tab.blocks.iter() {
            if let Some((start, end, raw_end)) = bounds(block) {
                if start <= session_abs && session_abs <= raw_end {
                    return Some(((start - history, 0), (end - history, last_col)));
                }
            }
        }

        None
    }

    fn pixel_to_absolute(&self, x: f32, y: f32) -> (i32, usize) {
        let metrics = self.active_pane_metrics();
        let pad = self.pad;
        let line_height = metrics.line_height;
        let apr = self.active_pane_rect();
        let left = apr.x + pad.left;
        let top = apr.y + self.tab_bar_height + pad.top;
        let cx = (x - left).max(0.0);
        let col = ((cx / metrics.cell_advance) as usize)
            .min(self.grid_cols.saturating_sub(1));
        // Same pixel_offset correction as cell_at_1indexed, but with a signed
        // floor so a click just inside the top of viewport while the buffer
        // is shifted down resolves to row -1 (the extra row above the
        // viewport) when appropriate.
        let cy = (y - top - self.active_tab_ref().pixel_offset) / line_height;
        let vl = cy.floor() as i32;
        let vl = vl.max(-1).min(self.grid_rows as i32 - 1);
        let display_offset = self.active_tab_ref().live_term.offset_and_history().0 as i32;
        (vl - display_offset, col)
    }

    pub fn mouse_moved(&mut self, x: f32, y: f32, modifiers: ModifiersState) {
        self.mouse_pos = (x, y);
        let metrics = self.active_pane_metrics();
        let pad = self.pad;
        let line_height = metrics.line_height;

        // Context menu up — just track the hovered item.
        if self.context_menu.is_some() {
            let hit = self.context_menu_at(x, y);
            if let Some(menu) = self.context_menu.as_mut() {
                if menu.hovered != hit {
                    menu.hovered = hit;
                    self.window.request_redraw();
                }
            }
            return;
        }

        // Dragging a split divider — resize the split it belongs to.
        if let Some(drag) = self.divider_drag.as_ref() {
            let (outer, dir, path) = (drag.outer, drag.dir, drag.path.clone());
            let raw = match dir {
                SplitDir::Vertical => {
                    (x - DIVIDER_THICKNESS / 2.0 - outer.x)
                        / (outer.w - DIVIDER_THICKNESS).max(1.0)
                }
                SplitDir::Horizontal => {
                    (y - DIVIDER_THICKNESS / 2.0 - outer.y)
                        / (outer.h - DIVIDER_THICKNESS).max(1.0)
                }
            };
            let span = match dir {
                SplitDir::Vertical => outer.w,
                SplitDir::Horizontal => outer.h,
            };
            let ratio = clamp_ratio(raw, span);
            if let Some(r) = self.root_mut().split_ratio_at_mut(&path) {
                *r = ratio;
            }
            self.relayout();
            self.sync_active_grid();
            self.window.request_redraw();
            return;
        }

        // Dragging a corner split handle — refresh so the preview tracks.
        if self.split_gesture.is_some() {
            if self.cursor_icon != CursorIcon::Grabbing {
                self.cursor_icon = CursorIcon::Grabbing;
                self.window.set_cursor(CursorIcon::Grabbing);
            }
            self.window.request_redraw();
            return;
        }

        // Cursor feedback: grab over a corner handle, resize over a divider.
        let over_handle = self
            .pane_at(x, y)
            .map(|(_, r)| in_split_handle(r, x, y))
            .unwrap_or(false);
        let desired = if over_handle {
            CursorIcon::Grab
        } else {
            match self
                .root_ref()
                .divider_at(self.content_rect(), x, y)
                .map(|(_, _, d)| d)
            {
                Some(SplitDir::Vertical) => CursorIcon::ColResize,
                Some(SplitDir::Horizontal) => CursorIcon::RowResize,
                None => CursorIcon::Default,
            }
        };
        if desired != self.cursor_icon {
            self.cursor_icon = desired;
            self.window.set_cursor(desired);
        }

        // Mouse reporting takes precedence over selection / scroll.
        let mode = self.active_tab_mut().live_term.mode_flags();
        let reporting_active = mode.mouse_drag || mode.mouse_motion;
        if reporting_active {
            // Drag (1002): only when a button is held. Motion (1003): always.
            let button_held = self.active_tab_mut().dragging || mode.mouse_motion;
            if mode.mouse_motion || (mode.mouse_drag && button_held) {
                if let Some((col, row)) = self.cell_at_1indexed(x, y) {
                    let bytes = encode_mouse_report(
                        &mode,
                        MouseEvent::Motion,
                        modifiers,
                        col,
                        row,
                    );
                    if let Some(b) = bytes {
                        self.active_tab_mut().live_term.write(b);
                    }
                }
            }
            return;
        }

        if self.active_tab_mut().dragging {
            // macOS trackpad scrolling drags the cursor a hair, so we get
            // tiny mouse_moved events interleaved with wheel events. Without
            // this filter, every wheel-driven extension to the viewport
            // edge gets immediately snapped back to whatever cell the
            // cursor is currently over. Only count motion that crosses
            // half a cell from the last update.
            let (last_x, last_y) = self.active_tab_mut().last_drag_mouse_pos;
            let dx = (x - last_x).abs();
            let dy = (y - last_y).abs();
            let big_motion = dx >= metrics.cell_advance * 0.5 || dy >= line_height * 0.5;
            if big_motion {
                let (line, col) = self.pixel_to_absolute(x, y);
                if let Some(sel) = self.active_tab_mut().selection.as_mut() {
                    sel.extend_to(line, col);
                }
                self.active_tab_mut().last_drag_mouse_pos = (x, y);
                self.window.request_redraw();
            }

            // Auto-scroll if the cursor is past the viewport's top or
            // bottom edge: keep scrolling while the user holds the button
            // there, extending the selection as new content reveals.
            let apr = self.active_pane_rect();
            let pane_top = apr.y + self.tab_bar_height + pad.top;
            let pane_bottom = apr.y + apr.h;
            let new_dir = if y < pane_top + AUTOSCROLL_MARGIN_PX {
                Some(1)
            } else if y > pane_bottom - AUTOSCROLL_MARGIN_PX {
                Some(-1)
            } else {
                None
            };
            let was_off = self.active_tab_mut().autoscroll_dir.is_none();
            self.active_tab_mut().autoscroll_dir = new_dir;
            match new_dir {
                Some(_) if was_off => {
                    self.next_autoscroll_deadline =
                        Some(Instant::now() + Duration::from_millis(AUTOSCROLL_TICK_MS));
                    self.window.request_redraw();
                }
                None => self.next_autoscroll_deadline = None,
                _ => {}
            }
        }
    }

    pub fn mouse_down(&mut self, button: MouseButton, modifiers: ModifiersState) {
        // Modal eats input — clicks hit-test modal buttons; everything else
        // is swallowed until the user picks Cancel or Confirm.
        if self.modal.is_some() {
            if button == MouseButton::Left {
                if self.modal_click(self.mouse_pos.0, self.mouse_pos.1) {
                    let _ = self.proxy.send_event(UserEvent::Exit);
                }
            }
            return;
        }

        // Context menu up — any click resolves it (an item, or dismiss).
        if self.context_menu.is_some() {
            self.context_menu_click(self.mouse_pos.0, self.mouse_pos.1);
            return;
        }

        // A left-press on a split divider starts a resize drag.
        if button == MouseButton::Left {
            if let Some((path, outer, dir)) = self.root_ref().divider_at(
                self.content_rect(),
                self.mouse_pos.0,
                self.mouse_pos.1,
            ) {
                self.divider_drag = Some(DividerDrag { path, outer, dir });
                return;
            }
        }

        // A left-press on a pane's top-right corner handle starts a split
        // gesture (drag down to stack, drag left for side by side).
        if button == MouseButton::Left {
            if let Some((pid, prect)) = self.pane_at(self.mouse_pos.0, self.mouse_pos.1) {
                if in_split_handle(prect, self.mouse_pos.0, self.mouse_pos.1) {
                    self.split_gesture = Some(SplitGesture {
                        pid,
                        start: self.mouse_pos,
                    });
                    self.window.request_redraw();
                    return;
                }
            }
        }

        // Tab-bar hit test first — a click in a pane's own tab bar strip
        // switches / closes that pane's tabs and never starts a selection.
        if let Some((pid, prect)) = self.pane_at(self.mouse_pos.0, self.mouse_pos.1) {
            if self.mouse_pos.1 < prect.y + self.tab_bar_height {
                self.focus_pane(pid);
                if button == MouseButton::Left {
                    self.tab_bar_click(pid, prect);
                }
                return;
            }
        }

        // Otherwise the click lands in a pane's content — focus that pane
        // before anything routes to "the active pane".
        self.focus_pane_at(self.mouse_pos.0, self.mouse_pos.1);

        // Data-module pane click → translate pixel → (source line,
        // visual column) in the body and forward to the module so
        // it can move its cursor (Editor) or pick the row (Nav).
        if button == MouseButton::Left {
            if let Some((pid, prect)) = self.pane_at(self.mouse_pos.0, self.mouse_pos.1) {
                if self.dispatch_data_module_click(pid, prect) {
                    return;
                }
            }
        }

        let mode = self.active_tab_mut().live_term.mode_flags();
        if mode.mouse_report_click || mode.mouse_drag || mode.mouse_motion {
            if let Some((col, row)) = self.cell_at_1indexed(self.mouse_pos.0, self.mouse_pos.1) {
                let bytes = encode_mouse_report(
                    &mode,
                    MouseEvent::Press(button),
                    modifiers,
                    col,
                    row,
                );
                if let Some(b) = bytes {
                    self.active_tab_mut().live_term.write(b);
                }
            }
            return;
        }

        // Right-click opens the context menu.
        if button == MouseButton::Right {
            self.open_context_menu(self.mouse_pos.0, self.mouse_pos.1);
            return;
        }

        // Only the left button does anything further.
        if button != MouseButton::Left {
            return;
        }

        let (line, col) = self.pixel_to_absolute(self.mouse_pos.0, self.mouse_pos.1);

        // Cmd-click an OSC 8 hyperlink → open it; don't start a selection.
        if modifiers.super_key() {
            if let Some(uri) = self.active_tab_mut().live_term.hyperlink_at(line, col) {
                open_uri(&uri);
                return;
            }
            // Cmd-click inside a block → select the whole block (prompt +
            // output) and copy it. The command + output reads as a unit on
            // the clipboard — pair-friendly "share what just happened."
            if let Some((start, end)) = self.block_at_selection_line(line) {
                let tab = self.active_tab_mut();
                tab.selection = Some(Selection {
                    anchor_line: start.0,
                    anchor_col: start.1,
                    head_line: end.0,
                    head_col: end.1,
                });
                tab.dragging = false;
                self.copy_selection();
                self.window.request_redraw();
                return;
            }
        }
        let now = Instant::now();
        let click_count = match self.active_tab_mut().last_click {
            Some((t, cell, c)) if now.duration_since(t) < MULTI_CLICK_WINDOW && cell == (line, col) => {
                (c + 1).min(3)
            }
            _ => 1,
        };
        self.active_tab_mut().last_click = Some((now, (line, col), click_count));

        match click_count {
            1 => {
                self.active_tab_mut().selection = Some(Selection::from_anchor(line, col));
                self.active_tab_mut().dragging = true;
                self.active_tab_mut().last_drag_mouse_pos = self.mouse_pos;
            }
            2 => {
                let ((sl, sc), (el, ec)) = self.active_tab_mut().live_term.word_at(line, col);
                self.active_tab_mut().selection = Some(Selection {
                    anchor_line: sl,
                    anchor_col: sc,
                    head_line: el,
                    head_col: ec,
                });
                self.active_tab_mut().dragging = false;
                self.copy_selection();
            }
            _ => {
                let ((sl, sc), (el, ec)) = self.active_tab_mut().live_term.line_at(line);
                self.active_tab_mut().selection = Some(Selection {
                    anchor_line: sl,
                    anchor_col: sc,
                    head_line: el,
                    head_col: ec,
                });
                self.active_tab_mut().dragging = false;
                self.copy_selection();
            }
        }
        self.window.request_redraw();
    }

    pub fn mouse_up(&mut self, button: MouseButton, modifiers: ModifiersState) {
        // Finish a corner gesture: drag in splits the pane at the cursor,
        // drag back out removes it; a short drag cancels.
        if let Some(g) = self.split_gesture.take() {
            let dx = self.mouse_pos.0 - g.start.0;
            let dy = self.mouse_pos.1 - g.start.1;
            match gesture_outcome(dx, dy) {
                Some(GestureOutcome::Split(dir)) => {
                    let rect = self
                        .pane_layout()
                        .into_iter()
                        .find(|(id, _)| *id == g.pid)
                        .map(|(_, r)| r);
                    if let Some(r) = rect {
                        let ratio = split_ratio_from_cursor(
                            r,
                            dir,
                            self.mouse_pos.0,
                            self.mouse_pos.1,
                        );
                        self.focus_pane(g.pid);
                        self.split_active(dir, ratio);
                    }
                }
                Some(GestureOutcome::Remove) => {
                    self.focus_pane(g.pid);
                    self.request_close_active_pane();
                }
                None => {}
            }
            self.cursor_icon = CursorIcon::Default;
            self.window.set_cursor(CursorIcon::Default);
            self.window.request_redraw();
            return;
        }

        // End a divider drag, if one is in progress.
        if self.divider_drag.is_some() {
            self.divider_drag = None;
            return;
        }

        let mode = self.active_tab_mut().live_term.mode_flags();
        if mode.mouse_report_click || mode.mouse_drag || mode.mouse_motion {
            if let Some((col, row)) = self.cell_at_1indexed(self.mouse_pos.0, self.mouse_pos.1) {
                let bytes = encode_mouse_report(
                    &mode,
                    MouseEvent::Release(button),
                    modifiers,
                    col,
                    row,
                );
                if let Some(b) = bytes {
                    self.active_tab_mut().live_term.write(b);
                }
            }
            return;
        }

        if button != MouseButton::Left {
            return;
        }

        self.active_tab_mut().dragging = false;
        self.active_tab_mut().autoscroll_dir = None;
        self.next_autoscroll_deadline = None;
        if let Some(sel) = self.active_tab_mut().selection.as_ref() {
            if sel.is_empty() {
                self.active_tab_mut().selection = None;
            } else {
                self.copy_selection();
            }
        }
        self.window.request_redraw();
    }

    pub fn mouse_wheel(&mut self, delta: MouseScrollDelta, modifiers: ModifiersState) {
        // The wheel acts on the pane *under the cursor* — you can scroll a
        // pane's history without stealing keyboard focus from another.
        let pid = match self.pane_at(self.mouse_pos.0, self.mouse_pos.1) {
            Some((pid, _)) => pid,
            None => return,
        };
        let line_height = self.pane_metrics(pid).line_height;

        // Data-module panes have no PTY — wheel events scroll the
        // rendered body instead of being forwarded as arrow keys to
        // a shell that isn't there. TTY modules have their own PTY
        // and fall through to the regular path below.
        {
            let tab = self.pane_tab_mut(pid);
            let is_data_module = matches!(tab.kind, TabContentKind::Module(_))
                && tab.module_pty.is_none();
            if is_data_module {
                let pixels = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 3.0 * line_height,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                // Up wheel (positive y) reveals earlier content =
                // scroll_y goes negative (text moves down on screen).
                // The render path clamps against laid-out content
                // height, so over-scroll past the ends is a no-op.
                tab.module_scroll_y -= pixels;
                if tab.module_scroll_y < 0.0 {
                    tab.module_scroll_y = 0.0;
                }
                self.window.request_redraw();
                return;
            }
        }

        let mode = self.pane_tab_mut(pid).active_term().mode_flags();

        // Alt-screen TUIs (nano, vim, less, htop, man) replace the main
        // screen with their own — alacritty's scrollback is empty there,
        // so the normal scroll path is a no-op and the pane feels dead
        // to the wheel. When the app isn't asking for mouse reports
        // either, translate wheel events into Up/Down arrow key bytes
        // so its own scroll machinery responds. Matches what iTerm2 /
        // Alacritty / kitty do. The wheel acts on the pane under the
        // cursor; we route to that pane's PTY regardless of focus.
        let mouse_mode_active = mode.mouse_report_click || mode.mouse_drag || mode.mouse_motion;
        if mode.alt_screen && !mouse_mode_active {
            let lines = match delta {
                MouseScrollDelta::LineDelta(_, y) => y * 3.0,
                MouseScrollDelta::PixelDelta(p) => p.y as f32 / line_height,
            };
            // Cap at a sane per-event maximum so a runaway delta can't
            // flood the PTY with hundreds of arrow keys.
            let count = (lines.abs().round() as usize).min(100);
            if count == 0 {
                return;
            }
            // Application cursor mode (DECCKM): `ESC O A/B`. Default
            // mode: `ESC [ A/B`. Wheel-up = scroll content down in the
            // viewport = Up arrow.
            let seq: &[u8] = match (lines > 0.0, mode.app_cursor) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            let mut bytes = Vec::with_capacity(seq.len() * count);
            for _ in 0..count {
                bytes.extend_from_slice(seq);
            }
            self.pane_tab_mut(pid).active_term().write(bytes);
            self.window.request_redraw();
            return;
        }

        // If the foreground app wants scroll reports (vim, less, htop in
        // mouse mode), forward instead of scrolling the viewport. Reporting
        // only routes when the hovered pane is also the focused one — the
        // cell math resolves against the active pane's rect.
        if mouse_mode_active {
            if pid != self.active_pane {
                return;
            }
            let pixels = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => p.y as f32 / line_height,
            };
            let direction = if pixels > 0.0 {
                MouseEvent::WheelUp
            } else if pixels < 0.0 {
                MouseEvent::WheelDown
            } else {
                return;
            };
            if let Some((col, row)) = self.cell_at_1indexed(self.mouse_pos.0, self.mouse_pos.1) {
                if let Some(b) = encode_mouse_report(&mode, direction, modifiers, col, row) {
                    self.pane_tab_mut(pid).active_term().write(b);
                }
            }
            return;
        }

        // Work in physical pixels so the renderer can shift by the remainder
        // for pixel-smooth scrolling. LineDelta is real-wheel "clicks" (~3
        // lines each, scaled to pixels); PixelDelta is trackpad pixels.
        let pixels = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 3.0 * line_height,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };

        // Boundary block: once we're at the top of scrollback or the live
        // bottom, more events in that direction can't move the term. The
        // "actual != whole" zero-out logic below catches it on the
        // line-pop step, but on a fast trackpad burst (events at ~120 Hz)
        // each event accumulates a visible sub-line `pixel_offset` before
        // getting zeroed — the user sees the text shaking at the
        // boundary. Drop wheel events whose direction is blocked AND
        // whose existing residual is in the same direction (so a tiny
        // reversal still goes through to undo the smooth shift).
        let (cur_offset, history) = self.pane_tab_mut(pid).active_term().offset_and_history();
        let residual = self.pane_tab_mut(pid).pixel_offset;
        let blocked_up = pixels > 0.0 && cur_offset >= history && residual >= 0.0;
        let blocked_down = pixels < 0.0 && cur_offset == 0 && residual <= 0.0;
        if blocked_up || blocked_down {
            return;
        }

        self.pane_tab_mut(pid).pixel_offset += pixels;

        // Pop whole lines into the term; the remainder stays as a sub-line
        // pixel shift used at render time. `floor` keeps the remainder in
        // [0, line_height) for any input direction — but only when the
        // requested scroll actually happens. If alacritty clamps (we asked
        // Delta(-2) but were at offset=1), subtracting the full `whole`
        // leaves a residual that renders as motion in the wrong direction,
        // and floor's over-pop re-establishes the residual on every event
        // — so the bottom (offset=0) is never reached cleanly. Subtract by
        // the *actual* offset delta instead.
        let whole = (self.pane_tab_mut(pid).pixel_offset / line_height).floor() as i32;
        if whole != 0 {
            let (before, _) = self.pane_tab_mut(pid).active_term().offset_and_history();
            self.pane_tab_mut(pid).active_term_mut().scroll(TermScroll::Delta(whole));
            let (after, history) = self.pane_tab_mut(pid).active_term().offset_and_history();
            let actual = after as i32 - before as i32;
            self.pane_tab_mut(pid).pixel_offset -= actual as f32 * line_height;
            if actual != whole {
                // Clamped at a scrollback boundary; drop the residual.
                self.pane_tab_mut(pid).pixel_offset = 0.0;
            }
            let _ = history;

            // While dragging, extending the head to wherever the mouse pixel
            // sits would actually *shrink* the selection as scroll reveals
            // new content (the same pixel now points at an older row going
            // up, newer going down). Instead push the head to the viewport
            // edge in the scroll direction, so the selection grows to cover
            // the freshly-revealed lines. Pick whichever extends *further*
            // from the anchor — mouse position still wins when it's already
            // farther.
            if actual != 0 && pid == self.active_pane && self.active_tab_mut().dragging {
                let (mouse_line, mouse_col) =
                    self.pixel_to_absolute(self.mouse_pos.0, self.mouse_pos.1);
                let edge = if actual > 0 {
                    // Scrolled UP — viewport top is the oldest edge.
                    (-(after as i32), 0_usize)
                } else {
                    // Scrolled DOWN — viewport bottom is the newest edge.
                    (
                        self.grid_rows as i32 - 1 - after as i32,
                        self.grid_cols.saturating_sub(1),
                    )
                };
                if let Some(sel) = self.active_tab_mut().selection.as_mut() {
                    let edge_d = (edge.0 - sel.anchor_line).abs();
                    let mouse_d = (mouse_line - sel.anchor_line).abs();
                    let (head_line, head_col) = if edge_d > mouse_d {
                        edge
                    } else {
                        (mouse_line, mouse_col)
                    };
                    sel.extend_to(head_line, head_col);
                }
            }
        }

        self.window.request_redraw();
    }

    pub fn scroll_page(&self, up: bool) {
        let s = if up { TermScroll::PageUp } else { TermScroll::PageDown };
        self.active_tab_ref().live_term.scroll(s);
        self.window.request_redraw();
    }

    pub fn copy_selection(&mut self) {
        let Some(sel) = self.active_tab_mut().selection.as_ref() else { return };
        if sel.is_empty() {
            return;
        }
        let (start, end) = sel.normalized();
        let text = self.active_tab_mut().live_term.extract_text(start, end);
        if text.is_empty() {
            return;
        }
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(text);
        }
    }

    pub fn paste(&mut self) {
        let text = match self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
            Some(t) => t,
            None => return,
        };
        if text.is_empty() {
            return;
        }
        if self.active_tab_mut().live_term.mode_flags().bracketed_paste {
            // Wrap so the shell treats the whole paste as one input, not as
            // typed-and-pressed-enter for each newline. Strips any embedded
            // \e[201~ to keep the framing safe.
            let safe = text.replace("\x1b[201~", "");
            let mut bytes = Vec::with_capacity(safe.len() + 12);
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(safe.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            self.active_tab_mut().live_term.write(bytes);
        } else {
            self.active_tab_mut().live_term.write(text.into_bytes());
        }
    }

    pub fn ring_bell(&mut self, _tab_id: TabId) {
        // `bell_style = "none"` — the BEL does nothing.
        if self.config.bell_style == BellStyle::Silent {
            return;
        }
        // The flash overlay is window-wide for now; we don't visually
        // distinguish *which* tab rang the bell. Coalesce: a hostile `\a`
        // storm just extends the deadline; we don't touch the renderer state
        // otherwise and we don't re-request a redraw if the flash is already
        // on screen. The expiry render is scheduled via the main loop's
        // `WaitUntil(next_wakeup())`.
        let now = Instant::now();
        let was_active = self.bell_flash_until.is_some_and(|t| t > now);
        self.bell_flash_until = Some(now + BELL_DURATION);
        if !was_active {
            self.window.request_redraw();
        }
    }

    /// Apply a tab title from a shell `OSC 0/1/2`. This wins over the
    /// auto-title for as long as the shell keeps setting one. An empty or
    /// whitespace-only title is treated as "unset" — the auto-title takes
    /// over again on the next render. This is what TUIs that emit an empty
    /// title or a ResetTitle escape on exit (claude, vim, ssh) expect.
    /// Parse + decode + display a Kitty APC payload from `tab_id`. v1
    /// recognises only `a=T` (transmit-and-display); the image replaces
    /// any prior one on that tab and renders at the pane's top-left,
    /// scaled to fit. Bounded throughout — the parser caps per-image
    /// decoded bytes, the texture holds bytes equal to the cap at worst,
    /// and the prior image's GPU memory is freed when overwritten.
    pub fn handle_apc(&mut self, tab_id: TabId, data: &[u8]) {
        let Some(cmd) = images::parse_kitty(data) else { return };
        // Only the transmit-and-display action shows in v1; transmit-only,
        // display-by-id, delete and query are no-ops until later commits.
        if !matches!(cmd.action, Action::TransmitDisplay) {
            return;
        }
        let Some(image) = images::decode_image(cmd.format, cmd.width, cmd.height, &cmd.payload)
        else { return };
        let tex = self.texture_renderer.upload(&self.device, &self.queue, &image);

        let mut tabs: Vec<&mut Tab> = Vec::new();
        self.root.as_mut().expect("pane tree present").all_tabs_mut(&mut tabs);
        if let Some(tab) = tabs.into_iter().find(|t| t.id == tab_id) {
            tab.image = Some(tex);
        }
        self.window.request_redraw();
    }

    /// Apply one OSC 133 shell-integration mark to a tab's block store.
    /// The block Model is Phase 2's spine — `Bn` labels render in the
    /// pane gutter from here. Bounded: per-tab block cap at
    /// `MAX_BLOCKS_PER_TAB`; label buffers are tiny pre-shaped strings.
    pub fn handle_shell_integration(
        &mut self,
        tab_id: TabId,
        kind: char,
        exit: Option<i32>,
        line: i32,
    ) {
        // Scale the new block's label to its owning pane's font scale
        // so the label sits flush with content rows at that pane's
        // size. Content-anchored chrome stays consistent with content.
        let scale = self.scale_for_tab(tab_id);
        let label_font_size = crate::blocks::LABEL_FONT_SIZE * scale;
        let label_line_h = (crate::blocks::LABEL_LINE_H * scale).max(1.0);
        let effect = {
            let mut tabs: Vec<&mut Tab> = Vec::new();
            self.root.as_mut().expect("pane tree present").all_tabs_mut(&mut tabs);
            tabs.into_iter().find(|t| t.id == tab_id).map(|tab| {
                tab.blocks.on_mark(
                    kind,
                    exit,
                    line,
                    &mut self.font_system,
                    label_font_size,
                    label_line_h,
                )
            })
        };
        // Fan out to the proto subscriber. `closed` fires before `opened`
        // — that's the order they happened on an A-after-no-D path.
        if let Some(effect) = effect {
            if let Some((block_id, exit_code)) = effect.closed {
                self.proto_emit_event(crate::proto::EventPayload::BlockClosed {
                    tab_id: tab_id.0,
                    block_id,
                    exit_code,
                });
            }
            if let Some(block_id) = effect.opened {
                self.proto_emit_event(crate::proto::EventPayload::BlockOpened {
                    tab_id: tab_id.0,
                    block_id,
                });
            }
        }
        self.window.request_redraw();
    }

    /// A new module connected — drop any prior subscriber (v1 = single
    /// client; the new one wins).
    pub fn handle_proto_connect(&mut self) {
        self.proto_subscriber = None;
    }

    /// The module disconnected — clear the subscriber slot.
    pub fn handle_proto_disconnect(&mut self) {
        self.proto_subscriber = None;
    }

    /// Handle one parsed request from the proto socket.
    pub fn handle_proto_request(
        &mut self,
        req: crate::proto::Request,
        out: std::sync::mpsc::SyncSender<crate::proto::OutMessage>,
    ) {
        let payload = match req.method.as_str() {
            "list_tabs" => self.proto_list_tabs(),
            "list_blocks" => self.proto_list_blocks(&req.params),
            "get_block" => self.proto_get_block(&req.params),
            "subscribe" => {
                self.proto_subscriber = Some(out.clone());
                crate::proto::OutPayload::Subscribed
            }
            "set_tag" => self.proto_set_tag(&req.params),
            "remove_tag" => self.proto_remove_tag(&req.params),
            "cursor_at" => self.proto_cursor_at(&req.params),
            "cursor_clear" => self.proto_cursor_clear(&req.params),
            "export_tab" => self.proto_export_tab(&req.params),
            "stats" => self.proto_stats(),
            "list_modules" => crate::proto::OutPayload::Modules {
                modules: self.modules.list().to_vec(),
            },
            "reload_modules" => {
                self.reload_modules();
                crate::proto::OutPayload::Modules {
                    modules: self.modules.list().to_vec(),
                }
            }
            other => crate::proto::OutPayload::Error {
                message: format!("unknown method: {other}"),
            },
        };
        let _ = out.try_send(crate::proto::OutMessage { id: req.id, payload });
    }

    fn proto_list_tabs(&self) -> crate::proto::OutPayload {
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let tabs = all
            .iter()
            .map(|t| crate::proto::TabInfo {
                tab_id: t.id.0,
                title: t.title.clone(),
            })
            .collect();
        crate::proto::OutPayload::Tabs { tabs }
    }

    fn proto_list_blocks(&self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        let tab_id = TabId(tab_id_u64);
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        let blocks = tab.blocks.iter().map(block_to_info).collect();
        let cursor = tab.blocks.cursor();
        crate::proto::OutPayload::Blocks { blocks, cursor }
    }

    fn proto_mutate_tab<F>(&mut self, params: &serde_json::Value, f: F) -> crate::proto::OutPayload
    where
        F: FnOnce(&mut Tab) -> crate::proto::OutPayload,
    {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        let tab_id = TabId(tab_id_u64);
        let mut all: Vec<&mut Tab> = Vec::new();
        self.root
            .as_mut()
            .expect("pane tree present")
            .all_tabs_mut(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        f(tab)
    }

    fn proto_set_tag(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(block_id) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let block_id = block_id as u32;
        let Some(tag) = params.get("tag").and_then(|v| v.as_str()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tag".into(),
            };
        };
        let tag = tag.to_string();
        let payload = self.proto_mutate_tab(params, |tab| {
            if tab.blocks.add_tag(block_id, &tag) {
                crate::proto::OutPayload::Ok
            } else {
                crate::proto::OutPayload::Error {
                    message: format!("could not add tag {tag:?} to block {block_id}"),
                }
            }
        });
        self.window.request_redraw();
        payload
    }

    fn proto_remove_tag(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(block_id) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let block_id = block_id as u32;
        let Some(tag) = params.get("tag").and_then(|v| v.as_str()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tag".into(),
            };
        };
        let tag = tag.to_string();
        let payload = self.proto_mutate_tab(params, |tab| {
            if tab.blocks.remove_tag(block_id, &tag) {
                crate::proto::OutPayload::Ok
            } else {
                crate::proto::OutPayload::Error {
                    message: format!("tag {tag:?} not present on block {block_id}"),
                }
            }
        });
        self.window.request_redraw();
        payload
    }

    fn proto_cursor_at(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(block_id) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let block_id = block_id as u32;
        let payload = self.proto_mutate_tab(params, |tab| {
            if tab.blocks.set_cursor(block_id) {
                crate::proto::OutPayload::Ok
            } else {
                crate::proto::OutPayload::Error {
                    message: format!("no block with id {block_id}"),
                }
            }
        });
        self.window.request_redraw();
        payload
    }

    fn proto_cursor_clear(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let payload = self.proto_mutate_tab(params, |tab| {
            tab.blocks.clear_cursor();
            crate::proto::OutPayload::Ok
        });
        self.window.request_redraw();
        payload
    }

    fn proto_get_block(&self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        let Some(block_id_u64) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let tab_id = TabId(tab_id_u64);
        let block_id = block_id_u64 as u32;
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        let Some(block) = tab.blocks.find(block_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no block with id {block_id} in tab {tab_id_u64}"),
            };
        };
        let command = block_command_text(tab, block).unwrap_or_default();
        let output = block_output_text(tab, block).unwrap_or_default();
        crate::proto::OutPayload::Block {
            block: crate::proto::BlockData {
                info: block_to_info(block),
                command,
                output,
            },
        }
    }

    fn proto_export_tab(&self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        // Optional `since` — include only blocks with id >= since. Lets
        // the partner stream a session in chunks instead of always
        // exporting from the beginning.
        let since: Option<u32> = params
            .get("since")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let tab_id = TabId(tab_id_u64);
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        let blocks: Vec<crate::proto::BlockData> = tab
            .blocks
            .iter()
            .filter(|b| since.is_none_or(|s| b.id >= s))
            .map(|b| crate::proto::BlockData {
                info: block_to_info(b),
                command: block_command_text(tab, b).unwrap_or_default(),
                output: block_output_text(tab, b).unwrap_or_default(),
            })
            .collect();
        crate::proto::OutPayload::Export {
            tab_id: tab_id_u64,
            blocks,
        }
    }

    fn proto_stats(&self) -> crate::proto::OutPayload {
        let mut all: Vec<&Tab> = Vec::new();
        self.root
            .as_ref()
            .expect("pane tree present")
            .all_tabs(&mut all);
        let tabs: Vec<crate::proto::TabStats> = all
            .iter()
            .map(|t| crate::proto::TabStats {
                tab_id: t.id.0,
                title: t.title.clone(),
                cols: t.cols,
                rows: t.rows,
                blocks: t.blocks.iter().count(),
                open_block: t.blocks.open_id(),
                cursor_block: t.blocks.cursor(),
                has_image: t.image.is_some(),
            })
            .collect();

        // Frame stats — simple sort to find p99. Sample count caps at
        // `FRAME_TIMER_CAP`, so the sort is O(n log n) on a small n.
        let samples: Vec<f32> = self.frame_samples.iter().copied().collect();
        let (avg_ms, p99_ms, max_ms) = if samples.is_empty() {
            (0.0, 0.0, 0.0)
        } else {
            let sum: f32 = samples.iter().sum();
            let avg = sum / samples.len() as f32;
            let mut sorted = samples.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let p99_idx = ((sorted.len() as f32 * 0.99) as usize).min(sorted.len() - 1);
            let p99 = sorted[p99_idx];
            let max = sorted[sorted.len() - 1];
            (avg, p99, max)
        };

        crate::proto::OutPayload::Stats(crate::proto::StatsPayload {
            version: env!("CARGO_PKG_VERSION"),
            peak_rss_bytes: process_rss_peak_bytes(),
            frame: crate::proto::FrameStats {
                frames_observed: self.frame_count,
                recent_samples: samples.len(),
                avg_ms,
                p99_ms,
                max_ms,
            },
            tabs,
            subscriber_connected: self.proto_subscriber.is_some(),
        })
    }

    fn proto_emit_event(&mut self, event: crate::proto::EventPayload) {
        let Some(out) = self.proto_subscriber.as_ref() else { return };
        let msg = crate::proto::OutMessage {
            id: 0,
            payload: crate::proto::OutPayload::Event(event),
        };
        if out.try_send(msg).is_err() {
            // Disconnected or queue overflowed — drop the subscriber
            // rather than let it stall the main thread.
            self.proto_subscriber = None;
        }
    }

    pub fn set_tab_title(&mut self, tab_id: TabId, title: String) {
        if title.trim().is_empty() {
            let mut tabs: Vec<&mut Tab> = Vec::new();
            self.root.as_mut().expect("pane tree present").all_tabs_mut(&mut tabs);
            if let Some(tab) = tabs.into_iter().find(|t| t.id == tab_id) {
                tab.shell_title = None;
                // Force refresh_auto_titles to rebuild on the next render.
                tab.last_auto_title.clear();
            }
            self.window.request_redraw();
            return;
        }
        let new_buf = make_title_buffer(
            &mut self.font_system,
            &title,
            self.tab_font_size,
            self.tab_line_h,
            self.tab_max_width,
        );
        let active_id = self.active_tab_ref().id;
        {
            let mut tabs: Vec<&mut Tab> = Vec::new();
            self.root.as_mut().expect("pane tree present").all_tabs_mut(&mut tabs);
            if let Some(tab) = tabs.into_iter().find(|t| t.id == tab_id) {
                tab.shell_title = Some(title.clone());
                tab.title = title;
                tab.title_buffer = new_buf;
            }
        }
        if tab_id == active_id {
            self.window.set_title(&self.active_tab_ref().title);
        }
    }

    /// Refresh every tab's auto-title from the OS. Each call does a handful
    /// of `proc_*` syscalls per tab, so it's throttled well below the render
    /// rate — a title only changes on `cd` or a foreground-process switch,
    /// neither of which needs sub-second latency. Tabs that received an OSC
    /// title from their shell keep that.
    fn refresh_auto_titles(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_title_refresh) < Duration::from_millis(500) {
            return;
        }
        self.last_title_refresh = now;
        let active_id = self.active_tab_ref().id;
        let mut tabs: Vec<&mut Tab> = Vec::new();
        self.root.as_mut().expect("pane tree present").all_tabs_mut(&mut tabs);
        let mut new_window_title: Option<String> = None;
        for tab in tabs {
            if tab.shell_title.is_some() {
                continue;
            }
            let new_auto = tab.live_term.compute_auto_title();
            if new_auto != tab.last_auto_title {
                tab.title_buffer = make_title_buffer(
                    &mut self.font_system,
                    &new_auto,
                    self.tab_font_size,
                    self.tab_line_h,
                    self.tab_max_width,
                );
                tab.last_auto_title = new_auto.clone();
                tab.title = new_auto;
                if tab.id == active_id {
                    new_window_title = Some(tab.title.clone());
                }
            }
        }
        if let Some(t) = new_window_title {
            self.window.set_title(&t);
        }
    }

    /// Write bytes to the active tab's PTY (keyboard input path).
    pub fn write_active(&self, bytes: Vec<u8>) {
        let tab = self.active_tab_ref();
        match &tab.kind {
            TabContentKind::Module(_) => {
                // TTY module: feed the PTY raw, just like a shell.
                if let Some(pty) = tab.module_pty.as_ref() {
                    pty.write(bytes);
                    return;
                }
                // Data module: marshal to JSON via the session.
                if let Some(sess) = tab.module_session.as_ref() {
                    sess.send_input(&bytes);
                }
            }
            _ => tab.live_term.write(bytes),
        }
    }

    /// Switch a pane's active tab to a different content kind. Spawns
    /// or tears down the module process as needed; clears the cached
    /// content buffer so the next render rebuilds.
    fn set_tab_kind(&mut self, pane: PaneId, kind: TabContentKind) {
        // Resolve manifest before borrowing self.root mutably.
        let manifest = match &kind {
            TabContentKind::Module(id) => self.modules.find(id).cloned(),
            _ => None,
        };
        let proxy = self.proxy.clone();

        // Pane metrics + grid size — needed up front for a TTY module
        // because we have to spawn its LiveTerm at the right size.
        let pane_metrics = self.pane_metrics(pane);
        let pane_rect = self
            .pane_layout()
            .into_iter()
            .find(|(id, _)| *id == pane)
            .map(|(_, r)| r);
        let scrollback = self.config.scrollback;
        let pad = self.pad;
        let tab_bar_height = self.tab_bar_height;

        let Some(p) = self.root.as_mut().and_then(|n| n.find_mut(pane)) else {
            return;
        };
        let tab = p.active_tab_mut();
        let tab_id = tab.id;
        let prior_cwd = tab.live_term.current_dir();

        // Tearing down the prior sessions (if any) drops the Child /
        // PTY and joins the IO threads via Drop.
        tab.module_session = None;
        tab.module_pty = None;
        tab.last_module_body.clear();
        tab.kind = kind.clone();
        tab.content_buffer = None;
        // Bringing the shell back to view: it needs to reshape from
        // its real state, not the stale TTY-module frame.
        tab.buffer_dirty = true;
        tab.last_text_runs.clear();

        if let Some(manifest) = manifest {
            match manifest.kind {
                crate::modules::ModuleKind::Data => {
                    tab.module_session =
                        crate::modules::ModuleSession::spawn(&manifest, tab_id, proxy);
                }
                crate::modules::ModuleKind::Tty => {
                    // Compute the grid this LiveTerm should be born at —
                    // same shape as `pane_grid` so the module starts at
                    // a size the pane actually has room for.
                    let (cols, rows) = pane_rect
                        .map(|rect| {
                            pane_grid(
                                rect,
                                pane_metrics.cell_advance,
                                pane_metrics.line_height,
                                pad,
                                tab_bar_height,
                            )
                        })
                        .unwrap_or((80, 24));
                    let binary = manifest.binary.to_string_lossy().to_string();
                    let lt = LiveTerm::new_with_command(
                        cols,
                        rows,
                        pane_metrics.cell_advance,
                        pane_metrics.line_height,
                        proxy,
                        tab_id,
                        prior_cwd,
                        scrollback,
                        Some((binary, Vec::new())),
                    );
                    tab.module_pty = Some(lt);
                }
            }
        }
        self.window.request_redraw();
    }

    /// Re-discover modules from disk and refresh chrome labels.
    /// Active sessions keep running — if the user removed a module
    /// whose pane is currently shown, the session lives until the
    /// user switches kind. New modules become selectable from the
    /// dropdown immediately.
    fn reload_modules(&mut self) {
        self.modules = crate::modules::Registry::discover();
        // Rebuild the per-kind label buffers. Built-ins stay; module
        // entries reflect the fresh registry.
        let mut next: std::collections::HashMap<String, Buffer> =
            std::collections::HashMap::new();
        let ksw = kind_selector_w(self.config.tab_font_size);
        let label = |fs: &mut FontSystem, name: &str| {
            make_title_buffer(
                fs,
                &format!("{name} ▾"),
                self.tab_font_size,
                self.tab_line_h,
                ksw,
            )
        };
        next.insert("shell".into(), label(&mut self.font_system, "Shell"));
        next.insert("welcome".into(), label(&mut self.font_system, "Welcome"));
        for m in self.modules.list() {
            next.insert(m.id.clone(), label(&mut self.font_system, &m.name));
        }
        self.kind_label_buffers = next;
        self.window.request_redraw();
    }

    /// Resolve a click in `pid`'s content area to a (source line,
    /// visual col) inside the data module's body and dispatch as a
    /// `click` event. Returns true if the click was routed to a
    /// data module — caller short-circuits selection / hyperlink /
    /// the rest of the click pipeline in that case.
    fn dispatch_data_module_click(
        &mut self,
        pid: PaneId,
        prect: PaneRect,
    ) -> bool {
        // Only data modules (no PTY behind them) handle clicks via
        // the wire; TTY modules + shells use the existing selection
        // / mouse-report paths.
        let is_data_module = {
            let pane = match self.root_ref().find(pid) {
                Some(p) => p,
                None => return false,
            };
            let tab = pane.active_tab_ref();
            matches!(tab.kind, TabContentKind::Module(_))
                && tab.module_pty.is_none()
                && tab.module_session.is_some()
        };
        if !is_data_module {
            return false;
        }
        let metrics = self.pane_metrics(pid);
        let pad = self.pad;
        let px = prect.x + pad.left;
        let py = prect.y + self.tab_bar_height + pad.top;
        let local_x = self.mouse_pos.0 - px;
        let local_y = self.mouse_pos.1 - py;
        if local_x < 0.0 || local_y < 0.0 {
            return false;
        }
        // Translate local_y → source line via layout_runs lookup so
        // wrapped lines map back to their source index correctly.
        let scroll_y = self
            .root_ref()
            .find(pid)
            .map(|p| p.active_tab_ref().module_scroll_y)
            .unwrap_or(0.0);
        let target_y = local_y + scroll_y;
        let line_height = metrics.line_height;
        let mut source_line: Option<u32> = None;
        if let Some(buf) = self
            .root_ref()
            .find(pid)
            .and_then(|p| p.active_tab_ref().content_buffer.as_ref())
        {
            let mut acc = 0.0_f32;
            for run in buf.layout_runs() {
                if target_y >= acc && target_y < acc + line_height {
                    source_line = Some(run.line_i as u32);
                    break;
                }
                acc += line_height;
            }
        }
        let Some(line) = source_line else {
            return false;
        };
        let col = (local_x / metrics.cell_advance).max(0.0).round() as u32;
        if let Some(sess) = self
            .root_ref()
            .find(pid)
            .and_then(|p| p.active_tab_ref().module_session.as_ref())
        {
            sess.send_click(line, col);
        }
        true
    }

    /// A shell pane reported a new cwd via OSC 7. Broadcast it to
    /// every live data-module session so paired views (Nav follows
    /// shell, …) can react. Same shape as the focus broadcast — one
    /// event, every other module decides what to do with it.
    pub fn handle_cwd_changed(&mut self, _tab_id: TabId, path: &std::path::Path) {
        crate::logging::info(&format!("shell cwd → {}", path.display()));
        let path_str = path.to_string_lossy().to_string();
        let mut tabs: Vec<&mut Tab> = Vec::new();
        self.root
            .as_mut()
            .expect("pane tree present")
            .all_tabs_mut(&mut tabs);
        for tab in tabs {
            if let Some(sess) = tab.module_session.as_ref() {
                sess.send_cwd(&path_str);
            }
        }
    }

    /// One message from a running module. Updates the tab's cached
    /// body (replace-only in v1) and asks for a redraw.
    pub fn handle_module_message(
        &mut self,
        tab_id: TabId,
        msg: crate::modules::ModuleMessage,
    ) {
        match msg {
            crate::modules::ModuleMessage::SetText {
                body,
                scroll_to_line,
                cursor,
                dim_left_cols,
                highlight_line,
            } => {
                // Bound the body — a runaway module that tries to
                // shape a 1 GB string would freeze the window.
                if body.len() > MAX_MODULE_BODY_BYTES {
                    crate::logging::warn(&format!(
                        "module tab {}: set_text body {} bytes > cap {} — dropped",
                        tab_id.0,
                        body.len(),
                        MAX_MODULE_BODY_BYTES
                    ));
                    return;
                }
                let mut tabs: Vec<&mut Tab> = Vec::new();
                self.root
                    .as_mut()
                    .expect("pane tree present")
                    .all_tabs_mut(&mut tabs);
                let Some(tab) =
                    tabs.into_iter().find(|t| t.id == tab_id)
                else {
                    return;
                };
                let body_changed = tab.last_module_body != body;
                let new_cursor = cursor.map(|c| (c.line, c.col));
                let cursor_changed = tab.module_cursor != new_cursor;
                if body_changed {
                    tab.last_module_body = body.clone();
                    if let Some(sess) = tab.module_session.as_mut() {
                        sess.body = body;
                    }
                    tab.content_buffer = None;
                    // set_text and set_image are exclusive — a fresh
                    // body drops whatever image (still or animated)
                    // was on screen.
                    tab.image = None;
                    tab.animation = None;
                    // If the module doesn't tell us where to scroll,
                    // assume "new content, take me to the top." If it
                    // does (nav, editor, …), respect its position
                    // through the pending_ensure_visible path below
                    // so a wheel-scroll isn't reset out from under
                    // the user every keystroke.
                    if scroll_to_line.is_none() {
                        tab.module_scroll_y = 0.0;
                    }
                }
                tab.module_cursor = new_cursor;
                let dim_changed = tab.module_dim_cols != dim_left_cols;
                tab.module_dim_cols = dim_left_cols;
                let highlight_changed = tab.module_highlight_line != highlight_line;
                tab.module_highlight_line = highlight_line;
                if let Some(line) = scroll_to_line {
                    tab.pending_ensure_visible = Some(line);
                }
                if body_changed
                    || cursor_changed
                    || dim_changed
                    || highlight_changed
                    || scroll_to_line.is_some()
                {
                    self.window.request_redraw();
                }
            }
            crate::modules::ModuleMessage::SetImage { path } => {
                // Read + decode in the host so the module wire stays
                // a small text protocol. Errors go back to the module
                // as log lines so the module's author can spot them
                // in the regular log without crashing the pane.
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        crate::logging::warn(&format!(
                            "module tab {}: set_image read failed for {path}: {e}",
                            tab_id.0
                        ));
                        return;
                    }
                };
                let Some(decoded) = crate::images::decode_any_animated(&bytes) else {
                    crate::logging::warn(&format!(
                        "module tab {}: set_image decode failed for {path} \
                         (supported: png/jpeg/gif/webp/bmp; oversize images rejected)",
                        tab_id.0
                    ));
                    return;
                };
                // Upload all frames *before* taking a mut borrow on
                // the pane tree — the texture path needs &device/&queue
                // off self while the tree lookup wants &mut self.root.
                let (still, animation) = match decoded {
                    crate::images::DecodedImage::Static(img) => {
                        let tex = self
                            .texture_renderer
                            .upload(&self.device, &self.queue, &img);
                        (Some(tex), None)
                    }
                    crate::images::DecodedImage::Animated { frames, total_ms } => {
                        let mut textures = Vec::with_capacity(frames.len());
                        let mut cumulative = Vec::with_capacity(frames.len());
                        let mut max_w = 0u32;
                        let mut max_h = 0u32;
                        let mut acc = 0u64;
                        for (img, delay) in frames {
                            max_w = max_w.max(img.width);
                            max_h = max_h.max(img.height);
                            let tex = self
                                .texture_renderer
                                .upload(&self.device, &self.queue, &img);
                            textures.push(tex);
                            acc = acc.saturating_add(delay);
                            cumulative.push(acc);
                        }
                        let anim = TabAnimation {
                            frames: textures,
                            width: max_w,
                            height: max_h,
                            cumulative,
                            total_ms,
                            started_at: Instant::now(),
                        };
                        (None, Some(anim))
                    }
                };
                let mut tabs: Vec<&mut Tab> = Vec::new();
                self.root
                    .as_mut()
                    .expect("pane tree present")
                    .all_tabs_mut(&mut tabs);
                if let Some(tab) = tabs.into_iter().find(|t| t.id == tab_id) {
                    tab.image = still;
                    tab.animation = animation;
                    tab.last_module_body.clear();
                    if let Some(sess) = tab.module_session.as_mut() {
                        sess.body.clear();
                    }
                    tab.content_buffer = None;
                    tab.module_scroll_y = 0.0;
                    self.window.request_redraw();
                }
            }
            crate::modules::ModuleMessage::Log { message } => {
                crate::logging::info(&format!("module tab {}: {message}", tab_id.0));
            }
            crate::modules::ModuleMessage::PublishFocus { path } => {
                // Cross-pane signaling — broadcast the new focus to
                // every *other* live data module session. Paired views
                // (nav → preview / editor) react via this single event.
                crate::logging::info(&format!(
                    "module tab {}: focus {path}",
                    tab_id.0
                ));
                let mut tabs: Vec<&mut Tab> = Vec::new();
                self.root
                    .as_mut()
                    .expect("pane tree present")
                    .all_tabs_mut(&mut tabs);
                for tab in tabs {
                    if tab.id == tab_id {
                        continue;
                    }
                    if let Some(sess) = tab.module_session.as_ref() {
                        sess.send_focus(&path);
                    }
                }
            }
        }
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

    // ── Frame ────────────────────────────────────────────────────────────

    pub fn render(&mut self) {
        check_rss_kill_switch(self.rss_kill_bytes);
        self.refresh_auto_titles();
        let frame_start = Instant::now();

        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        // Cursor blink — one phase shared by every pane. alacritty's
        // CursorStyle.blinking is false unless the shell sends `\e[1/3/5 q`;
        // respecting that strictly freezes the cursor in default zsh/bash,
        // so we blink whenever the window is focused — unless the user has
        // turned `cursor_blink` off in the config.
        let blink = self.focused && self.config.cursor_blink;
        let blink_on = if blink {
            let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
            elapsed_ms % CURSOR_BLINK_PERIOD_MS < CURSOR_BLINK_PERIOD_MS / 2
        } else {
            true
        };
        // Surface the next blink phase change as a deadline so the main loop's
        // WaitUntil wakes us — no per-frame thread spawn.
        self.next_blink_deadline = if blink {
            let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
            let half = CURSOR_BLINK_PERIOD_MS / 2;
            let into_half = elapsed_ms % half;
            Some(Instant::now() + Duration::from_millis((half - into_half).max(1)))
        } else {
            None
        };
        // render_pane re-arms this if a pane is autoscrolling.
        self.next_autoscroll_deadline = None;

        // Lay out the window's pane tree, then render each pane into its rect.
        let layout = self.pane_layout();
        let active_pane = self.active_pane;
        let mut below: Vec<RectInstance> = Vec::new();
        let mut above: Vec<RectInstance> = Vec::new();
        let mut tab_bar: Vec<RectInstance> = Vec::new();
        let mut draws: Vec<PaneDraw> = Vec::with_capacity(layout.len());
        for (pid, rect) in &layout {
            let d = self.render_pane(
                *pid,
                *rect,
                *pid == active_pane,
                blink_on,
                &mut below,
                &mut above,
                &mut tab_bar,
            );
            draws.push(d);
        }

        // Split divider seams drawn on top of pane content.
        collect_dividers(self.root_ref(), self.content_rect(), &mut above);

        // Live preview of a corner-handle gesture: a gold line at the
        // cursor-placed split, or a red wash over a pane about to be removed.
        if let Some(g) = self.split_gesture.as_ref() {
            let dx = self.mouse_pos.0 - g.start.0;
            let dy = self.mouse_pos.1 - g.start.1;
            if let Some((_, r)) = layout.iter().find(|(id, _)| *id == g.pid).copied() {
                match gesture_outcome(dx, dy) {
                    Some(GestureOutcome::Split(dir)) => {
                        let ratio = split_ratio_from_cursor(
                            r,
                            dir,
                            self.mouse_pos.0,
                            self.mouse_pos.1,
                        );
                        let preview = match dir {
                            SplitDir::Horizontal => [
                                r.x,
                                r.y + (r.h - DIVIDER_THICKNESS) * ratio,
                                r.w,
                                DIVIDER_THICKNESS,
                            ],
                            SplitDir::Vertical => [
                                r.x + (r.w - DIVIDER_THICKNESS) * ratio,
                                r.y,
                                DIVIDER_THICKNESS,
                                r.h,
                            ],
                        };
                        above.push(RectInstance {
                            rect: preview,
                            color: TAB_ACTIVE_UNDERLINE,
                        });
                    }
                    Some(GestureOutcome::Remove) => {
                        above.push(RectInstance {
                            rect: [r.x, r.y, r.w, r.h],
                            color: REMOVE_PREVIEW_COLOR,
                        });
                    }
                    None => {}
                }
            }
        }

        // Find bar background — a floating box at the active pane's
        // top-right. The query text is drawn by the tab text renderer.
        let find_bar_origin = if self.find.is_some() {
            let apr = self.active_pane_rect();
            let bx = apr.x + apr.w - FIND_BAR_W - FIND_BAR_MARGIN;
            let by = apr.y + self.tab_bar_height + FIND_BAR_MARGIN;
            above.push(RectInstance {
                rect: [bx - 1.0, by - 1.0, FIND_BAR_W + 2.0, FIND_BAR_H + 2.0],
                color: FIND_BAR_BORDER,
            });
            above.push(RectInstance {
                rect: [bx, by, FIND_BAR_W, FIND_BAR_H],
                color: FIND_BAR_BG,
            });
            Some((bx, by))
        } else {
            None
        };

        // Bell flash: a soft warm overlay over the whole surface. Auto-clears
        // when the deadline passes; a thread already scheduled a wakeup.
        if let Some(until) = self.bell_flash_until {
            if Instant::now() < until {
                above.push(RectInstance {
                    rect: [
                        0.0,
                        0.0,
                        self.surface_config.width as f32,
                        self.surface_config.height as f32,
                    ],
                    color: BELL_COLOR,
                });
            } else {
                self.bell_flash_until = None;
            }
        }

        let resolution = [
            self.surface_config.width as f32,
            self.surface_config.height as f32,
        ];
        // The modal and the context menu share the rects_modal /
        // modal_text_renderer pipelines — they're mutually exclusive in
        // practice and the modal wins if both are somehow set.
        let overlay_rects = if self.modal.is_some() {
            self.build_modal_rects()
        } else {
            self.build_menu_rects()
        };
        self.rects_below.prepare(&self.queue, &below, resolution);
        self.rects_above.prepare(&self.queue, &above, resolution);
        // `tab_bar` gets more entries in phase 2 (block-label highlights),
        // so its `prepare` is deferred to after that pass — uploading
        // here would freeze it before the highlights land.
        self.rects_modal
            .prepare(&self.queue, &overlay_rects, resolution);

        // Modal text preparation — independent renderer so its draw can come
        // after the modal background rects.
        if let Some(modal) = self.modal.as_ref() {
            let surface_w = self.surface_config.width as f32;
            let surface_h = self.surface_config.height as f32;
            let card_x = (surface_w - MODAL_CARD_W) * 0.5;
            let card_y = (surface_h - MODAL_CARD_H) * 0.5;
            let title_color = Color::rgb(235, 235, 245);
            let body_color = Color::rgb(180, 180, 195);
            let cancel_color = Color::rgb(200, 200, 215);
            let confirm_color = Color::rgb(245, 240, 240);
            let inset = 28.0;
            let title_top = card_y + inset;
            let body_top = title_top + MODAL_LINE_H + 8.0;
            let card_bounds = TextBounds {
                left: card_x as i32,
                top: card_y as i32,
                right: (card_x + MODAL_CARD_W) as i32,
                bottom: (card_y + MODAL_CARD_H) as i32,
            };
            let cr = modal.cancel_rect;
            let fr = modal.confirm_rect;
            let areas = [
                TextArea {
                    buffer: &modal.title_buf,
                    left: card_x + inset,
                    top: title_top,
                    scale: 1.0,
                    bounds: card_bounds,
                    default_color: title_color,
                    custom_glyphs: &[],
                },
                TextArea {
                    buffer: &modal.body_buf,
                    left: card_x + inset,
                    top: body_top,
                    scale: 1.0,
                    bounds: card_bounds,
                    default_color: body_color,
                    custom_glyphs: &[],
                },
                TextArea {
                    buffer: &modal.cancel_buf,
                    left: cr.0 + (cr.2 - MODAL_BTN_W * 0.55) * 0.5,
                    top: cr.1 + (cr.3 - MODAL_LINE_H) * 0.5,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: cr.0 as i32,
                        top: cr.1 as i32,
                        right: (cr.0 + cr.2) as i32,
                        bottom: (cr.1 + cr.3) as i32,
                    },
                    default_color: cancel_color,
                    custom_glyphs: &[],
                },
                TextArea {
                    buffer: &modal.confirm_buf,
                    left: fr.0 + (fr.2 - MODAL_BTN_W * 0.55) * 0.5,
                    top: fr.1 + (fr.3 - MODAL_LINE_H) * 0.5,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: fr.0 as i32,
                        top: fr.1 as i32,
                        right: (fr.0 + fr.2) as i32,
                        bottom: (fr.1 + fr.3) as i32,
                    },
                    default_color: confirm_color,
                    custom_glyphs: &[],
                },
            ];
            self.modal_text_renderer
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    areas,
                    &mut self.swash_cache,
                )
                .expect("terminite: modal text prepare failed");
        } else if let Some(menu) = self.context_menu.as_ref() {
            // Context-menu item labels go through the same text renderer.
            let label_color = Color::rgb(225, 225, 235);
            let disabled_color = Color::rgb(110, 110, 125);
            let text_inset = 18.0;
            let areas: Vec<TextArea> = menu
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let row_y = menu.y + i as f32 * MENU_ITEM_H;
                    TextArea {
                        buffer: &item.label_buf,
                        left: menu.x + text_inset,
                        top: row_y + (MENU_ITEM_H - MODAL_LINE_H) * 0.5,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: menu.x as i32,
                            top: row_y as i32,
                            right: (menu.x + MENU_WIDTH) as i32,
                            bottom: (row_y + MENU_ITEM_H) as i32,
                        },
                        default_color: if item.enabled {
                            label_color
                        } else {
                            disabled_color
                        },
                        custom_glyphs: &[],
                    }
                })
                .collect();
            self.modal_text_renderer
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    areas,
                    &mut self.swash_cache,
                )
                .expect("terminite: menu text prepare failed");
        }

        // Per-pane image placements: collected during phase 2 (root is
        // borrowed for the text areas anyway), prepared after the text
        // prep, drawn in the render pass between content and the tab bar.
        let mut texture_instances: Vec<TextureInstance> = Vec::new();
        let mut texture_bgs: Vec<wgpu::BindGroup> = Vec::new();

        // Content text + per-pane tab-bar labels. Phase 2: every pane's
        // buffers are already refreshed, so we can take the immutable
        // borrows the TextAreas need. Content goes through `text_renderer`,
        // tab labels + find bar through `tab_text_renderer`.
        {
            let root = self.root.as_ref().expect("pane tree present");
            let pad = self.pad;
            let line_height = self.line_height;
            let active_color = Color::rgb(230, 230, 240);
            let inactive_color = Color::rgb(140, 140, 160);
            let close_color = Color::rgb(160, 160, 170);
            // Subdued; a block label is chrome, not content.
            let block_label_color = Color::rgb(110, 110, 130);
            let mut content_areas: Vec<TextArea> = Vec::with_capacity(draws.len());
            let mut tab_areas: Vec<TextArea> = Vec::new();
            for d in &draws {
                let pane = root.find(d.pid).expect("drawn pane present");
                let pane_rect = layout
                    .iter()
                    .find(|(id, _)| *id == d.pid)
                    .map(|(_, r)| *r)
                    .expect("drawn pane present in layout");
                let tab_ref = pane.active_tab_ref();
                // Non-shell kinds render from `content_buffer`. If for
                // some reason it's missing (race between kind switch
                // and render), fall back to the empty text_buffer so
                // we don't crash.
                let body_buffer = match tab_ref.kind {
                    TabContentKind::Shell => &tab_ref.text_buffer,
                    _ => tab_ref
                        .content_buffer
                        .as_ref()
                        .unwrap_or(&tab_ref.text_buffer),
                };
                // Data modules scroll their body via `module_scroll_y`.
                // Bounds clip the over-flow so scrolled-out content
                // doesn't leak past the pane.
                let is_data_module = matches!(tab_ref.kind, TabContentKind::Module(_))
                    && tab_ref.module_pty.is_none();
                let scroll_y = if is_data_module { tab_ref.module_scroll_y } else { 0.0 };
                // When a data-module pane is showing an image (still or
                // animated), suppress the text body — otherwise the
                // placeholder body bleeds through behind the image.
                // Shells with kitty images keep both (text + overlaid
                // image) as before.
                let suppress_text = is_data_module
                    && (tab_ref.image.is_some() || tab_ref.animation.is_some());
                if !suppress_text {
                    // Render the body once with the default color
                    // and (if requested) overlay a second pass with
                    // a dim color clipped to the leftmost N cells —
                    // glyphon shapes the buffer once; the two
                    // TextAreas just write the same glyphs in
                    // different colors within different clip rects.
                    let metrics = self.pane_metrics(d.pid);
                    let dim_cols = tab_ref.module_dim_cols.unwrap_or(0);
                    let dim_w = dim_cols as f32 * metrics.cell_advance;
                    let normal_left_clip = if dim_cols > 0 {
                        (d.text_left + dim_w) as i32
                    } else {
                        d.bounds.left
                    };
                    let normal_bounds = TextBounds {
                        left: normal_left_clip.max(d.bounds.left),
                        ..d.bounds
                    };
                    content_areas.push(TextArea {
                        buffer: body_buffer,
                        left: d.text_left,
                        top: d.text_top - scroll_y,
                        scale: 1.0,
                        bounds: normal_bounds,
                        default_color: Color::rgb(DEFAULT_FG.0, DEFAULT_FG.1, DEFAULT_FG.2),
                        custom_glyphs: &[],
                    });
                    if dim_cols > 0 {
                        let dim_bounds = TextBounds {
                            left: d.bounds.left,
                            top: d.bounds.top,
                            right: ((d.text_left + dim_w) as i32).min(d.bounds.right),
                            bottom: d.bounds.bottom,
                        };
                        if dim_bounds.right > dim_bounds.left {
                            content_areas.push(TextArea {
                                buffer: body_buffer,
                                left: d.text_left,
                                top: d.text_top - scroll_y,
                                scale: 1.0,
                                bounds: dim_bounds,
                                default_color: Color::rgb(110, 110, 130),
                                custom_glyphs: &[],
                            });
                        }
                    }
                }
                // Kind selector label — leftmost in the bar. Looked up
                // by the kind's stable key. If a module was unregistered
                // since the tab last switched to it, the buffer is gone
                // and we just skip rendering the label (the dropdown
                // still works to pick a new kind).
                let active_kind = &pane.active_tab_ref().kind;
                if let Some(label_buf) = self.kind_label_buffers.get(active_kind.key()) {
                    let bar_top = pane_rect.y;
                    let text_top =
                        bar_top + (self.tab_bar_height - self.tab_line_h) / 2.0;
                    let ksw_label = kind_selector_w(self.config.tab_font_size);
                    tab_areas.push(TextArea {
                        buffer: label_buf,
                        left: pane_rect.x + TAB_LABEL_INSET,
                        top: text_top,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: pane_rect.x as i32,
                            top: bar_top as i32,
                            right: (pane_rect.x + ksw_label) as i32,
                            bottom: (bar_top + self.tab_bar_height) as i32,
                        },
                        default_color: active_color,
                        custom_glyphs: &[],
                    });
                }
                for slot in &d.tabs {
                    let tab = &pane.tabs[slot.index];
                    tab_areas.push(TextArea {
                        buffer: &tab.title_buffer,
                        left: slot.label_left,
                        top: slot.text_top,
                        scale: 1.0,
                        bounds: slot.label_bounds,
                        default_color: if slot.is_active {
                            active_color
                        } else {
                            inactive_color
                        },
                        custom_glyphs: &[],
                    });
                    tab_areas.push(TextArea {
                        buffer: &self.close_buffer,
                        left: slot.close_left,
                        top: slot.text_top,
                        scale: 1.0,
                        bounds: slot.close_bounds,
                        default_color: close_color,
                        custom_glyphs: &[],
                    });
                }
                // Pane's image. Scaled to fit the content area (never
                // upscaled). Data-module panes (Preview, etc.) center
                // the image — that's the natural "viewer" framing.
                // Shell panes keep the top-left placement that kitty
                // graphics emitters expect for inline display. Clone
                // is cheap — wgpu BindGroup is ref-counted internally.
                //
                // For animated images we pick the current frame here
                // and scale against the animation's envelope (max
                // width/height across frames) so the layout doesn't
                // wobble between frames of different sizes.
                let img_info: Option<(&TextureImage, u32, u32)> =
                    if let Some(anim) = tab_ref.animation.as_ref() {
                        Some((anim.current_frame(), anim.width, anim.height))
                    } else if let Some(img) = tab_ref.image.as_ref() {
                        Some((img, img.width, img.height))
                    } else {
                        None
                    };
                if let Some((tex, nw_u, nh_u)) = img_info {
                    let ox = pane_rect.x + pad.left;
                    let oy = pane_rect.y + self.tab_bar_height + pad.top;
                    let max_w = (pane_rect.x + pane_rect.w - ox - pad.right).max(1.0);
                    let max_h =
                        (pane_rect.y + pane_rect.h - oy - pad.bottom).max(1.0);
                    let nw = nw_u as f32;
                    let nh = nh_u as f32;
                    let scale = (max_w / nw).min(max_h / nh).min(1.0);
                    let sw = nw * scale;
                    let sh = nh * scale;
                    let (x, y) = if is_data_module {
                        (
                            ox + (max_w - sw) * 0.5,
                            oy + (max_h - sh) * 0.5,
                        )
                    } else {
                        (ox, oy)
                    };
                    texture_instances.push(TextureInstance {
                        rect: [x, y, sw, sh],
                    });
                    texture_bgs.push(tex.bind_group().clone());
                }
                // Block IDs (`Bn`) in the pane's left-gutter strip.
                // Coords are session-absolute (`abs = history + cursor.line`
                // at fire time); to find the current screen vl, unwind
                // both the rows that have since scrolled into history and
                // the user's current scroll position.
                // Per-pane scale affects the row stride used for block-
                // label vertical placement — labels track content rows.
                let pane_line_height = self.pane_metrics(d.pid).line_height;
                let y_shift = tab_ref.pixel_offset;
                let (display_offset, history) =
                    tab_ref.live_term.offset_and_history();
                let display_offset = display_offset as i32;
                let history = history as i32;
                let rows = tab_ref.rows as i32;
                let py = pane_rect.y + self.tab_bar_height + pad.top;
                let gutter_left = self.gutter_left;
                // Right-align each label against a fixed anchor just
                // inside the content edge. The label grows leftward as
                // the digit count climbs (B7 → B12 → B323 all end at the
                // same x), and `gutter_left` becomes the minimum-left
                // clip — when a label overruns it (very long ids in a
                // narrow gutter), the leading "B" gets clipped rather
                // than overlapping the line. `gutter_gap` is the space
                // between the label's right edge and the line content.
                let label_right = pane_rect.x + pad.left - self.gutter_gap;
                let label_left_min = pane_rect.x + gutter_left;
                // v_pad + label_line_h are now per-block (labels scale
                // with the pane that owned them at creation time).
                // Reads off the block in the loop below.
                // Visual signal lives in a background highlight behind
                // the label (like an HTML `<mark>`), not in the text
                // color. Text color alone reads as "another shade of
                // gray" — a filled block of color pops unambiguously.
                //   - cursored: bright warm fill, dark text for contrast.
                //   - tagged:   dim cool fill, label color unchanged.
                //   - default:  no fill, subdued label color.
                let cursor_bg: [f32; 4] = [1.0, 0.83, 0.30, 0.95];
                let tagged_bg: [f32; 4] = [0.45, 0.50, 0.65, 0.45];
                let cursor_text = Color::rgb(20, 20, 30);
                let tagged_text = Color::rgb(40, 40, 60);
                let highlight_pad_x = self.highlight_pad_x;
                let highlight_pad_y = self.highlight_pad_y;
                let highlight_offset_y = self.highlight_offset_y;
                let cursor_block_id = tab_ref.blocks.cursor();
                for block in tab_ref.blocks.iter() {
                    let Some(abs) = block.anchor_line() else { continue };
                    let vl = abs - history + display_offset;
                    if vl < 0 || vl >= rows {
                        continue;
                    }
                    let row_top = py + vl as f32 * pane_line_height + y_shift;
                    let label_line_h = block.label_line_h;
                    let v_pad =
                        ((pane_line_height - label_line_h) * 0.5).max(0.0);
                    let top = row_top + v_pad;
                    let left = label_right - block.label_width;
                    let is_cursor = Some(block.id) == cursor_block_id;
                    let bg = if is_cursor {
                        Some(cursor_bg)
                    } else if !block.tags.is_empty() {
                        Some(tagged_bg)
                    } else {
                        None
                    };
                    if let Some(color) = bg {
                        // Highlight clamped to the gutter strip so it
                        // never bleeds into line content. tab_bar rect
                        // layer renders before tab_text_renderer, so the
                        // fill sits behind the label text. The pads +
                        // offset come from config so the box can be
                        // dialed in live without a recompile.
                        let hx = (left - highlight_pad_x).max(pane_rect.x);
                        let hr = (label_right + highlight_pad_x)
                            .min(pane_rect.x + pad.left);
                        let hw = (hr - hx).max(0.0);
                        let hy = top - highlight_pad_y + highlight_offset_y;
                        let hh = label_line_h + highlight_pad_y * 2.0;
                        tab_bar.push(RectInstance {
                            rect: [hx, hy, hw, hh],
                            color,
                        });
                    }
                    let text_color = if is_cursor {
                        cursor_text
                    } else if !block.tags.is_empty() {
                        tagged_text
                    } else {
                        block_label_color
                    };
                    tab_areas.push(TextArea {
                        buffer: &block.label_buffer,
                        left,
                        top,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: label_left_min as i32,
                            top: row_top as i32,
                            right: label_right as i32,
                            bottom: (row_top + line_height) as i32,
                        },
                        default_color: text_color,
                        custom_glyphs: &[],
                    });
                }
            }
            // The find bar's text rides in the tab text renderer.
            if let (Some(find), Some((bx, by))) = (self.find.as_ref(), find_bar_origin) {
                tab_areas.push(TextArea {
                    buffer: &find.bar_buf,
                    left: bx + 16.0,
                    top: by + (FIND_BAR_H - MODAL_LINE_H) * 0.5,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: bx as i32,
                        top: by as i32,
                        right: (bx + FIND_BAR_W) as i32,
                        bottom: (by + FIND_BAR_H) as i32,
                    },
                    default_color: Color::rgb(225, 225, 235),
                    custom_glyphs: &[],
                });
            }
            self.text_renderer
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    content_areas,
                    &mut self.swash_cache,
                )
                .expect("terminite: text prepare failed");
            self.tab_text_renderer
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    tab_areas,
                    &mut self.swash_cache,
                )
                .expect("terminite: tab bar text prepare failed");
        }

        // Upload the tab-bar rects now that phase 2 has pushed any
        // block-label highlights into the same Vec — render order still
        // puts these behind `tab_text_renderer`, so the rect sits behind
        // the label glyphs.
        self.rects_tab_bar
            .prepare(&self.queue, &tab_bar, resolution);

        // Stage the image instance buffer; render happens between content
        // (text + decorations) and the tab bar, so images sit above the
        // cell grid but below per-pane chrome.
        self.texture_renderer
            .prepare(&self.queue, &texture_instances, resolution);

        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self
                    .instance
                    .create_surface(self.window.clone())
                    .expect("terminite: failed to recreate the surface");
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return;
            }
            other => {
                eprintln!("terminite: surface status: {other:?}");
                return;
            }
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("terminite frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(BACKGROUND),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // One full-window scissor — the panes tile the whole surface and
            // every rect / TextArea is already clipped to its own pane box,
            // so no per-pane scissor switching is needed.
            pass.set_scissor_rect(
                0,
                0,
                self.surface_config.width,
                self.surface_config.height,
            );

            self.rects_below.render(&mut pass);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("terminite: text render failed");
            self.rects_above.render(&mut pass);

            // Decoded images, atop the cell grid but below the tab bar.
            self.texture_renderer.render(&mut pass, &texture_bgs);

            // Per-pane tab bars drawn on top of the content.
            self.rects_tab_bar.render(&mut pass);
            self.tab_text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("terminite: tab bar text render failed");

            // Modal and context menu sit on top of *everything* — they
            // share the rects_modal / modal_text_renderer pipelines.
            if self.modal.is_some() || self.context_menu.is_some() {
                self.rects_modal.render(&mut pass);
                self.modal_text_renderer
                    .render(&self.atlas, &self.viewport, &mut pass)
                    .expect("terminite: overlay text render failed");
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        surface_texture.present();
        self.atlas.trim();

        // Frame-time bookkeeping for the stats verb. Sample is the
        // wall-clock interval from the start of this frame through
        // present; close enough to "what the user perceives as
        // frame cost" for debug purposes.
        let dt = frame_start.elapsed().as_secs_f32() * 1000.0;
        if self.frame_samples.len() == FRAME_TIMER_CAP {
            self.frame_samples.pop_front();
        }
        self.frame_samples.push_back(dt);
        self.last_frame_end = Some(Instant::now());
        self.frame_count = self.frame_count.saturating_add(1);
    }

    /// Render one pane into its rect: draw its own tab bar, then tick
    /// autoscroll, snapshot the active tab, refresh its text buffer, and emit
    /// clipped background / selection / cursor / decoration rects. Returns
    /// where to place the content + tab-label text (phase 2 in `render`).
    #[allow(clippy::too_many_arguments)]
    /// Render path for a pane whose active tab is *not* a shell. Builds
    /// (or rebuilds) the tab's `content_buffer` to fit the current pane
    /// rect, then returns a `PaneDraw` pointing into it. Welcome lives
    /// here in Bundle 6 step 1; future built-in kinds slot in alongside.
    fn render_non_shell_pane(
        &mut self,
        pid: PaneId,
        rect: PaneRect,
        tab_slots: Vec<TabLabelSlot>,
        kind: TabContentKind,
        blink_on: bool,
        below: &mut Vec<RectInstance>,
    ) -> PaneDraw {
        let metrics = self.pane_metrics(pid);
        let pad = self.pad;
        let px = rect.x + pad.left;
        let py = rect.y + self.tab_bar_height + pad.top;
        let content_w = (rect.w - pad.left - pad.right).max(1.0);
        let content_h = (rect.h - self.tab_bar_height - pad.top - pad.bottom)
            .max(metrics.line_height);

        // Build / refresh content_buffer if needed. For modules,
        // prefer the live session's body (what the module asked us
        // to render); fall back to the placeholder while waiting.
        let body = {
            let session_body = self
                .root
                .as_ref()
                .and_then(|n| n.find(pid))
                .and_then(|p| p.active_tab_ref().module_session.as_ref())
                .map(|s| s.body.clone())
                .filter(|s| !s.is_empty());
            session_body.unwrap_or_else(|| non_shell_body(&kind, &self.modules))
        };
        let font_size = metrics.font_size;
        let line_height = metrics.line_height;
        let family = self.font_family.clone();
        let tab = self
            .root
            .as_mut()
            .and_then(|n| n.find_mut(pid))
            .map(|p| p.active_tab_mut());
        if let Some(tab) = tab {
            let needs_build = tab.content_buffer.is_none();
            if needs_build {
                let mut buf = Buffer::new(
                    &mut self.font_system,
                    Metrics::new(font_size, line_height),
                );
                // Width-only constraint — glyphon wraps to pane
                // width but lays out every line. Height = None
                // means long bodies don't get truncated; the render
                // path applies `module_scroll_y` + clipping bounds
                // to show only what fits.
                buf.set_size(&mut self.font_system, Some(content_w), None);
                let attrs = Attrs::new().family(font_family(&family));
                buf.set_text(
                    &mut self.font_system,
                    &body,
                    &attrs,
                    Shaping::Advanced,
                    None,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
                tab.content_buffer = Some(buf);
            } else if let Some(buf) = tab.content_buffer.as_mut() {
                // Keep the buffer width matched to the current pane.
                // Height stays unbounded so all lines are laid out
                // for scrolling.
                buf.set_size(&mut self.font_system, Some(content_w), None);
            }
            // Clamp module_scroll_y so a pane shrink or a shorter
            // body can't leave us scrolled past the end. The render
            // pass subtracts this from text_top to scroll content.
            if let Some(buf) = tab.content_buffer.as_ref() {
                let total_h = buf.layout_runs().count() as f32 * line_height;
                let max_scroll = (total_h - content_h).max(0.0);
                // Honor a pending ensure_visible from set_text:
                // adjust scroll just enough to bring the target line
                // into the visible window. "Just enough" means we
                // leave the user's wheel position alone when the
                // target is already on screen.
                if let Some(line) = tab.pending_ensure_visible.take() {
                    let target_top = line as f32 * line_height;
                    let target_bottom = target_top + line_height;
                    if target_top < tab.module_scroll_y {
                        tab.module_scroll_y = target_top;
                    } else if target_bottom > tab.module_scroll_y + content_h {
                        tab.module_scroll_y = target_bottom - content_h;
                    }
                }
                if tab.module_scroll_y > max_scroll {
                    tab.module_scroll_y = max_scroll;
                }
                if tab.module_scroll_y < 0.0 {
                    tab.module_scroll_y = 0.0;
                }
            }
        }

        // Optional row highlight (Nav's current entry, Editor's
        // cursor row). Painted below text/cursor so glyphs read
        // cleanly on top.
        if let Some(tab) = self
            .root
            .as_ref()
            .and_then(|n| n.find(pid))
            .map(|p| p.active_tab_ref())
        {
            if let Some(hline) = tab.module_highlight_line {
                let mut found_y: Option<f32> = None;
                if let Some(buf) = tab.content_buffer.as_ref() {
                    let mut acc = 0.0_f32;
                    for run in buf.layout_runs() {
                        if run.line_i as u32 == hline {
                            found_y = Some(acc);
                            break;
                        }
                        acc += line_height;
                    }
                }
                if let Some(ly) = found_y {
                    let hy = py + ly - tab.module_scroll_y;
                    let top = hy.max(py);
                    let bot = (hy + line_height).min(py + content_h);
                    if bot > top {
                        below.push(RectInstance {
                            rect: [px, top, content_w, bot - top],
                            color: [1.0, 200.0 / 255.0, 80.0 / 255.0, 0.10],
                        });
                    }
                }
            }
        }

        // Host-rendered cursor for data modules that asked for one
        // (Editor). Uses the same color the shell cursor uses + the
        // same blink — same look as anywhere else in terminite.
        // Cells are sized at this pane's metrics; we compute the
        // cursor's source-line y, subtract module_scroll_y, and clip
        // to the content rect via a bounds check.
        if let Some(tab) = self
            .root
            .as_ref()
            .and_then(|n| n.find(pid))
            .map(|p| p.active_tab_ref())
        {
            if let Some((cline, ccol)) = tab.module_cursor {
                // Find the actual y of the cursor's source line by
                // walking layout_runs. With width-only buffer sizing
                // a long line wraps into multiple runs — the source
                // line index ≠ the iteration index. Treating them as
                // the same was the bug Daniel hit: lines past the
                // first wrap were drawn one row too low per wrap.
                let mut found_y: Option<f32> = None;
                let mut found_run_w: f32 = 0.0;
                if let Some(buf) = tab.content_buffer.as_ref() {
                    let mut acc = 0.0_f32;
                    for run in buf.layout_runs() {
                        if run.line_i as u32 == cline {
                            found_y = Some(acc);
                            found_run_w = run.line_w;
                            break;
                        }
                        acc += line_height;
                    }
                }
                if let Some(line_y) = found_y {
                    let cy = py + line_y - tab.module_scroll_y;
                    let row_top = cy;
                    let row_bottom = cy + line_height;
                    let visible = row_bottom > py && row_top < py + content_h;
                    if visible && blink_on {
                        // Column → x. For unwrapped lines this is
                        // exact. For lines that wrap before the
                        // cursor column the math is still on the
                        // first run; the cursor appears at start of
                        // line instead of inside the wrap. v1
                        // limitation — we don't wrap in editor /
                        // nav so it's a non-issue in practice.
                        let cell_advance = metrics.cell_advance;
                        let cx = (px + (ccol as f32) * cell_advance)
                            .min(px + found_run_w + cell_advance);
                        let crect = [
                            cx - CURSOR_PAD_X,
                            cy - CURSOR_PAD_Y,
                            cell_advance + 2.0 * CURSOR_PAD_X,
                            line_height + 2.0 * CURSOR_PAD_Y,
                        ];
                        let cl = crect[0].max(px);
                        let ct = crect[1].max(py);
                        let cr = (crect[0] + crect[2]).min(px + content_w);
                        let cb = (crect[1] + crect[3]).min(py + content_h);
                        if cr > cl && cb > ct {
                            below.push(RectInstance {
                                rect: [cl, ct, cr - cl, cb - ct],
                                color: CURSOR_COLOR,
                            });
                        }
                    }
                }
            }
        }

        PaneDraw {
            pid,
            text_left: px,
            text_top: py,
            bounds: TextBounds {
                left: px as i32,
                top: py as i32,
                right: (rect.x + rect.w - pad.right) as i32,
                bottom: (rect.y + rect.h - pad.bottom) as i32,
            },
            tabs: tab_slots,
        }
    }

    fn render_pane(
        &mut self,
        pid: PaneId,
        rect: PaneRect,
        is_active: bool,
        blink_on: bool,
        below: &mut Vec<RectInstance>,
        above: &mut Vec<RectInstance>,
        tab_bar: &mut Vec<RectInstance>,
    ) -> PaneDraw {
        let metrics = self.pane_metrics(pid);
        let cell_advance = metrics.cell_advance;
        let line_height = metrics.line_height;
        let pad = self.pad;
        // This pane's own tab bar fills the top strip of its rect.
        let tab_slots = self.build_pane_tab_bar(pid, rect, is_active, tab_bar);

        // Per-pane background tint — pushed first into `below` so it
        // sits beneath everything else. Low alpha keeps text legible;
        // the palette color stays recognisable as a hint, not a wash.
        let pane_bg_idx = self
            .root
            .as_ref()
            .and_then(|n| n.find(pid))
            .map(|p| p.bg_idx)
            .unwrap_or(0);
        if pane_bg_idx != 0 {
            let [r, g, b, _] = palette_color(pane_bg_idx);
            below.push(RectInstance {
                rect: [
                    rect.x,
                    rect.y + self.tab_bar_height,
                    rect.w,
                    rect.h - self.tab_bar_height,
                ],
                color: [r, g, b, 0.18],
            });
        }

        // Non-shell content kinds short-circuit the whole shell render
        // path. The Welcome card (and future built-in kinds) lives in
        // `content_buffer`, built lazily here.
        let active_kind = self
            .root
            .as_ref()
            .and_then(|n| n.find(pid))
            .map(|p| p.active_tab_ref().kind.clone())
            .unwrap_or(TabContentKind::Shell);
        // TTY modules render through the same path shells use (they
        // draw via terminal escape sequences, parsed by alacritty).
        // Only data modules + Welcome short-circuit to a static body.
        let is_tty_module = match &active_kind {
            TabContentKind::Module(id) => self
                .modules
                .find(id)
                .map(|m| m.kind == crate::modules::ModuleKind::Tty)
                .unwrap_or(false),
            _ => false,
        };
        if active_kind != TabContentKind::Shell && !is_tty_module {
            return self.render_non_shell_pane(
                pid, rect, tab_slots, active_kind, blink_on, below,
            );
        }

        // Content origin and clip box — below this pane's tab bar, inset
        // on all four sides by the configured padding.
        let px = rect.x + pad.left;
        let py = rect.y + self.tab_bar_height + pad.top;
        let box_l = px;
        let box_t = py;
        let box_r = rect.x + rect.w - pad.right;
        let box_b = rect.y + rect.h - pad.bottom;
        // Clip a rect to this pane's content box; `None` if fully outside.
        // Hides the extra row above the pane until it scrolls into view, and
        // keeps one pane's rects out of its neighbour.
        let clip = |r: [f32; 4]| -> Option<[f32; 4]> {
            let nx = r[0].max(box_l);
            let ny = r[1].max(box_t);
            let nr = (r[0] + r[2]).min(box_r);
            let nb = (r[1] + r[3]).min(box_b);
            if nr <= nx || nb <= ny {
                None
            } else {
                Some([nx, ny, nr - nx, nb - ny])
            }
        };

        // ── Autoscroll tick (only a drag-selecting tab has a direction) ──
        let autoscroll_dir = self
            .root
            .as_mut()
            .expect("pane tree present")
            .find_mut(pid)
            .expect("pane present")
            .active_tab_ref()
            .autoscroll_dir;
        if let Some(dir) = autoscroll_dir {
            {
                let tab = self
                    .root
                    .as_mut()
                    .expect("pane tree present")
                    .find_mut(pid)
                    .expect("pane present")
                    .active_tab_mut();
                tab.active_term_mut().scroll(TermScroll::Delta(dir));
                let (after, _history) = tab.active_term().offset_and_history();
                let (c, r) = (tab.cols, tab.rows);
                if let Some(sel) = tab.selection.as_mut() {
                    let edge = if dir > 0 {
                        (-(after as i32), 0)
                    } else {
                        (r as i32 - 1 - after as i32, c.saturating_sub(1))
                    };
                    sel.extend_to(edge.0, edge.1);
                }
            }
            self.next_autoscroll_deadline =
                Some(Instant::now() + Duration::from_millis(AUTOSCROLL_TICK_MS));
        }

        // ── Snapshot the pane's active tab ──
        let Snapshot {
            text_runs,
            bg_runs,
            deco_runs,
            link_runs,
            cursor_line,
            cursor_col,
            cursor_shape,
            cursor_blinking,
            has_extra_row,
        } = self
            .root
            .as_mut()
            .expect("pane tree present")
            .find_mut(pid)
            .expect("pane present")
            .active_tab_mut()
            .active_term_mut()
            .snapshot();
        let _ = cursor_blinking;

        // ── Refresh the active tab's text buffer if its content changed ──
        {
            let tab = self
                .root
                .as_mut()
                .expect("pane tree present")
                .find_mut(pid)
                .expect("pane present")
                .active_tab_mut();
            let stale = tab.buffer_dirty || text_runs != tab.last_text_runs;
            if stale {
                let default_attrs =
                    Attrs::new().family(font_family(&self.font_family));
                tab.text_buffer.set_rich_text(
                    &mut self.font_system,
                    text_runs.iter().map(|(text, style)| {
                        let mut attrs = default_attrs.clone().color(style.color);
                        if style.bold {
                            attrs = attrs.weight(Weight::BOLD);
                        }
                        if style.italic {
                            attrs = attrs.style(Style::Italic);
                        }
                        (text.as_str(), attrs)
                    }),
                    &default_attrs,
                    Shaping::Advanced,
                    None,
                );
                tab.text_buffer
                    .shape_until_scroll(&mut self.font_system, false);
                tab.last_text_runs = text_runs;
                tab.buffer_dirty = false;
            }
        }

        // ── Active tab geometry reads ──
        let (y_shift, selection, display_offset, cols, rows) = {
            let tab = self
                .root
                .as_mut()
                .expect("pane tree present")
                .find_mut(pid)
                .expect("pane present")
                .active_tab_mut();
            (
                tab.pixel_offset,
                tab.selection,
                tab.active_term().offset_and_history().0 as i32,
                tab.cols,
                tab.rows,
            )
        };

        // ── Background runs ──
        for run in &bg_runs {
            if let Some(rc) = clip([
                px + run.start_col as f32 * cell_advance,
                py + run.line as f32 * line_height + y_shift,
                run.width as f32 * cell_advance,
                line_height,
            ]) {
                below.push(RectInstance {
                    rect: rc,
                    color: color_to_floats(run.color),
                });
            }
        }

        // ── Selection highlight: one rect per row, absolute Line coords
        // converted to viewport rows via display_offset. vl = -1 is allowed
        // so the highlight enters smoothly from the top during scroll. ──
        if let Some(sel) = selection {
            if !sel.is_empty() {
                let ((s_line, s_col), (e_line, e_col)) = sel.normalized();
                for abs_line in s_line..=e_line {
                    let vl = abs_line + display_offset;
                    if vl < -1 || vl >= rows as i32 {
                        continue;
                    }
                    let col_start = if abs_line == s_line { s_col } else { 0 };
                    let col_end_raw = if abs_line == e_line { e_col + 1 } else { cols };
                    let col_end = col_end_raw.min(cols);
                    let col_start = col_start.min(cols);
                    if col_start >= col_end {
                        continue;
                    }
                    if let Some(rc) = clip([
                        px + col_start as f32 * cell_advance,
                        py + vl as f32 * line_height + y_shift,
                        (col_end - col_start) as f32 * cell_advance,
                        line_height,
                    ]) {
                        below.push(RectInstance { rect: rc, color: SELECTION_COLOR });
                    }
                }
            }
        }

        // ── Find-match highlights (active pane's search only) ──
        if is_active {
            if let Some(find) = self.find.as_ref() {
                for (i, &(line, cs, ce)) in find.matches.iter().enumerate() {
                    let vl = line + display_offset;
                    if vl < -1 || vl >= rows as i32 {
                        continue;
                    }
                    let color = if i == find.current {
                        FIND_CURRENT_COLOR
                    } else {
                        FIND_MATCH_COLOR
                    };
                    if let Some(rc) = clip([
                        px + cs as f32 * cell_advance,
                        py + vl as f32 * line_height + y_shift,
                        (ce - cs + 1) as f32 * cell_advance,
                        line_height,
                    ]) {
                        below.push(RectInstance { rect: rc, color });
                    }
                }
            }
        }

        // ── Cursor (last in `below` so it sits on top of selection/bgs) ──
        let cursor_visible = !matches!(cursor_shape, CursorShapeKind::Hidden);
        if cursor_visible && blink_on {
            let cx = px + cursor_col as f32 * cell_advance;
            let cy_base = py + (cursor_line.max(0) as f32) * line_height + y_shift;
            let (crect, is_hollow) = match cursor_shape {
                CursorShapeKind::Block | CursorShapeKind::HollowBlock => (
                    [
                        cx - CURSOR_PAD_X,
                        cy_base - CURSOR_PAD_Y,
                        cell_advance + 2.0 * CURSOR_PAD_X,
                        line_height + 2.0 * CURSOR_PAD_Y,
                    ],
                    matches!(cursor_shape, CursorShapeKind::HollowBlock),
                ),
                CursorShapeKind::Beam => ([cx, cy_base, 2.0, line_height], false),
                CursorShapeKind::Underline => {
                    ([cx, cy_base + line_height - 2.0, cell_advance, 2.0], false)
                }
                CursorShapeKind::Hidden => ([0.0; 4], false),
            };
            if is_hollow {
                let [x, y, w, h] = crect;
                let t = 1.5;
                for edge in [
                    [x, y, w, t],
                    [x, y + h - t, w, t],
                    [x, y, t, h],
                    [x + w - t, y, t, h],
                ] {
                    if let Some(rc) = clip(edge) {
                        below.push(RectInstance { rect: rc, color: CURSOR_COLOR });
                    }
                }
            } else if let Some(rc) = clip(crect) {
                below.push(RectInstance { rect: rc, color: CURSOR_COLOR });
            }
        }

        // ── Decorations (underline / double underline / strikeout) ──
        for run in &deco_runs {
            let x = px + run.start_col as f32 * cell_advance;
            let w = run.width as f32 * cell_advance;
            let line_y = py + run.line as f32 * line_height + y_shift;
            let (y, h) = match run.kind {
                DecorationKind::Underline | DecorationKind::DoubleUnderline => {
                    (line_y + line_height - UNDERLINE_THICKNESS, UNDERLINE_THICKNESS)
                }
                DecorationKind::Strikeout => (
                    line_y + line_height / 2.0 - STRIKEOUT_THICKNESS / 2.0,
                    STRIKEOUT_THICKNESS,
                ),
            };
            let color = color_to_floats(run.color);
            if let Some(rc) = clip([x, y, w, h]) {
                above.push(RectInstance { rect: rc, color });
            }
            if matches!(run.kind, DecorationKind::DoubleUnderline) {
                if let Some(rc) =
                    clip([x, y - DOUBLE_UNDERLINE_GAP, w, UNDERLINE_THICKNESS])
                {
                    above.push(RectInstance { rect: rc, color });
                }
            }
        }

        // ── OSC 8 hyperlink underlines ──
        for run in &link_runs {
            let x = px + run.start_col as f32 * cell_advance;
            let w = run.width as f32 * cell_advance;
            let line_y = py + run.line as f32 * line_height + y_shift;
            if let Some(rc) = clip([
                x,
                line_y + line_height - UNDERLINE_THICKNESS,
                w,
                UNDERLINE_THICKNESS,
            ]) {
                above.push(RectInstance { rect: rc, color: LINK_UNDERLINE_COLOR });
            }
        }

        // The buffer's top sits one line up when an extra row is present; the
        // y_shift slides it into view as pixel_offset grows.
        let text_top = if has_extra_row {
            py - line_height + y_shift
        } else {
            py + y_shift
        };
        PaneDraw {
            pid,
            text_left: px,
            text_top,
            bounds: TextBounds {
                left: box_l as i32,
                top: box_t as i32,
                right: box_r as i32,
                bottom: box_b as i32,
            },
            tabs: tab_slots,
        }
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
