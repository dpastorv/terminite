//! Tab, tab animation, and tab-content kind.

use super::*;

/// One shell — a PTY plus its title and view state. The unit you tab
/// between inside a pane. (Pre-inversion this was `Pane`; the window now
/// owns the pane tree directly and each pane leaf owns a `Vec<Tab>`.)
pub(super) struct Tab {
    pub(super) id: TabId,
    pub(super) title: String,
    /// Tab-bar label buffer; rebuilt whenever the displayed title changes.
    pub(super) title_buffer: Buffer,
    /// Shell-set title (OSC 0/1/2) — when present, wins over the auto title.
    pub(super) shell_title: Option<String>,
    /// Last auto-title we computed; rebuild the buffer only on changes.
    pub(super) last_auto_title: String,
    pub(super) live_term: LiveTerm,
    /// This tab's own cosmic-text content buffer.
    pub(super) text_buffer: Buffer,
    /// Per-cell glyphs from the last snapshot, for the grid-aligned render path.
    /// Rebuilt each frame the active tab is drawn; cheap (the shaping is cached
    /// in the renderer's glyph cache, this is just placement data).
    pub(super) cell_glyphs: Vec<crate::term::CellGlyph>,
    /// Grid size this tab's PTY is currently sized to.
    pub(super) cols: usize,
    pub(super) rows: usize,
    pub(super) pixel_offset: f32,
    pub(super) selection: Option<Selection>,
    pub(super) dragging: bool,
    pub(super) last_drag_mouse_pos: (f32, f32),
    pub(super) last_click: Option<(Instant, (i32, usize), u8)>,
    pub(super) last_text_runs: Vec<(String, SpanStyle)>,
    /// Whether `text_buffer` currently holds `last_text_runs`'s content.
    pub(super) buffer_dirty: bool,
    pub(super) autoscroll_dir: Option<i32>,
    /// Most recent image from a Kitty `a=T` (transmit+display). v1 shows
    /// one image per tab at the pane's top-left, scaled to fit. Dropped
    /// when the tab drops (closes the GPU texture too).
    pub(super) image: Option<TextureImage>,
    /// Set when an animated image (multi-frame GIF) is showing. Holds
    /// pre-uploaded per-frame textures + timing; the render path picks
    /// the current frame and the wakeup scheduler keeps the loop
    /// ticking. Mutually exclusive with `image` in practice — set_image
    /// clears both before installing one or the other.
    pub(super) animation: Option<TabAnimation>,
    /// Per-tab block Model — populated from OSC 133 marks. Phase 2's
    /// shared coordinate system (`B7`) for the pair lives here.
    pub(super) blocks: BlockStore,
    /// Which content type this tab is currently showing. Shell is the
    /// default and the only one with a live PTY behind it; other
    /// kinds suppress the shell render path and substitute their
    /// own. Bundle 6 step 1 — the dropdown stays inside built-ins.
    pub(super) kind: TabContentKind,
    /// Lazily-shaped buffer for non-shell content (e.g., Welcome).
    /// `None` when the kind is Shell or the buffer hasn't been built
    /// for the current size. Rebuilt on resize.
    pub(super) content_buffer: Option<Buffer>,
    /// Live *data* module session — module talks JSON via stdio,
    /// pushes `set_text` frames. None when not in a data module.
    pub(super) module_session: Option<crate::modules::ModuleSession>,
    /// Cached body a data module last asked us to render.
    pub(super) last_module_body: String,
    /// Live *tty* module — a second LiveTerm pointed at the module's
    /// binary instead of the user's shell. Rendered via the same
    /// vte/alacritty path as shells; input flows here for the
    /// duration the pane shows the module. None when not in a TTY
    /// module.
    pub(super) module_pty: Option<LiveTerm>,
    /// Palette index for the tab's color band. `0` is none. Set via
    /// the right-click "Tab color" item; cycles through the palette.
    pub(super) color_idx: u8,
    /// Vertical scroll offset (pixels) for data-module content. Reset
    /// to 0 whenever the body changes (unless `scroll_to_line` was
    /// supplied). Clamped against laid-out content height in the
    /// render path. Only data modules use this; shells have their
    /// own scrollback and TTY modules drive their own buffer.
    pub(super) module_scroll_y: f32,
    /// "Please ensure this 0-indexed source line is visible after the
    /// next render" — set by `SetText { scroll_to_line: Some(N) }`,
    /// consumed (and cleared) by `render_non_shell_pane` once it
    /// knows the laid-out content height. Lets nav keep its cursor
    /// on screen as the user moves it.
    pub(super) pending_ensure_visible: Option<u32>,
    /// Host-rendered cursor position for a data module (Editor). The
    /// render path draws a block cursor at (line, col) in the body
    /// using the same color + blink as a shell cursor. Stays `None`
    /// for modules with no cursor (Preview, Nav, …).
    pub(super) module_cursor: Option<(u32, u32)>,
    /// Per-source-line gutter labels — empty string = no label for
    /// that line. Editor sends "1", "2", … for content lines and
    /// "" for header/prompt/blank. Rendered host-side at the y of
    /// each line's *first* layout run only, in a dim color, to the
    /// left of content. Content shifts right by the widest label's
    /// width when present.
    pub(super) module_gutter: Option<Vec<String>>,
    /// Lazily-shaped buffer for the gutter labels themselves —
    /// rebuilt whenever `module_gutter` changes. Holds the joined
    /// gutter strings; the render path places it per first-run y.
    /// `None` when there's no gutter to render.
    pub(super) gutter_buffer: Option<Buffer>,
    /// 0-indexed source line painted with a subtle background rect
    /// — Nav's selection row, Editor's cursor row. Spans all wrap
    /// segments of that line for continuous highlight.
    pub(super) module_highlight_line: Option<u32>,
    /// Syntect language token (e.g. "rs", "py") for this body, or
    /// `None` for plain rendering. Editor sends a value derived
    /// from the file extension; Nav / Preview leave it `None`.
    pub(super) module_language: Option<String>,
    /// Cached per-source-line color spans from syntect. Recomputed
    /// on body or language change; reused otherwise so steady-state
    /// cursor moves stay cheap.
    pub(super) module_highlights: Option<crate::highlight::LineSpans>,
    /// Last `publish_focus` path this tab's module saw — persisted
    /// in the layout file so Editor reopens the same file on
    /// restore. Updated whenever the host sends a focus event to
    /// this tab's module session.
    pub(super) last_focused_path: Option<String>,
    /// Multi-click bookkeeping for data-module panes — mirrors the
    /// shell-tab last_click pattern with body coordinates. Reset
    /// (or rolled over) by `dispatch_data_module_click`.
    pub(super) last_module_click: Option<(Instant, u32, u32, u8)>,
}

