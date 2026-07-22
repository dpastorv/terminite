//! Input routing — selection geometry, mouse, scroll, and clipboard.

use super::*;

impl Renderer {
    // ── Mouse / keyboard input routing ────────────────────────────────────

    /// Convert a mouse pixel position into an absolute (Line, Column) using
    /// the current display_offset. Used for both selection start and extend.
    /// Look up the block whose row range contains a clicked selection-abs
    /// line. Returns the block's selection-coordinate range
    /// `((start_line, 0), (end_line, last_col))` — ready to drop into a
    /// `Selection`. Translates between the block store's session-absolute
    /// coordinates (history + cursor at fire time) and the selection
    /// model's `vl - display_offset` convention.
    pub(super) fn block_at_selection_line(&self, sel_line: i32) -> Option<((i32, usize), (i32, usize))> {
        let tab = self.active_tab_ref();
        let (_, history) = tab.live_term.offset_and_history();
        let history = history as i32;
        let session_abs = sel_line + history;
        let last_col = tab.cols.saturating_sub(1);

        // Iterates each block's start + raw-end pair once.
        let bounds = |block: &crate::blocks::Block| -> Option<(i32, i32, i32)> {
            let start = block.prompt_line.or(block.output_start_line)?;
            let raw_end = block
                .output_end_line
                .or(block.command_end_line)
                .or(block.prompt_line)?;
            let end = if raw_end > start { raw_end - 1 } else { raw_end };
            Some((start, end, raw_end))
        };

        // Pass 1 — unambiguous match. The trimmed range stops one row
        // above `output_end_line`, which is where the NEXT block's
        // prompt sits. So clicking on a row that visually says "demo$
        // echo hi" finds the echo-hi block (its trimmed range starts
        // at its prompt_line) rather than the prior false-block (which
        // claims that row only via its trailing-prompt overlap).
        for block in tab.blocks.iter() {
            if let Some((start, end, _)) = bounds(block) {
                if start <= session_abs && session_abs <= end {
                    return Some(((start - history, 0), (end - history, last_col)));
                }
            }
        }

        // Pass 2 — fall back to the raw range. Picks up clicks on the
        // trailing-prompt row of a no-output block that has no
        // following block yet (the open final block before the next
        // prompt fires).
        for block in tab.blocks.iter() {
            if let Some((start, end, raw_end)) = bounds(block) {
                if start <= session_abs && session_abs <= raw_end {
                    return Some(((start - history, 0), (end - history, last_col)));
                }
            }
        }

        None
    }

    pub(super) fn pixel_to_absolute(&self, x: f32, y: f32) -> (i32, usize) {
        let metrics = self.active_pane_metrics();
        let pad = self.pad;
        let line_height = metrics.line_height;
        let apr = self.active_pane_rect();
        let left = apr.x + pad.left;
        let top = apr.y + self.tab_bar_height + pad.top;
        let cx = (x - left).max(0.0);
        let col = ((cx / metrics.cell_advance) as usize)
            .min(self.grid_cols.saturating_sub(1));
        // Same pixel_offset correction as cell_at_1indexed, but with a signed
        // floor so a click just inside the top of viewport while the buffer
        // is shifted down resolves to row -1 (the extra row above the
        // viewport) when appropriate.
        let cy = (y - top - self.active_tab_ref().pixel_offset) / line_height;
        let vl = cy.floor() as i32;
        let vl = vl.max(-1).min(self.grid_rows as i32 - 1);
        let display_offset = self.active_tab_ref().live_term.offset_and_history().0 as i32;
        (vl - display_offset, col)
    }

