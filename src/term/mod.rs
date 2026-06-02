//! The live terminal: PTY thread, snapshot of the cell grid into rendering-
//! friendly data (styled text runs, background runs, decoration runs).

use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as TermEventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};

pub use alacritty_terminal::grid::Scroll as TermScroll;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};

pub use alacritty_terminal::vte::ansi::CursorShape as CursorShapeKind;
use glyphon::Color;
use winit::event_loop::EventLoopProxy;

use crate::palette::{dim_color, resolve_color, BACKGROUND_RGB, DEFAULT_FG};
use crate::{TabId, UserEvent};

// OS process-introspection helpers (cwd, names, pgid). Re-exported so existing
// `crate::term::process_display_name` call sites keep working unchanged.
mod procinfo;
pub(crate) use procinfo::*;

/// Visual style of a contiguous text run. Two cells join the same run only
/// when every field matches.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SpanStyle {
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
}

/// A horizontal run of cells sharing a background color, in cell coordinates.
/// `line` is signed so -1 can represent the "extra row above the viewport"
/// used for pixel-smooth scrolling.
#[derive(Clone, Copy, PartialEq)]
pub struct BgRun {
    pub line: i32,
    pub start_col: usize,
    pub width: usize,
    pub color: Color,
}

#[derive(Clone, Copy, PartialEq)]
pub enum DecorationKind {
    Underline,
    DoubleUnderline,
    Strikeout,
}

#[derive(Clone, Copy, PartialEq)]
pub struct DecorationRun {
    pub kind: DecorationKind,
    pub line: i32,
    pub start_col: usize,
    pub width: usize,
    pub color: Color,
}

/// A run of cells that all carry the same OSC 8 hyperlink. `line` is signed
/// for the extra-row-above convention, like `BgRun`.
#[derive(Clone, PartialEq)]
pub struct LinkRun {
    pub line: i32,
    pub start_col: usize,
    pub width: usize,
    pub uri: String,
}

/// One frame's worth of rendering data extracted from the cell grid.
pub struct Snapshot {
    pub text_runs: Vec<(String, SpanStyle)>,
    pub bg_runs: Vec<BgRun>,
    pub deco_runs: Vec<DecorationRun>,
    pub link_runs: Vec<LinkRun>,
    pub cursor_line: i32,
    pub cursor_col: usize,
    pub cursor_shape: CursorShape,
    pub cursor_blinking: bool,
    /// True if the snapshot's first row of text is the "extra row above the
    /// viewport" used for pixel-smooth scrolling. False when no scrollback is
    /// available above the current top (history exhausted or display_offset 0
    /// with empty history).
    pub has_extra_row: bool,
}

/// Snapshot of the relevant `TermMode` flags. Pulled out so the renderer can
/// branch on mode without holding a term lock.
#[derive(Default, Clone, Copy)]
pub struct ModeFlags {
    pub bracketed_paste: bool,
    pub mouse_report_click: bool,
    pub mouse_drag: bool,
    pub mouse_motion: bool,
    pub sgr_mouse: bool,
    pub alt_screen: bool,
    pub focus_in_out: bool,
    /// DECCKM — the app wants `ESC O A/B/C/D` for arrow keys instead of
    /// `ESC [ A/B/C/D`. vi-style TUIs flip this on; the wheel→arrow
    /// translation in alt screen has to match.
    pub app_cursor: bool,
}

#[derive(Debug)]
pub struct GridSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize { self.rows }
    fn screen_lines(&self) -> usize { self.rows }
    fn columns(&self) -> usize { self.cols }
}

/// Bridge between the PTY thread and winit's event loop. The proxy wakes the
/// render loop on any event. The pty_sender is filled in *after* the event
/// loop is created (it returns the sender), so we hold it behind a Mutex —
/// when alacritty emits `Event::PtyWrite` (CPR responses, clipboard replies,
/// color queries, device-attribute requests), we forward the bytes back to
/// the shell.
///
/// Each Notifier carries its tab's `TabId` so per-shell events (title, bell)
/// can be routed back to the correct tab.
#[derive(Clone)]
pub struct Notifier {
    pub proxy: EventLoopProxy<UserEvent>,
    pub pty_sender: Arc<Mutex<Option<EventLoopSender>>>,
    pub tab_id: TabId,
    /// Working directory as last reported by the shell via OSC 7. Shared
    /// with `LiveTerm` so `current_dir()` can read it. In-band beats the
    /// `proc_pidinfo` reach-around, which macOS TCC blocks for unsigned
    /// binaries (see friction-log 2026-05-21).
    pub reported_cwd: Arc<Mutex<Option<PathBuf>>>,
}

