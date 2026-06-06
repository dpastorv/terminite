//! Terminal I/O — bell, APC, shell integration, writes, tab kind/title.

use super::*;

/// The OS window title — the application's identity. Per-tab context (the
/// shell, cwd, agent, or module) lives in terminite's own tab bar, so the
/// title bar wears the app name rather than the active shell's process name.
pub(super) const WINDOW_TITLE: &str = "terminite";

impl Renderer {
    pub fn ring_bell(&mut self, _tab_id: TabId) {
        // `bell_style = "none"` — the BEL does nothing.
        if self.config.bell_style == BellStyle::Silent {
            return;
        }
        // The flash overlay is window-wide for now; we don't visually
        // distinguish *which* tab rang the bell. Coalesce: a hostile `\a`
        // storm just extends the deadline; we don't touch the renderer state
        // otherwise and we don't re-request a redraw if the flash is already
        // on screen. The expiry render is scheduled via the main loop's
        // `WaitUntil(next_wakeup())`.
        let now = Instant::now();
        let was_active = self.bell_flash_until.is_some_and(|t| t > now);
        self.bell_flash_until = Some(now + BELL_DURATION);
        if !was_active {
            self.window.request_redraw();
        }
    }

    /// Apply a tab title from a shell `OSC 0/1/2`. This wins over the
    /// auto-title for as long as the shell keeps setting one. An empty or
    /// whitespace-only title is treated as "unset" — the auto-title takes
    /// over again on the next render. This is what TUIs that emit an empty
    /// title or a ResetTitle escape on exit (claude, vim, ssh) expect.
    /// Parse + decode + display a Kitty APC payload from `tab_id`. v1
    /// recognises only `a=T` (transmit-and-display); the image replaces
    /// any prior one on that tab and renders at the pane's top-left,
    /// scaled to fit. Bounded throughout — the parser caps per-image
    /// decoded bytes, the texture holds bytes equal to the cap at worst,
    /// and the prior image's GPU memory is freed when overwritten.
    pub fn handle_apc(&mut self, tab_id: TabId, data: &[u8]) {
        let Some(cmd) = images::parse_kitty(data) else { return };
        // Only the transmit-and-display action shows in v1; transmit-only,
        // display-by-id, delete and query are no-ops until later commits.
        if !matches!(cmd.action, Action::TransmitDisplay) {
            return;
        }
        let Some(image) = images::decode_image(cmd.format, cmd.width, cmd.height, &cmd.payload)
        else { return };
        let tex = self.texture_renderer.upload(&self.device, &self.queue, &image);

        let mut tabs: Vec<&mut Tab> = Vec::new();
        self.root.as_mut().expect("pane tree present").all_tabs_mut(&mut tabs);
        if let Some(tab) = tabs.into_iter().find(|t| t.id == tab_id) {
            tab.image = Some(tex);
        }
        self.window.request_redraw();
    }

    /// Apply one OSC 133 shell-integration mark to a tab's block store.
    /// The block Model is Phase 2's spine — `Bn` labels render in the
    /// pane gutter from here. Bounded: per-tab block cap at
    /// `MAX_BLOCKS_PER_TAB`; label buffers are tiny pre-shaped strings.
    pub fn handle_shell_integration(
        &mut self,
        tab_id: TabId,
        kind: char,
        exit: Option<i32>,
        line: i32,
    ) {
        // Scale the new block's label to its owning pane's font scale
        // so the label sits flush with content rows at that pane's
        // size. Content-anchored chrome stays consistent with content.
        let scale = self.scale_for_tab(tab_id);
        let label_font_size = crate::blocks::LABEL_FONT_SIZE * scale;
        let label_line_h = (crate::blocks::LABEL_LINE_H * scale).max(1.0);
        let effect = {
            let mut tabs: Vec<&mut Tab> = Vec::new();
            self.root.as_mut().expect("pane tree present").all_tabs_mut(&mut tabs);
            tabs.into_iter().find(|t| t.id == tab_id).map(|tab| {
                tab.blocks.on_mark(
                    kind,
                    exit,
                    line,
                    &mut self.font_system,
                    label_font_size,
                    label_line_h,
                )
            })
        };
        // Fan out to the proto subscriber. `closed` fires before `opened`
        // — that's the order they happened on an A-after-no-D path.
        if let Some(effect) = effect {
            if let Some((block_id, exit_code)) = effect.closed {
                self.proto_emit_event(crate::proto::EventPayload::BlockClosed {
                    tab_id: tab_id.0,
                    block_id,
                    exit_code,
                });
            }
            if let Some(block_id) = effect.opened {
                self.proto_emit_event(crate::proto::EventPayload::BlockOpened {
                    tab_id: tab_id.0,
                    block_id,
                });
            }
        }
        self.window.request_redraw();
    }


