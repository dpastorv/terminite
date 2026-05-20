//! terminite — a terminal emulator for the human-AI pair.
//!
//! Rect-renderer pass: a small wgpu pipeline draws filled rectangles for cell
//! backgrounds, which lights up everything the glyph layer couldn't do alone —
//! background colors, inverse cells, and (next) selection highlights, status
//! bars, and a properly-sized cursor.

use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as TermEventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};
use bytemuck::{Pod, Zeroable};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, Style,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

/// terminite's resting background — deep, quiet, not pure black.
const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.04,
    g: 0.04,
    b: 0.06,
    a: 1.0,
};
const BACKGROUND_RGB: (u8, u8, u8) = (10, 10, 15);

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;

/// Padding from the window edge to the text.
const TEXT_LEFT: f32 = 24.0;
const TEXT_TOP: f32 = 24.0;

const CURSOR_FONT_SIZE: f32 = FONT_SIZE + 4.0;
const CURSOR_LINE_HEIGHT: f32 = LINE_HEIGHT + 4.0;
const CURSOR_VERTICAL_PADDING: f32 = (CURSOR_FONT_SIZE - FONT_SIZE) / 2.0;
const CURSOR_X_OFFSET: f32 = 2.0;
const CURSOR_Y_OFFSET: f32 = -CURSOR_VERTICAL_PADDING - 1.0;

/// Sixteen-color palette tuned in the One Dark family.
const PALETTE_16: [(u8, u8, u8); 16] = [
    (40, 44, 52),
    (224, 108, 117),
    (152, 195, 121),
    (229, 192, 123),
    (97, 175, 239),
    (198, 120, 221),
    (86, 182, 194),
    (171, 178, 191),
    (92, 99, 112),
    (224, 108, 117),
    (152, 195, 121),
    (229, 192, 123),
    (97, 175, 239),
    (198, 120, 221),
    (86, 182, 194),
    (220, 223, 228),
];

const DEFAULT_FG: (u8, u8, u8) = (220, 220, 220);

const fn half(rgb: (u8, u8, u8)) -> (u8, u8, u8) {
    (rgb.0 / 2, rgb.1 / 2, rgb.2 / 2)
}

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
        NamedColor::DimBlack => half(PALETTE_16[0]),
        NamedColor::DimRed => half(PALETTE_16[1]),
        NamedColor::DimGreen => half(PALETTE_16[2]),
        NamedColor::DimYellow => half(PALETTE_16[3]),
        NamedColor::DimBlue => half(PALETTE_16[4]),
        NamedColor::DimMagenta => half(PALETTE_16[5]),
        NamedColor::DimCyan => half(PALETTE_16[6]),
        NamedColor::DimWhite => half(PALETTE_16[7]),
        NamedColor::DimForeground => half(DEFAULT_FG),
        NamedColor::BrightForeground => (255, 255, 255),
        NamedColor::Background => BACKGROUND_RGB,
        NamedColor::Cursor => (255, 200, 80),
        _ => DEFAULT_FG,
    }
}

fn indexed_rgb(idx: u8) -> (u8, u8, u8) {
    if (idx as usize) < PALETTE_16.len() {
        return PALETTE_16[idx as usize];
    }
    if idx < 232 {
        let levels: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let i = (idx - 16) as usize;
        return (levels[i / 36], levels[(i / 6) % 6], levels[i % 6]);
    }
    let level = 8 + (idx - 232) * 10;
    (level, level, level)
}

fn dim_color(c: Color) -> Color {
    Color::rgba(c.r() / 2, c.g() / 2, c.b() / 2, c.a())
}

fn color_to_floats(c: Color) -> [f32; 4] {
    [
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
        c.a() as f32 / 255.0,
    ]
}

/// Visual style of a contiguous text run. Two cells join the same run only
/// when every field matches.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SpanStyle {
    color: Color,
    bold: bool,
    italic: bool,
}

/// A horizontal run of cells sharing a background color, in cell coordinates.
#[derive(Clone, Copy, PartialEq)]
struct BgRun {
    line: usize,
    start_col: usize,
    width: usize,
    color: Color,
}