impl EventListener for Notifier {
    fn send_event(&self, event: TermEvent) {
        match &event {
            TermEvent::PtyWrite(text) => {
                if let Ok(guard) = self.pty_sender.lock() {
                    if let Some(sender) = guard.as_ref() {
                        let _ = sender.send(Msg::Input(text.clone().into_bytes().into()));
                    }
                }
            }
            TermEvent::Title(title) => {
                let _ = self
                    .proxy
                    .send_event(UserEvent::SetTitle(self.tab_id, title.clone()));
            }
            TermEvent::ResetTitle => {
                // Treat this as "shell wants the auto-title back" — handled
                // by sending an empty SetTitle, which our renderer reads as
                // "clear the shell-set title and let the auto-title take
                // over." Without this, exiting a TUI that set its own title
                // (claude, vim, ssh) would leave the stale title on the tab.
                let _ = self
                    .proxy
                    .send_event(UserEvent::SetTitle(self.tab_id, String::new()));
            }
            TermEvent::Bell => {
                let _ = self.proxy.send_event(UserEvent::Bell(self.tab_id));
            }
            TermEvent::CwdChanged(uri) => {
                // OSC 7: the shell told us its working directory in-band.
                if let Some(path) = parse_osc7_path(uri) {
                    let changed = if let Ok(mut guard) = self.reported_cwd.lock() {
                        let was = guard.clone();
                        *guard = Some(path.clone());
                        was.as_deref() != Some(path.as_path())
                    } else {
                        false
                    };
                    // Fire a UserEvent only when the path actually
                    // changed — otherwise a chatty shell that re-emits
                    // OSC 7 every prompt floods the broadcast.
                    if changed {
                        let _ = self.proxy.send_event(UserEvent::CwdChanged {
                            tab_id: self.tab_id,
                            path,
                        });
                    }
                }
            }
            TermEvent::ShellIntegration { kind, exit, line } => {
                // OSC 133 → main thread, where it feeds the block Model.
                let _ = self.proxy.send_event(UserEvent::ShellIntegration {
                    tab_id: self.tab_id,
                    kind: *kind,
                    exit: *exit,
                    line: *line,
                });
            }
            TermEvent::Apc(data) => {
                // APC payloads (Kitty graphics) — move to the main thread
                // for parsing + decoding so the PTY thread / term lock
                // stays free. Capped at `vte::APC_MAX_BYTES` upstream.
                let _ = self
                    .proxy
                    .send_event(UserEvent::Apc(self.tab_id, data.clone()));
            }
            _ => {}
        }
        let _ = self.proxy.send_event(UserEvent::Wakeup);
    }
}

/// The live terminal: the shared `Term`, the I/O thread driving its PTY, and
/// the channel used to push bytes back into the shell.
pub struct LiveTerm {
    term: Arc<FairMutex<Term<Notifier>>>,
    sender: EventLoopSender,
    cell_advance: f32,
    line_height: f32,
    /// PID of the shell process at the other end of the PTY. Captured before
    /// the `Pty` was moved into the I/O thread so we can query `proc_pidinfo`
    /// (cwd inheritance, name) later.
    shell_pid: i32,
    /// Master PTY fd (alacritty owns it through the I/O thread; we just
    /// borrow the int). Used as a `tcgetpgrp` fallback when our slave open
    /// failed.
    master_fd: RawFd,
    /// A read-only fd to the SLAVE end of the PTY (opened via ptsname +
    /// O_NOCTTY). Used with `tcgetpgrp` to find the foreground process
    /// group. We close it in `Drop`. `-1` when the open failed.
    slave_fd: RawFd,
    /// Working directory as reported by the shell via OSC 7; shared with the
    /// `Notifier` that writes it. `None` until the shell first emits OSC 7.
    reported_cwd: Arc<Mutex<Option<PathBuf>>>,
}

impl Drop for LiveTerm {
    fn drop(&mut self) {
        // Wind down alacritty's PTY I/O thread. It owns the master fd and
        // the child process; `Msg::Shutdown` breaks its loop so it drops the
        // `Pty`, which closes the master and SIGHUPs the shell. Without this
        // every closed tab leaks a thread *and* an orphaned shell — the same
        // family of leak as the 2026-05-20 OOM incident.
        let _ = self.sender.send(Msg::Shutdown);
        if self.slave_fd >= 0 {
            unsafe { libc::close(self.slave_fd) };
        }
    }
}

