//! terminite — a terminal emulator for the human-AI pair.
//!
//! Foreground colors now flow from the cell grid into glyphon as rich-text
//! spans. Backgrounds and styles (bold/italic/underline) follow when the rect
//! renderer arrives — they need pixel boxes, not glyph runs.

use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as TermEventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

/// terminite's resting background — deep, quiet, not pure black.
const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.04,
    g: 0.04,
    b: 0.06,
    a: 1.0,
};

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;

/// Padding from the window edge to the text.
const TEXT_LEFT: f32 = 24.0;
const TEXT_TOP: f32 = 24.0;

/// The cursor block is rendered at a slightly larger font than the text so it
/// wraps the letter with breathing room above and below — the M (or any
/// character) sits centered inside the cursor instead of beside it.
const CURSOR_FONT_SIZE: f32 = FONT_SIZE + 4.0;
const CURSOR_LINE_HEIGHT: f32 = LINE_HEIGHT + 4.0;

/// Half of the cursor's extra height — the amount we lift the cursor up to
/// center it on the text. Derived from the font sizes so changing them keeps
/// the geometry honest.
const CURSOR_VERTICAL_PADDING: f32 = (CURSOR_FONT_SIZE - FONT_SIZE) / 2.0;

/// Tiny visual nudges so the cursor sits where the eye expects, not where the
/// cell math says. The Y offset lifts the cursor by its vertical padding (to
/// center it on the text) plus one more pixel for taste — that last 1.0 is
/// the irreducible taste portion. Geometric correctness ≠ visual correctness.
const CURSOR_X_OFFSET: f32 = 2.0;
const CURSOR_Y_OFFSET: f32 = -CURSOR_VERTICAL_PADDING - 1.0;

/// Sixteen-color palette tuned in the One Dark family. Indices 0–7 are the
/// base ANSI colors, 8–15 the bright variants. The 256-color cube and the
/// grayscale ramp are computed from the standard xterm levels.
const PALETTE_16: [(u8, u8, u8); 16] = [
    (40, 44, 52),    //  0  black
    (224, 108, 117), //  1  red
    (152, 195, 121), //  2  green
    (229, 192, 123), //  3  yellow
    (97, 175, 239),  //  4  blue
    (198, 120, 221), //  5  magenta
    (86, 182, 194),  //  6  cyan
    (171, 178, 191), //  7  white
    (92, 99, 112),   //  8  bright black
    (224, 108, 117), //  9  bright red
    (152, 195, 121), // 10  bright green
    (229, 192, 123), // 11  bright yellow
    (97, 175, 239),  // 12  bright blue
    (198, 120, 221), // 13  bright magenta
    (86, 182, 194),  // 14  bright cyan
    (220, 223, 228), // 15  bright white
];

const DEFAULT_FG: (u8, u8, u8) = (220, 220, 220);

/// Map a vte ANSI color into a glyphon Color through terminite's palette.
fn resolve_color(color: AnsiColor) -> Color {
    let (r, g, b) = match color {
        AnsiColor::Spec(rgb) => (rgb.r, rgb.g, rgb.b),
        AnsiColor::Named(name) => named_rgb(name),
        AnsiColor::Indexed(idx) => indexed_rgb(idx),
    };
    Color::rgb(r, g, b)
}

