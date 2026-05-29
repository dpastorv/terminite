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

// ── moved from mod.rs ───────────────────────────────

/// A pane's rectangle in physical pixels (top-left origin).
#[derive(Clone, Copy)]
pub(super) struct PaneRect {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) w: f32,
    pub(super) h: f32,
}


/// A leaf of the window's pane tree — a self-contained workspace with its
/// own tab bar. Every leaf is equal; this is the Blender area model.
pub(super) struct Pane {
    pub(super) tabs: Vec<Tab>,
    pub(super) active_tab: usize,
    /// Background palette index for the pane's content area. `0` is
    /// none (transparent). Set via the right-click "Pane bg" item.
    pub(super) bg_idx: u8,
    /// Multiplier on the global `font_size` for this pane's content.
    /// `1.0` is the default; cycled via the right-click "Pane scale"
    /// item through `PANE_SCALE_PRESETS`. Buffer metrics + the pane's
    /// grid are rebuilt when this changes.
    pub(super) font_scale: f32,
}

/// Available pane-scale presets — cycled through by the right-click
/// menu item. Default 100% is first so a freshly-set pane reads the
/// "off" state cleanly.
pub(super) const PANE_SCALE_PRESETS: &[f32] = &[1.0, 0.8, 0.65, 1.25, 1.5];

/// Per-pane render metrics — the global config scaled by the pane's
/// `font_scale`. Returned by `pane_metrics`; callers that used to
/// read `self.font_size` / `self.cell_advance` / `self.line_height`
/// pull from here when rendering a specific pane.
#[derive(Copy, Clone)]
pub(super) struct PaneMetrics {
    pub(super) font_size: f32,
    pub(super) cell_advance: f32,
    pub(super) line_height: f32,
}

pub(super) fn next_pane_scale(current: f32) -> f32 {
    let idx = PANE_SCALE_PRESETS
        .iter()
        .position(|s| (s - current).abs() < 0.01)
        .unwrap_or(0);
    PANE_SCALE_PRESETS[(idx + 1) % PANE_SCALE_PRESETS.len()]
}

impl Pane {
    pub(super) fn single(tab: Tab) -> Self {
        Self {
            tabs: vec![tab],
            active_tab: 0,
            bg_idx: 0,
            font_scale: 1.0,
        }
    }

    pub(super) fn active_tab_ref(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub(super) fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }
}

/// Identifies one pane (leaf) within a tab's pane tree. Monotonic.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) struct PaneId(pub(super) u64);


