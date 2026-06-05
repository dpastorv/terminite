//! terminite — a terminal emulator for the human-AI pair.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, KeyEvent, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

mod activities;
mod blocks;
mod codex_bridge;
mod config;
mod config_io;
mod fileclaims;
mod fonts;
mod highlight;
mod images;
mod layout;
mod logging;
mod mcp;
mod modules;
mod modules_watch;
mod palette;
mod presence;
mod proto;
mod proto_client;
mod rect;
mod renderer;
mod term;
mod texture;

use renderer::{Renderer, SplitDir};

// ── Layout constants shared across modules ─────────────────────────────────

/// Per-process cap on retained crash dumps. Older dumps are dropped.
const MAX_CRASH_DUMPS: usize = 20;

/// Install a panic hook that writes a crash dump (panic message,
/// source location, backtrace) to `~/.terminite/log/crashes/`. Also
/// logs the panic to the regular log so a debug pane can pick it up.
/// Without this, a panic prints to stderr nobody reads and the
/// window vanishes.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".into());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };
        let backtrace = std::backtrace::Backtrace::force_capture();
        logging::error(&format!("panic at {location}: {payload}"));
        write_crash_dump(&location, &payload, &backtrace.to_string());
        // Let the default hook also fire (stderr) — useful when running
        // `cargo run` in a console.
        default(info);
    }));
}

/// Write one crash-dump file and trim the oldest if the cap is hit.
fn write_crash_dump(location: &str, payload: &str, backtrace: &str) {
    let Some(dir) = logging::crash_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let filename = format!("{}.txt", logging::filename_timestamp_now());
    let path = dir.join(&filename);
    let body = format!(
        "terminite crash dump\nversion: {}\nlocation: {}\nmessage: {}\n\nbacktrace:\n{}\n",
        env!("CARGO_PKG_VERSION"),
        location,
        payload,
        backtrace,
    );
    let _ = std::fs::write(&path, body);
    trim_crash_dumps(&dir);
}

/// Keep at most `MAX_CRASH_DUMPS`; drop oldest by mtime.
fn trim_crash_dumps(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            let m = e.metadata().ok()?.modified().ok()?;
            Some((p, m))
        })
        .collect();
    if files.len() <= MAX_CRASH_DUMPS {
        return;
    }
    files.sort_by_key(|(_, m)| *m);
    for (p, _) in &files[..files.len() - MAX_CRASH_DUMPS] {
        let _ = std::fs::remove_file(p);
    }
}

/// Decode the embedded app icon to an RGBA `winit::window::Icon`. Compiled
/// into the binary via `include_bytes!`, so terminite carries its own
/// brand asset — no fs read at runtime, no path dependency.
///
/// winit's window-level icon shows on Windows + X11 immediately. On macOS,
/// proper dock-icon display needs a `.app` bundle (the OS reads the icon
/// from the bundle's `Icon.icns`, not from a running window). The call is
/// still worth making — it's free on the platforms where it works, and
/// the bundling step later just points the packaging tool at the same PNG.
fn leaf_count(node: &layout::LayoutNode) -> usize {
    match node {
        layout::LayoutNode::Pane(_) => 1,
        layout::LayoutNode::Split { first, second, .. } => leaf_count(first) + leaf_count(second),
    }
}