fn named_rgb(name: NamedColor) -> (u8, u8, u8) {
    match name {
        NamedColor::Black => PALETTE_16[0],
        NamedColor::Red => PALETTE_16[1],
        NamedColor::Green => PALETTE_16[2],
        NamedColor::Yellow => PALETTE_16[3],
        NamedColor::Blue => PALETTE_16[4],
        NamedColor::Magenta => PALETTE_16[5],
        NamedColor::Cyan => PALETTE_16[6],
        NamedColor::White => PALETTE_16[7],
        NamedColor::BrightBlack => PALETTE_16[8],
        NamedColor::BrightRed => PALETTE_16[9],
        NamedColor::BrightGreen => PALETTE_16[10],
        NamedColor::BrightYellow => PALETTE_16[11],
        NamedColor::BrightBlue => PALETTE_16[12],
        NamedColor::BrightMagenta => PALETTE_16[13],
        NamedColor::BrightCyan => PALETTE_16[14],
        NamedColor::BrightWhite => PALETTE_16[15],
        NamedColor::Background => (10, 10, 15),
        NamedColor::Cursor => (255, 200, 80),
        // Foreground, dim variants, bright/dim foreground, and anything new.
        _ => DEFAULT_FG,
    }
}

fn indexed_rgb(idx: u8) -> (u8, u8, u8) {
    if (idx as usize) < PALETTE_16.len() {
        return PALETTE_16[idx as usize];
    }
    if idx < 232 {
        // 6×6×6 RGB cube starting at 16.
        let levels: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let i = (idx - 16) as usize;
        return (levels[i / 36], levels[(i / 6) % 6], levels[i % 6]);
    }
    // Grayscale ramp 232..=255.
    let level = 8 + (idx - 232) * 10;
    (level, level, level)
}

/// Compute how many columns and rows of monospace cells fit in a surface of
/// the given physical size, accounting for terminite's padding.
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

/// Measure the actual monospace cell advance by shaping a single character and
/// reading its laid-out width. Replaces the old `font_size × 0.6` guess, which
/// drifted across a line on the font cosmic-text actually picks.
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

/// Grid dimensions for `Term::new`. `alacritty_terminal` asks for any
/// `Dimensions` implementor; one of our own keeps the rows/cols intent clear.
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// A no-op listener for terminal events. In a later slice this becomes the
/// bridge that wakes the render thread on `Event::Wakeup` and dispatches
/// title / bell / exit. For now we redraw every frame and read the grid fresh.
#[derive(Clone)]
struct Notifier;

impl EventListener for Notifier {
    fn send_event(&self, _event: TermEvent) {}
}

/// The live terminal: the shared `Term`, the I/O thread driving its PTY, and
/// the channel used to push bytes back into the shell.
struct LiveTerm {
    term: Arc<FairMutex<Term<Notifier>>>,
    sender: EventLoopSender,
    cell_advance: f32,
}

impl LiveTerm {
    fn new(cols: usize, rows: usize, cell_advance: f32) -> Self {
        let size = GridSize { cols, rows };
        let term = Term::new(TermConfig::default(), &size, Notifier);
        let term = Arc::new(FairMutex::new(term));

        let window_size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: cell_advance as u16,
            cell_height: LINE_HEIGHT as u16,
        };
        let pty = tty::new(&tty::Options::default(), window_size, 0)
            .expect("terminite: failed to open the PTY");

        let event_loop = TermEventLoop::new(term.clone(), Notifier, pty, false, false)
            .expect("terminite: failed to start the PTY event loop");
        let sender = event_loop.channel();
        // Detach the I/O thread; we keep the channel sender to drive input,
        // resize, and (eventually) shutdown.
        let _ = event_loop.spawn();