/// Hard cap on a single `set_text` body. A 16 MB body is already past
/// what glyphon can shape interactively; anything larger is almost
/// certainly a runaway module. We log + drop the message rather than
/// rebuild the content buffer for it.
pub(super) const MAX_MODULE_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Per-tab animation state for multi-frame images (GIFs). Frames are
/// uploaded to the GPU once at decode time; the render path picks the
/// current one off the cumulative-delay table without copying.
///
/// Bounded by [`crate::images::MAX_ANIMATED_BYTES`] and
/// [`crate::images::MAX_ANIMATED_FRAMES`] upstream — by the time we
/// allocate textures the frame list is already capped.
pub(super) struct TabAnimation {
    /// Frame textures in playback order. `frames.len() == cumulative.len()`.
    pub(super) frames: Vec<TextureImage>,
    /// Display dimensions (max width/height across frames). Frames in
    /// a GIF can technically vary in size; we render every frame
    /// scaled into the same envelope so the pane doesn't jitter.
    pub(super) width: u32,
    pub(super) height: u32,
    /// `cumulative[i]` is the ms timestamp at which frame `i` *ends*.
    /// Lookup is a partition_point against `elapsed % total_ms`.
    pub(super) cumulative: Vec<u64>,
    /// Total loop length in ms (== `cumulative.last()`).
    pub(super) total_ms: u64,
    /// Wall-clock origin for the running loop. Stays fixed; the
    /// render path reads `started_at.elapsed()` for the position.
    pub(super) started_at: Instant,
}

