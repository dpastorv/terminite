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
use crate::{TabId, UserEvent, LINE_HEIGHT};

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

/// One frame's worth of rendering data extracted from the cell grid.
pub struct Snapshot {
    pub text_runs: Vec<(String, SpanStyle)>,
    pub bg_runs: Vec<BgRun>,
    pub deco_runs: Vec<DecorationRun>,
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
    #[allow(dead_code)]
    pub alt_screen: bool,
    pub focus_in_out: bool,
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
}

impl Drop for LiveTerm {
    fn drop(&mut self) {
        if self.slave_fd >= 0 {
            unsafe { libc::close(self.slave_fd) };
        }
    }
}

impl LiveTerm {
    pub fn new(
        cols: usize,
        rows: usize,
        cell_advance: f32,
        proxy: EventLoopProxy<UserEvent>,
        tab_id: TabId,
        cwd: Option<PathBuf>,
    ) -> Self {
        let pty_sender: Arc<Mutex<Option<EventLoopSender>>> = Arc::new(Mutex::new(None));
        let notifier = Notifier {
            proxy,
            pty_sender: pty_sender.clone(),
            tab_id,
        };
        let size = GridSize { cols, rows };
        let term = Term::new(TermConfig::default(), &size, notifier.clone());
        let term = Arc::new(FairMutex::new(term));

        let window_size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: cell_advance as u16,
            cell_height: LINE_HEIGHT as u16,
        };