/// A binary tree of panes. Every leaf is a shell; every split divides its
/// rect between two children by `ratio`.
pub(super) enum PaneNode {
    Leaf { id: PaneId, pane: Pane },
    Split {
        dir: SplitDir,
        /// Fraction of the parent rect given to `first`.
        ratio: f32,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

/// Pixel gap between split panes — also the divider's hit/draw thickness.
pub(super) const DIVIDER_THICKNESS: f32 = 6.0;

impl PaneNode {
    /// Recursively assign pixel rects to every leaf for a given outer rect.
    pub(super) fn layout(&self, rect: PaneRect, out: &mut Vec<(PaneId, PaneRect)>) {
        match self {
            PaneNode::Leaf { id, .. } => out.push((*id, rect)),
            PaneNode::Split { dir, ratio, first, second } => {
                let (r1, r2) = split_rect(rect, *dir, *ratio);
                first.layout(r1, out);
                second.layout(r2, out);
            }
        }
    }

    /// Consume the tree and return a new one where the leaf with `target`
    /// has been replaced by a Split at `ratio`: the old leaf becomes
    /// `first`, a fresh leaf `(new_id, new_pane)` becomes `second`.
    pub(super) fn into_split(
        self,
        target: PaneId,
        dir: SplitDir,
        new_id: PaneId,
        new_pane: Pane,
        ratio: f32,
    ) -> PaneNode {
        match self {
            PaneNode::Leaf { id, pane } if id == target => PaneNode::Split {
                dir,
                ratio,
                first: Box::new(PaneNode::Leaf { id, pane }),
                second: Box::new(PaneNode::Leaf { id: new_id, pane: new_pane }),
            },
            leaf @ PaneNode::Leaf { .. } => leaf,
            PaneNode::Split { dir: d, ratio: r, first, second } => {
                if first.find(target).is_some() {
                    PaneNode::Split {
                        dir: d,
                        ratio: r,
                        first: Box::new(
                            first.into_split(target, dir, new_id, new_pane, ratio),
                        ),
                        second,
                    }
                } else {
                    PaneNode::Split {
                        dir: d,
                        ratio: r,
                        first,
                        second: Box::new(
                            second.into_split(target, dir, new_id, new_pane, ratio),
                        ),
                    }
                }
            }
        }
    }

    /// Consume the tree and return one with the `target` leaf removed —
    /// its sibling subtree takes the parent's place. Returns `None` if the
    /// tree was a single leaf == target (i.e. nothing left).
    pub(super) fn into_closed(self, target: PaneId) -> Option<PaneNode> {
        match self {
            PaneNode::Leaf { id, .. } if id == target => None,
            leaf @ PaneNode::Leaf { .. } => Some(leaf),
            PaneNode::Split { dir, ratio, first, second } => {
                let first_has = first.find(target).is_some();
                let second_has = second.find(target).is_some();
                if first_has {
                    match first.into_closed(target) {
                        Some(f) => Some(PaneNode::Split {
                            dir,
                            ratio,
                            first: Box::new(f),
                            second,
                        }),
                        None => Some(*second),
                    }
                } else if second_has {
                    match second.into_closed(target) {
                        Some(s) => Some(PaneNode::Split {
                            dir,
                            ratio,
                            first,
                            second: Box::new(s),
                        }),
                        None => Some(*first),
                    }
                } else {
                    Some(PaneNode::Split { dir, ratio, first, second })
                }
            }
        }
    }

    /// Walk to the `Split` whose immediate layout produced `divider`-th
    /// boundary and adjust its ratio. Used by divider drag (Stage D); for
    /// now only `find`/`layout` are exercised.
    pub(super) fn find(&self, target: PaneId) -> Option<&Pane> {
        match self {
            PaneNode::Leaf { id, pane } => (*id == target).then_some(pane),
            PaneNode::Split { first, second, .. } => {
                first.find(target).or_else(|| second.find(target))
            }
        }
    }

    pub(super) fn find_mut(&mut self, target: PaneId) -> Option<&mut Pane> {
        match self {
            PaneNode::Leaf { id, pane } => (*id == target).then_some(pane),
            PaneNode::Split { first, second, .. } => {
                if let Some(p) = first.find_mut(target) {
                    return Some(p);
                }
                second.find_mut(target)
            }
        }
    }

    pub(super) fn first_leaf_id(&self) -> PaneId {
        match self {
            PaneNode::Leaf { id, .. } => *id,
            PaneNode::Split { first, .. } => first.first_leaf_id(),
        }
    }

    pub(super) fn leaf_count(&self) -> usize {
        match self {
            PaneNode::Leaf { .. } => 1,
            PaneNode::Split { first, second, .. } => {
                first.leaf_count() + second.leaf_count()
            }
        }
    }

    /// Collect a mutable reference to every tab in every pane of the tree.
    pub(super) fn all_tabs_mut<'a>(&'a mut self, out: &mut Vec<&'a mut Tab>) {
        match self {
            PaneNode::Leaf { pane, .. } => {
                for t in pane.tabs.iter_mut() {
                    out.push(t);
                }
            }
            PaneNode::Split { first, second, .. } => {
                first.all_tabs_mut(out);
                second.all_tabs_mut(out);
            }
        }
    }

    /// Immutable variant of `all_tabs_mut` — read-only walk of every
    /// tab in the tree.
    pub(super) fn all_tabs<'a>(&'a self, out: &mut Vec<&'a Tab>) {
        match self {
            PaneNode::Leaf { pane, .. } => {
                for t in pane.tabs.iter() {
                    out.push(t);
                }
            }
            PaneNode::Split { first, second, .. } => {
                first.all_tabs(out);
                second.all_tabs(out);
            }
        }
    }