        Self {
            term,
            sender,
            cell_advance,
        }
    }

    /// Resize the underlying `Term` and notify the PTY.
    fn resize(&self, cols: usize, rows: usize) {
        {
            let mut term = self.term.lock();
            term.resize(GridSize { cols, rows });
        }
        let _ = self.sender.send(Msg::Resize(WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: self.cell_advance as u16,
            cell_height: LINE_HEIGHT as u16,
        }));
    }

    /// Send bytes to the shell over the PTY.
    fn write(&self, bytes: Vec<u8>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    /// Snapshot the visible grid as a list of color runs, plus the cursor
    /// position. Adjacent cells with the same foreground color are merged
    /// into one run so glyphon shapes them as a single span.
    fn snapshot(&self) -> (Vec<(String, Color)>, i32, usize) {
        let term = self.term.lock();
        let grid = term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        let cursor_line = grid.cursor.point.line.0;
        let cursor_col = grid.cursor.point.column.0;

        let default_color = resolve_color(AnsiColor::Named(NamedColor::Foreground));
        let mut runs: Vec<(String, Color)> = Vec::new();
        let mut current_color = default_color;
        let mut current_text = String::new();

        for line in 0..rows {
            let row = &grid[Line(line as i32)];
            // Trim trailing default-fg spaces so glyphon doesn't shape miles of
            // invisible cells. A non-default-fg space (a colored block) counts
            // as content and is kept.
            let mut last_content = 0;
            for col in (0..cols).rev() {
                let cell = &row[Column(col)];
                let is_blank = cell.c == ' '
                    && matches!(cell.fg, AnsiColor::Named(NamedColor::Foreground));
                if !is_blank {
                    last_content = col + 1;
                    break;
                }
            }
            for col in 0..last_content {
                let cell = &row[Column(col)];
                let color = resolve_color(cell.fg);
                if !current_text.is_empty() && color != current_color {
                    runs.push((std::mem::take(&mut current_text), current_color));
                }
                if current_text.is_empty() {
                    current_color = color;
                }
                current_text.push(cell.c);
            }
            current_text.push('\n');
        }
        if !current_text.is_empty() {
            runs.push((current_text, current_color));
        }

        (runs, cursor_line, cursor_col)
    }
}

/// Translate a winit key press into the bytes a shell expects on stdin.
fn key_to_bytes(event: &KeyEvent, modifiers: ModifiersState) -> Option<Vec<u8>> {
    if event.state != ElementState::Pressed {
        return None;
    }
    // Ctrl + letter — translate to the corresponding control byte (Ctrl-C = 3,
    // Ctrl-D = 4, …). Driven by the logical key so keyboard layout is honored.
    if modifiers.control_key() {
        if let Key::Character(text) = &event.logical_key {
            let mut chars = text.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                let lower = c.to_ascii_lowercase();
                if lower.is_ascii_lowercase() {
                    return Some(vec![(lower as u8) & 0x1f]);
                }
            }
        }
    }
    match &event.logical_key {
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(b"\x7f".to_vec()),
        Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
        Key::Named(NamedKey::Escape) => Some(b"\x1b".to_vec()),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
        Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
        _ => event.text.as_ref().map(|s| s.as_bytes().to_vec()),
    }
}

/// Everything needed to put pixels on the window.
struct Renderer {
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
    cursor_buffer: Buffer,
    /// Cached last-rendered grid snapshot; lets us skip re-shaping when the
    /// terminal hasn't changed between frames.
    last_snapshot: Vec<(String, Color)>,
    /// Measured monospace cell advance in physical pixels.
    cell_advance: f32,

    live_term: LiveTerm,

    // Window last: winit/wgpu want it dropped after the surface.
    window: Arc<Window>,
}