impl TabAnimation {
    pub(super) fn current_index(&self) -> usize {
        if self.total_ms == 0 || self.frames.is_empty() {
            return 0;
        }
        let elapsed = self.started_at.elapsed().as_millis() as u64 % self.total_ms;
        self.cumulative
            .partition_point(|c| *c <= elapsed)
            .min(self.frames.len() - 1)
    }

    pub(super) fn current_frame(&self) -> &TextureImage {
        &self.frames[self.current_index()]
    }

    /// Wall-clock instant when the next frame should appear. The main
    /// loop uses this for `ControlFlow::WaitUntil` so we wake exactly
    /// at the frame boundary, no per-tick polling.
    pub(super) fn next_wakeup(&self) -> Option<Instant> {
        if self.total_ms == 0 || self.frames.is_empty() {
            return None;
        }
        let total_elapsed = self.started_at.elapsed().as_millis() as u64;
        let phase = total_elapsed % self.total_ms;
        let loops = total_elapsed / self.total_ms;
        let idx = self.cumulative.partition_point(|c| *c <= phase);
        let boundary = *self.cumulative.get(idx).unwrap_or(&self.total_ms);
        let absolute_ms = loops * self.total_ms + boundary;
        let offset_ms = absolute_ms.saturating_sub(total_elapsed);
        Some(Instant::now() + Duration::from_millis(offset_ms))
    }
}

/// What a tab currently shows. `Shell` has a live PTY behind it;
/// `Welcome` is a built-in static card; `Module(id)` is an
/// externally-registered module (step 2a: placeholder render only;
/// step 2b spawns the process and wires IPC).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum TabContentKind {
    Shell,
    Welcome,
    Module(String),
}

impl TabContentKind {
    /// Stable string key for label-buffer lookup. Built-ins get
    /// hard-coded strings; modules use their id.
    pub(super) fn key(&self) -> &str {
        match self {
            TabContentKind::Shell => "shell",
            TabContentKind::Welcome => "welcome",
            TabContentKind::Module(id) => id.as_str(),
        }
    }
}

impl Tab {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        id: TabId,
        title: String,
        title_buffer: Buffer,
        live_term: LiveTerm,
        text_buffer: Buffer,
        cols: usize,
        rows: usize,
    ) -> Self {
        Self {
            id,
            title,
            title_buffer,
            shell_title: None,
            last_auto_title: String::new(),
            live_term,
            text_buffer,
            cols,
            rows,
            pixel_offset: 0.0,
            selection: None,
            dragging: false,
            last_drag_mouse_pos: (0.0, 0.0),
            last_click: None,
            cell_glyphs: Vec::new(),
            last_text_runs: Vec::new(),
            buffer_dirty: true,
            autoscroll_dir: None,
            image: None,
            animation: None,
            blocks: BlockStore::new(),
            kind: TabContentKind::Shell,
            content_buffer: None,
            module_session: None,
            last_module_body: String::new(),
            module_pty: None,
            color_idx: 0,
            module_scroll_y: 0.0,
            pending_ensure_visible: None,
            module_cursor: None,
            module_gutter: None,
            gutter_buffer: None,
            module_highlight_line: None,
            module_language: None,
            module_highlights: None,
            last_focused_path: None,
            last_module_click: None,
        }
    }

    /// The LiveTerm that should drive snapshot / input for this tab.
    /// TTY modules supply their own; everything else uses the tab's
    /// permanent shell. Preserves "non-destructive switch" — the
    /// shell stays alive in `live_term` even while a TTY module is
    /// running.
    pub(super) fn active_term(&self) -> &LiveTerm {
        match (&self.kind, self.module_pty.as_ref()) {
            (TabContentKind::Module(_), Some(pty)) => pty,
            _ => &self.live_term,
        }
    }

    pub(super) fn active_term_mut(&mut self) -> &mut LiveTerm {
        if matches!(self.kind, TabContentKind::Module(_)) && self.module_pty.is_some() {
            self.module_pty.as_mut().unwrap()
        } else {
            &mut self.live_term
        }
    }
}



// ── moved from mod.rs ───────────────────────────────