    /// Collect every leaf `Pane` in the tree. Used when we need
    /// pane-level state (font_scale, bg) rather than per-tab data.
    pub(super) fn all_panes<'a>(&'a self, out: &mut Vec<&'a Pane>) {
        match self {
            PaneNode::Leaf { pane, .. } => out.push(pane),
            PaneNode::Split { first, second, .. } => {
                first.all_panes(out);
                second.all_panes(out);
            }
        }
    }


    /// Find the split divider under a point. Returns the path to the owning
    /// `Split`, that split's outer rect, and its orientation.
    pub(super) fn divider_at(
        &self,
        rect: PaneRect,
        x: f32,
        y: f32,
    ) -> Option<(Vec<usize>, PaneRect, SplitDir)> {
        let PaneNode::Split { dir, ratio, first, second } = self else {
            return None;
        };
        let (r1, r2) = split_rect(rect, *dir, *ratio);
        let m = DIVIDER_HIT_MARGIN;
        let self_hit = match dir {
            SplitDir::Vertical => {
                let gx = r1.x + r1.w;
                x >= gx - m
                    && x <= gx + DIVIDER_THICKNESS + m
                    && y >= rect.y
                    && y <= rect.y + rect.h
            }
            SplitDir::Horizontal => {
                let gy = r1.y + r1.h;
                y >= gy - m
                    && y <= gy + DIVIDER_THICKNESS + m
                    && x >= rect.x
                    && x <= rect.x + rect.w
            }
        };
        if self_hit {
            return Some((Vec::new(), rect, *dir));
        }
        if let Some((mut p, sr, sd)) = first.divider_at(r1, x, y) {
            p.insert(0, 0);
            return Some((p, sr, sd));
        }
        if let Some((mut p, sr, sd)) = second.divider_at(r2, x, y) {
            p.insert(0, 1);
            return Some((p, sr, sd));
        }
        None
    }

    /// Mutable reference to the ratio of the `Split` at `path`.
    pub(super) fn split_ratio_at_mut(&mut self, path: &[usize]) -> Option<&mut f32> {
        match self {
            PaneNode::Leaf { .. } => None,
            PaneNode::Split { ratio, first, second, .. } => match path.split_first() {
                None => Some(ratio),
                Some((&0, rest)) => first.split_ratio_at_mut(rest),
                Some((_, rest)) => second.split_ratio_at_mut(rest),
            },
        }
    }
}

/// Colour of the seam drawn in a split's divider gap.
pub(super) const DIVIDER_COLOR: [f32; 4] = [0.20, 0.20, 0.26, 1.0];

/// Extra grab margin each side of a divider — the seam is thin, so the
/// hit zone is widened for comfortable dragging.
pub(super) const DIVIDER_HIT_MARGIN: f32 = 5.0;

/// Smallest a pane is allowed to be dragged to (tab bar + a row + padding).
pub(super) const MIN_PANE: f32 = 140.0;

/// Clamp a split ratio so neither child shrinks below `MIN_PANE`.
pub(super) fn clamp_ratio(ratio: f32, span: f32) -> f32 {
    let usable = (span - DIVIDER_THICKNESS).max(1.0);
    let min_frac = (MIN_PANE / usable).min(0.45);
    ratio.clamp(min_frac, 1.0 - min_frac)
}

/// Hit-box size of the corner split handle (top-right of every pane).
pub(super) const SPLIT_HANDLE_SIZE: f32 = 18.0;
/// Leg length of the triangular grip drawn in that corner.
pub(super) const SPLIT_GRIP: f32 = 14.0;
/// Resting colour of the split grip.
pub(super) const SPLIT_HANDLE_COLOR: [f32; 4] = [0.34, 0.34, 0.40, 1.0];
/// Minimum drag distance before a corner gesture commits.
pub(super) const SPLIT_GESTURE_THRESHOLD: f32 = 24.0;
/// Translucent wash over a pane the corner gesture would remove.
pub(super) const REMOVE_PREVIEW_COLOR: [f32; 4] = [0.55, 0.16, 0.16, 0.38];

/// Draw the corner split grip — a small right triangle flush to a pane's
/// top-right corner (a "peel"), approximated by 1px-tall steps.
pub(super) fn push_split_grip(out: &mut Vec<RectInstance>, pane: PaneRect, color: [f32; 4]) {
    let corner_x = pane.x + pane.w;
    let steps = SPLIT_GRIP as usize;
    for i in 0..steps {
        let w = SPLIT_GRIP - i as f32;
        out.push(RectInstance {
            rect: [corner_x - w, pane.y + i as f32, w, 1.0],
            color,
        });
    }
}