    pub fn mouse_moved(&mut self, x: f32, y: f32, modifiers: ModifiersState) {
        self.mouse_pos = (x, y);
        let metrics = self.active_pane_metrics();
        let pad = self.pad;
        let line_height = metrics.line_height;

        // Context menu up — just track the hovered item.
        if self.context_menu.is_some() {
            let hit = self.context_menu_at(x, y);
            if let Some(menu) = self.context_menu.as_mut() {
                if menu.hovered != hit {
                    menu.hovered = hit;
                    self.window.request_redraw();
                }
            }
            return;
        }

        // Dragging a display-settings font-size slider. The `*_inner` setters
        // early-return when the rounded size is unchanged (so trackpad
        // micro-moves don't churn the grid) and report whether it changed — we
        // only rebuild the card's thumb + label on a real step.
        if let Some(kind) = self.slider_drag {
            if let Some(pt) = self.display_slider_drag_pt(kind, x) {
                let changed = match kind {
                    SliderKind::Content => self.set_font_size_inner(pt),
                    SliderKind::Tab => self.set_tab_font_size_inner(pt),
                    SliderKind::TabHeight => self.set_tab_bar_height_inner(pt),
                };
                if changed {
                    self.open_display_settings();
                }
            }
            return;
        }

        // Dragging a split divider — resize the split it belongs to.
        if let Some(drag) = self.divider_drag.as_ref() {
            let (outer, dir, path) = (drag.outer, drag.dir, drag.path.clone());
            let raw = match dir {
                SplitDir::Vertical => {
                    (x - DIVIDER_THICKNESS / 2.0 - outer.x)
                        / (outer.w - DIVIDER_THICKNESS).max(1.0)
                }
                SplitDir::Horizontal => {
                    (y - DIVIDER_THICKNESS / 2.0 - outer.y)
                        / (outer.h - DIVIDER_THICKNESS).max(1.0)
                }
            };
            let span = match dir {
                SplitDir::Vertical => outer.w,
                SplitDir::Horizontal => outer.h,
            };
            let ratio = clamp_ratio(raw, span);
            if let Some(r) = self.root_mut().split_ratio_at_mut(&path) {
                *r = ratio;
            }
            self.relayout();
            self.sync_active_grid();
            self.window.request_redraw();
            return;
        }

        // Dragging a corner split handle — refresh so the preview tracks.
        if self.split_gesture.is_some() {
            if self.cursor_icon != CursorIcon::Grabbing {
                self.cursor_icon = CursorIcon::Grabbing;
                self.window.set_cursor(CursorIcon::Grabbing);
            }
            self.window.request_redraw();
            return;
        }

        // Cursor feedback: grab over a corner handle, resize over a divider.
        let over_handle = self
            .pane_at(x, y)
            .map(|(_, r)| in_split_handle(r, x, y))
            .unwrap_or(false);
        let desired = if over_handle {
            CursorIcon::Grab
        } else {
            match self
                .root_ref()
                .divider_at(self.content_rect(), x, y)
                .map(|(_, _, d)| d)
            {
                Some(SplitDir::Vertical) => CursorIcon::ColResize,
                Some(SplitDir::Horizontal) => CursorIcon::RowResize,
                None => CursorIcon::Default,
            }
        };
        if desired != self.cursor_icon {
            self.cursor_icon = desired;
            self.window.set_cursor(desired);
        }

        // Mouse reporting takes precedence over selection / scroll.
        let mode = self.active_tab_mut().live_term.mode_flags();
        let reporting_active = mode.mouse_drag || mode.mouse_motion;
        if reporting_active {
            // Drag (1002): only when a button is held. Motion (1003): always.
            let button_held = self.active_tab_mut().dragging || mode.mouse_motion;
            if mode.mouse_motion || (mode.mouse_drag && button_held) {
                if let Some((col, row)) = self.cell_at_1indexed(x, y) {
                    let bytes = encode_mouse_report(
                        &mode,
                        MouseEvent::Motion,
                        modifiers,
                        col,
                        row,
                    );
                    if let Some(b) = bytes {
                        self.active_tab_mut().live_term.write(b);
                    }
                }
            }
            return;
        }

        if self.active_tab_mut().dragging {
            // macOS trackpad scrolling drags the cursor a hair, so we get
            // tiny mouse_moved events interleaved with wheel events. Without
            // this filter, every wheel-driven extension to the viewport
            // edge gets immediately snapped back to whatever cell the
            // cursor is currently over. Only count motion that crosses
            // half a cell from the last update.
            let (last_x, last_y) = self.active_tab_mut().last_drag_mouse_pos;
            let dx = (x - last_x).abs();
            let dy = (y - last_y).abs();
            let big_motion = dx >= metrics.cell_advance * 0.5 || dy >= line_height * 0.5;
            if big_motion {
                let (line, col) = self.pixel_to_absolute(x, y);
                if let Some(sel) = self.active_tab_mut().selection.as_mut() {
                    sel.extend_to(line, col);
                }
                self.active_tab_mut().last_drag_mouse_pos = (x, y);
                self.window.request_redraw();
            }

            // Auto-scroll if the cursor is past the viewport's top or
            // bottom edge: keep scrolling while the user holds the button
            // there, extending the selection as new content reveals.
            let apr = self.active_pane_rect();
            let pane_top = apr.y + self.tab_bar_height + pad.top;
            let pane_bottom = apr.y + apr.h;
            let new_dir = if y < pane_top + AUTOSCROLL_MARGIN_PX {
                Some(1)
            } else if y > pane_bottom - AUTOSCROLL_MARGIN_PX {
                Some(-1)
            } else {
                None
            };
            let was_off = self.active_tab_mut().autoscroll_dir.is_none();
            self.active_tab_mut().autoscroll_dir = new_dir;
            match new_dir {
                Some(_) if was_off => {
                    self.next_autoscroll_deadline =
                        Some(Instant::now() + Duration::from_millis(AUTOSCROLL_TICK_MS));
                    self.window.request_redraw();
                }
                None => self.next_autoscroll_deadline = None,
                _ => {}
            }
        }
    }