fn load_app_icon() -> Option<winit::window::Icon> {
    const ICON_BYTES: &[u8] = include_bytes!("../logo/terminite-icon.png");
    let decoder = png::Decoder::new(ICON_BYTES);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    // Normalize to RGBA — winit::window::Icon wants exactly 4 bytes/px.
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(buf.len() / 3 * 4);
            for chunk in buf.chunks_exact(3) {
                out.extend_from_slice(chunk);
                out.push(255);
            }
            out
        }
        _ => return None,
    };
    winit::window::Icon::from_rgba(rgba, info.width, info.height).ok()
}

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
    /// An APC payload from the shell (Kitty graphics et al.). Moved onto
    /// the main thread for parsing + decoding so the PTY thread / term
    /// lock stays free during the work.
    Apc(TabId, Vec<u8>),
    /// OSC 133 shell-integration mark. `kind` is the FinalTerm letter
    /// (`A`/`B`/`C`/`D`); `exit` carries the exit code on a `D` mark;
    /// `line` is the cursor's absolute line at fire time, for scroll-
    /// anchored block placement.
    ShellIntegration {
        tab_id: TabId,
        kind: char,
        exit: Option<i32>,
        line: i32,
    },
    /// A new module connected to the proto socket. Drops any prior
    /// subscriber slot — v1 is single-client.
    ProtoConnect,
    /// A request line arrived on the proto socket. The reply (and any
    /// future subscription events) ride `out` back to the connection's
    /// writer.
    ProtoRequest {
        conn_id: u64,
        /// PID of the connecting process — used to place an agent in its pane
        /// when its CLI didn't forward `TERMINITE_PANE` to the MCP server.
        peer_pid: Option<i32>,
        request: proto::Request,
        out: std::sync::mpsc::SyncSender<proto::OutMessage>,
    },
    /// A proto connection closed. Clears the subscriber slot and drops the
    /// connection's room presence (if it had joined).
    ProtoDisconnect {
        conn_id: u64,
    },
    /// A module process pushed a message via its stdout. Bundle 6
    /// step 2b — drives the pane's rendered content.
    ModuleMessage {
        tab_id: TabId,
        msg: modules::ModuleMessage,
    },
    /// A shell pane's cwd changed (OSC 7). The renderer broadcasts a
    /// `cwd` event to every live data-module session so paired views
    /// like Nav can follow along. Fires only on actual change to
    /// keep the wire quiet.
    CwdChanged {
        tab_id: TabId,
        path: std::path::PathBuf,
    },
    /// Something in `~/.terminite/modules/` changed on disk —
    /// a module added, removed, or renamed. The renderer
    /// re-discovers and refreshes the dropdown. Debounced
    /// upstream so a multi-file drop only fires once.
    ModulesChanged,
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
    // xterm modifier encoding for arrow / Home / End / Delete with
    // Shift / Alt / Ctrl held. The modifier number is `1 + shift +
    // alt*2 + ctrl*4` (xterm convention) — `Shift = 2`, `Alt = 3`,
    // `Shift+Alt = 4`, `Ctrl = 5`, `Ctrl+Shift = 6`, `Ctrl+Alt = 7`,
    // `Ctrl+Shift+Alt = 8`. Lets editor modules read Shift+Arrow as
    // selection extension and Opt+Arrow as word jump, and gives
    // shells (bash/zsh) the modifier info their key bindings expect.
    let shift = modifiers.shift_key() as u8;
    let alt = modifiers.alt_key() as u8;
    let ctrl = modifiers.control_key() as u8;
    let mod_num = 1 + shift + alt * 2 + ctrl * 4;
    let arrow_letter = match &event.logical_key {
        Key::Named(NamedKey::ArrowUp) => Some(b'A'),
        Key::Named(NamedKey::ArrowDown) => Some(b'B'),
        Key::Named(NamedKey::ArrowRight) => Some(b'C'),
        Key::Named(NamedKey::ArrowLeft) => Some(b'D'),
        Key::Named(NamedKey::Home) => Some(b'H'),
        Key::Named(NamedKey::End) => Some(b'F'),
        _ => None,
    };
    if let Some(letter) = arrow_letter {
        return if mod_num > 1 {
            Some(format!("\x1b[1;{mod_num}{}", letter as char).into_bytes())
        } else {
            Some(vec![b'\x1b', b'[', letter])
        };
    }
    if matches!(&event.logical_key, Key::Named(NamedKey::Delete)) {
        return if mod_num > 1 {
            Some(format!("\x1b[3;{mod_num}~").into_bytes())
        } else {
            Some(b"\x1b[3~".to_vec())
        };
    }
    match &event.logical_key {
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(b"\x7f".to_vec()),
        Key::Named(NamedKey::Tab) => {
            if shift > 0 {
                Some(b"\x1b[Z".to_vec()) // xterm "back-tab"
            } else {
                Some(b"\t".to_vec())
            }
        }
        Key::Named(NamedKey::Escape) => Some(b"\x1b".to_vec()),
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
    /// Module-protocol server. Dropping it removes the socket file.
    /// `None` if the bind failed at startup — terminite still runs,
    /// just without the module surface available. Held only for its
    /// `Drop` impl; never read.
    #[allow(dead_code)]
    proto_server: Option<proto::ProtoServer>,
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
        // Re-deliver any stalled directed message (the comms base owns progress).
        // Cheap: a no-op unless a delivery deadline has come due.
        if let Some(r) = self.renderer.as_mut() {
            r.check_stalls();
            r.notify_freed_waiters();
            r.try_pty_deliveries();
            r.flush_pty_submits();
        }
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
        let mut attributes = Window::default_attributes()
            .with_title("terminite")
            .with_inner_size(LogicalSize::new(900.0, 600.0));
        if let Some(icon) = load_app_icon() {
            attributes = attributes.with_window_icon(Some(icon));
        }
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("terminite: failed to create the window"),
        );
        // Allow IME composition input (dead keys, accents, CJK input methods).
        window.set_ime_allowed(true);
        let mut renderer = pollster::block_on(Renderer::new(window.clone(), self.proxy.clone()));
        // Restore last layout if one exists. Failure (missing file,
        // parse error, cap breach) falls through to the freshly-
        // constructed default shell — we never block startup.
        match layout::load() {
            Ok(Some(saved)) => {
                logging::info(&format!(
                    "layout: restoring {} pane(s)",
                    leaf_count(&saved.root),
                ));
                renderer.restore_layout(saved);
            }
            Ok(None) => {}
            Err(e) => logging::warn(&format!("layout: load failed: {e}")),
        }
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
            UserEvent::Apc(tab_id, data) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.handle_apc(tab_id, &data);
                }
            }
            UserEvent::ShellIntegration { tab_id, kind, exit, line } => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.handle_shell_integration(tab_id, kind, exit, line);
                }
            }
            UserEvent::ProtoConnect => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.handle_proto_connect();
                }
            }
            UserEvent::ProtoRequest { conn_id, peer_pid, request, out } => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.handle_proto_request(conn_id, peer_pid, request, out);
                }
            }
            UserEvent::ProtoDisconnect { conn_id } => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.handle_proto_disconnect(conn_id);
                }
            }
            UserEvent::ModuleMessage { tab_id, msg } => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.handle_module_message(tab_id, msg);
                }
            }
            UserEvent::CwdChanged { tab_id, path } => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.handle_cwd_changed(tab_id, &path);
                }
            }
            UserEvent::ModulesChanged => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.handle_modules_changed();
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
                                // Cmd+Shift+F: cycle through the bundled fonts.
                                Some('f') => {
                                    if let Some(r) = self.renderer.as_mut() {
                                        r.cycle_font(true);
                                    }
                                    return;
                                }
                                _ => {}
                            }
                        }

                        match ch {
                            // Zoom: Cmd+= / Cmd++ in, Cmd+- out, Cmd+0 reset.
                            Some('=') | Some('+') => {
                                if let Some(r) = self.renderer.as_mut() {
                                    r.zoom_by(2.0);
                                }
                                return;
                            }
                            Some('-') | Some('_') => {
                                if let Some(r) = self.renderer.as_mut() {
                                    r.zoom_by(-2.0);
                                }
                                return;
                            }
                            Some('0') => {
                                if let Some(r) = self.renderer.as_mut() {
                                    r.zoom_reset();
                                }
                                return;
                            }
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
                    if let Some(renderer) = self.renderer.as_mut() {
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

fn main() -> std::process::ExitCode {
    // Subcommand dispatch first: `terminite tabs / blocks / block / watch`
    // run as a CLI client against the socket of a separately-running
    // terminite window. No subcommand → launch the window.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = proto_client::dispatch(&args) {
        return code;
    }

    // Window-mode bootstrap. Logging + panic hook come up first so
    // any failure during init has somewhere to land.
    logging::init();
    install_panic_hook();
    logging::info(&format!(
        "terminite starting (version {})",
        env!("CARGO_PKG_VERSION")
    ));

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("terminite: failed to start the event loop");
    let proxy = event_loop.create_proxy();
    // Stand up the module-protocol socket before the window so a
    // late-arriving client doesn't race the listener. A `None` here
    // means the bind failed; terminite still runs, just without the
    // module surface — see proto::ProtoServer::start for the cases.
    let proto_server = proto::ProtoServer::start(proxy.clone());
    let mut terminite = Terminite {
        renderer: None,
        modifiers: ModifiersState::default(),
        proxy,
        proto_server,
    };
    event_loop
        .run_app(&mut terminite)
        .expect("terminite: the event loop exited with an error");
    std::process::ExitCode::SUCCESS
}