/// True if a point is inside a pane's corner split-handle hit box.
pub(super) fn in_split_handle(pane: PaneRect, x: f32, y: f32) -> bool {
    x >= pane.x + pane.w - SPLIT_HANDLE_SIZE
        && x <= pane.x + pane.w
        && y >= pane.y
        && y <= pane.y + SPLIT_HANDLE_SIZE
}

/// What a committed corner-handle gesture does.
#[derive(Clone, Copy)]
pub(super) enum GestureOutcome {
    Split(SplitDir),
    Remove,
}

/// Resolve a corner-drag delta: drag *into* the pane (down → stack,
/// left → side by side) splits it; drag back *out* (up / right) removes it.
/// `None` until the drag passes the commit threshold.
pub(super) fn gesture_outcome(dx: f32, dy: f32) -> Option<GestureOutcome> {
    if dx.hypot(dy) < SPLIT_GESTURE_THRESHOLD {
        return None;
    }
    Some(if dy.abs() > dx.abs() {
        if dy > 0.0 {
            GestureOutcome::Split(SplitDir::Horizontal)
        } else {
            GestureOutcome::Remove
        }
    } else if dx < 0.0 {
        GestureOutcome::Split(SplitDir::Vertical)
    } else {
        GestureOutcome::Remove
    })
}

/// Ratio for a cursor-placed split — where the divider lands inside `pane`,
/// clamped so neither side falls below `MIN_PANE`.
pub(super) fn split_ratio_from_cursor(pane: PaneRect, dir: SplitDir, cx: f32, cy: f32) -> f32 {
    let (raw, span) = match dir {
        SplitDir::Vertical => {
            ((cx - pane.x) / (pane.w - DIVIDER_THICKNESS).max(1.0), pane.w)
        }
        SplitDir::Horizontal => {
            ((cy - pane.y) / (pane.h - DIVIDER_THICKNESS).max(1.0), pane.h)
        }
    };
    clamp_ratio(raw, span)
}

/// Hard ceilings on the cell grid. No real terminal approaches these; they
/// exist so a degenerate font size, window size, or scrollback can't drive
/// a `Term` allocation (`cols × scrollback × Cell`) into OOM territory. The
/// per-frame RSS kill switch cannot catch a single runaway allocation, so
/// the grid must be bounded at the source.
pub(super) const MAX_GRID_COLS: usize = 600;
pub(super) const MAX_GRID_ROWS: usize = 400;
/// Cap on the rolling frame-time window used by the stats verb.
pub(super) const FRAME_TIMER_CAP: usize = 120;

/// Shared palette for the per-tab color band + per-pane background tint.
/// Index 0 is "none" (transparent, the off state). Colors borrow from
/// the One Dark family already in `src/palette.rs` so a colored pane
/// reads as part of terminite's existing visual language.
pub(super) const COLOR_PALETTE: &[(&str, [f32; 4])] = &[
    ("none",    [0.0, 0.0, 0.0, 0.0]),
    ("red",     [224.0 / 255.0, 108.0 / 255.0, 117.0 / 255.0, 1.0]),
    ("yellow",  [229.0 / 255.0, 192.0 / 255.0, 123.0 / 255.0, 1.0]),
    ("green",   [152.0 / 255.0, 195.0 / 255.0, 121.0 / 255.0, 1.0]),
    ("blue",    [ 97.0 / 255.0, 175.0 / 255.0, 239.0 / 255.0, 1.0]),
    ("magenta", [198.0 / 255.0, 120.0 / 255.0, 221.0 / 255.0, 1.0]),
    ("cyan",    [ 86.0 / 255.0, 182.0 / 255.0, 194.0 / 255.0, 1.0]),
];



// ── helpers moved from mod.rs ──────────────────────

/// Grid (cols, rows) that fits inside a pane's pixel rect. Each pane carves
/// its own `self.tab_bar_height` strip off the top, then the per-edge padding.
pub(super) fn pane_grid(
    rect: PaneRect,
    cell_advance: f32,
    line_height: f32,
    pad: Padding,
    tab_bar_height: f32,
) -> (usize, usize) {
    let avail_w = (rect.w - pad.left - pad.right).max(cell_advance);
    let avail_h =
        (rect.h - tab_bar_height - pad.top - pad.bottom).max(line_height);
    let cols = (avail_w / cell_advance).floor().max(1.0) as usize;
    let rows = (avail_h / line_height).floor().max(1.0) as usize;
    (cols.min(MAX_GRID_COLS), rows.min(MAX_GRID_ROWS))
}

