//! terminite — a terminal emulator for the human-AI pair.
//!
//! Slice 3b of Milestone 1: a real shell behind the screen. `alacritty_terminal`
//! spawns a PTY in its own thread; every frame the renderer locks the `Term`,
//! reads its cell grid, and draws it through glyphon. Slice 3c wires keyboard
//! input so the human (and the AI) can actually drive the shell.

use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::EventLoop as TermEventLoop;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

/// terminite's resting background — deep, quiet, not pure black.
const BACKGROUND: wgpu::Color = wgpu::Color { r: 0.04, g: 0.04, b: 0.06, a: 1.0 };

const TERM_COLS: usize = 80;
const TERM_ROWS: usize = 24;
const CELL_WIDTH_PX: u16 = 8;
const CELL_HEIGHT_PX: u16 = 20;

/// Grid dimensions for `Term::new`. `alacritty_terminal` asks for any
/// `Dimensions` implementor; one of our own keeps the rows/cols intent clear.
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize { self.rows }
    fn screen_lines(&self) -> usize { self.rows }
    fn columns(&self) -> usize { self.cols }
}

/// A no-op listener for terminal events. In a later slice this becomes the
/// bridge that wakes the render thread on `Event::Wakeup` and dispatches
/// title / bell / exit. For now we redraw every frame and read the grid fresh.
#[derive(Clone)]
struct Notifier;

impl EventListener for Notifier {
    fn send_event(&self, _event: TermEvent) {}
}

/// The live terminal: the shared `Term` plus an I/O thread driving its PTY.
struct LiveTerm {
    term: Arc<FairMutex<Term<Notifier>>>,
}

impl LiveTerm {
    fn new() -> Self {
        let size = GridSize { cols: TERM_COLS, rows: TERM_ROWS };
        let term = Term::new(TermConfig::default(), &size, Notifier);
        let term = Arc::new(FairMutex::new(term));

        let window_size = WindowSize {
            num_lines: TERM_ROWS as u16,
            num_cols: TERM_COLS as u16,
            cell_width: CELL_WIDTH_PX,
            cell_height: CELL_HEIGHT_PX,
        };
        let pty = tty::new(&tty::Options::default(), window_size, 0)
            .expect("terminite: failed to open the PTY");

        let event_loop = TermEventLoop::new(term.clone(), Notifier, pty, false, false)
            .expect("terminite: failed to start the PTY event loop");
        // Detach the I/O thread. Slice 3c will keep the channel sender to drive
        // input and shutdown; for now the OS reaps the thread on exit.
        let _ = event_loop.spawn();

        Self { term }
    }

    /// Snapshot the visible grid into a String. One row per line, trailing
    /// whitespace trimmed so glyphon doesn't render miles of empty space.
    fn snapshot(&self) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        let mut out = String::with_capacity((cols + 1) * rows);
        for line in 0..rows {
            let row = &grid[Line(line as i32)];
            let mut line_text = String::with_capacity(cols);
            for col in 0..cols {
                line_text.push(row[Column(col)].c);
            }
            out.push_str(line_text.trim_end());
            out.push('\n');
        }
        out
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
    /// Cached last-rendered grid snapshot; lets us skip re-shaping when the
    /// terminal hasn't changed between frames.
    last_snapshot: String,

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
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        let mut text_buffer = Buffer::new(&mut font_system, Metrics::new(14.0, 20.0));
        let physical_width = (width as f64 * scale_factor) as f32;
        let physical_height = (height as f64 * scale_factor) as f32;
        text_buffer.set_size(&mut font_system, Some(physical_width), Some(physical_height));
        text_buffer.set_text(
            &mut font_system,
            "",
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        text_buffer.shape_until_scroll(&mut font_system, false);

        let live_term = LiveTerm::new();

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
            last_snapshot: String::new(),
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
        self.text_buffer.shape_until_scroll(&mut self.font_system, false);
    }

    fn render(&mut self) {
        // Pull the latest grid; re-shape only when it actually changed.
        let snapshot = self.live_term.snapshot();
        if snapshot != self.last_snapshot {
            self.text_buffer.set_text(
                &mut self.font_system,
                &snapshot,
                &Attrs::new().family(Family::Monospace),
                Shaping::Advanced,
                None,
            );
            self.text_buffer.shape_until_scroll(&mut self.font_system, false);
            self.last_snapshot = snapshot;
        }

        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                [TextArea {
                    buffer: &self.text_buffer,
                    left: 24.0,
                    top: 24.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: 0,
                        right: self.surface_config.width as i32,
                        bottom: self.surface_config.height as i32,
                    },
                    default_color: Color::rgb(220, 220, 220),
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
        let mut encoder =
            self.device
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
        let Some(renderer) = self.renderer.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => renderer.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                renderer.render();
                renderer.window.request_redraw();
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
