//! Module lifecycle, layout persistence, and module messages.

use super::*;

impl Renderer {
    /// fs-watch fired — re-discover modules. Coalesced upstream so
    /// this only runs once per real change.
    pub fn handle_modules_changed(&mut self) {
        crate::logging::info("modules_watch: reloading registry");
        self.reload_modules();
    }

    /// Re-discover modules from disk and refresh chrome labels.
    /// Active sessions keep running — if the user removed a module
    /// whose pane is currently shown, the session lives until the
    /// user switches kind. New modules become selectable from the
    /// dropdown immediately.
    pub fn reload_modules(&mut self) {
        self.modules = crate::modules::Registry::discover();
        // Rebuild the per-kind label buffers. Built-ins stay; module
        // entries reflect the fresh registry.
        let mut next: std::collections::HashMap<String, Buffer> =
            std::collections::HashMap::new();
        let ksw = kind_selector_w(self.config.tab_font_size);
        let label = |fs: &mut FontSystem, name: &str| {
            make_title_buffer(
                fs,
                &format!("{name} ▾"),
                self.tab_font_size,
                self.tab_line_h,
                ksw,
            )
        };
        next.insert("shell".into(), label(&mut self.font_system, "Shell"));
        next.insert("welcome".into(), label(&mut self.font_system, "Welcome"));
        for m in self.modules.list() {
            next.insert(m.id.clone(), label(&mut self.font_system, &m.name));
        }
        for preset in crate::acp::presets() {
            next.insert(
                preset.display_name.into(),
                label(&mut self.font_system, preset.display_name),
            );
        }
        self.kind_label_buffers = next;
        self.window.request_redraw();
    }