// ─── Rect renderer ──────────────────────────────────────────────────────────

/// Per-rectangle instance data uploaded to the GPU.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct RectInstance {
    rect: [f32; 4],  // x, y, w, h in physical pixels
    color: [f32; 4], // rgba in [0, 1]
}

/// Uniforms shared by every rectangle in a frame.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct RectUniforms {
    resolution: [f32; 2],
    _pad: [f32; 2],
}

const RECT_WGSL: &str = r#"
struct Uniforms {
    resolution: vec2<f32>,
    _pad: vec2<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
) -> VertexOutput {
    let corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[vid];
    let px = rect.x + c.x * rect.z;
    let py = rect.y + c.y * rect.w;
    let ndc_x = px / uniforms.resolution.x * 2.0 - 1.0;
    let ndc_y = 1.0 - py / uniforms.resolution.y * 2.0;
    var out: VertexOutput;
    out.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

/// A small wgpu pipeline that draws an instanced batch of solid colored
/// rectangles in physical-pixel coordinates. The vertex shader generates a
/// quad on the fly from `@builtin(vertex_index)` so no vertex buffer is needed
/// beyond the per-instance data.
struct RectRenderer {
    pipeline: wgpu::RenderPipeline,
    instance_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    capacity: u32,
    count: u32,
}

impl RectRenderer {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terminite rect shader"),
            source: wgpu::ShaderSource::Wgsl(RECT_WGSL.into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("terminite rect bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terminite rect uniforms"),
            size: std::mem::size_of::<RectUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terminite rect bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terminite rect pl"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let capacity: u32 = 4096;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terminite rect instances"),
            size: capacity as u64 * std::mem::size_of::<RectInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terminite rect pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<RectInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                        wgpu::VertexAttribute {
                            offset: 16,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            instance_buffer,
            uniform_buffer,
            bind_group,
            capacity,
            count: 0,
        }
    }

    fn prepare(&mut self, queue: &wgpu::Queue, rects: &[RectInstance], resolution: [f32; 2]) {
        let count = rects.len().min(self.capacity as usize);
        self.count = count as u32;
        if count > 0 {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&rects[..count]),
            );
        }
        let uniforms = RectUniforms {
            resolution,
            _pad: [0.0, 0.0],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..4, 0..self.count);
    }
}

// ─── Grid / terminal ────────────────────────────────────────────────────────

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

/// Cross-thread events that wake terminite's render loop. The terminal's I/O
/// thread sends `Wakeup` whenever the shell produces output that needs to be
/// drawn; the winit loop responds by requesting one redraw.
#[derive(Debug)]
enum UserEvent {
    Wakeup,
}

/// Bridge between the PTY thread and winit's event loop. Holding an
/// `EventLoopProxy` lets us request redraws from off-thread without polling.
#[derive(Clone)]
struct Notifier {
    proxy: EventLoopProxy<UserEvent>,
}

impl EventListener for Notifier {
    fn send_event(&self, _event: TermEvent) {
        let _ = self.proxy.send_event(UserEvent::Wakeup);
    }
}

struct LiveTerm {
    term: Arc<FairMutex<Term<Notifier>>>,
    sender: EventLoopSender,
    cell_advance: f32,
}

impl LiveTerm {
    fn new(
        cols: usize,
        rows: usize,
        cell_advance: f32,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        let notifier = Notifier { proxy };
        let size = GridSize { cols, rows };
        let term = Term::new(TermConfig::default(), &size, notifier.clone());
        let term = Arc::new(FairMutex::new(term));

        let window_size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: cell_advance as u16,
            cell_height: LINE_HEIGHT as u16,
        };

        let mut tty_options = tty::Options::default();
        tty_options
            .env
            .insert("TERM".to_string(), "xterm-256color".to_string());
        tty_options
            .env
            .insert("COLORTERM".to_string(), "truecolor".to_string());

        let pty = tty::new(&tty_options, window_size, 0)
            .expect("terminite: failed to open the PTY");

