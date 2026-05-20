//! The Renderer: assembles backgrounds, decorations, text, the cursor, and
//! selection highlights into a single frame. Two `RectRenderer` instances
//! sandwich the text — one draws *below* (backgrounds + selection + cursor),
//! one draws *above* (decorations).

use std::sync::Arc;

use arboard::Clipboard;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, Style,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use winit::event::MouseScrollDelta;
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

use crate::palette::{color_to_floats, DEFAULT_FG};
use crate::rect::{RectInstance, RectRenderer};
use crate::term::{DecorationKind, LiveTerm, Snapshot, SpanStyle, TermScroll};
use crate::{UserEvent, BACKGROUND, FONT_SIZE, LINE_HEIGHT, TEXT_LEFT, TEXT_TOP};

const UNDERLINE_THICKNESS: f32 = 1.5;
const DOUBLE_UNDERLINE_GAP: f32 = 2.0;
const STRIKEOUT_THICKNESS: f32 = 1.5;

const CURSOR_PAD_X: f32 = 1.0;
const CURSOR_PAD_Y: f32 = 1.0;
const CURSOR_COLOR: [f32; 4] = [1.0, 200.0 / 255.0, 80.0 / 255.0, 180.0 / 255.0];

/// Translucent steel-blue selection highlight.
const SELECTION_COLOR: [f32; 4] = [0.32, 0.46, 0.75, 0.35];

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

    /// Only updates the head if the new point is at least as far from the
    /// anchor as the current head. A single drag stays additive — scrolling
    /// extends the selection in the scroll direction, and mid-drag mouse
    /// motion can extend further but never pulls the head back toward the
    /// anchor.
    fn extend_to_max(&mut self, line: i32, col: usize) {
        let new_line_d = (line - self.anchor_line).abs();
        let cur_line_d = (self.head_line - self.anchor_line).abs();
        if new_line_d > cur_line_d {
            self.head_line = line;
            self.head_col = col;
        } else if new_line_d == cur_line_d && line == self.head_line {
            // Same line as current head — extend the col only if it's
            // further from the anchor's col.
            let cur_col_d = (self.head_col as i32 - self.anchor_col as i32).abs();
            let new_col_d = (col as i32 - self.anchor_col as i32).abs();
            if new_col_d > cur_col_d {
                self.head_col = col;
            }
        }
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

    last_text_runs: Vec<(String, SpanStyle)>,
    cell_advance: f32,
    grid_cols: usize,
    grid_rows: usize,

    // Mouse / selection state.
    mouse_pos: (f32, f32),
    /// Pixel position at which the selection head was last updated via mouse
    /// motion. Used to filter sub-cell trackpad jitter that would otherwise
    /// clobber the wheel-driven edge extension during a drag-scroll.
    last_drag_mouse_pos: (f32, f32),
    selection: Option<Selection>,
    dragging: bool,
    clipboard: Option<Clipboard>,
    /// Sub-line pixel offset for smooth scrolling. Always in [0, LINE_HEIGHT).
    /// Whole lines are popped into `display_offset` as soon as the
    /// accumulator crosses a line; the remainder is rendered as a vertical
    /// shift so the viewport slides instead of snapping.
    pixel_offset: f32,

    pub live_term: LiveTerm,

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
        let live_term = LiveTerm::new(cols, rows, cell_advance, proxy);

        // Clipboard is optional; it's possible the platform refuses to give us
        // one (sandboxing, missing service). Copy/paste then become no-ops.
        let clipboard = Clipboard::new().ok();

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
            last_text_runs: Vec::new(),
            cell_advance,
            grid_cols: cols,
            grid_rows: rows,
            mouse_pos: (0.0, 0.0),
            last_drag_mouse_pos: (0.0, 0.0),
            selection: None,
            dragging: false,
            clipboard,
            pixel_offset: 0.0,
            live_term,
            window,
        }
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
        self.live_term.resize(cols, rows);
        self.grid_cols = cols;
        self.grid_rows = rows;
        // A resize invalidates the snapshot cache *and* the selection — the
        // cells the user was selecting may now be in a different place.
        self.last_text_runs.clear();
        self.selection = None;
    }

    // ── Mouse / keyboard input routing ────────────────────────────────────

    fn pixel_to_cell(&self, px: f32, py: f32) -> (usize, usize) {
        let cx = (px - TEXT_LEFT).max(0.0);
        let cy = (py - TEXT_TOP).max(0.0);
        let col = (cx / self.cell_advance) as usize;
        let line = (cy / LINE_HEIGHT) as usize;
        (
            line.min(self.grid_rows.saturating_sub(1)),
            col.min(self.grid_cols.saturating_sub(1)),
        )
    }

    /// Convert a mouse pixel position into an absolute (Line, Column) using
    /// the current display_offset. Used for both selection start and extend.
    fn pixel_to_absolute(&self, x: f32, y: f32) -> (i32, usize) {
        let (vl, col) = self.pixel_to_cell(x, y);
        let display_offset = self.live_term.offset_and_history().0 as i32;
        (vl as i32 - display_offset, col)
    }

    pub fn mouse_moved(&mut self, x: f32, y: f32) {
        self.mouse_pos = (x, y);
        if self.dragging {
            // macOS trackpad scrolling drags the cursor a hair, so we get
            // tiny mouse_moved events interleaved with wheel events. Without
            // this filter, every wheel-driven extension to the viewport
            // edge gets immediately snapped back to whatever cell the
            // cursor is currently over. Only count motion that crosses
            // half a cell from the last update.
            let (last_x, last_y) = self.last_drag_mouse_pos;
            let dx = (x - last_x).abs();
            let dy = (y - last_y).abs();
            if dx < self.cell_advance * 0.5 && dy < LINE_HEIGHT * 0.5 {
                return;
            }
            let (line, col) = self.pixel_to_absolute(x, y);
            if let Some(sel) = self.selection.as_mut() {
                sel.extend_to_max(line, col);
            }
            self.last_drag_mouse_pos = (x, y);
            self.window.request_redraw();
        }
    }

    pub fn mouse_down(&mut self) {
        let (line, col) = self.pixel_to_absolute(self.mouse_pos.0, self.mouse_pos.1);
        self.selection = Some(Selection::from_anchor(line, col));
        self.dragging = true;
        self.last_drag_mouse_pos = self.mouse_pos;
        self.window.request_redraw();
    }

    pub fn mouse_up(&mut self) {
        self.dragging = false;
        if let Some(sel) = self.selection.as_ref() {
            if sel.is_empty() {
                self.selection = None;
            } else {
                self.copy_selection();
            }
        }
        self.window.request_redraw();
    }

    pub fn mouse_wheel(&mut self, delta: MouseScrollDelta) {
        // Work in physical pixels so the renderer can shift by the remainder
        // for pixel-smooth scrolling. LineDelta is real-wheel "clicks" (~3
        // lines each, scaled to pixels); PixelDelta is trackpad pixels.
        let pixels = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 3.0 * LINE_HEIGHT,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        self.pixel_offset += pixels;

        // Pop whole lines into the term; the remainder stays as a sub-line
        // pixel shift used at render time. `floor` keeps the remainder in
        // [0, LINE_HEIGHT) for any input direction — but only when the
        // requested scroll actually happens. If alacritty clamps (we asked
        // Delta(-2) but were at offset=1), subtracting the full `whole`
        // leaves a residual that renders as motion in the wrong direction,
        // and floor's over-pop re-establishes the residual on every event
        // — so the bottom (offset=0) is never reached cleanly. Subtract by
        // the *actual* offset delta instead.
        let whole = (self.pixel_offset / LINE_HEIGHT).floor() as i32;
        if whole != 0 {
            let (before, _) = self.live_term.offset_and_history();
            self.live_term.scroll(TermScroll::Delta(whole));
            let (after, history) = self.live_term.offset_and_history();
            let actual = after as i32 - before as i32;
            self.pixel_offset -= actual as f32 * LINE_HEIGHT;
            if actual != whole {
                // Clamped at a boundary; drop the residual.
                self.pixel_offset = 0.0;
                let at_top = whole > 0 && after >= history;
                let at_live = whole < 0 && after == 0;
                if at_top {
                    eprintln!(
                        "[scroll] hit top boundary: offset={} history={} (rows={}) topRow='{}'",
                        after,
                        history,
                        self.grid_rows,
                        self.live_term.debug_top_row()
                    );
                } else if at_live {
                    eprintln!(
                        "[scroll] hit live boundary: offset={} history={} (rows={}) {}",
                        after,
                        history,
                        self.grid_rows,
                        self.live_term.debug_bottom_strip(self.grid_rows)
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
            if actual != 0 && self.dragging {
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
                if let Some(sel) = self.selection.as_mut() {
                    // Extend by the further of the viewport edge or the
                    // current mouse position; `extend_to_max` keeps the
                    // selection additive across the whole drag.
                    sel.extend_to_max(edge.0, edge.1);
                    sel.extend_to_max(mouse_line, mouse_col);
                }
            }
        }

        self.window.request_redraw();
    }

    pub fn scroll_page(&self, up: bool) {
        let s = if up { TermScroll::PageUp } else { TermScroll::PageDown };
        self.live_term.scroll(s);
        self.window.request_redraw();
    }

    pub fn copy_selection(&mut self) {
        let Some(sel) = self.selection.as_ref() else { return };
        if sel.is_empty() {
            return;
        }
        let (start, end) = sel.normalized();
        let text = self.live_term.extract_text(start, end);
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
        self.live_term.write(text.into_bytes());
    }

    // ── Frame ────────────────────────────────────────────────────────────

    pub fn render(&mut self) {
        let Snapshot {
            text_runs,
            bg_runs,
            deco_runs,
            cursor_line,
            cursor_col,
            has_extra_row,
        } = self.live_term.snapshot();

        if text_runs != self.last_text_runs {
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
            self.last_text_runs = text_runs;
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
        let y_shift = self.pixel_offset;
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
        if let Some(sel) = self.selection.as_ref() {
            if !sel.is_empty() {
                let ((s_line, s_col), (e_line, e_col)) = sel.normalized();
                let cols = self.grid_cols;
                let rows = self.grid_rows as i32;
                let display_offset = self.live_term.offset_and_history().0 as i32;
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
        below.push(RectInstance {
            rect: [
                TEXT_LEFT + cursor_col as f32 * cell_advance - CURSOR_PAD_X,
                TEXT_TOP + (cursor_line.max(0) as f32) * LINE_HEIGHT + y_shift - CURSOR_PAD_Y,
                cell_advance + 2.0 * CURSOR_PAD_X,
                LINE_HEIGHT + 2.0 * CURSOR_PAD_Y,
            ],
            color: CURSOR_COLOR,
        });

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

        let resolution = [
            self.surface_config.width as f32,
            self.surface_config.height as f32,
        ];
        self.rects_below.prepare(&self.queue, &below, resolution);
        self.rects_above.prepare(&self.queue, &above, resolution);

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
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        surface_texture.present();
        self.atlas.trim();
    }
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
