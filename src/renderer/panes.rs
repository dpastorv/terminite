//! Pane layout — resize, relayout, splits, focus, tab-bar hit-testing.

use super::*;

impl Renderer {
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        self.relayout();
        self.sync_active_grid();
    }

    /// The whole window — the rect the pane tree fills. Each pane carves its
    /// own tab bar off the top of its slice.
    pub(super) fn content_rect(&self) -> PaneRect {
        PaneRect {
            x: 0.0,
            y: 0.0,
            w: self.surface_config.width as f32,
            h: self.surface_config.height as f32,
        }
    }

    /// Pixel rect of the active pane.
    pub(super) fn active_pane_rect(&self) -> PaneRect {
        let active = self.active_pane;
        self.pane_layout()
            .into_iter()
            .find(|(id, _)| *id == active)
            .map(|(_, r)| r)
            .unwrap_or_else(|| self.content_rect())
    }

    /// Recompute every pane's pixel rect and resize every tab's PTY / buffer
    /// to fit. Background tabs are kept accurate too — shells resize on the
    /// SIGWINCH alacritty sends, so they must stay correct for the switch.
    /// Per-pane render metrics, computed by scaling the global values
    /// by `pane.font_scale`. Approximate — actual cell advance depends
    /// on font shaping at the target size — but close enough for v1.
    pub(super) fn pane_metrics(&self, pid: PaneId) -> PaneMetrics {
        let scale = self
            .root_ref()
            .find(pid)
            .map(|p| p.font_scale)
            .unwrap_or(1.0);
        PaneMetrics {
            font_size: self.font_size * scale,
            cell_advance: self.cell_advance * scale,
            line_height: (self.line_height * scale).round().max(1.0),
        }
    }

    pub(super) fn active_pane_metrics(&self) -> PaneMetrics {
        self.pane_metrics(self.active_pane)
    }

    /// Find the `font_scale` of whatever pane currently owns `tab_id`,
    /// or 1.0 if the tab vanished. Used when shaping block labels so
    /// they're sized to the pane's content from the moment they're
    /// created.
    pub(super) fn scale_for_tab(&self, tab_id: TabId) -> f32 {
        let mut leaves: Vec<&Pane> = Vec::new();
        self.root_ref().all_panes(&mut leaves);
        for p in leaves {
            if p.tabs.iter().any(|t| t.id == tab_id) {
                return p.font_scale;
            }
        }
        1.0
    }

    /// Set one pane's font scale and rebuild its tab buffers + grid.
    /// Cheap when nothing changed.
    pub(super) fn apply_pane_scale(&mut self, pid: PaneId, scale: f32) {
        // Short-circuit on no-op.
        let changed = self
            .root
            .as_mut()
            .and_then(|n| n.find_mut(pid))
            .map(|p| {
                let diff = (p.font_scale - scale).abs() > 0.01;
                if diff {
                    p.font_scale = scale;
                }
                diff
            })
            .unwrap_or(false);
        if !changed {
            return;
        }
        let metrics = self.pane_metrics(pid);
        let font_metrics = Metrics::new(metrics.font_size, metrics.line_height);
        // Block labels are content-anchored, so they scale too. Use
        // `LABEL_LINE_H * scale` rather than `metrics.line_height` —
        // the label has its own line-height ratio independent of the
        // content's `line_height` multiplier.
        let scale = metrics.font_size / self.font_size;
        let label_font_size = crate::blocks::LABEL_FONT_SIZE * scale;
        let label_line_h = (crate::blocks::LABEL_LINE_H * scale).max(1.0);
        if let Some(p) = self.root.as_mut().and_then(|n| n.find_mut(pid)) {
            for tab in p.tabs.iter_mut() {
                tab.text_buffer.set_metrics(&mut self.font_system, font_metrics);
                tab.content_buffer = None;
                tab.buffer_dirty = true;
                tab.last_text_runs.clear();
                tab.blocks.rescale_labels(
                    &mut self.font_system,
                    label_font_size,
                    label_line_h,
                );
            }
        }
        self.relayout();
        self.sync_active_grid();
        self.window.request_redraw();
    }

    pub(super) fn relayout(&mut self) {
        for (pid, rect) in self.pane_layout() {
            let metrics = self.pane_metrics(pid);
            let (cols, rows) = pane_grid(
                rect,
                metrics.cell_advance,
                metrics.line_height,
                self.pad,
                self.tab_bar_height,
            );
            let content_h = (rect.h - self.tab_bar_height).max(1.0);
            let pane = self
                .root
                .as_mut()
                .expect("pane tree present")
                .find_mut(pid)
                .expect("laid-out pane present");
            for tab in pane.tabs.iter_mut() {
                tab.text_buffer.set_size(
                    &mut self.font_system,
                    Some(rect.w.max(1.0)),
                    Some(content_h),
                );
                if tab.cols != cols || tab.rows != rows {
                    // Resize the shell *and* any active TTY module —
                    // both need to react to pane geometry changes.
                    tab.live_term.resize(cols, rows);
                    if let Some(pty) = tab.module_pty.as_ref() {
                        pty.resize(cols, rows);
                    }
                    tab.cols = cols;
                    tab.rows = rows;
                    // A resize invalidates the snapshot cache and selection.
                    tab.last_text_runs.clear();
                    tab.buffer_dirty = true;
                    tab.selection = None;
                }
            }
        }
    }

    /// Apply the layout-affecting config knobs (per-edge padding,
    /// `gutter_left`, `line_height` multiplier) to the running window.
    /// Called from `focus_changed` after `Config::load`, so the tuning
    /// loop is: edit `~/.config/terminite/config.toml` in a side pane,
    /// click back into terminite, see the values apply.
    ///
    /// Only line_height needs per-tab work — it lives in each buffer's
    /// `Metrics`, so we touch every tab to update the metrics and mark
    /// the snapshot dirty. Padding / gutter_left are positional and
    /// propagate to the next frame automatically; `relayout` recomputes
    /// the grid with the new pad on top.
    pub(super) fn apply_live_layout(&mut self) {
        let new_line_height =
            (self.font_size * LINE_H_RATIO * self.config.line_height).round();
        let line_height_changed = (new_line_height - self.line_height).abs() > f32::EPSILON;
        let pad_or_gutter_changed = self.pad != self.config.padding
            || self.gutter_left != self.config.gutter_left
            || self.gutter_gap != self.config.gutter_gap
            || self.highlight_pad_x != self.config.highlight_pad_x
            || self.highlight_pad_y != self.config.highlight_pad_y
            || self.highlight_offset_y != self.config.highlight_offset_y
            || self.tab_min_width != self.config.tab_min_width
            || self.tab_max_width != self.config.tab_max_width;
        if !line_height_changed && !pad_or_gutter_changed {
            return;
        }

        self.pad = self.config.padding;
        self.gutter_left = self.config.gutter_left;
        self.gutter_gap = self.config.gutter_gap;
        self.highlight_pad_x = self.config.highlight_pad_x;
        self.highlight_pad_y = self.config.highlight_pad_y;
        self.highlight_offset_y = self.config.highlight_offset_y;
        self.tab_min_width = self.config.tab_min_width;
        self.tab_max_width = self.config.tab_max_width;
        self.line_height = new_line_height;

        if line_height_changed {
            let metrics = Metrics::new(self.font_size, new_line_height);
            let mut tabs: Vec<&mut Tab> = Vec::new();
            self.root
                .as_mut()
                .expect("pane tree present")
                .all_tabs_mut(&mut tabs);
            for tab in tabs {
                tab.text_buffer.set_metrics(&mut self.font_system, metrics);
                tab.buffer_dirty = true;
            }
        }

        self.relayout();
        self.sync_active_grid();
        self.window.request_redraw();
    }

    /// Mirror the active tab's grid into `grid_cols` / `grid_rows`, which the
    /// mouse / autoscroll paths read.
    pub(super) fn sync_active_grid(&mut self) {
        let t = self.active_tab_ref();
        let (cols, rows) = (t.cols, t.rows);
        self.grid_cols = cols;
        self.grid_rows = rows;
    }

    /// Split the active pane in two at `ratio`; the new pane (one fresh tab)
    /// is focused.
    pub fn split_active(&mut self, dir: SplitDir, ratio: f32) {
        let target = self.active_pane;
        let cwd = self.active_tab_ref().live_term.current_dir();
        let tab_id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        let new_pid = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        // Provisional size; `relayout` immediately corrects it.
        let live = LiveTerm::new(
            self.grid_cols.max(1),
            self.grid_rows.max(1),
            self.cell_advance,
            self.line_height,
            self.proxy.clone(),
            tab_id,
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
        let buf = make_content_buffer(
            &mut self.font_system,
            self.cell_advance,
            self.line_height,
            self.font_size,
            &self.font_family,
            100.0,
            100.0,
        );
        let new_tab = Tab::new(
            tab_id,
            title,
            title_buf,
            live,
            buf,
            self.grid_cols.max(1),
            self.grid_rows.max(1),
        );
        let root = self.root.take().expect("pane tree present");
        self.root = Some(root.into_split(target, dir, new_pid, Pane::single(new_tab), ratio));
        self.active_pane = new_pid;
        self.relayout();
        self.sync_active_grid();
        self.window.request_redraw();
        self.persist_layout();
    }

    /// Close the active pane. Returns true if it was the window's last pane
    /// (the window should then exit).
    pub fn close_active_pane(&mut self) -> bool {
        if self.root_ref().leaf_count() <= 1 {
            return true;
        }
        let target = self.active_pane;
        let root = self.root.take().expect("pane tree present");
        let new_root = root.into_closed(target).expect("more than one leaf remains");
        self.active_pane = new_root.first_leaf_id();
        self.root = Some(new_root);
        self.relayout();
        self.sync_active_grid();
        self.window.set_title(&self.active_tab_ref().title);
        self.window.request_redraw();
        self.persist_layout();
        false
    }

    /// Close the active pane, but if any of its tabs has a non-shell process
    /// running, open a confirmation modal first (the corner-drag remove
    /// path — it takes the whole pane and every tab in it).
    pub(super) fn request_close_active_pane(&mut self) {
        if self.modal.is_some() || self.root_ref().leaf_count() <= 1 {
            return;
        }
        let busy = self
            .active_pane_ref()
            .tabs
            .iter()
            .any(|t| t.live_term.has_active_process());
        if !busy {
            let _ = self.close_active_pane();
            return;
        }
        let pane = self.active_pane_ref();
        let name = pane
            .tabs
            .iter()
            .find(|t| t.live_term.has_active_process())
            .and_then(|t| t.live_term.foreground_pid())
            .and_then(proc_name_of)
            .unwrap_or_else(|| "A process".to_string());
        let tab_count = pane.tabs.len();
        let body = if tab_count > 1 {
            format!("{name} is running in this pane ({tab_count} tabs).")
        } else {
            format!("{name} is running in this pane.")
        };
        self.open_modal(
            ModalAction::ClosePane,
            "Close pane?".to_string(),
            body,
            "Cancel",
            "Close",
        );
    }

    /// Make a pane the active one.
    pub(super) fn focus_pane(&mut self, pid: PaneId) {
        if self.active_pane != pid {
            // Drop any selection the *prior* pane's active tab still
            // holds — keeping it across a pane switch reads as
            // "stale highlight in the pane I just left." Each pane
            // re-selects on its own click.
            let prior = self.active_pane;
            if let Some(p) = self.root.as_mut().and_then(|n| n.find_mut(prior)) {
                let tab = p.active_tab_mut();
                tab.selection = None;
                tab.dragging = false;
            }
            self.active_pane = pid;
            self.sync_active_grid();
            self.window.set_title(&self.active_tab_ref().title);
            // Hot-reload also fires on in-window pane focus — editing the
            // config in a side pane and clicking back into a shell pane is
            // the natural tuning loop, and the window focus event doesn't
            // fire for that. `apply_live_layout` early-returns when
            // nothing changed, so the per-click cost is one ~1.7 KB read.
            self.config = Config::load();
            self.apply_live_layout();
            self.window.request_redraw();
        }
    }

    /// Move keyboard focus to the neighbouring pane in a direction. `dx` /
    /// `dy` are -1 / 0 / +1; we probe just past the active pane's edge (in
    /// the divider gap's far side) and focus whatever pane lands there.
    pub fn focus_dir(&mut self, dx: f32, dy: f32) {
        let a = self.active_pane_rect();
        let past = DIVIDER_THICKNESS + 1.0;
        let probe_x = if dx > 0.0 {
            a.x + a.w + past
        } else if dx < 0.0 {
            a.x - past
        } else {
            a.x + a.w / 2.0
        };
        let probe_y = if dy > 0.0 {
            a.y + a.h + past
        } else if dy < 0.0 {
            a.y - past
        } else {
            a.y + a.h / 2.0
        };
        if let Some((pid, _)) = self.pane_at(probe_x, probe_y) {
            self.focus_pane(pid);
        }
    }

    /// Make the pane under a window-relative point the active one. Returns
    /// true if a pane was hit.
    pub(super) fn focus_pane_at(&mut self, x: f32, y: f32) -> bool {
        if let Some((pid, _)) = self.pane_at(x, y) {
            self.focus_pane(pid);
            true
        } else {
            false
        }
    }

    /// Handle a left-click inside pane `pid`'s tab-bar strip: switch to the
    /// clicked tab, or close it if the × close-zone was hit.
    pub(super) fn tab_bar_click(&mut self, pid: PaneId, prect: PaneRect) {
        let ksw = kind_selector_w(self.config.tab_font_size);
        // Kind-selector hit first — leftmost zone of the bar.
        if self.mouse_pos.0 < prect.x + ksw {
            self.open_kind_dropdown(pid, prect);
            return;
        }
        let (title_widths, active) = {
            let pane = self.root_ref().find(pid).expect("pane present");
            let widths: Vec<f32> = pane
                .tabs
                .iter()
                .map(|t| measure_title_width(&t.title_buffer))
                .collect();
            (widths, pane.active_tab)
        };
        let layout = pane_tab_layout(prect, &title_widths, active, self.tab_min_width, self.tab_max_width, ksw);
        let mut hit: Option<(usize, f32, f32)> = None;
        for (i, (tx, tw, _)) in layout.iter().enumerate() {
            if self.mouse_pos.0 >= *tx && self.mouse_pos.0 < *tx + *tw {
                hit = Some((i, *tx, *tw));
                break;
            }
        }
        let Some((i, tx, tw)) = hit else { return };
        if self.mouse_pos.0 >= tx + tw - TAB_CLOSE_WIDTH {
            // Don't let a stray × click close the window — the very last tab
            // of the very last pane stays put; Cmd+W is the deliberate path.
            let last = self.root_ref().leaf_count() == 1 && title_widths.len() == 1;
            if !last {
                self.active_pane_mut().active_tab = i;
                self.close_active_tab();
            }
        } else {
            self.switch_to_tab(i);
        }
        self.window.request_redraw();
    }


}
