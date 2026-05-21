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
use winit::window::Window;

use crate::palette::{color_to_floats, DEFAULT_FG};
use crate::rect::{RectInstance, RectRenderer};
use crate::term::{CursorShapeKind, DecorationKind, LiveTerm, ModeFlags, Snapshot, SpanStyle, TermScroll};
use crate::{TabId, UserEvent, BACKGROUND, FONT_SIZE, LINE_HEIGHT, TEXT_LEFT, TEXT_TOP};

const UNDERLINE_THICKNESS: f32 = 1.5;
const DOUBLE_UNDERLINE_GAP: f32 = 2.0;
const STRIKEOUT_THICKNESS: f32 = 1.5;

const CURSOR_PAD_X: f32 = 1.0;
const CURSOR_PAD_Y: f32 = 1.0;
const CURSOR_COLOR: [f32; 4] = [1.0, 200.0 / 255.0, 80.0 / 255.0, 180.0 / 255.0];

/// Translucent steel-blue selection highlight.
const SELECTION_COLOR: [f32; 4] = [0.32, 0.46, 0.75, 0.35];

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
/// the text area begins at `TEXT_TOP + TAB_BAR_HEIGHT`.
const TAB_BAR_HEIGHT: f32 = 44.0;
/// Minimum width of a tab in the tab bar. When more tabs are open than the
/// bar can fit at full width, they shrink uniformly down to this floor.
const TAB_MIN_WIDTH: f32 = 80.0;
/// Maximum width of a tab in the tab bar — keeps tabs from spanning the
/// whole bar when there's only one or two.
const TAB_MAX_WIDTH: f32 = 360.0;
const TAB_ACTIVE_BG: [f32; 4] = [0.10, 0.10, 0.13, 1.0];
const TAB_INACTIVE_BG: [f32; 4] = [0.06, 0.06, 0.08, 1.0];
const TAB_ACTIVE_UNDERLINE: [f32; 4] =
    [1.0, 200.0 / 255.0, 80.0 / 255.0, 1.0];
const TAB_SEPARATOR: [f32; 4] = [0.16, 0.16, 0.20, 1.0];

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

/// One tab in the window. Owns a PTY + everything that's conceptually
/// per-shell — scroll state, selection, click history, snapshot cache.
struct Tab {
    id: TabId,
    title: String,
    live_term: LiveTerm,

    pixel_offset: f32,
    selection: Option<Selection>,
    dragging: bool,
    last_drag_mouse_pos: (f32, f32),
    last_click: Option<(Instant, (i32, usize), u8)>,
    last_text_runs: Vec<(String, SpanStyle)>,
    autoscroll_dir: Option<i32>,
}

impl Tab {
    fn new(id: TabId, title: String, live_term: LiveTerm) -> Self {
        Self {
            id,
            title,
            live_term,
            pixel_offset: 0.0,
            selection: None,
            dragging: false,
            last_drag_mouse_pos: (0.0, 0.0),
            last_click: None,
            last_text_runs: Vec::new(),
            autoscroll_dir: None,
        }
    }
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
    text_buffer: Buffer,

    rects_below: RectRenderer,
    rects_above: RectRenderer,
    /// Tab bar rendered with a separate scissor zone above the text area.
    rects_tab_bar: RectRenderer,

    cell_advance: f32,
    grid_cols: usize,
    grid_rows: usize,

    /// Tabs and the index of the currently-active one. Per-tab state lives
    /// inside `Tab`; the renderer's mouse/keyboard/wheel input is routed to
    /// `self.tabs[self.active]`.
    tabs: Vec<Tab>,
    active: usize,
    /// Monotonic counter for new TabId allocation.
    next_tab_id: u64,
    /// Which tab's content the shared `text_buffer` currently holds. When
    /// this drifts from the active tab, `render()` forces a `set_rich_text`
    /// regardless of the per-tab cache (otherwise switching back to a tab
    /// whose cache still matches its grid would leave the buffer showing
    /// the previously-active tab).
    text_buffer_tab: Option<TabId>,

    // Shared mouse / system state. Mouse position is window-relative.
    mouse_pos: (f32, f32),
    clipboard: Option<Clipboard>,

    /// Visual bell deadline; `Some(t)` means draw a flash overlay until `t`.
    bell_flash_until: Option<Instant>,
    /// Whether the window has keyboard focus — gates cursor blink.
    focused: bool,
    /// Renderer start time; cursor-blink phase is computed from elapsed.
    start_time: Instant,
    /// In-flight IME preedit text rendered near the cursor.
    preedit: String,

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