    pub fn mouse_down(&mut self, button: MouseButton, modifiers: ModifiersState) {
        // Modal eats input — clicks hit-test modal buttons; everything else
        // is swallowed until the user picks Cancel or Confirm.
        if self.modal.is_some() {
            if button == MouseButton::Left {
                if self.modal_click(self.mouse_pos.0, self.mouse_pos.1) {
                    let _ = self.proxy.send_event(UserEvent::Exit);
                }
            }
            return;
        }

        // Context menu up — any click resolves it (an item, or dismiss).
        if self.context_menu.is_some() {
            self.context_menu_click(self.mouse_pos.0, self.mouse_pos.1);
            return;
        }

        // Display settings overlay — a press on a slider track starts a drag;
        // a press on Reset restores both axes to their configured defaults.
        // Drags apply via the non-persisting `*_inner`; mouse_up persists once.
        if button == MouseButton::Left && self.has_display_settings() {
            if let Some((kind, pt)) = self.display_slider_at(self.mouse_pos.0, self.mouse_pos.1) {
                self.slider_drag = Some(kind);
                match kind {
                    SliderKind::Content => self.set_font_size_inner(pt),
                    SliderKind::Tab => self.set_tab_font_size_inner(pt),
                    SliderKind::TabHeight => self.set_tab_bar_height_inner(pt),
                };
                self.open_display_settings(); // refresh thumb + label
                return;
            }
            if self.hit_display_reset(self.mouse_pos.0, self.mouse_pos.1) {
                self.set_font_size(self.config.font_size);
                self.set_tab_font_size(self.config.tab_font_size);
                self.set_tab_bar_height(self.config.tab_bar_height);
                self.open_display_settings();
                return;
            }
        }

        // A left-press on a split divider starts a resize drag.
        if button == MouseButton::Left {
            if let Some((path, outer, dir)) = self.root_ref().divider_at(
                self.content_rect(),
                self.mouse_pos.0,
                self.mouse_pos.1,
            ) {
                self.divider_drag = Some(DividerDrag { path, outer, dir });
                return;
            }
        }

        // A left-press on a pane's top-right corner handle starts a split
        // gesture (drag down to stack, drag left for side by side).
        if button == MouseButton::Left {
            if let Some((pid, prect)) = self.pane_at(self.mouse_pos.0, self.mouse_pos.1) {
                if in_split_handle(prect, self.mouse_pos.0, self.mouse_pos.1) {
                    self.split_gesture = Some(SplitGesture {
                        pid,
                        start: self.mouse_pos,
                    });
                    self.window.request_redraw();
                    return;
                }
            }
        }

        // Tab-bar hit test first — a click in a pane's own tab bar strip
        // switches / closes that pane's tabs and never starts a selection.
        if let Some((pid, prect)) = self.pane_at(self.mouse_pos.0, self.mouse_pos.1) {
            if self.mouse_pos.1 < prect.y + self.tab_bar_height {
                self.focus_pane(pid);
                if button == MouseButton::Left {
                    self.tab_bar_click(pid, prect);
                }
                return;
            }
        }

        // Otherwise the click lands in a pane's content — focus that pane
        // before anything routes to "the active pane".
        self.focus_pane_at(self.mouse_pos.0, self.mouse_pos.1);

        // Data-module pane click → translate pixel → (source line,
        // visual column) in the body and forward to the module so
        // it can move its cursor (Editor) or pick the row (Nav).
        if button == MouseButton::Left {
            if let Some((pid, prect)) = self.pane_at(self.mouse_pos.0, self.mouse_pos.1) {
                if self.dispatch_data_module_click(pid, prect) {
                    return;
                }
            }
        }

        let mode = self.active_tab_mut().live_term.mode_flags();
        if mode.mouse_report_click || mode.mouse_drag || mode.mouse_motion {
            if let Some((col, row)) = self.cell_at_1indexed(self.mouse_pos.0, self.mouse_pos.1) {
                let bytes = encode_mouse_report(
                    &mode,
                    MouseEvent::Press(button),
                    modifiers,
                    col,
                    row,
                );
                if let Some(b) = bytes {
                    self.active_tab_mut().live_term.write(b);
                }
            }
            return;
        }

        // Right-click opens the context menu.
        if button == MouseButton::Right {
            self.open_context_menu(self.mouse_pos.0, self.mouse_pos.1);
            return;
        }

        // Only the left button does anything further.
        if button != MouseButton::Left {
            return;
        }

        let (line, col) = self.pixel_to_absolute(self.mouse_pos.0, self.mouse_pos.1);

        // Cmd-click an OSC 8 hyperlink → open it; don't start a selection.
        if modifiers.super_key() {
            if let Some(uri) = self.active_tab_mut().live_term.hyperlink_at(line, col) {
                open_uri(&uri);
                return;
            }
            // Cmd-click inside a block → select the whole block (prompt +
            // output) and copy it. The command + output reads as a unit on
            // the clipboard — pair-friendly "share what just happened."
            if let Some((start, end)) = self.block_at_selection_line(line) {
                let tab = self.active_tab_mut();
                tab.selection = Some(Selection {
                    anchor_line: start.0,
                    anchor_col: start.1,
                    head_line: end.0,
                    head_col: end.1,
                });
                tab.dragging = false;
                self.copy_selection();
                self.window.request_redraw();
                return;
            }
        }
        let now = Instant::now();
        let click_count = match self.active_tab_mut().last_click {
            Some((t, cell, c)) if now.duration_since(t) < MULTI_CLICK_WINDOW && cell == (line, col) => {
                (c + 1).min(3)
            }
            _ => 1,
        };
        self.active_tab_mut().last_click = Some((now, (line, col), click_count));

        match click_count {
            1 => {
                self.active_tab_mut().selection = Some(Selection::from_anchor(line, col));
                self.active_tab_mut().dragging = true;
                self.active_tab_mut().last_drag_mouse_pos = self.mouse_pos;
            }
            2 => {
                let ((sl, sc), (el, ec)) = self.active_tab_mut().live_term.word_at(line, col);
                self.active_tab_mut().selection = Some(Selection {
                    anchor_line: sl,
                    anchor_col: sc,
                    head_line: el,
                    head_col: ec,
                });
                self.active_tab_mut().dragging = false;
                self.copy_selection();
            }
            _ => {
                let ((sl, sc), (el, ec)) = self.active_tab_mut().live_term.line_at(line);
                self.active_tab_mut().selection = Some(Selection {
                    anchor_line: sl,
                    anchor_col: sc,
                    head_line: el,
                    head_col: ec,
                });
                self.active_tab_mut().dragging = false;
                self.copy_selection();
            }
        }
        self.window.request_redraw();
    }