impl LiveTerm {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cols: usize,
        rows: usize,
        cell_advance: f32,
        line_height: f32,
        proxy: EventLoopProxy<UserEvent>,
        tab_id: TabId,
        cwd: Option<PathBuf>,
        scrollback: usize,
    ) -> Self {
        Self::new_with_command(
            cols, rows, cell_advance, line_height, proxy, tab_id, cwd, scrollback, None,
        )
    }

    /// Same as `new` but runs a specific command instead of the user's
    /// shell. Used by TTY modules (yazi, helix, nvim, htop, …) — the
    /// extension-surface inhabitants that draw via terminal escape
    /// sequences rather than line-delimited JSON.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_command(
        cols: usize,
        rows: usize,
        cell_advance: f32,
        line_height: f32,
        proxy: EventLoopProxy<UserEvent>,
        tab_id: TabId,
        cwd: Option<PathBuf>,
        scrollback: usize,
        command: Option<(String, Vec<String>)>,
    ) -> Self {
        let pty_sender: Arc<Mutex<Option<EventLoopSender>>> = Arc::new(Mutex::new(None));
        let reported_cwd: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let notifier = Notifier {
            proxy,
            pty_sender: pty_sender.clone(),
            tab_id,
            reported_cwd: reported_cwd.clone(),
        };
        let size = GridSize { cols, rows };
        let term_config = TermConfig {
            scrolling_history: scrollback,
            ..TermConfig::default()
        };
        let term = Term::new(term_config, &size, notifier.clone());
        let term = Arc::new(FairMutex::new(term));

        let window_size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: cell_advance as u16,
            cell_height: line_height as u16,
        };

        // Explicit terminal capabilities for the shell.
        let mut tty_options = tty::Options::default();
        tty_options
            .env
            .insert("TERM".to_string(), "xterm-256color".to_string());
        tty_options
            .env
            .insert("COLORTERM".to_string(), "truecolor".to_string());
        // Self-announce into every spawned PTY: any process inside a
        // terminite pane can now detect it's in terminite (`TERMINITE` =
        // version) and find the room socket (`TERMINITE_SOCKET`). This is
        // the standard terminal self-identification pattern — cf.
        // `TERM_PROGRAM`, `ITERM_SESSION_ID`, `INSIDE_EMACS`. The
        // per-pane host-assigned room id (`TERMINITE_ACTOR`) layers on
        // top of this for agent panes.
        tty_options
            .env
            .insert("TERMINITE".to_string(), env!("CARGO_PKG_VERSION").to_string());
        let socket = std::env::var_os("TERMINITE_SOCKET")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".terminite/socket")));
        if let Some(socket) = socket {
            tty_options
                .env
                .insert("TERMINITE_SOCKET".to_string(), socket.to_string_lossy().into_owned());
        }
        // Per-pane id, so an agent that joins the room can tell terminite
        // which pane it's in — terminite tints that pane in the actor's color.
        tty_options
            .env
            .insert("TERMINITE_PANE".to_string(), tab_id.0.to_string());
        if let Some(cwd) = cwd {
            tty_options.working_directory = Some(cwd);
        }
        // TTY-module path: run a specific binary instead of $SHELL.
        // alacritty's `tty::Options.shell` controls what gets exec'd
        // when the PTY child starts.
        if let Some((program, args)) = command {
            tty_options.shell = Some(tty::Shell::new(program, args));
        }

        let pty = tty::new(&tty_options, window_size, 0)
            .expect("terminite: failed to open the PTY");
        // Capture the shell's PID before the Pty is moved into the I/O
        // thread. We also want a tty fd we can `tcgetpgrp` on — and on
        // macOS that's unreliable on the master end, so open the slave
        // ourselves via ptsname (O_NOCTTY keeps it from becoming *our*
        // controlling terminal).
        let shell_pid = pty.child().id() as i32;
        let master_fd = pty.file().as_raw_fd();
        let slave_fd = unsafe {
            let slave_path = libc::ptsname(master_fd);
            if slave_path.is_null() {
                -1
            } else {
                libc::open(slave_path, libc::O_RDONLY | libc::O_NOCTTY)
            }
        };

        let event_loop = TermEventLoop::new(term.clone(), notifier, pty, false, false)
            .expect("terminite: failed to start the PTY event loop");
        let sender = event_loop.channel();
        // Share the sender with the Notifier so PtyWrite events from alacritty
        // (CPR, clipboard, DA, color queries) can write back to the shell.
        if let Ok(mut guard) = pty_sender.lock() {
            *guard = Some(sender.clone());
        }
        let _ = event_loop.spawn();

        Self {
            term,
            sender,
            cell_advance,
            line_height,
            shell_pid,
            master_fd,
            slave_fd,
            reported_cwd,
        }
    }

    #[allow(dead_code)]
    pub fn shell_pid(&self) -> i32 { self.shell_pid }

    /// The shell's current working directory. Prefers the in-band OSC 7
    /// report (set by the `Notifier`); falls back to `proc_pidinfo`, which
    /// macOS TCC blocks for unsigned binaries. macOS zsh emits OSC 7 from
    /// `/etc/zshrc` out of the box, so the report path is the usual one.
    pub fn current_dir(&self) -> Option<PathBuf> {
        if let Ok(guard) = self.reported_cwd.lock() {
            if let Some(path) = guard.as_ref() {
                return Some(path.clone());
            }
        }
        proc_cwd(self.shell_pid)
    }

    /// Foreground process group ID of the tty. When it's the shell, the
    /// user is at a prompt; when it's something else, that PID names the
    /// running process (vim, claude, htop, etc.). We try the slave fd first
    /// (most reliable on macOS) and fall back to the master if the slave
    /// open didn't succeed.
    pub fn foreground_pid(&self) -> Option<i32> {
        for &fd in &[self.slave_fd, self.master_fd] {
            if fd < 0 {
                continue;
            }
            let pgid = unsafe { libc::tcgetpgrp(fd) };
            if pgid > 0 {
                return Some(pgid);
            }
        }
        None
    }

    /// Diagnostic: returns `(slave_fd, master_fd, slave_pgid, master_pgid)`
    /// so we can see what each fd is actually reporting.
    #[allow(dead_code)]
    pub fn pgid_debug(&self) -> (i32, i32, i32, i32) {
        let s_pgid = if self.slave_fd >= 0 {
            unsafe { libc::tcgetpgrp(self.slave_fd) }
        } else {
            -1
        };
        let m_pgid = if self.master_fd >= 0 {
            unsafe { libc::tcgetpgrp(self.master_fd) }
        } else {
            -1
        };
        (self.slave_fd, self.master_fd, s_pgid, m_pgid)
    }

    /// True when something other than a shell is in the foreground.
    ///
    /// Three layers of "not really running" detection, because the raw
    /// `foreground_pid() == shell_pid` test misfires:
    /// 1. Trivially equal — at a prompt, foreground is the shell.
    /// 2. The foreground process's process group matches the shell's PID
    ///    (so it's a subshell that shares the shell's job-control group).
    /// 3. The foreground process's name is one of the known shell binaries
    ///    (zsh, bash, fish, …). Also covers the case where the foreground
    ///    PID is one off from `shell_pid` because zsh forked briefly for
    ///    `.zshrc` and didn't reclaim the tty's foreground PGID.
    /// 4. We couldn't look up the foreground's name at all (zombie /
    ///    permission). We err toward *not* warning here — better than
    ///    over-warning on every Cmd+W.
    pub fn has_active_process(&self) -> bool {
        let Some(fg) = self.foreground_pid() else { return false };
        if fg == self.shell_pid {
            return false;
        }
        if proc_pgid(fg) == Some(self.shell_pid) {
            return false;
        }
        match process_display_name(fg).as_deref() {
            Some("zsh") | Some("bash") | Some("fish") | Some("sh") | Some("dash")
            | Some("ksh") | Some("tcsh") | Some("csh") | Some("login") => false,
            Some(_) => true,
            None => false,
        }
    }

    /// Auto-generated tab title. When we can read the shell's cwd:
    /// `"<process> · <cwd>"`. When we can't (macOS proc_pidinfo VNODE is
    /// blocked or returns empty on recent OS versions): just `"<process>"`.
    /// Better to show less than to show a wrong cwd.
    pub fn compute_auto_title(&self) -> String {
        let fg = self.foreground_pid().unwrap_or(self.shell_pid);
        let name = proc_basename(fg)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "shell".to_string());
        match self.current_dir() {
            Some(p) => format!("{name} · {}", display_cwd(&p)),
            None => name,
        }
    }

    pub fn resize(&self, cols: usize, rows: usize) {
        {
            let mut term = self.term.lock();
            term.resize(GridSize { cols, rows });
        }
        let _ = self.sender.send(Msg::Resize(WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: self.cell_advance as u16,
            cell_height: self.line_height as u16,
        }));
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn mode_flags(&self) -> ModeFlags {
        let term = self.term.lock();
        let m = term.mode();
        ModeFlags {
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            mouse_report_click: m.contains(TermMode::MOUSE_REPORT_CLICK),
            mouse_drag: m.contains(TermMode::MOUSE_DRAG),
            mouse_motion: m.contains(TermMode::MOUSE_MOTION),
            sgr_mouse: m.contains(TermMode::SGR_MOUSE),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            focus_in_out: m.contains(TermMode::FOCUS_IN_OUT),
            app_cursor: m.contains(TermMode::APP_CURSOR),
        }
    }

    /// Find the word boundaries around (line, col). Word chars: alphanumeric
    /// and `_`. If the target cell isn't a word char, returns a single-cell
    /// "word." `line` is in absolute alacritty coordinates.
    pub fn word_at(&self, line: i32, col: usize) -> ((i32, usize), (i32, usize)) {
        let term = self.term.lock();
        let grid = term.grid();
        let cols = grid.columns();
        let history = grid.history_size() as i32;
        let max_line = grid.screen_lines() as i32 - 1;
        let line = line.clamp(-history, max_line);
        if cols == 0 {
            return ((line, col), (line, col));
        }
        let col = col.min(cols - 1);
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let row = &grid[Line(line)];
        let target = row[Column(col)].c;
        if !is_word(target) {
            return ((line, col), (line, col));
        }
        let mut start = col;
        while start > 0 && is_word(row[Column(start - 1)].c) {
            start -= 1;
        }
        let mut end = col;
        while end + 1 < cols && is_word(row[Column(end + 1)].c) {
            end += 1;
        }
        ((line, start), (line, end))
    }

    /// Whole-line selection range. `line` is in absolute alacritty coordinates.
    pub fn line_at(&self, line: i32) -> ((i32, usize), (i32, usize)) {
        let term = self.term.lock();
        let cols = term.grid().columns();
        let end = cols.saturating_sub(1);
        ((line, 0), (line, end))
    }

    /// Whole-buffer selection range — top of history to the last visible row.
    pub fn whole_buffer(&self) -> ((i32, usize), (i32, usize)) {
        let term = self.term.lock();
        let grid = term.grid();
        let history = grid.history_size() as i32;
        let rows = grid.screen_lines() as i32;
        let cols = grid.columns().saturating_sub(1);
        ((-history, 0), (rows - 1, cols))
    }

    /// OSC 8 hyperlink URI at an absolute (line, col), if any.
    pub fn hyperlink_at(&self, line: i32, col: usize) -> Option<String> {
        let term = self.term.lock();
        let grid = term.grid();
        let cols = grid.columns();
        let history = grid.history_size() as i32;
        let max_line = grid.screen_lines() as i32 - 1;
        if cols == 0 || line < -history || line > max_line {
            return None;
        }
        let row = &grid[Line(line)];
        row[Column(col.min(cols - 1))]
            .hyperlink()
            .map(|h| h.uri().to_string())
    }

    /// Search the whole grid (history + viewport) for `needle`, case-
    /// insensitively. Returns absolute `(line, col_start, col_end)` matches
    /// in top-to-bottom order. `col_end` is inclusive.
    pub fn search(&self, needle: &str) -> Vec<(i32, usize, usize)> {
        if needle.is_empty() {
            return Vec::new();
        }
        let needle_lower = needle.to_lowercase();
        let term = self.term.lock();
        let grid = term.grid();
        let cols = grid.columns();
        let rows = grid.screen_lines() as i32;
        let history = grid.history_size() as i32;
        let mut matches = Vec::new();
        for line in -history..rows {
            let row = &grid[Line(line)];
            // Build the row's text plus a column map so a byte offset in the
            // joined string maps back to a cell column.
            let mut text = String::new();
            let mut col_of_byte: Vec<usize> = Vec::new();
            for col in 0..cols {
                let c = row[Column(col)].c;
                for _ in 0..c.len_utf8() {
                    col_of_byte.push(col);
                }
                text.push(c);
            }
            let hay = text.to_lowercase();
            // to_lowercase can change byte length; fall back to a simple
            // char-wise scan when lengths diverge to keep the map valid.
            if hay.len() != text.len() {
                let chars: Vec<char> = text.chars().collect();
                let lneedle: Vec<char> = needle_lower.chars().collect();
                if lneedle.is_empty() {
                    continue;
                }
                let lower: Vec<char> =
                    chars.iter().flat_map(|c| c.to_lowercase()).collect();
                // Approximate: match against the per-char lowercased vec only
                // when it's 1:1 with chars; otherwise skip the row.
                if lower.len() == chars.len() {
                    let mut i = 0;
                    while i + lneedle.len() <= lower.len() {
                        if lower[i..i + lneedle.len()] == lneedle[..] {
                            matches.push((
                                line,
                                i,
                                i + lneedle.len() - 1,
                            ));
                            i += lneedle.len();
                        } else {
                            i += 1;
                        }
                    }
                }
                continue;
            }
            let mut from = 0;
            while let Some(rel) = hay[from..].find(&needle_lower) {
                let byte_start = from + rel;
                let byte_end = byte_start + needle_lower.len() - 1;
                if let (Some(&cs), Some(&ce)) =
                    (col_of_byte.get(byte_start), col_of_byte.get(byte_end))
                {
                    matches.push((line, cs, ce));
                }
                from = byte_start + needle_lower.len();
            }
        }
        matches
    }

    /// Shift the visible viewport up or down through the scrollback.
    pub fn scroll(&self, scroll: Scroll) {
        let mut term = self.term.lock();
        term.scroll_display(scroll);
    }

    /// Scroll so an absolute line lands roughly a third of the way down the
    /// viewport — used to bring a find match into view.
    pub fn scroll_to_line(&self, abs_line: i32, rows: usize) {
        let mut term = self.term.lock();
        let history = term.grid().history_size() as i32;
        let target = (rows as i32 / 3 - abs_line).clamp(0, history);
        let current = term.grid().display_offset() as i32;
        let delta = target - current;
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta));
        }
    }

    /// Current `(display_offset, history_size)` — used by the renderer to
    /// clamp the sub-line pixel offset to the available scroll range.
    pub fn offset_and_history(&self) -> (usize, usize) {
        let term = self.term.lock();
        (term.grid().display_offset(), term.grid().history_size())
    }

    /// Extract the text from a (line, col) range — start..=end inclusive on
    /// both endpoints, in *absolute* alacritty Line coordinates. Wide-char
    /// spacers are skipped; zero-width combining marks are appended to their
    /// base character. Per-row trailing spaces are trimmed before joining
    /// with newlines.
    pub fn extract_text(&self, start: (i32, usize), end: (i32, usize)) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let cols = grid.columns();
        let rows = grid.screen_lines() as i32;
        let history = grid.history_size() as i32;
        if cols == 0 {
            return String::new();
        }
        // Valid alacritty Line range: [-history, rows - 1].
        let (start_line, start_col) = start;
        let (end_line_raw, end_col) = end;
        let start_line = start_line.max(-history);
        let end_line = end_line_raw.min(rows - 1);
        if start_line > end_line {
            return String::new();
        }

        let mut out = String::new();
        for line in start_line..=end_line {
            let row = &grid[Line(line)];
            let col_start = if line == start_line { start_col } else { 0 };
            let col_end_raw = if line == end_line { end_col.saturating_add(1) } else { cols };
            let col_start = col_start.min(cols);
            let col_end = col_end_raw.min(cols);

            let mut line_text = String::new();
            if col_start < col_end {
                for col in col_start..col_end {
                    let cell = &row[Column(col)];
                    if cell
                        .flags
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        continue;
                    }
                    line_text.push(cell.c);
                    if let Some(zw) = cell.zerowidth() {
                        for ch in zw {
                            line_text.push(*ch);
                        }
                    }
                }
            }
            out.push_str(line_text.trim_end());
            if line != end_line {
                out.push('\n');
            }
        }
        out
    }

    /// Snapshot the visible grid: styled text runs, background runs, decoration
    /// runs (underline / double-underline / strikeout), and cursor position.
    /// One lock per frame; all four products emerge in a single walk.
    ///
    /// `display_offset` is the number of lines scrolled up into history. We
    /// shift `Line(N)` lookups by `-display_offset` so the snapshot follows
    /// the viewport when the user scrolls back.
    pub fn snapshot(&self) -> Snapshot {
        let term = self.term.lock();
        let cursor_style = term.cursor_style();
        let grid = term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        let display_offset = grid.display_offset() as i32;
        let history_size = grid.history_size() as i32;
        let cursor_line = grid.cursor.point.line.0 + display_offset;
        let cursor_col = grid.cursor.point.column.0;

        // We always want the renderer to be able to shift the text by a
        // sub-line pixel offset. That requires one *extra* row above the
        // visible viewport — its bottom slides into view as pixel_offset
        // grows. It's only safe to fetch when there's history beyond the
        // current scroll position.
        let has_extra_row = display_offset + 1 <= history_size;
        let start_vis: i32 = if has_extra_row { -1 } else { 0 };

        let mut text_runs: Vec<(String, SpanStyle)> = Vec::new();
        let mut bg_runs: Vec<BgRun> = Vec::new();
        let mut deco_runs: Vec<DecorationRun> = Vec::new();
        let mut link_runs: Vec<LinkRun> = Vec::new();
        let mut current_style = SpanStyle {
            color: Color::rgb(DEFAULT_FG.0, DEFAULT_FG.1, DEFAULT_FG.2),
            bold: false,
            italic: false,
        };
        let mut current_text = String::new();

        for line in start_vis..(rows as i32) {
            let row = &grid[Line(line - display_offset)];

            // Trim trailing plain cells from the text side.
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

            let mut bg_open: Option<(usize, Color)> = None;
            // (start, color, is_double)
            let mut under_open: Option<(usize, Color, bool)> = None;
            let mut strike_open: Option<(usize, Color)> = None;
            // (start, uri)
            let mut link_open: Option<(usize, String)> = None;

            for col in 0..cols {
                let cell = &row[Column(col)];

                // ── Background side: every cell contributes.
                let inverse = cell.flags.contains(Flags::INVERSE);
                let bg_ansi = if inverse { cell.fg } else { cell.bg };
                let bg_color_opt = match bg_ansi {
                    AnsiColor::Named(NamedColor::Background) => None,
                    other => Some(resolve_color(other)),
                };
                // ── Hyperlink side: cells join a run while the URI matches.
                let link_uri = cell.hyperlink().map(|h| h.uri().to_string());
                match (&link_open, &link_uri) {
                    (Some((_, prev)), Some(new)) if prev == new => {}
                    _ => {
                        if let Some((start, uri)) = link_open.take() {
                            link_runs.push(LinkRun {
                                line: line as i32,
                                start_col: start,
                                width: col - start,
                                uri,
                            });
                        }
                        if let Some(uri) = link_uri.clone() {
                            link_open = Some((col, uri));
                        }
                    }
                }

                match (bg_open, bg_color_opt) {
                    (Some((_, prev)), Some(new)) if prev == new => {}
                    _ => {
                        if let Some((start, color)) = bg_open.take() {
                            bg_runs.push(BgRun {
                                line: line as i32,
                                start_col: start,
                                width: col - start,
                                color,
                            });
                        }
                        if let Some(c) = bg_color_opt {
                            bg_open = Some((col, c));
                        }
                    }
                }

                // ── Decorations: underline, double underline, strikeout.
                let text_style = cell_style(cell);
                let text_color = text_style.color;

                let under_state = if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
                    Some((text_color, true))
                } else if cell.flags.contains(Flags::UNDERLINE) {
                    Some((text_color, false))
                } else {
                    None
                };
                match (under_open, under_state) {
                    (Some((_, pc, pd)), Some((nc, nd))) if pc == nc && pd == nd => {}
                    _ => {
                        if let Some((start, color, double)) = under_open.take() {
                            deco_runs.push(DecorationRun {
                                kind: if double {
                                    DecorationKind::DoubleUnderline
                                } else {
                                    DecorationKind::Underline
                                },
                                line: line as i32,
                                start_col: start,
                                width: col - start,
                                color,
                            });
                        }
                        if let Some((c, d)) = under_state {
                            under_open = Some((col, c, d));
                        }
                    }
                }

                let strike_state = if cell.flags.contains(Flags::STRIKEOUT) {
                    Some(text_color)
                } else {
                    None
                };
                match (strike_open, strike_state) {
                    (Some((_, pc)), Some(nc)) if pc == nc => {}
                    _ => {
                        if let Some((start, color)) = strike_open.take() {
                            deco_runs.push(DecorationRun {
                                kind: DecorationKind::Strikeout,
                                line: line as i32,
                                start_col: start,
                                width: col - start,
                                color,
                            });
                        }
                        if let Some(c) = strike_state {
                            strike_open = Some((col, c));
                        }
                    }
                }

                // ── Text side: skip spacers, stop past last content.
                if col >= last_content {
                    continue;
                }
                let is_spacer = cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
                if is_spacer {
                    continue;
                }
                if !current_text.is_empty() && text_style != current_style {
                    text_runs.push((std::mem::take(&mut current_text), current_style));
                }
                if current_text.is_empty() {
                    current_style = text_style;
                }
                current_text.push(cell.c);
                if let Some(zw) = cell.zerowidth() {
                    for ch in zw {
                        current_text.push(*ch);
                    }
                }
            }

            // Flush open runs at end-of-row.
            if let Some((start, color)) = bg_open.take() {
                bg_runs.push(BgRun {
                    line: line as i32,
                    start_col: start,
                    width: cols - start,
                    color,
                });
            }
            if let Some((start, color, double)) = under_open.take() {
                deco_runs.push(DecorationRun {
                    kind: if double {
                        DecorationKind::DoubleUnderline
                    } else {
                        DecorationKind::Underline
                    },
                    line: line as i32,
                    start_col: start,
                    width: cols - start,
                    color,
                });
            }
            if let Some((start, color)) = strike_open.take() {
                deco_runs.push(DecorationRun {
                    kind: DecorationKind::Strikeout,
                    line: line as i32,
                    start_col: start,
                    width: cols - start,
                    color,
                });
            }
            if let Some((start, uri)) = link_open.take() {
                link_runs.push(LinkRun {
                    line: line as i32,
                    start_col: start,
                    width: cols - start,
                    uri,
                });
            }
            current_text.push('\n');
        }
        if !current_text.is_empty() {
            text_runs.push((current_text, current_style));
        }

        Snapshot {
            text_runs,
            bg_runs,
            deco_runs,
            link_runs,
            cursor_line,
            cursor_col,
            cursor_shape: cursor_style.shape,
            cursor_blinking: cursor_style.blinking,
            has_extra_row,
        }
    }
}