    /// Held so new tabs can construct their `LiveTerm` with a Notifier
    /// pointing back at this event loop.
    proxy: EventLoopProxy<UserEvent>,

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
        let cell_advance = measure_cell_advance(&mut font_system);

        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        let rects_below = RectRenderer::new(&device, format, "below");
        let rects_above = RectRenderer::new(&device, format, "above");
        let rects_tab_bar = RectRenderer::new(&device, format, "tab_bar");

        // winit's PhysicalSize is already in physical pixels — earlier code
        // multiplied by scale_factor a second time, so the grid math thought
        // the surface was 2x taller than it actually was on Retina, and rows
        // past visible got snapshotted into the buffer but rendered off the
        // bottom of the window.
        let physical_width = width as f32;
        let physical_height = height as f32;

        let mut text_buffer = Buffer::new(&mut font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        text_buffer.set_size(&mut font_system, Some(physical_width), Some(physical_height));
        // Force every glyph to exactly one cell wide. Without this, cosmic-text
        // uses each glyph's natural advance, which drifts slightly even within
        // a monospace font and breaks column alignment (visible in `ls`,
        // tables, ASCII art).
        text_buffer.set_monospace_width(&mut font_system, Some(cell_advance));
        text_buffer.set_text(
            &mut font_system,
            "",
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        text_buffer.shape_until_scroll(&mut font_system, false);

        let (cols, rows) = compute_grid_size(physical_width, physical_height, cell_advance);
        let first_tab_id = TabId(0);
        let live_term = LiveTerm::new(
            cols,
            rows,
            cell_advance,
            proxy.clone(),
            first_tab_id,
            None,
        );

        // Clipboard is optional; it's possible the platform refuses to give us
        // one (sandboxing, missing service). Copy/paste then become no-ops.
        let clipboard = Clipboard::new().ok();

        let first_tab = Tab::new(first_tab_id, "terminite".to_string(), live_term);

        Self {
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
            text_buffer,
            rects_below,
            rects_above,
            rects_tab_bar,
            cell_advance,
            grid_cols: cols,
            grid_rows: rows,
            tabs: vec![first_tab],
            active: 0,
            next_tab_id: 1,
            text_buffer_tab: None,
            mouse_pos: (0.0, 0.0),
            clipboard,
            bell_flash_until: None,
            focused: true,
            start_time: Instant::now(),
            preedit: String::new(),
            next_blink_deadline: None,
            next_autoscroll_deadline: None,
            rss_kill_bytes: rss_kill_threshold_bytes(),
            proxy,
            window,
        }
    }

    pub fn new_tab(&mut self) {
        // Inherit the active tab's shell cwd into the new shell.
        let cwd = self
            .tabs
            .get(self.active)
            .and_then(|t| t.live_term.current_dir());
        let id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        let live_term = LiveTerm::new(
            self.grid_cols,
            self.grid_rows,
            self.cell_advance,
            self.proxy.clone(),
            id,
            cwd,
        );
        let tab = Tab::new(id, "terminite".to_string(), live_term);
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.window.set_title(&self.tabs[self.active].title);
        self.window.request_redraw();
    }

    /// Close the active tab. Returns true if the window should exit (no
    /// tabs remain).
    pub fn close_active_tab(&mut self) -> bool {
        if self.tabs.len() <= 1 {
            return true;
        }
        let idx = self.active;
        self.tabs.remove(idx);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        self.window.set_title(&self.tabs[self.active].title);
        self.window.request_redraw();
        false
    }

    pub fn switch_to_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() || idx == self.active {
            return;
        }
        self.active = idx;
        self.window.set_title(&self.tabs[self.active].title);
        self.window.request_redraw();
    }

    pub fn next_tab(&mut self) {
        if self.tabs.len() <= 1 {
            return;
        }
        let idx = (self.active + 1) % self.tabs.len();
        self.switch_to_tab(idx);
    }

    pub fn prev_tab(&mut self) {
        if self.tabs.len() <= 1 {
            return;
        }
        let idx = if self.active == 0 {
            self.tabs.len() - 1
        } else {
            self.active - 1
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
        let physical_width = width as f32;
        let physical_height = height as f32;
        self.text_buffer.set_size(
            &mut self.font_system,
            Some(physical_width),
            Some(physical_height),
        );
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);

        let (cols, rows) = compute_grid_size(physical_width, physical_height, self.cell_advance);
        // Broadcast the new grid size to every tab's PTY; inactive tabs are
        // still expected to keep accurate state for when the user switches
        // back, and shells like ssh / vim resize on the SIGWINCH alacritty
        // sends.
        for tab in &mut self.tabs {
            tab.live_term.resize(cols, rows);
        }
        self.grid_cols = cols;
        self.grid_rows = rows;
        // A resize invalidates the snapshot cache *and* the selection — the
        // cells the user was selecting may now be in a different place.
        self.tabs[self.active].last_text_runs.clear();
        self.tabs[self.active].selection = None;
    }