    pub fn mouse_up(&mut self, button: MouseButton, modifiers: ModifiersState) {
        // End a font-size slider drag. Drags applied via the non-persisting
        // `*_inner` setters, so persist the final size once here.
        if self.slider_drag.take().is_some() {
            self.persist_layout();
            return;
        }

        // Finish a corner gesture: drag in splits the pane at the cursor,
        // drag back out removes it; a short drag cancels.
        if let Some(g) = self.split_gesture.take() {
            let dx = self.mouse_pos.0 - g.start.0;
            let dy = self.mouse_pos.1 - g.start.1;
            match gesture_outcome(dx, dy) {
                Some(GestureOutcome::Split(dir)) => {
                    let rect = self
                        .pane_layout()
                        .into_iter()
                        .find(|(id, _)| *id == g.pid)
                        .map(|(_, r)| r);
                    if let Some(r) = rect {
                        let ratio = split_ratio_from_cursor(
                            r,
                            dir,
                            self.mouse_pos.0,
                            self.mouse_pos.1,
                        );
                        self.focus_pane(g.pid);
                        self.split_active(dir, ratio);
                    }
                }
                Some(GestureOutcome::Remove) => {
                    // Blender join: the pane the handle is dragged ONTO is the
                    // one consumed; the source pane (g.pid) survives and grows.
                    // Do nothing if the cursor isn't over a different pane —
                    // there's no neighbour to join into.
                    if let Some((target, _)) =
                        self.pane_at(self.mouse_pos.0, self.mouse_pos.1)
                    {
                        if target != g.pid {
                            self.focus_pane(target);
                            self.request_close_active_pane();
                        }
                    }
                }
                None => {}
            }
            self.cursor_icon = CursorIcon::Default;
            self.window.set_cursor(CursorIcon::Default);
            self.window.request_redraw();
            return;
        }

        // End a divider drag, if one is in progress.
        if self.divider_drag.is_some() {
            self.divider_drag = None;
            return;
        }

        let mode = self.active_tab_mut().live_term.mode_flags();
        if mode.mouse_report_click || mode.mouse_drag || mode.mouse_motion {
            if let Some((col, row)) = self.cell_at_1indexed(self.mouse_pos.0, self.mouse_pos.1) {
                let bytes = encode_mouse_report(
                    &mode,
                    MouseEvent::Release(button),
                    modifiers,
                    col,
                    row,
                );
                if let Some(b) = bytes {
                    self.active_tab_mut().live_term.write(b);
                }
            }
            return;
        }

        if button != MouseButton::Left {
            return;
        }

        self.active_tab_mut().dragging = false;
        self.active_tab_mut().autoscroll_dir = None;
        self.next_autoscroll_deadline = None;
        if let Some(sel) = self.active_tab_mut().selection.as_ref() {
            if sel.is_empty() {
                self.active_tab_mut().selection = None;
            } else {
                self.copy_selection();
            }
        }
        self.window.request_redraw();
    }

