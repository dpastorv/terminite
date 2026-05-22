//! terminite — a terminal emulator for the human-AI pair.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, KeyEvent, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

mod config;
mod images;
mod palette;
mod rect;
mod renderer;
mod term;

use renderer::{Renderer, SplitDir};

// ── Layout constants shared across modules ─────────────────────────────────

/// terminite's resting background — deep, quiet, not pure black.
pub const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.04,
    g: 0.04,
    b: 0.06,
    a: 1.0,
};

// Font size, line height, and text padding are no longer constants — they
// come from the config (see `config.rs`) and live on the renderer as
// runtime metrics, measured against the configured font at startup.

// ── Cross-thread event into winit ──────────────────────────────────────────

/// Identifies one tab inside the window. Monotonic; survives reordering.
/// `Notifier` for each tab carries its own `TabId` so per-shell events
/// (title, bell) can be routed to the right tab.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

/// Events that wake terminite's render loop. The terminal's I/O thread sends
/// these from off-thread; the winit loop responds on the next tick.
#[derive(Debug)]
pub enum UserEvent {
    /// Generic wake — output arrived, redraw the frame.
    Wakeup,
    /// Shell emitted an OSC 0/1/2 title; update the tab's title.
    SetTitle(TabId, String),
    /// Shell emitted `\a` (bell). Visual flash.
    Bell(TabId),
    /// Exit requested from inside the renderer (e.g., user confirmed
    /// closing the last tab via the in-window modal).
    Exit,
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
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        // A WaitUntil deadline came due — request a redraw so the renderer
        // can advance the bell flash, cursor blink, or autoscroll tick.
        if matches!(cause, StartCause::ResumeTimeReached { .. })
            && let Some(r) = self.renderer.as_ref()
        {
            r.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Drive the renderer's pending deadlines via the native scheduler
        // instead of detached threads — the latter pinned the machine on
        // bell storms (2026-05-20 watchdog panic).
        let flow = self
            .renderer
            .as_ref()
            .and_then(|r| r.next_wakeup())
            .map(ControlFlow::WaitUntil)
            .unwrap_or(ControlFlow::Wait);
        event_loop.set_control_flow(flow);
    }

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
        // Allow IME composition input (dead keys, accents, CJK input methods).
        window.set_ime_allowed(true);
        let renderer = pollster::block_on(Renderer::new(window.clone(), self.proxy.clone()));
        self.renderer = Some(renderer);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wakeup => {
                if let Some(renderer) = self.renderer.as_ref() {
                    renderer.window.request_redraw();
                }
            }
            UserEvent::SetTitle(tab_id, title) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.set_tab_title(tab_id, title);
                }
            }
            UserEvent::Bell(tab_id) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.ring_bell(tab_id);
                }
            }
            UserEvent::Exit => event_loop.exit(),
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
            WindowEvent::Focused(focused) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.focus_changed(focused);
                }
            }
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::Ime(ime) => {
                if let Some(r) = self.renderer.as_mut() {
                    match ime {
                        Ime::Preedit(text, _cursor) => r.ime_preedit(text),
                        Ime::Commit(text) => r.ime_commit(text),
                        Ime::Enabled | Ime::Disabled => r.ime_preedit(String::new()),
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // Modal eats keyboard input — only Esc / Enter / Return do
                // anything; everything else is swallowed.
                if let Some(r) = self.renderer.as_mut() {
                    if r.has_modal() {
                        if event.state == ElementState::Pressed {
                            match &event.logical_key {
                                Key::Named(NamedKey::Escape) => {
                                    r.modal_cancel();
                                }
                                Key::Named(NamedKey::Enter) => {
                                    if r.modal_confirm() {
                                        event_loop.exit();
                                    }
                                }
                                _ => {}
                            }
                        }
                        return;
                    }
                    // Context menu: Esc dismisses; any other key just
                    // dismisses and is swallowed.
                    if r.has_context_menu() {
                        if event.state == ElementState::Pressed {
                            r.dismiss_context_menu();
                        }
                        return;
                    }
                    // Find bar open: keyboard edits the query. Cmd+F closes
                    // it; Esc closes; Enter / Shift+Enter cycle matches;
                    // Backspace edits; printable chars append.
                    if r.has_find() && event.state == ElementState::Pressed {
                        let shift = self.modifiers.shift_key();
                        let cmd = self.modifiers.super_key();
                        if cmd {
                            if let Key::Character(t) = &event.logical_key {
                                if t.chars().next().map(|c| c.to_ascii_lowercase())
                                    == Some('f')
                                {
                                    r.close_find();
                                    return;
                                }
                            }
                        }
                        match &event.logical_key {
                            Key::Named(NamedKey::Escape) => r.close_find(),
                            Key::Named(NamedKey::Enter) => {
                                if shift {
                                    r.find_prev();
                                } else {
                                    r.find_next();
                                }
                            }
                            Key::Named(NamedKey::Backspace) => r.find_backspace(),
                            Key::Character(t) if !cmd && !self.modifiers.control_key() => {
                                for ch in t.chars() {
                                    if !ch.is_control() {
                                        r.find_input(ch);
                                    }
                                }
                            }
                            Key::Named(NamedKey::Space) => r.find_input(' '),
                            _ => {}
                        }
                        return;
                    }
                }
                // Cmd-shortcuts: copy, paste, quit, tab ops (Cmd on macOS =
                // super_key in winit's ModifiersState).
                if event.state == ElementState::Pressed && self.modifiers.super_key() {
                    // Cmd+Opt+Arrow: move keyboard focus between split panes.
                    if self.modifiers.alt_key() {
                        let dir = match &event.logical_key {
                            Key::Named(NamedKey::ArrowLeft) => Some((-1.0, 0.0)),
                            Key::Named(NamedKey::ArrowRight) => Some((1.0, 0.0)),
                            Key::Named(NamedKey::ArrowUp) => Some((0.0, -1.0)),
                            Key::Named(NamedKey::ArrowDown) => Some((0.0, 1.0)),
                            _ => None,
                        };
                        if let Some((dx, dy)) = dir {
                            if let Some(r) = self.renderer.as_mut() {
                                r.focus_dir(dx, dy);
                            }
                            return;
                        }
                    }
                    if let Key::Character(text) = &event.logical_key {
                        let ch = text.chars().next().map(|c| c.to_ascii_lowercase());
                        let shift = self.modifiers.shift_key();

                        // Cmd+Shift+] / Cmd+Shift+[: next / previous tab.
                        // Cmd+Shift+D: split the active pane stacked.
                        if shift {
                            match ch {
                                Some(']') => {
                                    if let Some(r) = self.renderer.as_mut() {
                                        r.next_tab();
                                    }
                                    return;
                                }
                                Some('[') => {
                                    if let Some(r) = self.renderer.as_mut() {
                                        r.prev_tab();
                                    }
                                    return;
                                }
                                Some('d') => {
                                    if let Some(r) = self.renderer.as_mut() {
                                        r.split_active(SplitDir::Horizontal, 0.5);
                                    }
                                    return;
                                }
                                _ => {}
                            }
                        }

                        match ch {
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
                            Some('q') => {
                                event_loop.exit();
                                return;
                            }
                            Some('f') => {
                                if let Some(r) = self.renderer.as_mut() {
                                    r.open_find();
                                }
                                return;
                            }
                            Some('t') => {
                                if let Some(r) = self.renderer.as_mut() {
                                    r.new_tab();
                                }
                                return;
                            }
                            // Cmd+D: split the active pane side by side.
                            Some('d') => {
                                if let Some(r) = self.renderer.as_mut() {
                                    r.split_active(SplitDir::Vertical, 0.5);
                                }
                                return;
                            }
                            // Cmd+W: close the active tab. Cascades to
                            // closing the pane (its last tab) and then the
                            // window (its last pane).
                            Some('w') => {
                                if let Some(r) = self.renderer.as_mut() {
                                    if r.close_active_tab() {
                                        event_loop.exit();
                                    }
                                }
                                return;
                            }
                            // Cmd+1 … Cmd+9: jump to that tab index.
                            Some(d) if d.is_ascii_digit() && d != '0' => {
                                let idx = (d as u8 - b'1') as usize;
                                if let Some(r) = self.renderer.as_mut() {
                                    r.switch_to_tab(idx);
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
                    if let Some(renderer) = self.renderer.as_ref() {
                        renderer.write_active(bytes);
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(r) = self.renderer.as_mut() {
                    r.mouse_moved(position.x as f32, position.y as f32, self.modifiers);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(r) = self.renderer.as_mut() {
                    match state {
                        ElementState::Pressed => r.mouse_down(button, self.modifiers),
                        ElementState::Released => r.mouse_up(button, self.modifiers),
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(r) = self.renderer.as_mut() {
                    r.mouse_wheel(delta, self.modifiers);
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