/// Resolve a process's current working directory via the OS. macOS-only for
/// now; other platforms could read `/proc/<pid>/cwd` etc.
///
/// **Known failure on recent macOS:** `proc_pidinfo(PROC_PIDVNODEPATHINFO)`
/// returns `EPERM` for unsigned binaries (e.g. `cargo run` output) reading
/// another process's cwd. We surface that as `None`; the real fix is OSC 7
/// support in the parser (tracked separately — needs a vte +
/// alacritty_terminal fork because vte 0.15 doesn't dispatch OSC 7).
#[cfg(target_os = "macos")]
/// Parse the path out of an OSC 7 `file://host/path` URI. The host segment
/// is dropped (for a local shell it's just the machine name; for a remote
/// one the path won't resolve locally anyway, which is harmless — an
/// invalid working directory is ignored on the next `new_tab`).
fn parse_osc7_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.trim().strip_prefix("file://")?;
    // `rest` is `host/path` or `/path` (empty host); the path starts at the
    // first slash.
    let slash = rest.find('/')?;
    Some(PathBuf::from(percent_decode(&rest[slash..])))
}

/// Minimal percent-decoding for OSC 7 paths (spaces etc. arrive as `%20`).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Translate a cell into its text visual style, honoring inverse, dim, hidden.
pub fn cell_style(cell: &Cell) -> SpanStyle {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7_path_parsing() {
        // host segment dropped, plain path
        assert_eq!(
            parse_osc7_path("file://mac.local/Users/d/dev"),
            Some(PathBuf::from("/Users/d/dev")),
        );
        // empty host (file:///...)
        assert_eq!(
            parse_osc7_path("file:///Users/d"),
            Some(PathBuf::from("/Users/d")),
        );
        // percent-encoded space
        assert_eq!(
            parse_osc7_path("file://h/Users/d/My%20Code"),
            Some(PathBuf::from("/Users/d/My Code")),
        );
        // trailing whitespace tolerated
        assert_eq!(
            parse_osc7_path("file://h/tmp\n"),
            Some(PathBuf::from("/tmp")),
        );
        // not OSC 7 / malformed
        assert_eq!(parse_osc7_path("https://example.com"), None);
        assert_eq!(parse_osc7_path("file://hostonly"), None);
    }

    #[test]
    fn percent_decode_basics() {
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%"); // dangling % left as-is
        assert_eq!(percent_decode("%2Fx%2Fy"), "/x/y");
    }
}