    pub fn mouse_wheel(&mut self, delta: MouseScrollDelta, modifiers: ModifiersState) {
        // Cmd+wheel zooms the font instead of scrolling. (Cmd, not Ctrl —
        // macOS reserves Ctrl+scroll for its own screen zoom, so it never
        // reaches us.) We round to whole pixels, and set_font_size early-returns
        // when the size is unchanged — so trackpad pixel-deltas (tiny per event)
        // only relayout when they actually cross an integer, no SIGWINCH storm.
        if modifiers.super_key() {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y * 2.0, // ~2px per wheel notch
                MouseScrollDelta::PixelDelta(p) => p.y as f32 * 0.05, // trackpad: gentle
            };
            if dy != 0.0 {
                self.set_font_size((self.font_size + dy).round());
            }
            return;
        }

        // The wheel acts on the pane *under the cursor* — you can scroll a
        // pane's history without stealing keyboard focus from another.
        let pid = match self.pane_at(self.mouse_pos.0, self.mouse_pos.1) {
            Some((pid, _)) => pid,
            None => return,
        };
        let line_height = self.pane_metrics(pid).line_height;

        // Data-module panes have no PTY — wheel events scroll the
        // rendered body instead of being forwarded as arrow keys to
        // a shell that isn't there. TTY modules have their own PTY
        // and fall through to the regular path below.
        {
            let tab = self.pane_tab_mut(pid);
            let is_data_module = matches!(tab.kind, TabContentKind::Module(_))
                && tab.module_pty.is_none();
            if is_data_module {
                let pixels = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 3.0 * line_height,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                // Up wheel (positive y) reveals earlier content =
                // scroll_y goes negative (text moves down on screen).
                // The render path clamps against laid-out content
                // height, so over-scroll past the ends is a no-op.
                tab.module_scroll_y -= pixels;
                if tab.module_scroll_y < 0.0 {
                    tab.module_scroll_y = 0.0;
                }
                self.window.request_redraw();
                return;
            }
        }

        let mode = self.pane_tab_mut(pid).active_term().mode_flags();