        let event_loop = TermEventLoop::new(term.clone(), notifier, pty, false, false)
            .expect("terminite: failed to start the PTY event loop");
        let sender = event_loop.channel();
        let _ = event_loop.spawn();

        Self {
            term,
            sender,
            cell_advance,
        }
    }

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

    fn write(&self, bytes: Vec<u8>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    /// Snapshot the visible grid: styled text runs, background runs, cursor.
    /// One lock per frame.
    fn snapshot(&self) -> (Vec<(String, SpanStyle)>, Vec<BgRun>, i32, usize) {
        let term = self.term.lock();
        let grid = term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        let cursor_line = grid.cursor.point.line.0;
        let cursor_col = grid.cursor.point.column.0;

        let mut text_runs: Vec<(String, SpanStyle)> = Vec::new();
        let mut bg_runs: Vec<BgRun> = Vec::new();
        let mut current_style = SpanStyle {
            color: Color::rgb(DEFAULT_FG.0, DEFAULT_FG.1, DEFAULT_FG.2),
            bold: false,
            italic: false,
        };
        let mut current_text = String::new();

        for line in 0..rows {
            let row = &grid[Line(line as i32)];

            // Trim trailing default-fg, default-bg, no-flag cells.
            let mut last_content = 0;
            for col in (0..cols).rev() {
                let cell = &row[Column(col)];
                let plain = cell.c == ' '
                    && matches!(cell.fg, AnsiColor::Named(NamedColor::Foreground))
                    && matches!(cell.bg, AnsiColor::Named(NamedColor::Background))
                    && cell.flags.is_empty();
                if !plain {
                    last_content = col + 1;
                    break;
                }
            }

            // Background runs walk *all* cells in this row so wide-char spacers
            // contribute to the bg under an emoji or CJK glyph. Track an open run.
            let mut bg_open: Option<(usize, Color)> = None; // (start_col, color)
            let flush_bg = |bg_runs: &mut Vec<BgRun>, open: &mut Option<(usize, Color)>, end: usize| {
                if let Some((start, color)) = open.take() {
                    bg_runs.push(BgRun {
                        line,
                        start_col: start,
                        width: end - start,
                        color,
                    });
                }
            };

            for col in 0..cols {
                let cell = &row[Column(col)];

                // Background side: every cell contributes.
                let inverse = cell.flags.contains(Flags::INVERSE);
                let bg_ansi = if inverse { cell.fg } else { cell.bg };
                let bg_color_opt = match bg_ansi {
                    AnsiColor::Named(NamedColor::Background) => None,
                    other => Some(resolve_color(other)),
                };
                match (bg_open, bg_color_opt) {
                    (Some((_, prev)), Some(new)) if prev == new => {
                        // Continue the current run.
                    }
                    _ => {
                        flush_bg(&mut bg_runs, &mut bg_open, col);
                        if let Some(c) = bg_color_opt {
                            bg_open = Some((col, c));
                        }
                    }
                }

                // Text side: skip spacers, and stop accumulating past last_content.
                if col >= last_content {
                    continue;
                }
                let is_spacer = cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
                if is_spacer {
                    continue;
                }

                let style = cell_style(cell);
                if !current_text.is_empty() && style != current_style {
                    text_runs.push((std::mem::take(&mut current_text), current_style));
                }
                if current_text.is_empty() {
                    current_style = style;
                }
                current_text.push(cell.c);
                if let Some(zw) = cell.zerowidth() {
                    for ch in zw {
                        current_text.push(*ch);
                    }
                }
            }
            // Flush any open bg run at end-of-row.
            flush_bg(&mut bg_runs, &mut bg_open, cols);
            current_text.push('\n');
        }
        if !current_text.is_empty() {
            text_runs.push((current_text, current_style));
        }

        (text_runs, bg_runs, cursor_line, cursor_col)
    }
}

