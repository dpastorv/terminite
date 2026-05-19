//! terminite — a terminal emulator for the human-AI pair.
//!
//! A human and an AI agent, co-users of one shared surface. See `guide/` for the
//! vision, architecture, and decisions behind every line of this.
//!
//! Milestone 1 is a window that opens instantly. This is its first slice: the
//! window itself. The next slices wire in the GPU renderer (wgpu) and the live
//! terminal grid (alacritty_terminal).

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

/// The terminite application. For now it owns a single window; the multiplexer,
/// renderer, and panes arrive as the architecture is built out.
#[derive(Default)]
struct Terminite {
    window: Option<Window>,
}

impl ApplicationHandler for Terminite {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attributes = Window::default_attributes()
            .with_title("terminite")
            .with_inner_size(LogicalSize::new(900.0, 600.0));

        self.window = Some(
            event_loop
                .create_window(attributes)
                .expect("terminite: failed to create the window"),
        );
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
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