        // Alt-screen TUIs (nano, vim, less, htop, man) replace the main
        // screen with their own — alacritty's scrollback is empty there,
        // so the normal scroll path is a no-op and the pane feels dead
        // to the wheel. When the app isn't asking for mouse reports
        // either, translate wheel events into Up/Down arrow key bytes
        // so its own scroll machinery responds. Matches what iTerm2 /
        // Alacritty / kitty do. The wheel acts on the pane under the
        // cursor; we route to that pane's PTY regardless of focus.
        let mouse_mode_active = mode.mouse_report_click || mode.mouse_drag || mode.mouse_motion;
        if mode.alt_screen && !mouse_mode_active {
            let lines = match delta {
                MouseScrollDelta::LineDelta(_, y) => y * 3.0,
                MouseScrollDelta::PixelDelta(p) => p.y as f32 / line_height,
            };
            // Cap at a sane per-event maximum so a runaway delta can't
            // flood the PTY with hundreds of arrow keys.
            let count = (lines.abs().round() as usize).min(100);
            if count == 0 {
                return;
            }
            // Application cursor mode (DECCKM): `ESC O A/B`. Default
            // mode: `ESC [ A/B`. Wheel-up = scroll content down in the
            // viewport = Up arrow.
            let seq: &[u8] = match (lines > 0.0, mode.app_cursor) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            let mut bytes = Vec::with_capacity(seq.len() * count);
            for _ in 0..count {
                bytes.extend_from_slice(seq);
            }
            self.pane_tab_mut(pid).active_term().write(bytes);
            self.window.request_redraw();
            return;
        }