    pub fn set_tab_title(&mut self, tab_id: TabId, title: String) {
        if title.trim().is_empty() {
            let mut tabs: Vec<&mut Tab> = Vec::new();
            self.root.as_mut().expect("pane tree present").all_tabs_mut(&mut tabs);
            if let Some(tab) = tabs.into_iter().find(|t| t.id == tab_id) {
                tab.shell_title = None;
                // Force refresh_auto_titles to rebuild on the next render.
                tab.last_auto_title.clear();
            }
            self.window.request_redraw();
            return;
        }
        let new_buf = make_title_buffer(
            &mut self.font_system,
            &title,
            self.tab_font_size,
            self.tab_line_h,
            self.tab_max_width,
        );
        let active_id = self.active_tab_ref().id;
        {
            let mut tabs: Vec<&mut Tab> = Vec::new();
            self.root.as_mut().expect("pane tree present").all_tabs_mut(&mut tabs);
            if let Some(tab) = tabs.into_iter().find(|t| t.id == tab_id) {
                tab.shell_title = Some(title.clone());
                tab.title = title;
                tab.title_buffer = new_buf;
            }
        }
        if tab_id == active_id {
            // The OS window title is the app identity; per-tab context (shell,
            // cwd, agent, module) lives in the in-app tab bar.
            self.window.set_title(WINDOW_TITLE);
        }
    }