impl Renderer {
    pub fn new_tab(&mut self) {
        // Inherit the active tab's shell cwd into the new shell.
        // Inherit the active tab's shell cwd into the new shell.
        let cwd = self.active_tab_ref().live_term.current_dir();
        let id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        // The new tab joins the active pane, sized to that pane's rect.
        let rect = self.active_pane_rect();
        let (cols, rows) = pane_grid(rect, self.cell_advance, self.line_height, self.pad, self.tab_bar_height);
        let live_term = LiveTerm::new(
            cols,
            rows,
            self.cell_advance,
            self.line_height,
            self.proxy.clone(),
            id,
            cwd,
            self.config.scrollback,
        );
        let title = "terminite".to_string();
        let title_buf = make_title_buffer(
            &mut self.font_system,
            &title,
            self.tab_font_size,
            self.tab_line_h,
            self.tab_max_width,
        );
        let text_buf = make_content_buffer(
            &mut self.font_system,
            self.cell_advance,
            self.line_height,
            self.font_size,
            &self.font_family,
            rect.w,
            rect.h,
        );
        let tab = Tab::new(id, title, title_buf, live_term, text_buf, cols, rows);
        let pane = self.active_pane_mut();
        pane.tabs.push(tab);
        pane.active_tab = pane.tabs.len() - 1;
        self.sync_active_grid();
        self.window.set_title(&self.active_tab_ref().title);
        self.window.request_redraw();
        self.persist_layout();
    }

    /// Request closing the active tab. If a non-shell process is in the
    /// foreground, opens an in-window modal — the caller observes `false`
    /// (didn't close) and the actual close happens when the user confirms.
    /// Otherwise closes immediately. Returns true if the window should
    /// exit (no tabs remain).
    pub fn close_active_tab(&mut self) -> bool {
        if self.modal.is_some() {
            return false;
        }
        let live = &self.active_tab_mut().live_term;
        if live.has_active_process() {
            let proc_name = live
                .foreground_pid()
                .and_then(proc_name_of)
                .unwrap_or_else(|| "A process".to_string());
            let title = "Close tab?".to_string();
            let body = format!("{proc_name} is running in this tab.");
            self.open_modal(ModalAction::CloseTab, title, body, "Cancel", "Close");
            return false;
        }
        self.do_close_active_tab()
    }

    /// Close the active tab. If it was the pane's last tab the pane closes
    /// too; if that was the window's last pane, returns true (window exits).
    pub(super) fn do_close_active_tab(&mut self) -> bool {
        let pane = self.active_pane_mut();
        if pane.tabs.len() > 1 {
            let idx = pane.active_tab;
            pane.tabs.remove(idx);
            if pane.active_tab >= pane.tabs.len() {
                pane.active_tab = pane.tabs.len() - 1;
            }
            self.sync_active_grid();
            self.window.set_title(&self.active_tab_ref().title);
            self.window.request_redraw();
            self.persist_layout();
            return false;
        }
        // Last tab in this pane — close the pane itself.
        // close_active_pane already persists on its own.
        self.close_active_pane()
    }

    /// Switch the active pane to one of its tabs by index.
    pub fn switch_to_tab(&mut self, idx: usize) {
        let pane = self.active_pane_mut();
        if idx >= pane.tabs.len() || idx == pane.active_tab {
            return;
        }
        // Drop the prior tab's selection + drag state — same reason
        // we clear them on a pane switch. Otherwise a stale highlight
        // (and worse, a silent "your Cmd+C did nothing, clipboard
        // kept tab N's text") survives the switch.
        {
            let prior = pane.active_tab_mut();
            prior.selection = None;
            prior.dragging = false;
        }
        pane.active_tab = idx;
        self.sync_active_grid();
        self.window.set_title(&self.active_tab_ref().title);
        self.window.request_redraw();
    }

    pub fn next_tab(&mut self) {
        let pane = self.active_pane_ref();
        if pane.tabs.len() <= 1 {
            return;
        }
        let idx = (pane.active_tab + 1) % pane.tabs.len();
        self.switch_to_tab(idx);
    }

    pub fn prev_tab(&mut self) {
        let pane = self.active_pane_ref();
        if pane.tabs.len() <= 1 {
            return;
        }
        let idx = if pane.active_tab == 0 {
            pane.tabs.len() - 1
        } else {
            pane.active_tab - 1
        };
        self.switch_to_tab(idx);
    }

}
