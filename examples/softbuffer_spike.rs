//! Spike: can CPU rendering (softbuffer + cosmic-text) push a full Retina
//! screen of scrolling monospace text smoothly, at low energy, with no
//! present-flash? This is the throwaway experiment behind a possible move off
//! wgpu — it touches nothing in terminite proper.
//!
//! Run:  cargo run --release --example softbuffer_spike
//! Quit: Esc or close the window.
//!
//! What to watch:
//!   - stdout: sustained FPS + avg/max frame-ms at your native Retina size.
//!     ~60+ fps steady = CPU rendering is viable; <30 = marginal.
//!   - the scroll: is it buttery, or does it hitch?
//!   - the flash: does the desktop ever show through? (it shouldn't — softbuffer
//!     blits synchronously; there's no CAMetalLayer async present.)
//!   - Activity Monitor / battery: CPU% + "Energy Impact" while scrolling.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Instant;

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Scroll, Shaping, SwashCache};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const BG: (u8, u8, u8) = (0x1e, 0x1e, 0x22);
const FG: (u8, u8, u8) = (0xd4, 0xd4, 0xdc);
const SCROLL_PX_PER_FRAME: f32 = 3.0;

/// Build a big block of representative terminal text — varied ASCII plus a
/// little Unicode so glyph fallback is exercised, enough lines that scrolling
/// keeps revealing fresh content (forcing real per-line layout each frame).
fn sample_text() -> String {
    let mut s = String::with_capacity(400_000);
    for i in 0..6000u32 {
        match i % 5 {
            0 => s.push_str(&format!(
                "{i:5} │ fn process_chunk(buf: &[u8], off: usize) -> Result<Vec<u8>, Error> {{\n"
            )),
            1 => s.push_str(&format!(
                "{i:5} │     let n = buf.len().min(4096);  // αβγ ∑ 日本語 — fallback test\n"
            )),
            2 => s.push_str(&format!(
                "{i:5} │     for (j, b) in buf.iter().enumerate() {{ out[j] ^= b.rotate_left(3); }}\n"
            )),
            3 => s.push_str(&format!(
                "{i:5} │ WARN  2026-07-27T14:{:02}:{:02}Z worker#{} retry (attempt {})\n",
                i % 60, (i * 7) % 60, i % 8, i % 4
            )),
            _ => s.push_str(&format!("{i:5} │ {}\n", "=".repeat(80))),
        }
    }
    s
}

struct App {
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    font_system: FontSystem,
    swash_cache: SwashCache,
    text_buffer: Option<Buffer>,
    line_height: f32,
    scroll_px: f32,
    // Frame timing.
    last_frame: Option<Instant>,
    since_report: Instant,
    frames: u32,
    acc_ms: f32,
    max_ms: f32,
}

impl App {
    fn new() -> Self {
        App {
            window: None,
            surface: None,
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            text_buffer: None,
            line_height: 0.0,
            scroll_px: 0.0,
            last_frame: None,
            since_report: Instant::now(),
            frames: 0,
            acc_ms: 0.0,
            max_ms: 0.0,
        }
    }

    fn render(&mut self) {
        let Some(window) = self.window.clone() else { return };
        let Some(surface) = self.surface.as_mut() else { return };
        let Some(text_buffer) = self.text_buffer.as_mut() else { return };

        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        surface
            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
            .unwrap();

        // Advance the scroll (fractional for smooth motion; integer part picks
        // the top line, so fresh lines lay out as they scroll in).
        self.scroll_px += SCROLL_PX_PER_FRAME;
        let total = 6000usize;
        let line = ((self.scroll_px / self.line_height) as usize) % total.saturating_sub(1).max(1);
        let vertical = self.scroll_px % self.line_height;
        text_buffer.set_scroll(Scroll { line, vertical, horizontal: 0.0 });
        text_buffer.shape_until_scroll(&mut self.font_system, false);

        let mut sb = surface.buffer_mut().unwrap();
        let bg_u32 = ((BG.0 as u32) << 16) | ((BG.1 as u32) << 8) | BG.2 as u32;
        sb.fill(bg_u32);

        // cosmic-text rasterizes each glyph once (SwashCache) and hands us
        // filled rects to blit — exactly a CPU terminal's draw path.
        let width = w as i32;
        let height = h as i32;
        text_buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            Color::rgb(FG.0, FG.1, FG.2),
            |px, py, rw, rh, color| {
                let a = color.a() as u32;
                if a == 0 {
                    return;
                }
                let (cr, cg, cb) = (color.r() as u32, color.g() as u32, color.b() as u32);
                for yy in py..py + rh as i32 {
                    if yy < 0 || yy >= height {
                        continue;
                    }
                    let row = yy as usize * w as usize;
                    for xx in px..px + rw as i32 {
                        if xx < 0 || xx >= width {
                            continue;
                        }
                        let idx = row + xx as usize;
                        if a >= 255 {
                            sb[idx] = (cr << 16) | (cg << 8) | cb;
                        } else {
                            let d = sb[idx];
                            let (dr, dg, db) = ((d >> 16) & 0xff, (d >> 8) & 0xff, d & 0xff);
                            let blend = |s: u32, dst: u32| (s * a + dst * (255 - a)) / 255;
                            sb[idx] = (blend(cr, dr) << 16) | (blend(cg, dg) << 8) | blend(cb, db);
                        }
                    }
                }
            },
        );

        sb.present().unwrap();

        // Timing.
        let now = Instant::now();
        if let Some(prev) = self.last_frame {
            let ms = (now - prev).as_secs_f32() * 1000.0;
            self.acc_ms += ms;
            self.max_ms = self.max_ms.max(ms);
            self.frames += 1;
        }
        self.last_frame = Some(now);
        if self.since_report.elapsed().as_secs_f32() >= 1.0 && self.frames > 0 {
            let avg = self.acc_ms / self.frames as f32;
            let scale = window.scale_factor();
            println!(
                "{}x{} phys @ {:.1}x  |  {:.0} fps  avg {:.2} ms  max {:.2} ms",
                w, h, scale, self.frames as f32, avg, self.max_ms
            );
            self.frames = 0;
            self.acc_ms = 0.0;
            self.max_ms = 0.0;
            self.since_report = Instant::now();
        }
        window.request_redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("softbuffer spike — CPU render");
        let window = Rc::new(event_loop.create_window(attrs).unwrap());
        let context = softbuffer::Context::new(window.clone()).unwrap();
        let surface = softbuffer::Surface::new(&context, window.clone()).unwrap();

        // Render at native Retina density: physical font size = logical × scale.
        let scale = window.scale_factor() as f32;
        let font_size = 14.0 * scale;
        self.line_height = (font_size * 1.35).round();
        let size = window.inner_size();
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, self.line_height));
        buffer.set_size(
            &mut self.font_system,
            Some(size.width as f32),
            Some(size.height as f32),
        );
        buffer.set_text(
            &mut self.font_system,
            &sample_text(),
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );

        // softbuffer's Surface is self-contained in 0.4 — it doesn't retain a
        // borrow of the Context, so the local can drop here.
        drop(context);
        self.surface = Some(surface);
        self.text_buffer = Some(buffer);
        self.window = Some(window);
        event_loop.set_control_flow(ControlFlow::Poll);
        println!("softbuffer spike running — scrolling a full screen every frame. Esc to quit.");
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                event_loop.exit()
            }
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    event_loop.run_app(&mut app).unwrap();
}