        // If the foreground app wants scroll reports (vim, less, htop in
        // mouse mode), forward instead of scrolling the viewport. Reporting
        // only routes when the hovered pane is also the focused one — the
        // cell math resolves against the active pane's rect.
        if mouse_mode_active {
            if pid != self.active_pane {
                return;
            }
            let pixels = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => p.y as f32 / line_height,
            };
            let direction = if pixels > 0.0 {
                MouseEvent::WheelUp
            } else if pixels < 0.0 {
                MouseEvent::WheelDown
            } else {
                return;
            };
            if let Some((col, row)) = self.cell_at_1indexed(self.mouse_pos.0, self.mouse_pos.1) {
                if let Some(b) = encode_mouse_report(&mode, direction, modifiers, col, row) {
                    self.pane_tab_mut(pid).active_term().write(b);
                }
            }
            return;
        }

        // Work in physical pixels so the renderer can shift by the remainder
        // for pixel-smooth scrolling. LineDelta is real-wheel "clicks" (~3
        // lines each, scaled to pixels); PixelDelta is trackpad pixels.
        let pixels = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 3.0 * line_height,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };

        // Boundary block: once we're at the top of scrollback or the live
        // bottom, more events in that direction can't move the term. The
        // "actual != whole" zero-out logic below catches it on the
        // line-pop step, but on a fast trackpad burst (events at ~120 Hz)
        // each event accumulates a visible sub-line `pixel_offset` before
        // getting zeroed — the user sees the text shaking at the
        // boundary. Drop wheel events whose direction is blocked AND
        // whose existing residual is in the same direction (so a tiny
        // reversal still goes through to undo the smooth shift).
        let (cur_offset, history) = self.pane_tab_mut(pid).active_term().offset_and_history();
        let residual = self.pane_tab_mut(pid).pixel_offset;
        let blocked_up = pixels > 0.0 && cur_offset >= history && residual >= 0.0;
        let blocked_down = pixels < 0.0 && cur_offset == 0 && residual <= 0.0;
        if blocked_up || blocked_down {
            return;
        }

        self.pane_tab_mut(pid).pixel_offset += pixels;

        // Pop whole lines into the term; the remainder stays as a sub-line
        // pixel shift used at render time. `floor` keeps the remainder in
        // [0, line_height) for any input direction — but only when the
        // requested scroll actually happens. If alacritty clamps (we asked
        // Delta(-2) but were at offset=1), subtracting the full `whole`
        // leaves a residual that renders as motion in the wrong direction,
        // and floor's over-pop re-establishes the residual on every event
        // — so the bottom (offset=0) is never reached cleanly. Subtract by
        // the *actual* offset delta instead.
        let whole = (self.pane_tab_mut(pid).pixel_offset / line_height).floor() as i32;
        if whole != 0 {
            let (before, _) = self.pane_tab_mut(pid).active_term().offset_and_history();
            self.pane_tab_mut(pid).active_term_mut().scroll(TermScroll::Delta(whole));
            let (after, history) = self.pane_tab_mut(pid).active_term().offset_and_history();
            let actual = after as i32 - before as i32;
            self.pane_tab_mut(pid).pixel_offset -= actual as f32 * line_height;
            if actual != whole {
                // Clamped at a scrollback boundary; drop the residual.
                self.pane_tab_mut(pid).pixel_offset = 0.0;
            }
            let _ = history;

            // While dragging, extending the head to wherever the mouse pixel
            // sits would actually *shrink* the selection as scroll reveals
            // new content (the same pixel now points at an older row going
            // up, newer going down). Instead push the head to the viewport
            // edge in the scroll direction, so the selection grows to cover
            // the freshly-revealed lines. Pick whichever extends *further*
            // from the anchor — mouse position still wins when it's already
            // farther.
            if actual != 0 && pid == self.active_pane && self.active_tab_mut().dragging {
                let (mouse_line, mouse_col) =
                    self.pixel_to_absolute(self.mouse_pos.0, self.mouse_pos.1);
                let edge = if actual > 0 {
                    // Scrolled UP — viewport top is the oldest edge.
                    (-(after as i32), 0_usize)
                } else {
                    // Scrolled DOWN — viewport bottom is the newest edge.
                    (
                        self.grid_rows as i32 - 1 - after as i32,
                        self.grid_cols.saturating_sub(1),
                    )
                };
                if let Some(sel) = self.active_tab_mut().selection.as_mut() {
                    let edge_d = (edge.0 - sel.anchor_line).abs();
                    let mouse_d = (mouse_line - sel.anchor_line).abs();
                    let (head_line, head_col) = if edge_d > mouse_d {
                        edge
                    } else {
                        (mouse_line, mouse_col)
                    };
                    sel.extend_to(head_line, head_col);
                }
            }
        }

        self.window.request_redraw();
    }

    pub fn scroll_page(&self, up: bool) {
        let s = if up { TermScroll::PageUp } else { TermScroll::PageDown };
        self.active_tab_ref().live_term.scroll(s);
        self.window.request_redraw();
    }

    /// Cmd+Up / Cmd+Down (and Cmd+Home / Cmd+End) — jump the viewport to the
    /// top of scrollback or back down to the live prompt.
    pub fn scroll_to_edge(&self, top: bool) {
        let s = if top { TermScroll::Top } else { TermScroll::Bottom };
        self.active_tab_ref().live_term.scroll(s);
        self.window.request_redraw();
    }

    /// Cmd+K — clear the active pane's scrollback.
    pub fn clear_scrollback(&self) {
        self.active_tab_ref().live_term.clear_scrollback();
        self.window.request_redraw();
    }

    /// Cmd+A — select the whole buffer (history + screen) and copy it,
    /// matching the right-click "Select All". Shared by both entry points.
    pub fn select_all(&mut self) {
        let ((sl, sc), (el, ec)) = self.active_tab_mut().live_term.whole_buffer();
        self.active_tab_mut().selection = Some(Selection {
            anchor_line: sl,
            anchor_col: sc,
            head_line: el,
            head_col: ec,
        });
        self.copy_selection();
        self.window.request_redraw();
    }

    pub fn copy_selection(&mut self) {
        let Some(sel) = self.active_tab_mut().selection.as_ref() else { return };
        if sel.is_empty() {
            return;
        }
        let (start, end) = sel.normalized();
        let text = self.active_tab_mut().live_term.extract_text(start, end);
        if text.is_empty() {
            return;
        }
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(text);
        }
    }

    pub fn paste(&mut self) {
        let text = match self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
            Some(t) => t,
            None => return,
        };
        if text.is_empty() {
            return;
        }
        self.note_human_input();
        if self.active_tab_mut().live_term.mode_flags().bracketed_paste {
            // Wrap so the shell treats the whole paste as one input, not as
            // typed-and-pressed-enter for each newline. Strips any embedded
            // \e[201~ to keep the framing safe.
            let safe = text.replace("\x1b[201~", "");
            let mut bytes = Vec::with_capacity(safe.len() + 12);
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(safe.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            self.active_tab_mut().live_term.write(bytes);
        } else {
            self.active_tab_mut().live_term.write(text.into_bytes());
        }
    }
}