    /// Resolve a click in `pid`'s content area to a (source line,
    /// visual col) inside the data module's body and dispatch as a
    /// `click` event. Returns true if the click was routed to a
    /// data module — caller short-circuits selection / hyperlink /
    /// the rest of the click pipeline in that case.
    pub(super) fn dispatch_data_module_click(
        &mut self,
        pid: PaneId,
        prect: PaneRect,
    ) -> bool {
        // Only data modules (no PTY behind them) handle clicks via
        // the wire; TTY modules + shells use the existing selection
        // / mouse-report paths.
        let is_data_module = {
            let pane = match self.root_ref().find(pid) {
                Some(p) => p,
                None => return false,
            };
            let tab = pane.active_tab_ref();
            matches!(tab.kind, TabContentKind::Module(_))
                && tab.module_pty.is_none()
                && tab.module_session.is_some()
        };
        if !is_data_module {
            return false;
        }
        let metrics = self.pane_metrics(pid);
        let pad = self.pad;
        let px = prect.x + pad.left;
        let py = prect.y + self.tab_bar_height + pad.top;
        // Click in the gutter region (line-number column) → treat
        // as col 0 of the same source line. Subtract the gutter
        // width so the col we send to the module is in content
        // cells, not pane cells.
        let pane_content_w = (prect.w - pad.left - pad.right).max(0.0);
        let gutter_w = self
            .root_ref()
            .find(pid)
            .and_then(|p| p.active_tab_ref().module_gutter.as_ref())
            .map(|lbls| {
                let max_chars = lbls.iter().map(|s| s.chars().count()).max().unwrap_or(0) as f32;
                if max_chars > 0.0 {
                    ((max_chars + 1.0) * metrics.cell_advance).min(pane_content_w * 0.5)
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0);
        let local_x = (self.mouse_pos.0 - px - gutter_w).max(0.0);
        let local_y = self.mouse_pos.1 - py;
        if local_y < 0.0 {
            return false;
        }
        // Translate local_y → source line via layout_runs lookup so
        // wrapped lines map back to their source index correctly.
        let scroll_y = self
            .root_ref()
            .find(pid)
            .map(|p| p.active_tab_ref().module_scroll_y)
            .unwrap_or(0.0);
        let target_y = local_y + scroll_y;
        let line_height = metrics.line_height;
        let mut source_line: Option<u32> = None;
        if let Some(buf) = self
            .root_ref()
            .find(pid)
            .and_then(|p| p.active_tab_ref().content_buffer.as_ref())
        {
            let mut acc = 0.0_f32;
            for run in buf.layout_runs() {
                if target_y >= acc && target_y < acc + line_height {
                    source_line = Some(run.line_i as u32);
                    break;
                }
                acc += line_height;
            }
        }
        let Some(line) = source_line else {
            return false;
        };
        let col = (local_x / metrics.cell_advance).max(0.0).round() as u32;
        // Multi-click bookkeeping — rolls over the count if the
        // user clicks the same body cell within MULTI_CLICK_WINDOW.
        // Cap at 3 (single/double/triple); modules that don't care
        // about higher counts can clamp on their end.
        let now = Instant::now();
        let mut count = 1u8;
        if let Some(tab_mut) = self
            .root
            .as_mut()
            .and_then(|n| n.find_mut(pid))
            .map(|p| p.active_tab_mut())
        {
            count = match tab_mut.last_module_click {
                Some((t, l, c, n))
                    if now.duration_since(t) < MULTI_CLICK_WINDOW
                        && l == line
                        && c == col =>
                {
                    (n + 1).min(3)
                }
                _ => 1,
            };
            tab_mut.last_module_click = Some((now, line, col, count));
        }
        if let Some(sess) = self
            .root_ref()
            .find(pid)
            .and_then(|p| p.active_tab_ref().module_session.as_ref())
        {
            sess.send_click(line, col, count);
        }
        true
    }

    /// A shell pane reported a new cwd via OSC 7. Broadcast it to
    /// every live data-module session so paired views (Nav follows
    /// shell, …) can react. Same shape as the focus broadcast — one
    /// event, every other module decides what to do with it.
    pub fn handle_cwd_changed(&mut self, _tab_id: TabId, path: &std::path::Path) {
        crate::logging::info(&format!("shell cwd → {}", path.display()));
        let path_str = path.to_string_lossy().to_string();
        let mut tabs: Vec<&mut Tab> = Vec::new();
        self.root
            .as_mut()
            .expect("pane tree present")
            .all_tabs_mut(&mut tabs);
        for tab in tabs {
            if let Some(sess) = tab.module_session.as_ref() {
                sess.send_cwd(&path_str);
            }
        }
        self.persist_layout();
    }

    /// Build a fresh pane tree from a saved Layout. Returns false
    /// if anything went sideways (caller falls back to a default
    /// shell). Two-phase: build all tabs as plain shells first
    /// (using the persisted cwds), then walk the tree and switch
    /// non-shell tabs to their target kind via set_tab_kind, then
    /// replay focus events for tabs that had a focused_path.
    pub fn restore_layout(&mut self, layout: crate::layout::Layout) -> bool {
        let active_path = layout.active_pane_path.clone().unwrap_or_default();
        // Collect (pane_id, tab_index, target_kind, focused_path)
        // tuples while building so post-build switching doesn't have
        // to walk the layout tree again.
        let mut post_build: Vec<(PaneId, usize, TabContentKind, Option<String>)> = Vec::new();
        let new_root = self.build_layout_node(layout.root, &mut post_build);
        let active = pane_id_at_path(&new_root, &active_path)
            .or_else(|| {
                let mut first_id: Option<PaneId> = None;
                fn first_leaf(node: &PaneNode, out: &mut Option<PaneId>) {
                    if out.is_some() { return; }
                    match node {
                        PaneNode::Leaf { id, .. } => *out = Some(*id),
                        PaneNode::Split { first, second, .. } => {
                            first_leaf(first, out);
                            if out.is_none() { first_leaf(second, out); }
                        }
                    }
                }
                first_leaf(&new_root, &mut first_id);
                first_id
            })
            .unwrap_or(PaneId(0));
        // Install. The old tree's Drop closes its shells / modules.
        self.root = Some(new_root);
        self.active_pane = active;
        // Switch non-shell tabs to their target kind.
        for (pid, tab_idx, kind, _focused) in &post_build {
            // Make the target tab active in its pane, then switch
            // its kind — set_tab_kind operates on the pane's
            // currently-active tab.
            if let Some(pane) = self
                .root
                .as_mut()
                .and_then(|n| n.find_mut(*pid))
            {
                if *tab_idx < pane.tabs.len() {
                    pane.active_tab = *tab_idx;
                }
            }
            if !matches!(kind, TabContentKind::Shell) {
                self.set_tab_kind(*pid, kind.clone());
            }
        }
        // Replay focused_path for tabs that have one — fires a
        // synthetic focus event so the module re-renders (Editor
        // reopens the file, Preview re-renders, …).
        for (pid, tab_idx, _, focused) in &post_build {
            let Some(path) = focused.as_ref() else { continue };
            if let Some(pane) = self.root.as_ref().and_then(|n| n.find(*pid)) {
                if let Some(tab) = pane.tabs.get(*tab_idx) {
                    if let Some(sess) = tab.module_session.as_ref() {
                        sess.send_focus(path);
                    }
                }
            }
            if let Some(pane) = self.root.as_mut().and_then(|n| n.find_mut(*pid)) {
                if let Some(tab) = pane.tabs.get_mut(*tab_idx) {
                    tab.last_focused_path = Some(path.clone());
                }
            }
        }
        // Restore active tab pointers per pane to whatever the
        // layout said (we may have moved them above to drive
        // set_tab_kind; reset to the persisted values).
        self.sync_active_grid();
        self.window.set_title(&self.active_tab_ref().title);
        self.window.request_redraw();
        true
    }

    /// Recursively build a PaneNode from a LayoutNode. Logs target
    /// kinds + focused paths into `post_build` so the caller can
    /// switch and replay after the tree is in place.
    pub(super) fn build_layout_node(
        &mut self,
        node: crate::layout::LayoutNode,
        post_build: &mut Vec<(PaneId, usize, TabContentKind, Option<String>)>,
    ) -> PaneNode {
        match node {
            crate::layout::LayoutNode::Pane(pane_layout) => {
                let pid = PaneId(self.next_pane_id);
                self.next_pane_id += 1;
                let mut tabs: Vec<Tab> = Vec::with_capacity(pane_layout.tabs.len());
                for (i, tab_layout) in pane_layout.tabs.into_iter().enumerate() {
                    let kind = match &tab_layout.kind {
                        crate::layout::LayoutTabKind::Shell => TabContentKind::Shell,
                        crate::layout::LayoutTabKind::Welcome => TabContentKind::Welcome,
                        crate::layout::LayoutTabKind::Module { id } => {
                            TabContentKind::Module(id.clone())
                        }
                        crate::layout::LayoutTabKind::Agent { name } => {
                            TabContentKind::Agent(name.clone())
                        }
                    };
                    let cwd = tab_layout
                        .cwd
                        .as_deref()
                        .map(std::path::PathBuf::from)
                        .filter(|p| p.is_dir());
                    let tab_id = TabId(self.next_tab_id);
                    self.next_tab_id += 1;
                    let live_term = LiveTerm::new(
                        self.grid_cols,
                        self.grid_rows,
                        self.cell_advance,
                        self.line_height,
                        self.proxy.clone(),
                        tab_id,
                        cwd,
                        self.config.scrollback,
                    );
                    let title_buf = make_title_buffer(
                        &mut self.font_system,
                        &tab_layout.title,
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
                        100.0, // placeholder; relayout fixes it
                        100.0,
                    );
                    let mut tab = Tab::new(
                        tab_id,
                        tab_layout.title.clone(),
                        title_buf,
                        live_term,
                        text_buf,
                        self.grid_cols,
                        self.grid_rows,
                    );
                    tab.color_idx = tab_layout.color_idx;
                    post_build.push((pid, i, kind, tab_layout.focused_path));
                    tabs.push(tab);
                }
                let active_tab = pane_layout.active_tab.min(tabs.len().saturating_sub(1));
                let pane = Pane {
                    tabs,
                    active_tab,
                    bg_idx: pane_layout.bg_idx,
                    font_scale: pane_layout.font_scale,
                };
                PaneNode::Leaf { id: pid, pane }
            }
            crate::layout::LayoutNode::Split { dir, ratio, first, second } => {
                let dir = match dir {
                    crate::layout::LayoutSplitDir::Vertical => SplitDir::Vertical,
                    crate::layout::LayoutSplitDir::Horizontal => SplitDir::Horizontal,
                };
                PaneNode::Split {
                    dir,
                    ratio: ratio.clamp(0.1, 0.9),
                    first: Box::new(self.build_layout_node(*first, post_build)),
                    second: Box::new(self.build_layout_node(*second, post_build)),
                }
            }
        }
    }

    /// Walk the live pane tree → serializable Layout, then write it
    /// atomically to disk. Called on every structural change
    /// (split / close / new_tab / kind switch / publish_focus / cwd)
    /// so a crash mid-session doesn't lose the workspace.
    pub fn persist_layout(&self) {
        let Some(root) = self.root.as_ref() else {
            return;
        };
        let active_path = path_to(root, self.active_pane);
        let layout = crate::layout::Layout {
            version: crate::layout::LAYOUT_VERSION,
            active_pane_path: active_path,
            root: snapshot_node(root),
        };
        if let Err(e) = crate::layout::save(&layout) {
            crate::logging::warn(&format!("layout: save failed: {e}"));
        }
    }

    /// One message from a running module. Updates the tab's cached
    /// body (replace-only in v1) and asks for a redraw.
    pub fn handle_module_message(
        &mut self,
        tab_id: TabId,
        msg: crate::modules::ModuleMessage,
    ) {
        match msg {
            crate::modules::ModuleMessage::SetText {
                body,
                scroll_to_line,
                cursor,
                gutter,
                highlight_line,
                language,
            } => {
                // Bound the body — a runaway module that tries to
                // shape a 1 GB string would freeze the window.
                if body.len() > MAX_MODULE_BODY_BYTES {
                    crate::logging::warn(&format!(
                        "module tab {}: set_text body {} bytes > cap {} — dropped",
                        tab_id.0,
                        body.len(),
                        MAX_MODULE_BODY_BYTES
                    ));
                    return;
                }
                // Highlight up front — we need &self.highlight_store
                // before we take the mut borrow on tabs. The result
                // is dropped later if nothing actually changed.
                let new_highlights = language.as_deref().and_then(|lang| {
                    let spans = self.highlight_store.highlight(&body, lang);
                    if spans.is_empty() { None } else { Some(spans) }
                });
                let mut tabs: Vec<&mut Tab> = Vec::new();
                self.root
                    .as_mut()
                    .expect("pane tree present")
                    .all_tabs_mut(&mut tabs);
                let Some(tab) =
                    tabs.into_iter().find(|t| t.id == tab_id)
                else {
                    return;
                };
                let body_changed = tab.last_module_body != body;
                let new_cursor = cursor.map(|c| (c.line, c.col));
                let cursor_changed = tab.module_cursor != new_cursor;
                if body_changed {
                    tab.last_module_body = body.clone();
                    if let Some(sess) = tab.module_session.as_mut() {
                        sess.body = body;
                    }
                    tab.content_buffer = None;
                    // set_text and set_image are exclusive — a fresh
                    // body drops whatever image (still or animated)
                    // was on screen.
                    tab.image = None;
                    tab.animation = None;
                    // If the module doesn't tell us where to scroll,
                    // assume "new content, take me to the top." If it
                    // does (nav, editor, …), respect its position
                    // through the pending_ensure_visible path below
                    // so a wheel-scroll isn't reset out from under
                    // the user every keystroke.
                    if scroll_to_line.is_none() {
                        tab.module_scroll_y = 0.0;
                    }
                }
                tab.module_cursor = new_cursor;
                let gutter_changed = tab.module_gutter != gutter;
                if gutter_changed {
                    tab.module_gutter = gutter;
                    // Force gutter buffer rebuild — render path
                    // recreates it from the new labels on the next
                    // frame.
                    tab.gutter_buffer = None;
                }
                let highlight_changed = tab.module_highlight_line != highlight_line;
                tab.module_highlight_line = highlight_line;
                let language_changed = tab.module_language != language;
                let needs_rehighlight = body_changed || language_changed;
                if language_changed {
                    tab.module_language = language;
                }
                if needs_rehighlight {
                    tab.module_highlights = new_highlights;
                    // Force content buffer rebuild — rich-text
                    // construction uses the new spans (or absence
                    // thereof) on the next render pass.
                    tab.content_buffer = None;
                }
                if let Some(line) = scroll_to_line {
                    tab.pending_ensure_visible = Some(line);
                }
                if body_changed
                    || cursor_changed
                    || gutter_changed
                    || highlight_changed
                    || language_changed
                    || scroll_to_line.is_some()
                {
                    self.window.request_redraw();
                }
            }
            crate::modules::ModuleMessage::SetImage { path } => {
                // Read + decode in the host so the module wire stays
                // a small text protocol. Errors go back to the module
                // as log lines so the module's author can spot them
                // in the regular log without crashing the pane.
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        crate::logging::warn(&format!(
                            "module tab {}: set_image read failed for {path}: {e}",
                            tab_id.0
                        ));
                        return;
                    }
                };
                let Some(decoded) = crate::images::decode_any_animated(&bytes) else {
                    crate::logging::warn(&format!(
                        "module tab {}: set_image decode failed for {path} \
                         (supported: png/jpeg/gif/webp/bmp; oversize images rejected)",
                        tab_id.0
                    ));
                    return;
                };
                // Upload all frames *before* taking a mut borrow on
                // the pane tree — the texture path needs &device/&queue
                // off self while the tree lookup wants &mut self.root.
                let (still, animation) = match decoded {
                    crate::images::DecodedImage::Static(img) => {
                        let tex = self
                            .texture_renderer
                            .upload(&self.device, &self.queue, &img);
                        (Some(tex), None)
                    }
                    crate::images::DecodedImage::Animated { frames, total_ms } => {
                        let mut textures = Vec::with_capacity(frames.len());
                        let mut cumulative = Vec::with_capacity(frames.len());
                        let mut max_w = 0u32;
                        let mut max_h = 0u32;
                        let mut acc = 0u64;
                        for (img, delay) in frames {
                            max_w = max_w.max(img.width);
                            max_h = max_h.max(img.height);
                            let tex = self
                                .texture_renderer
                                .upload(&self.device, &self.queue, &img);
                            textures.push(tex);
                            acc = acc.saturating_add(delay);
                            cumulative.push(acc);
                        }
                        let anim = TabAnimation {
                            frames: textures,
                            width: max_w,
                            height: max_h,
                            cumulative,
                            total_ms,
                            started_at: Instant::now(),
                        };
                        (None, Some(anim))
                    }
                };
                let mut tabs: Vec<&mut Tab> = Vec::new();
                self.root
                    .as_mut()
                    .expect("pane tree present")
                    .all_tabs_mut(&mut tabs);
                if let Some(tab) = tabs.into_iter().find(|t| t.id == tab_id) {
                    tab.image = still;
                    tab.animation = animation;
                    tab.last_module_body.clear();
                    if let Some(sess) = tab.module_session.as_mut() {
                        sess.body.clear();
                    }
                    tab.content_buffer = None;
                    tab.module_scroll_y = 0.0;
                    self.window.request_redraw();
                }
            }
            crate::modules::ModuleMessage::ConfigRequest => {
                self.send_config_to_tab(tab_id);
            }
            crate::modules::ModuleMessage::ConfigSet { name, value } => {
                match self.apply_config_set(&name, &value) {
                    Ok(()) => {
                        crate::logging::info(&format!(
                            "config_set: {name} = {value}"
                        ));
                    }
                    Err(e) => crate::logging::warn(&format!("config_set: {e}")),
                }
                // Re-send the snapshot whether or not the set
                // succeeded — the module re-renders against the
                // current state (which may include an error).
                self.send_config_to_tab(tab_id);
            }
            crate::modules::ModuleMessage::Log { message } => {
                crate::logging::info(&format!("module tab {}: {message}", tab_id.0));
            }
            crate::modules::ModuleMessage::PublishFocus { path } => {
                // Cross-pane signaling — broadcast the new focus to
                // every *other* live data module session. Paired views
                // (nav → preview / editor) react via this single event.
                // We also record the path on each receiving tab so the
                // layout persistence captures "this pane had file X
                // open" — Editor reopens the same file on restore.
                crate::logging::info(&format!(
                    "module tab {}: focus {path}",
                    tab_id.0
                ));
                let mut tabs: Vec<&mut Tab> = Vec::new();
                self.root
                    .as_mut()
                    .expect("pane tree present")
                    .all_tabs_mut(&mut tabs);
                for tab in tabs {
                    if tab.id == tab_id {
                        continue;
                    }
                    if let Some(sess) = tab.module_session.as_ref() {
                        sess.send_focus(&path);
                        tab.last_focused_path = Some(path.clone());
                    }
                }
                self.persist_layout();
            }
        }
    }

}