    // ── Mouse / keyboard input routing ────────────────────────────────────

    /// Convert a mouse pixel position into an absolute (Line, Column) using
    /// the current display_offset. Used for both selection start and extend.
    fn pixel_to_absolute(&self, x: f32, y: f32) -> (i32, usize) {
        let cx = (x - TEXT_LEFT).max(0.0);
        let col = ((cx / self.cell_advance) as usize)
            .min(self.grid_cols.saturating_sub(1));
        // Same pixel_offset correction as pixel_to_cell, but with a signed
        // floor so a click just inside the top of viewport while the buffer
        // is shifted down resolves to row -1 (the extra row above the
        // viewport) when appropriate.
        let cy = (y - TEXT_TOP - self.tabs[self.active].pixel_offset) / LINE_HEIGHT;
        let vl = cy.floor() as i32;
        let vl = vl.max(-1).min(self.grid_rows as i32 - 1);
        let display_offset = self.tabs[self.active].live_term.offset_and_history().0 as i32;
        (vl - display_offset, col)
    }

    pub fn mouse_moved(&mut self, x: f32, y: f32, modifiers: ModifiersState) {
        self.mouse_pos = (x, y);

        // Mouse reporting takes precedence over selection / scroll.
        let mode = self.tabs[self.active].live_term.mode_flags();
        let reporting_active = mode.mouse_drag || mode.mouse_motion;
        if reporting_active {
            // Drag (1002): only when a button is held. Motion (1003): always.
            let button_held = self.tabs[self.active].dragging || mode.mouse_motion;
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
                        self.tabs[self.active].live_term.write(b);
                    }
                }
            }
            return;
        }

        if self.tabs[self.active].dragging {
            // macOS trackpad scrolling drags the cursor a hair, so we get
            // tiny mouse_moved events interleaved with wheel events. Without
            // this filter, every wheel-driven extension to the viewport
            // edge gets immediately snapped back to whatever cell the
            // cursor is currently over. Only count motion that crosses
            // half a cell from the last update.
            let (last_x, last_y) = self.tabs[self.active].last_drag_mouse_pos;
            let dx = (x - last_x).abs();
            let dy = (y - last_y).abs();
            let big_motion = dx >= self.cell_advance * 0.5 || dy >= LINE_HEIGHT * 0.5;
            if big_motion {
                let (line, col) = self.pixel_to_absolute(x, y);
                if let Some(sel) = self.tabs[self.active].selection.as_mut() {
                    sel.extend_to(line, col);
                }
                self.tabs[self.active].last_drag_mouse_pos = (x, y);
                self.window.request_redraw();
            }

            // Auto-scroll if the cursor is past the viewport's top or
            // bottom edge: keep scrolling while the user holds the button
            // there, extending the selection as new content reveals.
            let surface_h = self.surface_config.height as f32;
            let new_dir = if y < TEXT_TOP + AUTOSCROLL_MARGIN_PX {
                Some(1)
            } else if y > surface_h - AUTOSCROLL_MARGIN_PX {
                Some(-1)
            } else {
                None
            };
            let was_off = self.tabs[self.active].autoscroll_dir.is_none();
            self.tabs[self.active].autoscroll_dir = new_dir;
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
        // Tab-bar hit test first — left-click in the bar switches tabs and
        // never starts a selection.
        if self.mouse_pos.1 < TAB_BAR_HEIGHT && button == MouseButton::Left {
            if let Some(idx) = self.tab_at_x(self.mouse_pos.0) {
                if idx != self.active {
                    self.active = idx;
                    self.window
                        .set_title(&self.tabs[idx].title);
                    self.window.request_redraw();
                }
            }
            return;
        }

        let mode = self.tabs[self.active].live_term.mode_flags();
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
                    self.tabs[self.active].live_term.write(b);
                }
            }
            return;
        }

        // Only the left button starts a selection. Right/middle without
        // mouse-reporting are no-ops for v1 (right-click menu lands later
        // in Phase 1).
        if button != MouseButton::Left {
            return;
        }

        let (line, col) = self.pixel_to_absolute(self.mouse_pos.0, self.mouse_pos.1);
        let now = Instant::now();
        let click_count = match self.tabs[self.active].last_click {
            Some((t, cell, c)) if now.duration_since(t) < MULTI_CLICK_WINDOW && cell == (line, col) => {
                (c + 1).min(3)
            }
            _ => 1,
        };
        self.tabs[self.active].last_click = Some((now, (line, col), click_count));

        match click_count {
            1 => {
                self.tabs[self.active].selection = Some(Selection::from_anchor(line, col));
                self.tabs[self.active].dragging = true;
                self.tabs[self.active].last_drag_mouse_pos = self.mouse_pos;
            }
            2 => {
                let ((sl, sc), (el, ec)) = self.tabs[self.active].live_term.word_at(line, col);
                self.tabs[self.active].selection = Some(Selection {
                    anchor_line: sl,
                    anchor_col: sc,
                    head_line: el,
                    head_col: ec,
                });
                self.tabs[self.active].dragging = false;
                self.copy_selection();
            }
            _ => {
                let ((sl, sc), (el, ec)) = self.tabs[self.active].live_term.line_at(line);
                self.tabs[self.active].selection = Some(Selection {
                    anchor_line: sl,
                    anchor_col: sc,
                    head_line: el,
                    head_col: ec,
                });
                self.tabs[self.active].dragging = false;
                self.copy_selection();
            }
        }
        self.window.request_redraw();
        let _ = modifiers; // reserved for future Cmd-click hyperlink etc.
    }

    pub fn mouse_up(&mut self, button: MouseButton, modifiers: ModifiersState) {
        let mode = self.tabs[self.active].live_term.mode_flags();
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
                    self.tabs[self.active].live_term.write(b);
                }
            }
            return;
        }

        if button != MouseButton::Left {
            return;
        }

        self.tabs[self.active].dragging = false;
        self.tabs[self.active].autoscroll_dir = None;
        self.next_autoscroll_deadline = None;
        if let Some(sel) = self.tabs[self.active].selection.as_ref() {
            if sel.is_empty() {
                self.tabs[self.active].selection = None;
            } else {
                self.copy_selection();
            }
        }
        self.window.request_redraw();
    }

    pub fn mouse_wheel(&mut self, delta: MouseScrollDelta, modifiers: ModifiersState) {
        // If the foreground app wants scroll reports (vim, less, htop in
        // mouse mode), forward instead of scrolling the viewport.
        let mode = self.tabs[self.active].live_term.mode_flags();
        if mode.mouse_report_click || mode.mouse_drag || mode.mouse_motion {
            let pixels = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => p.y as f32 / LINE_HEIGHT,
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
                    self.tabs[self.active].live_term.write(b);
                }
            }
            return;
        }

        // Work in physical pixels so the renderer can shift by the remainder
        // for pixel-smooth scrolling. LineDelta is real-wheel "clicks" (~3
        // lines each, scaled to pixels); PixelDelta is trackpad pixels.
        let pixels = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 3.0 * LINE_HEIGHT,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        self.tabs[self.active].pixel_offset += pixels;

        // Pop whole lines into the term; the remainder stays as a sub-line
        // pixel shift used at render time. `floor` keeps the remainder in
        // [0, LINE_HEIGHT) for any input direction — but only when the
        // requested scroll actually happens. If alacritty clamps (we asked
        // Delta(-2) but were at offset=1), subtracting the full `whole`
        // leaves a residual that renders as motion in the wrong direction,
        // and floor's over-pop re-establishes the residual on every event
        // — so the bottom (offset=0) is never reached cleanly. Subtract by
        // the *actual* offset delta instead.
        let whole = (self.tabs[self.active].pixel_offset / LINE_HEIGHT).floor() as i32;
        if whole != 0 {
            let (before, _) = self.tabs[self.active].live_term.offset_and_history();
            self.tabs[self.active].live_term.scroll(TermScroll::Delta(whole));
            let (after, history) = self.tabs[self.active].live_term.offset_and_history();
            let actual = after as i32 - before as i32;
            self.tabs[self.active].pixel_offset -= actual as f32 * LINE_HEIGHT;
            if actual != whole {
                // Clamped at a boundary; drop the residual.
                self.tabs[self.active].pixel_offset = 0.0;
                let at_top = whole > 0 && after >= history;
                let at_live = whole < 0 && after == 0;
                if at_top {
                    eprintln!(
                        "[scroll] hit top boundary: offset={} history={} (rows={}) topRow='{}'",
                        after,
                        history,
                        self.grid_rows,
                        self.tabs[self.active].live_term.debug_top_row()
                    );
                } else if at_live {
                    eprintln!(
                        "[scroll] hit live boundary: offset={} history={} (rows={}) {}",
                        after,
                        history,
                        self.grid_rows,
                        self.tabs[self.active].live_term.debug_bottom_strip(self.grid_rows)
                    );
                }
            }

            // While dragging, extending the head to wherever the mouse pixel
            // sits would actually *shrink* the selection as scroll reveals
            // new content (the same pixel now points at an older row going
            // up, newer going down). Instead push the head to the viewport
            // edge in the scroll direction, so the selection grows to cover
            // the freshly-revealed lines. Pick whichever extends *further*
            // from the anchor — mouse position still wins when it's already
            // farther.
            if actual != 0 && self.tabs[self.active].dragging {
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
                if let Some(sel) = self.tabs[self.active].selection.as_mut() {
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
        self.tabs[self.active].live_term.scroll(s);
        self.window.request_redraw();
    }

    pub fn copy_selection(&mut self) {
        let Some(sel) = self.tabs[self.active].selection.as_ref() else { return };
        if sel.is_empty() {
            return;
        }
        let (start, end) = sel.normalized();
        let text = self.tabs[self.active].live_term.extract_text(start, end);
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
        if self.tabs[self.active].live_term.mode_flags().bracketed_paste {
            // Wrap so the shell treats the whole paste as one input, not as
            // typed-and-pressed-enter for each newline. Strips any embedded
            // \e[201~ to keep the framing safe.
            let safe = text.replace("\x1b[201~", "");
            let mut bytes = Vec::with_capacity(safe.len() + 12);
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(safe.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            self.tabs[self.active].live_term.write(bytes);
        } else {
            self.tabs[self.active].live_term.write(text.into_bytes());
        }
    }

    pub fn ring_bell(&mut self, _tab_id: TabId) {
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

    /// Apply a tab title (from a per-tab Notifier's `Event::Title`). Also
    /// updates the window title when the renamed tab is the active one.
    pub fn set_tab_title(&mut self, tab_id: TabId, title: String) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.title = title;
        }
        // The window title tracks the active tab.
        if let Some(active) = self.tabs.get(self.active) {
            self.window.set_title(&active.title);
        }
    }

    /// Write bytes to the active tab's PTY (keyboard input path).
    pub fn write_active(&self, bytes: Vec<u8>) {
        self.tabs[self.active].live_term.write(bytes);
    }

    /// Return the tab index under a given x coordinate, or None if x falls
    /// past the rightmost tab.
    fn tab_at_x(&self, x: f32) -> Option<usize> {
        for (i, (start, width, _)) in self.tab_bar_layout().into_iter().enumerate() {
            if x >= start && x < start + width {
                return Some(i);
            }
        }
        None
    }

    /// Geometry of each tab in the tab bar: `(x_start, width, is_active)`.
    /// Widths shrink uniformly between TAB_MIN_WIDTH and TAB_MAX_WIDTH as
    /// tabs are added.
    fn tab_bar_layout(&self) -> Vec<(f32, f32, bool)> {
        let n = self.tabs.len();
        if n == 0 {
            return Vec::new();
        }
        let surface_w = self.surface_config.width as f32;
        let per = (surface_w / n as f32).clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH);
        (0..n)
            .map(|i| (i as f32 * per, per, i == self.active))
            .collect()
    }

    /// Build the rect instances that make up the tab bar. Drawn after the
    /// text-area pass with the scissor widened so the bar appears above the
    /// content scissor zone.
    fn build_tab_bar_rects(&self) -> Vec<RectInstance> {
        let layout = self.tab_bar_layout();
        let mut rects = Vec::with_capacity(layout.len() * 3 + 1);

        // Full-width bar background (matches inactive tabs so the gaps look
        // intentional when tabs don't span the whole bar).
        rects.push(RectInstance {
            rect: [0.0, 0.0, self.surface_config.width as f32, TAB_BAR_HEIGHT],
            color: TAB_INACTIVE_BG,
        });

        for (x, w, is_active) in &layout {
            let color = if *is_active { TAB_ACTIVE_BG } else { TAB_INACTIVE_BG };
            rects.push(RectInstance {
                rect: [*x, 0.0, *w, TAB_BAR_HEIGHT],
                color,
            });
            // Thin separator on the right edge of each non-last tab.
            rects.push(RectInstance {
                rect: [*x + *w - 1.0, 6.0, 1.0, TAB_BAR_HEIGHT - 12.0],
                color: TAB_SEPARATOR,
            });
            if *is_active {
                // Underline for the active tab.
                rects.push(RectInstance {
                    rect: [*x + 6.0, TAB_BAR_HEIGHT - 3.0, *w - 12.0, 3.0],
                    color: TAB_ACTIVE_UNDERLINE,
                });
            }
        }

        // Bottom border line of the bar — clean break between bar and text
        // area, even when the inner padding gap is the same color.
        rects.push(RectInstance {
            rect: [
                0.0,
                TAB_BAR_HEIGHT,
                self.surface_config.width as f32,
                1.0,
            ],
            color: TAB_SEPARATOR,
        });

        rects
    }

    /// Earliest pending deadline the main loop should wake on
    /// (`ControlFlow::WaitUntil`). `None` = sleep until the next real event.
    pub fn next_wakeup(&self) -> Option<Instant> {
        [
            self.bell_flash_until,
            self.next_blink_deadline,
            self.next_autoscroll_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn focus_changed(&mut self, focused: bool) {
        self.focused = focused;
        // Optionally emit DEC focus reporting when an app asked for it.
        let mode = self.tabs[self.active].live_term.mode_flags();
        if mode.focus_in_out {
            let seq: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
            self.tabs[self.active].live_term.write(seq.to_vec());
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
            self.tabs[self.active].live_term.write(text.into_bytes());
        }
        self.window.request_redraw();
    }

    /// 1-indexed (col, row) inside the visible viewport, for mouse-reporting
    /// protocols. Returns `None` if the pointer is outside the text area.
    fn cell_at_1indexed(&self, x: f32, y: f32) -> Option<(u32, u32)> {
        if x < TEXT_LEFT {
            return None;
        }
        // Match pixel_to_cell's pixel_offset correction so the reported cell
        // is the one the user visually clicked on, not the natural-grid cell.
        let row_f = (y - TEXT_TOP - self.tabs[self.active].pixel_offset) / LINE_HEIGHT;
        if row_f < 0.0 {
            return None;
        }
        let col = ((x - TEXT_LEFT) / self.cell_advance) as u32 + 1;
        let row = row_f as u32 + 1;
        if col as usize > self.grid_cols || row as usize > self.grid_rows {
            return None;
        }
        Some((col, row))
    }

    // ── Frame ────────────────────────────────────────────────────────────

    pub fn render(&mut self) {
        check_rss_kill_switch(self.rss_kill_bytes);

        // Auto-scroll while drag-selecting past viewport edge. The main loop
        // wakes us at `next_autoscroll_deadline`; we tick once per wake and
        // re-arm.
        if let Some(dir) = self.tabs[self.active].autoscroll_dir {
            self.tabs[self.active].live_term.scroll(TermScroll::Delta(dir));
            let (after, _history) = self.tabs[self.active].live_term.offset_and_history();
            if let Some(sel) = self.tabs[self.active].selection.as_mut() {
                let edge = if dir > 0 {
                    (-(after as i32), 0)
                } else {
                    (
                        self.grid_rows as i32 - 1 - after as i32,
                        self.grid_cols.saturating_sub(1),
                    )
                };
                sel.extend_to(edge.0, edge.1);
            }
            self.next_autoscroll_deadline =
                Some(Instant::now() + Duration::from_millis(AUTOSCROLL_TICK_MS));
        } else {
            self.next_autoscroll_deadline = None;
        }

        let Snapshot {
            text_runs,
            bg_runs,
            deco_runs,
            cursor_line,
            cursor_col,
            cursor_shape,
            cursor_blinking,
            has_extra_row,
        } = self.tabs[self.active].live_term.snapshot();

        let active_id = self.tabs[self.active].id;
        let buffer_holds_other_tab = self.text_buffer_tab != Some(active_id);
        let cache_stale = text_runs != self.tabs[self.active].last_text_runs;
        if buffer_holds_other_tab || cache_stale {
            let default_attrs = Attrs::new().family(Family::Monospace);
            self.text_buffer.set_rich_text(
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
            self.text_buffer
                .shape_until_scroll(&mut self.font_system, false);
            self.tabs[self.active].last_text_runs = text_runs;
            self.text_buffer_tab = Some(active_id);
        }

        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        let cell_advance = self.cell_advance;
        // Pixel-smooth scroll: every visible Y origin is shifted by this much.
        let y_shift = self.tabs[self.active].pixel_offset;
        let mut below: Vec<RectInstance> = Vec::with_capacity(bg_runs.len() + 8);

        for run in &bg_runs {
            below.push(RectInstance {
                rect: [
                    TEXT_LEFT + run.start_col as f32 * cell_advance,
                    TEXT_TOP + run.line as f32 * LINE_HEIGHT + y_shift,
                    run.width as f32 * cell_advance,
                    LINE_HEIGHT,
                ],
                color: color_to_floats(run.color),
            });
        }

        // Selection highlight: one rect per row of the selection. Coordinates
        // are absolute (Line index); convert to viewport rows by adding the
        // current display_offset so the highlight rides along with content.
        // We allow vl = -1 (one row above the viewport) so the highlight
        // smoothly enters from the top during pixel-smooth scroll — matching
        // the extra-row text/background rendering. Without this, a selected
        // line briefly loses its highlight as it slides through that row.
        if let Some(sel) = self.tabs[self.active].selection.as_ref() {
            if !sel.is_empty() {
                let ((s_line, s_col), (e_line, e_col)) = sel.normalized();
                let cols = self.grid_cols;
                let rows = self.grid_rows as i32;
                let display_offset = self.tabs[self.active].live_term.offset_and_history().0 as i32;
                for abs_line in s_line..=e_line {
                    let vl = abs_line + display_offset;
                    if vl < -1 || vl >= rows {
                        continue;
                    }
                    let col_start = if abs_line == s_line { s_col } else { 0 };
                    let col_end_raw = if abs_line == e_line { e_col + 1 } else { cols };
                    let col_end = col_end_raw.min(cols);
                    let col_start = col_start.min(cols);
                    if col_start >= col_end {
                        continue;
                    }
                    below.push(RectInstance {
                        rect: [
                            TEXT_LEFT + col_start as f32 * cell_advance,
                            TEXT_TOP + vl as f32 * LINE_HEIGHT + y_shift,
                            (col_end - col_start) as f32 * cell_advance,
                            LINE_HEIGHT,
                        ],
                        color: SELECTION_COLOR,
                    });
                }
            }
        }

        // Cursor last in the below layer so it sits on top of selection and bgs.
        // Shape (CSI 0–6 q): block / underline / beam, plus hollow + hidden.
        // Blink phase: drawn only while window has focus and blink is enabled.
        // Default to blink-on whenever the window is focused. alacritty's
        // CursorStyle.blinking is false unless the shell explicitly sends
        // `\e[1/3/5 q`; respecting that strictly leaves the cursor frozen
        // in default zsh/bash, which is not the 2026 expectation. We always
        // blink when focused for v1; a config flag can opt out later.
        let _ = cursor_blinking;
        let blink_on = if self.focused {
            let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
            elapsed_ms % CURSOR_BLINK_PERIOD_MS < CURSOR_BLINK_PERIOD_MS / 2
        } else {
            true
        };
        let cursor_visible = !matches!(cursor_shape, CursorShapeKind::Hidden);
        if cursor_visible && blink_on {
            let cx = TEXT_LEFT + cursor_col as f32 * cell_advance;
            let cy_base =
                TEXT_TOP + (cursor_line.max(0) as f32) * LINE_HEIGHT + y_shift;
            let (rect, is_hollow) = match cursor_shape {
                CursorShapeKind::Block | CursorShapeKind::HollowBlock => (
                    [
                        cx - CURSOR_PAD_X,
                        cy_base - CURSOR_PAD_Y,
                        cell_advance + 2.0 * CURSOR_PAD_X,
                        LINE_HEIGHT + 2.0 * CURSOR_PAD_Y,
                    ],
                    matches!(cursor_shape, CursorShapeKind::HollowBlock),
                ),
                CursorShapeKind::Beam => (
                    [cx, cy_base, 2.0, LINE_HEIGHT],
                    false,
                ),
                CursorShapeKind::Underline => (
                    [
                        cx,
                        cy_base + LINE_HEIGHT - 2.0,
                        cell_advance,
                        2.0,
                    ],
                    false,
                ),
                CursorShapeKind::Hidden => ([0.0; 4], false),
            };
            if is_hollow {
                // Outline only: draw four 1-px edges of the block.
                let [x, y, w, h] = rect;
                let t = 1.5;
                below.push(RectInstance { rect: [x, y, w, t], color: CURSOR_COLOR });
                below.push(RectInstance { rect: [x, y + h - t, w, t], color: CURSOR_COLOR });
                below.push(RectInstance { rect: [x, y, t, h], color: CURSOR_COLOR });
                below.push(RectInstance { rect: [x + w - t, y, t, h], color: CURSOR_COLOR });
            } else {
                below.push(RectInstance { rect, color: CURSOR_COLOR });
            }
        }

        // Surface the next blink phase change as a deadline so the main
        // loop's WaitUntil wakes us — no per-frame thread spawn.
        self.next_blink_deadline = if self.focused {
            let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
            let half = CURSOR_BLINK_PERIOD_MS / 2;
            let into_half = elapsed_ms % half;
            let ms_to_next = (half - into_half).max(1);
            Some(Instant::now() + Duration::from_millis(ms_to_next))
        } else {
            None
        };

        let mut above: Vec<RectInstance> = Vec::with_capacity(deco_runs.len() * 2);
        for run in &deco_runs {
            let x = TEXT_LEFT + run.start_col as f32 * cell_advance;
            let w = run.width as f32 * cell_advance;
            let line_y = TEXT_TOP + run.line as f32 * LINE_HEIGHT + y_shift;
            let (y, h) = match run.kind {
                DecorationKind::Underline | DecorationKind::DoubleUnderline => (
                    line_y + LINE_HEIGHT - UNDERLINE_THICKNESS,
                    UNDERLINE_THICKNESS,
                ),
                DecorationKind::Strikeout => (
                    line_y + LINE_HEIGHT / 2.0 - STRIKEOUT_THICKNESS / 2.0,
                    STRIKEOUT_THICKNESS,
                ),
            };
            let color = color_to_floats(run.color);
            above.push(RectInstance {
                rect: [x, y, w, h],
                color,
            });
            if matches!(run.kind, DecorationKind::DoubleUnderline) {
                above.push(RectInstance {
                    rect: [x, y - DOUBLE_UNDERLINE_GAP, w, UNDERLINE_THICKNESS],
                    color,
                });
            }
        }

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
        let tab_bar_rects = self.build_tab_bar_rects();
        self.rects_below.prepare(&self.queue, &below, resolution);
        self.rects_above.prepare(&self.queue, &above, resolution);
        self.rects_tab_bar
            .prepare(&self.queue, &tab_bar_rects, resolution);

        // Clip text rendering to the viewport — keeps the extra row above the
        // viewport invisible when pixel_offset == 0, and only its bottom slides
        // into view as pixel_offset grows.
        let bounds = TextBounds {
            left: 0,
            top: TEXT_TOP as i32,
            right: self.surface_config.width as i32,
            bottom: self.surface_config.height as i32,
        };

        // text_runs starts with the extra row above the viewport (when
        // available), so the buffer's top sits one line up; the y_shift
        // slides it down into view as pixel_offset grows. When there's no
        // extra row, the buffer starts at the normal top.
        let text_top = if has_extra_row {
            TEXT_TOP - LINE_HEIGHT + y_shift
        } else {
            TEXT_TOP + y_shift
        };

        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                [TextArea {
                    buffer: &self.text_buffer,
                    left: TEXT_LEFT,
                    top: text_top,
                    scale: 1.0,
                    bounds,
                    default_color: Color::rgb(DEFAULT_FG.0, DEFAULT_FG.1, DEFAULT_FG.2),
                    custom_glyphs: &[],
                }],
                &mut self.swash_cache,
            )
            .expect("terminite: text prepare failed");

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

            // Scissor: same idea as TextBounds, but for the rect pipelines.
            // Without this, the extra-row backgrounds and decorations bleed
            // into the top padding area above the viewport.
            let scissor_y = TEXT_TOP as u32;
            let scissor_h = self
                .surface_config
                .height
                .saturating_sub(scissor_y);
            pass.set_scissor_rect(0, scissor_y, self.surface_config.width, scissor_h);

            self.rects_below.render(&mut pass);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("terminite: text render failed");
            self.rects_above.render(&mut pass);

            // Tab bar lives above the text area's scissor zone; widen the
            // scissor to the full surface so the bar isn't clipped out.
            pass.set_scissor_rect(
                0,
                0,
                self.surface_config.width,
                self.surface_config.height,
            );
            self.rects_tab_bar.render(&mut pass);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        surface_texture.present();
        self.atlas.trim();
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

fn compute_grid_size(
    physical_width: f32,
    physical_height: f32,
    cell_advance: f32,
) -> (usize, usize) {
    let available_w = (physical_width - TEXT_LEFT).max(cell_advance);
    let available_h = (physical_height - TEXT_TOP).max(LINE_HEIGHT);
    let cols = (available_w / cell_advance) as usize;
    let rows = (available_h / LINE_HEIGHT) as usize;
    (cols.max(2), rows.max(2))
}

fn measure_cell_advance(font_system: &mut FontSystem) -> f32 {
    let mut probe = Buffer::new(font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
    probe.set_size(font_system, Some(1000.0), Some(LINE_HEIGHT * 2.0));
    probe.set_text(
        font_system,
        "M",
        &Attrs::new().family(Family::Monospace),
        Shaping::Advanced,
        None,
    );
    probe.shape_until_scroll(font_system, false);
    probe
        .layout_runs()
        .next()
        .and_then(|run| run.glyphs.first())
        .map(|glyph| glyph.w)
        .unwrap_or(FONT_SIZE * 0.6)
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