// ── moved from mod.rs ───────────────────────────────

impl Renderer {
    pub fn ime_preedit(&mut self, text: String) {
        self.preedit = text;
        self.window.request_redraw();
    }

    pub fn ime_commit(&mut self, text: String) {
        self.preedit.clear();
        if !text.is_empty() {
            self.note_human_input();
            self.active_tab_mut().active_term().write(text.into_bytes());
        }
        self.window.request_redraw();
    }

    /// 1-indexed (col, row) inside the visible viewport, for mouse-reporting
    /// protocols. Returns `None` if the pointer is outside the text area.
    pub(super) fn cell_at_1indexed(&self, x: f32, y: f32) -> Option<(u32, u32)> {
        let metrics = self.active_pane_metrics();
        let pad = self.pad;
        let line_height = metrics.line_height;
        let apr = self.active_pane_rect();
        let left = apr.x + pad.left;
        let top = apr.y + self.tab_bar_height + pad.top;
        if x < left {
            return None;
        }
        // pixel_offset correction so the reported cell is the one the user
        // visually clicked on, not the natural-grid cell.
        let row_f = (y - top - self.active_tab_ref().pixel_offset) / line_height;
        if row_f < 0.0 {
            return None;
        }
        let col = ((x - left) / metrics.cell_advance) as u32 + 1;
        let row = row_f as u32 + 1;
        if col as usize > self.grid_cols || row as usize > self.grid_rows {
            return None;
        }
        Some((col, row))
    }
}

// ── helpers moved from mod.rs ──────────────────────

/// What kind of mouse event we're reporting. The button is needed for
/// press/release; motion/wheel use a synthetic encoding.
#[derive(Clone, Copy)]
pub(super) enum MouseEvent {
    Press(MouseButton),
    Release(MouseButton),
    Motion,
    WheelUp,
    WheelDown,
}

/// Encode a mouse event in the format the foreground app has asked for.
/// SGR (1006) preferred; X10 used as fallback. Modifiers add the standard
/// shift / alt / ctrl bits. Returns `None` if reporting isn't enabled.
pub(super) fn encode_mouse_report(
    mode: &ModeFlags,
    event: MouseEvent,
    modifiers: ModifiersState,
    col: u32,
    row: u32,
) -> Option<Vec<u8>> {
    if !mode.mouse_report_click && !mode.mouse_drag && !mode.mouse_motion {
        return None;
    }

    // Base button code.
    let (mut btn, is_release) = match event {
        MouseEvent::Press(b) => (button_code(b)?, false),
        MouseEvent::Release(b) => (button_code(b)?, true),
        MouseEvent::Motion => (32, false),         // motion modifier on no button
        MouseEvent::WheelUp => (64, false),
        MouseEvent::WheelDown => (65, false),
    };

    if modifiers.shift_key() {
        btn |= 4;
    }
    if modifiers.alt_key() {
        btn |= 8;
    }
    if modifiers.control_key() {
        btn |= 16;
    }

    if mode.sgr_mouse {
        let suffix = if is_release { 'm' } else { 'M' };
        Some(format!("\x1b[<{};{};{}{}", btn, col, row, suffix).into_bytes())
    } else {
        // X10: \e[M{btn+32}{col+32}{row+32}. Release uses button 3 (no info
        // about which button was released).
        let btn = if is_release { 3 } else { btn };
        let clamp = |v: u32| (v.min(223)) as u8;
        let mut out = Vec::with_capacity(6);
        out.extend_from_slice(b"\x1b[M");
        out.push((btn as u8).saturating_add(32));
        out.push(clamp(col).saturating_add(32));
        out.push(clamp(row).saturating_add(32));
        Some(out)
    }
}

pub(super) fn button_code(button: MouseButton) -> Option<u32> {
    Some(match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        _ => return None,
    })
}

/// Line height as a multiple of font size — the pre-config ratio (36 at
/// font size 28). Derives `line_height` from the configured `font_size`.
pub(super) const LINE_H_RATIO: f32 = 36.0 / 28.0;