/// Translate a cell into its text visual style, honoring inverse, dim, hidden.
fn cell_style(cell: &Cell) -> SpanStyle {
    let inverse = cell.flags.contains(Flags::INVERSE);
    let fg_ansi = if inverse { cell.bg } else { cell.fg };
    let mut color = resolve_color(fg_ansi);
    if cell.flags.contains(Flags::DIM) {
        color = dim_color(color);
    }
    if cell.flags.contains(Flags::HIDDEN) {
        color = Color::rgb(BACKGROUND_RGB.0, BACKGROUND_RGB.1, BACKGROUND_RGB.2);
    }
    SpanStyle {
        color,
        bold: cell.flags.contains(Flags::BOLD),
        italic: cell.flags.contains(Flags::ITALIC),
    }
}

fn key_to_bytes(event: &KeyEvent, modifiers: ModifiersState) -> Option<Vec<u8>> {
    if event.state != ElementState::Pressed {
        return None;
    }
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

// ─── Renderer ───────────────────────────────────────────────────────────────

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
    rect_renderer: RectRenderer,
    last_snapshot: Vec<(String, SpanStyle)>,
    cell_advance: f32,

    live_term: LiveTerm,

    window: Arc<Window>,
}

impl Renderer {
    async fn new(window: Arc<Window>, proxy: EventLoopProxy<UserEvent>) -> Self {
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
        let cell_advance = measure_cell_advance(&mut font_system);

        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let rect_renderer = RectRenderer::new(&device, format);

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

        let (cols, rows) = compute_grid_size(physical_width, physical_height, cell_advance);
        let live_term = LiveTerm::new(cols, rows, cell_advance, proxy);

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
            rect_renderer,
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

        let (cols, rows) = compute_grid_size(physical_width, physical_height, self.cell_advance);
        self.live_term.resize(cols, rows);
        self.last_snapshot.clear();
    }

    fn render(&mut self) {
        let (snapshot, bg_runs, cursor_line, cursor_col) = self.live_term.snapshot();
        if snapshot != self.last_snapshot {
            let default_attrs = Attrs::new().family(Family::Monospace);
            self.text_buffer.set_rich_text(
                &mut self.font_system,
                snapshot.iter().map(|(text, style)| {
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
            self.last_snapshot = snapshot;
        }

        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        // Convert bg_runs (cell coords) into pixel-space RectInstances.
        let cell_advance = self.cell_advance;
        let rect_instances: Vec<RectInstance> = bg_runs
            .iter()
            .map(|run| {
                let x = TEXT_LEFT + run.start_col as f32 * cell_advance;
                let y = TEXT_TOP + run.line as f32 * LINE_HEIGHT;
                let w = run.width as f32 * cell_advance;
                let h = LINE_HEIGHT;
                RectInstance {
                    rect: [x, y, w, h],
                    color: color_to_floats(run.color),
                }
            })
            .collect();
        self.rect_renderer.prepare(
            &self.queue,
            &rect_instances,
            [
                self.surface_config.width as f32,
                self.surface_config.height as f32,
            ],
        );

        let cursor_left = TEXT_LEFT + (cursor_col as f32) * self.cell_advance + CURSOR_X_OFFSET;
        let cursor_top = TEXT_TOP + (cursor_line.max(0) as f32) * LINE_HEIGHT + CURSOR_Y_OFFSET;
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

            // Backgrounds first, then text on top.
            self.rect_renderer.render(&mut pass);
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

struct Terminite {
    renderer: Option<Renderer>,
    modifiers: ModifiersState,
    proxy: EventLoopProxy<UserEvent>,
}

impl ApplicationHandler<UserEvent> for Terminite {
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
        let renderer = pollster::block_on(Renderer::new(window.clone(), self.proxy.clone()));
        self.renderer = Some(renderer);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wakeup => {
                if let Some(renderer) = self.renderer.as_ref() {
                    renderer.window.request_redraw();
                }
            }
        }
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
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("terminite: failed to start the event loop");
    let proxy = event_loop.create_proxy();
    let mut terminite = Terminite {
        renderer: None,
        modifiers: ModifiersState::default(),
        proxy,
    };
    event_loop
        .run_app(&mut terminite)
        .expect("terminite: the event loop exited with an error");
}