impl Renderer {
    async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);
        let scale_factor = window.scale_factor();

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
        // Measure the real cell advance from the font cosmic-text picks. Every
        // bit of cell/cursor math downstream uses this number.
        let cell_advance = measure_cell_advance(&mut font_system);

        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        let physical_width = (width as f64 * scale_factor) as f32;
        let physical_height = (height as f64 * scale_factor) as f32;

        let mut text_buffer = Buffer::new(&mut font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        text_buffer.set_size(&mut font_system, Some(physical_width), Some(physical_height));
        text_buffer.set_text(
            &mut font_system,
            "",
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        text_buffer.shape_until_scroll(&mut font_system, false);

        // Cursor: a separate buffer holding a single filled block at a slightly
        // larger font size so the cursor wraps the character it's on top of.
        let mut cursor_buffer = Buffer::new(
            &mut font_system,
            Metrics::new(CURSOR_FONT_SIZE, CURSOR_LINE_HEIGHT),
        );
        cursor_buffer.set_size(
            &mut font_system,
            Some(CURSOR_FONT_SIZE * 2.0),
            Some(CURSOR_LINE_HEIGHT * 2.0),
        );
        cursor_buffer.set_text(
            &mut font_system,
            "█",
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        cursor_buffer.shape_until_scroll(&mut font_system, false);

        // The grid sized for this window, using the measured advance.
        let (cols, rows) = compute_grid_size(physical_width, physical_height, cell_advance);
        let live_term = LiveTerm::new(cols, rows, cell_advance);

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
            cursor_buffer,
            last_snapshot: Vec::new(),
            cell_advance,
            live_term,
            window,
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        let scale = self.window.scale_factor();
        let physical_width = (width as f64 * scale) as f32;
        let physical_height = (height as f64 * scale) as f32;
        self.text_buffer.set_size(
            &mut self.font_system,
            Some(physical_width),
            Some(physical_height),
        );
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);

        // Recompute grid dimensions and push them through to Term + PTY.
        let (cols, rows) = compute_grid_size(physical_width, physical_height, self.cell_advance);
        self.live_term.resize(cols, rows);
        // Invalidate the snapshot cache so the next frame re-shapes the buffer
        // at the new size.
        self.last_snapshot.clear();
    }

    fn render(&mut self) {
        let (snapshot, cursor_line, cursor_col) = self.live_term.snapshot();
        if snapshot != self.last_snapshot {
            let default_attrs = Attrs::new().family(Family::Monospace);
            self.text_buffer.set_rich_text(
                &mut self.font_system,
                snapshot
                    .iter()
                    .map(|(text, color)| (text.as_str(), default_attrs.clone().color(*color))),
                &default_attrs,
                Shaping::Advanced,
                None,
            );
            self.text_buffer
                .shape_until_scroll(&mut self.font_system, false);
            self.last_snapshot = snapshot;
        }

        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        let cursor_left = TEXT_LEFT + (cursor_col as f32) * self.cell_advance + CURSOR_X_OFFSET;
        let cursor_top =
            TEXT_TOP + (cursor_line.max(0) as f32) * LINE_HEIGHT + CURSOR_Y_OFFSET;
        let bounds = TextBounds {
            left: 0,
            top: 0,
            right: self.surface_config.width as i32,
            bottom: self.surface_config.height as i32,
        };

        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                [
                    TextArea {
                        buffer: &self.text_buffer,
                        left: TEXT_LEFT,
                        top: TEXT_TOP,
                        scale: 1.0,
                        bounds,
                        default_color: Color::rgb(DEFAULT_FG.0, DEFAULT_FG.1, DEFAULT_FG.2),
                        custom_glyphs: &[],
                    },
                    TextArea {
                        buffer: &self.cursor_buffer,
                        left: cursor_left,
                        top: cursor_top,
                        scale: 1.0,
                        bounds,
                        default_color: Color::rgba(255, 200, 80, 180),
                        custom_glyphs: &[],
                    },
                ],
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

            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("terminite: text render failed");
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        surface_texture.present();
        self.atlas.trim();
    }
}

/// The terminite application.
#[derive(Default)]
struct Terminite {
    renderer: Option<Renderer>,
    modifiers: ModifiersState,
}

impl ApplicationHandler for Terminite {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("terminite")
            .with_inner_size(LogicalSize::new(900.0, 600.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("terminite: failed to create the window"),
        );
        let renderer = pollster::block_on(Renderer::new(window.clone()));
        self.renderer = Some(renderer);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(bytes) = key_to_bytes(&event, self.modifiers) {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.live_term.write(bytes);
                    }
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.render();
                    renderer.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("terminite: failed to start the event loop");
    let mut terminite = Terminite::default();
    event_loop
        .run_app(&mut terminite)
        .expect("terminite: the event loop exited with an error");
}