        // Explicit terminal capabilities for the shell.
        let mut tty_options = tty::Options::default();
        tty_options
            .env
            .insert("TERM".to_string(), "xterm-256color".to_string());
        tty_options
            .env
            .insert("COLORTERM".to_string(), "truecolor".to_string());
        if let Some(cwd) = cwd {
            tty_options.working_directory = Some(cwd);
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
            shell_pid,
            master_fd,
            slave_fd,
        }
    }

    pub fn shell_pid(&self) -> i32 { self.shell_pid }

    /// Query the OS for the shell's current working directory. macOS uses
    /// `proc_pidinfo(PROC_PIDVNODEPATHINFO)`; other platforms return None
    /// until we add an equivalent path (Linux: `/proc/<pid>/cwd`).
    pub fn current_dir(&self) -> Option<PathBuf> {
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

    /// True when something other than the shell is in the foreground —
    /// gates the "close confirmation" dialog. If we can't read the tty
    /// (somehow), don't warn: better to occasionally over-trust than to
    /// pop the dialog on every prompt.
    pub fn has_active_process(&self) -> bool {
        match self.foreground_pid() {
            Some(pid) => pid != self.shell_pid,
            None => false,
        }
    }

    /// Auto-generated tab title: `"<process> · <cwd-basename>"`. Uses the
    /// foreground process when it's not the shell, otherwise the shell name.
    /// Cwd shows the last path component, with `~` for HOME. We use a middle
    /// dot (U+00B7) — it lives in Latin-1 and renders in every monospace
    /// font we'll ever ship, where the em-dash sometimes doesn't.
    pub fn compute_auto_title(&self) -> String {
        let fg = self.foreground_pid().unwrap_or(self.shell_pid);
        let name = proc_basename(fg)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "shell".to_string());
        let cwd_str = self
            .current_dir()
            .map(|p| display_cwd(&p))
            .unwrap_or_else(|| "~".to_string());
        format!("{name} · {cwd_str}")
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
            cell_height: LINE_HEIGHT as u16,
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

    /// Shift the visible viewport up or down through the scrollback.
    pub fn scroll(&self, scroll: Scroll) {
        let mut term = self.term.lock();
        term.scroll_display(scroll);
    }

    /// Current `(display_offset, history_size)` — used by the renderer to
    /// clamp the sub-line pixel offset to the available scroll range.
    pub fn offset_and_history(&self) -> (usize, usize) {
        let term = self.term.lock();
        (term.grid().display_offset(), term.grid().history_size())
    }

    /// Return up to 40 characters of the topmost visible row's content, for
    /// diagnostic logging. Trimmed.
    pub fn debug_top_row(&self) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let display_offset = grid.display_offset() as i32;
        let row = &grid[Line(0 - display_offset)];
        let cap = grid.columns().min(40);
        let mut s = String::with_capacity(cap);
        for col in 0..cap {
            s.push(row[Column(col)].c);
        }
        s.trim().to_string()
    }

    /// Diagnostic dump: cursor position plus the content of the last 3 visible
    /// rows (the cursor row and the two above), so we can see whether
    /// snapshot is missing the bottom of the viewport.
    pub fn debug_bottom_strip(&self, rows: usize) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let display_offset = grid.display_offset() as i32;
        let cursor = grid.cursor.point;
        let take = |line: i32| -> String {
            let row = &grid[Line(line - display_offset)];
            let cap = grid.columns().min(40);
            let mut s = String::with_capacity(cap);
            for col in 0..cap {
                s.push(row[Column(col)].c);
            }
            s.trim().to_string()
        };
        let last = rows as i32 - 1;
        format!(
            "cursor=Line({}),Col({}) bottom3='{}' / '{}' / '{}'",
            cursor.line.0,
            cursor.column.0,
            take(last - 2),
            take(last - 1),
            take(last),
        )
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

            for col in 0..cols {
                let cell = &row[Column(col)];

                // ── Background side: every cell contributes.
                let inverse = cell.flags.contains(Flags::INVERSE);
                let bg_ansi = if inverse { cell.fg } else { cell.bg };
                let bg_color_opt = match bg_ansi {
                    AnsiColor::Named(NamedColor::Background) => None,
                    other => Some(resolve_color(other)),
                };
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
            current_text.push('\n');
        }
        if !current_text.is_empty() {
            text_runs.push((current_text, current_style));
        }

        Snapshot {
            text_runs,
            bg_runs,
            deco_runs,
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
#[cfg(target_os = "macos")]
fn proc_cwd(pid: i32) -> Option<PathBuf> {
    use std::mem::MaybeUninit;
    let mut info: MaybeUninit<libc::proc_vnodepathinfo> = MaybeUninit::uninit();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr() as *mut libc::c_void,
            size,
        )
    };
    if n <= 0 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let raw = info.pvi_cdir.vip_path;
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(raw.as_ptr() as *const u8, raw.len())
    };
    let nul = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let s = std::str::from_utf8(&bytes[..nul]).ok()?;
    if s.is_empty() {
        return None;
    }
    Some(PathBuf::from(s))
}

#[cfg(not(target_os = "macos"))]
fn proc_cwd(_pid: i32) -> Option<PathBuf> {
    None
}

/// Short process name (`comm`), e.g. `"zsh"`, `"vim"`, `"claude"`. Uses
/// macOS's `proc_name` rather than the executable path's basename — the
/// latter sometimes lands on a version-numbered directory ("2.1.146") when
/// a CLI is installed in a versioned layout, which makes terrible tab labels.
#[cfg(target_os = "macos")]
fn proc_basename(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe {
        libc::proc_name(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32)
    };
    if n <= 0 {
        return None;
    }
    let len = n as usize;
    let bytes = &buf[..len];
    let nul = bytes.iter().position(|&b| b == 0).unwrap_or(len);
    let s = std::str::from_utf8(&bytes[..nul]).ok()?;
    if s.is_empty() {
        return None;
    }
    Some(s.to_string())
}

#[cfg(not(target_os = "macos"))]
fn proc_basename(_pid: i32) -> Option<String> {
    None
}

/// Human-readable cwd: `~` for HOME, `~/foo/bar` for paths under HOME, last
/// path component otherwise.
fn display_cwd(cwd: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if cwd == home {
            return "~".to_string();
        }
        if let Ok(rel) = cwd.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    cwd.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| cwd.display().to_string())
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