    /// Refresh every tab's auto-title from the OS. Each call does a handful
    /// of `proc_*` syscalls per tab, so it's throttled well below the render
    /// rate — a title only changes on `cd` or a foreground-process switch,
    /// neither of which needs sub-second latency. Tabs that received an OSC
    /// title from their shell keep that.
    pub(super) fn refresh_auto_titles(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_title_refresh) < Duration::from_millis(500) {
            return;
        }
        self.last_title_refresh = now;
        let mut tabs: Vec<&mut Tab> = Vec::new();
        self.root.as_mut().expect("pane tree present").all_tabs_mut(&mut tabs);
        for tab in tabs {
            // Resolve the title this tab should show.
            let new_auto = match &tab.shell_title {
                // An informative shell-set title wins — don't touch it. The one
                // exception: a title that's merely the cwd folder name (codex
                // names its terminal after the directory, so its tab reads
                // "terminite") tells you nothing about who's here. If the room
                // knows which agent sits in this pane, label the seat by the
                // agent instead. Every other CLI's title (✳ Claude Code, Kimi
                // Code, Qwen - terminite) differs from the folder and is kept.
                Some(t) => {
                    let is_folder_name = tab
                        .live_term
                        .current_dir()
                        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
                        .is_some_and(|folder| folder == *t);
                    match self.roster.slug_for_pane(tab.id.0) {
                        Some(slug) if is_folder_name => {
                            super::proto::agent_name_from_slug(&slug)
                        }
                        _ => continue,
                    }
                }
                None => tab.live_term.compute_auto_title(),
            };
            if new_auto != tab.last_auto_title {
                tab.title_buffer = make_title_buffer(
                    &mut self.font_system,
                    &new_auto,
                    self.tab_font_size,
                    self.tab_line_h,
                    self.tab_max_width,
                );
                tab.last_auto_title = new_auto.clone();
                tab.title = new_auto;
            }
        }
        // The window title stays the app name; only the tab bar reflects
        // these per-tab titles.
    }

    /// Write bytes to the active tab's PTY (keyboard input path).
    /// Stamp that the human just typed into the active pane — the PTY floor's
    /// "actively in use" signal (so it holds, instead of stomping your input).
    pub(super) fn note_human_input(&mut self) {
        let id = self.active_tab_ref().id.0;
        self.last_human_input.insert(id, std::time::Instant::now());
    }

    /// Config flag: should Option/Alt be sent as Meta (`ESC`+char)?
    pub(crate) fn option_as_meta(&self) -> bool {
        self.config.option_as_meta
    }

    /// Whether the active pane's terminal is in application-cursor-keys
    /// mode (DECCKM) — so the keyboard path can send `ESC O A` instead of
    /// `ESC [ A` for arrows / Home / End. Data-module panes have no app
    /// running, so this is `false` and they get the default `ESC [` form.
    pub(crate) fn active_app_cursor(&self) -> bool {
        self.active_tab_ref().active_term().mode_flags().app_cursor
    }

    pub fn write_active(&mut self, bytes: Vec<u8>) {
        self.note_human_input();
        let tab = self.active_tab_ref();
        match &tab.kind {
            TabContentKind::Module(_) => {
                // TTY module: feed the PTY raw, just like a shell.
                if let Some(pty) = tab.module_pty.as_ref() {
                    pty.write(bytes);
                    return;
                }
                // Data module: marshal to JSON via the session.
                if let Some(sess) = tab.module_session.as_ref() {
                    sess.send_input(&bytes);
                }
            }
            _ => tab.live_term.write(bytes),
        }
    }

    /// Switch a pane's active tab to a different content kind. Spawns
    /// or tears down the module process as needed; clears the cached
    /// content buffer so the next render rebuilds.
    pub(super) fn set_tab_kind(&mut self, pane: PaneId, kind: TabContentKind) {
        // Resolve manifest before borrowing self.root mutably.
        let manifest = match &kind {
            TabContentKind::Module(id) => self.modules.find(id).cloned(),
            _ => None,
        };
        let proxy = self.proxy.clone();

        // Pane metrics + grid size — needed up front for a TTY module
        // because we have to spawn its LiveTerm at the right size.
        let pane_metrics = self.pane_metrics(pane);
        let pane_rect = self
            .pane_layout()
            .into_iter()
            .find(|(id, _)| *id == pane)
            .map(|(_, r)| r);
        let scrollback = self.config.scrollback;
        let pad = self.pad;
        let tab_bar_height = self.tab_bar_height;

        let Some(p) = self.root.as_mut().and_then(|n| n.find_mut(pane)) else {
            return;
        };
        let tab = p.active_tab_mut();
        let tab_id = tab.id;
        let prior_cwd = tab.live_term.current_dir();

        // Tearing down the prior sessions (if any) drops the Child /
        // PTY and joins the IO threads via Drop.
        tab.module_session = None;
        tab.module_pty = None;
        tab.last_module_body.clear();
        tab.kind = kind.clone();
        tab.content_buffer = None;
        // Bringing the shell back to view: it needs to reshape from
        // its real state, not the stale TTY-module frame.
        tab.buffer_dirty = true;
        tab.last_text_runs.clear();
        // Clear every piece of module-rendered state — otherwise
        // gutter labels, the cursor, syntax highlights, and the
        // selection band from the *previous* module stay on screen
        // until the new one's first set_text arrives.
        tab.module_cursor = None;
        tab.module_gutter = None;
        tab.gutter_buffer = None;
        tab.module_highlight_line = None;
        tab.module_language = None;
        tab.module_highlights = None;
        tab.module_scroll_y = 0.0;
        tab.pending_ensure_visible = None;
        tab.image = None;
        tab.animation = None;
        tab.last_focused_path = None;

        if let Some(manifest) = manifest {
            match manifest.kind {
                crate::modules::ModuleKind::Data => {
                    tab.module_session =
                        crate::modules::ModuleSession::spawn(&manifest, tab_id, proxy);
                }
                crate::modules::ModuleKind::Tty => {
                    // Compute the grid this LiveTerm should be born at —
                    // same shape as `pane_grid` so the module starts at
                    // a size the pane actually has room for.
                    let (cols, rows) = pane_rect
                        .map(|rect| {
                            pane_grid(
                                rect,
                                pane_metrics.cell_advance,
                                pane_metrics.line_height,
                                pad,
                                tab_bar_height,
                            )
                        })
                        .unwrap_or((80, 24));
                    let binary = manifest.binary.to_string_lossy().to_string();
                    let lt = LiveTerm::new_with_command(
                        cols,
                        rows,
                        pane_metrics.cell_advance,
                        pane_metrics.line_height,
                        proxy,
                        tab_id,
                        prior_cwd,
                        scrollback,
                        Some((binary, Vec::new())),
                    );
                    tab.module_pty = Some(lt);
                }
            }
        }
        self.window.request_redraw();
        self.persist_layout();
    }

}