/// Geometry of each tab inside a pane's tab bar: `(x_start, width, is_active)`.
///
/// Widths are per-tab dynamic: each tab's *ideal* width is its measured
/// title width plus the chrome insets (label inset + close-glyph
/// reservation), clamped to `[min_width, max_width]`. If the sum fits in
/// the available bar, each tab gets exactly its ideal. If not, every
/// tab shrinks proportionally; nothing drops below `min_width` even
/// then. With enough tabs this can overflow — accept that, the user
/// either closes some or lives with clipping.
pub(super) fn pane_tab_layout(
    rect: PaneRect,
    title_widths: &[f32],
    active: usize,
    min_width: f32,
    max_width: f32,
    kind_selector_w: f32,
) -> Vec<(f32, f32, bool)> {
    let n = title_widths.len();
    if n == 0 {
        return Vec::new();
    }
    // Reserve the left edge for the kind-selector dropdown (Blender-
    // style area-type picker) and the top-right corner for the split
    // handle.
    let avail = (rect.w - SPLIT_HANDLE_SIZE - kind_selector_w).max(min_width);
    let chrome = TAB_LABEL_INSET + TAB_CLOSE_WIDTH;
    let ideal: Vec<f32> = title_widths
        .iter()
        .map(|w| (w + chrome).clamp(min_width, max_width))
        .collect();
    let total: f32 = ideal.iter().sum();
    let widths: Vec<f32> = if total <= avail {
        ideal
    } else {
        let factor = avail / total;
        ideal.iter().map(|w| (w * factor).max(min_width)).collect()
    };
    let mut x = rect.x + kind_selector_w;
    let mut out = Vec::with_capacity(n);
    for (i, w) in widths.into_iter().enumerate() {
        out.push((x, w, i == active));
        x += w;
    }
    out
}

/// Render-shaped width of a chrome buffer's first line. Used to size
/// tabs to their actual title text rather than equal share.
pub(super) fn measure_title_width(buf: &Buffer) -> f32 {
    buf.layout_runs().next().map(|r| r.line_w).unwrap_or(0.0)
}

/// Walk the pane tree, emitting one rect per split divider gap.
pub(super) fn collect_dividers(node: &PaneNode, rect: PaneRect, out: &mut Vec<RectInstance>) {
    if let PaneNode::Split { dir, ratio, first, second } = node {
        let (r1, r2) = split_rect(rect, *dir, *ratio);
        let gap = match dir {
            SplitDir::Vertical => [r1.x + r1.w, rect.y, DIVIDER_THICKNESS, rect.h],
            SplitDir::Horizontal => [rect.x, r1.y + r1.h, rect.w, DIVIDER_THICKNESS],
        };
        out.push(RectInstance { rect: gap, color: DIVIDER_COLOR });
        collect_dividers(first, r1, out);
        collect_dividers(second, r2, out);
    }
}

pub(super) fn split_rect(r: PaneRect, dir: SplitDir, ratio: f32) -> (PaneRect, PaneRect) {
    let d = DIVIDER_THICKNESS;
    match dir {
        SplitDir::Vertical => {
            let first_w = ((r.w - d) * ratio).max(0.0);
            let second_w = (r.w - d - first_w).max(0.0);
            (
                PaneRect { x: r.x, y: r.y, w: first_w, h: r.h },
                PaneRect {
                    x: r.x + first_w + d,
                    y: r.y,
                    w: second_w,
                    h: r.h,
                },
            )
        }
        SplitDir::Horizontal => {
            let first_h = ((r.h - d) * ratio).max(0.0);
            let second_h = (r.h - d - first_h).max(0.0);
            (
                PaneRect { x: r.x, y: r.y, w: r.w, h: first_h },
                PaneRect {
                    x: r.x,
                    y: r.y + first_h + d,
                    w: r.w,
                    h: second_h,
                },
            )
        }
    }
}


