//! terminite — a terminal emulator for the human-AI pair.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

mod palette;
mod rect;
mod renderer;
mod term;

use renderer::Renderer;

// ── Layout constants shared across modules ─────────────────────────────────

/// terminite's resting background — deep, quiet, not pure black.
pub const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.04,
    g: 0.04,
    b: 0.06,
    a: 1.0,
};

pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 20.0;

/// Padding from the window edge to the text.
pub const TEXT_LEFT: f32 = 24.0;
pub const TEXT_TOP: f32 = 24.0;

// ── Cross-thread event into winit ──────────────────────────────────────────

/// Events that wake terminite's render loop. The terminal's I/O thread sends
/// `Wakeup` whenever the shell produces output that needs to be drawn; the
/// winit loop responds by requesting one redraw.
#[derive(Debug)]
pub enum UserEvent {
    Wakeup,
}

// ── Input translation ──────────────────────────────────────────────────────

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

// ── The app handler ────────────────────────────────────────────────────────

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
                // Cmd-shortcuts: copy and paste (Cmd on macOS = super_key in
                // winit's ModifiersState).
                if event.state == ElementState::Pressed && self.modifiers.super_key() {
                    if let Key::Character(text) = &event.logical_key {
                        let lower = text.chars().next().map(|c| c.to_ascii_lowercase());
                        match lower {
                            Some('c') => {
                                if let Some(r) = self.renderer.as_mut() {
                                    r.copy_selection();
                                }
                                return;
                            }
                            Some('v') => {
                                if let Some(r) = self.renderer.as_mut() {
                                    r.paste();
                                }
                                return;
                            }
                            _ => {}
                        }
                    }
                }
                // Shift+PageUp / PageDown scroll the viewport instead of
                // sending escape sequences to the shell.
                if event.state == ElementState::Pressed && self.modifiers.shift_key() {
                    match &event.logical_key {
                        Key::Named(NamedKey::PageUp) => {
                            if let Some(r) = self.renderer.as_ref() {
                                r.scroll_page(true);
                            }
                            return;
                        }
                        Key::Named(NamedKey::PageDown) => {
                            if let Some(r) = self.renderer.as_ref() {
                                r.scroll_page(false);
                            }
                            return;
                        }
                        _ => {}
                    }
                }
                if let Some(bytes) = key_to_bytes(&event, self.modifiers) {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.live_term.write(bytes);
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(r) = self.renderer.as_mut() {
                    r.mouse_moved(position.x as f32, position.y as f32);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    if let Some(r) = self.renderer.as_mut() {
                        match state {
                            ElementState::Pressed => r.mouse_down(),
                            ElementState::Released => r.mouse_up(),
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(r) = self.renderer.as_mut() {
                    r.mouse_wheel(delta);
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
