//! Frame assembly — render(), per-pane rendering, non-shell panes.

use super::*;

impl Renderer {
    // ── Frame ────────────────────────────────────────────────────────────

    pub fn render(&mut self) {
        check_rss_kill_switch(self.rss_kill_bytes);
        self.refresh_auto_titles();
        let frame_start = Instant::now();

        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        // Cursor blink — one phase shared by every pane. alacritty's
        // CursorStyle.blinking is false unless the shell sends `\e[1/3/5 q`;
        // respecting that strictly freezes the cursor in default zsh/bash,
        // so we blink whenever the window is focused — unless the user has
        // turned `cursor_blink` off in the config.
        let blink = self.focused && self.config.cursor_blink;
        let blink_on = if blink {
            let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
            elapsed_ms % CURSOR_BLINK_PERIOD_MS < CURSOR_BLINK_PERIOD_MS / 2
        } else {
            true
        };
        // Surface the next blink phase change as a deadline so the main loop's
        // WaitUntil wakes us — no per-frame thread spawn.
        self.next_blink_deadline = if blink {
            let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
            let half = CURSOR_BLINK_PERIOD_MS / 2;
            let into_half = elapsed_ms % half;
            Some(Instant::now() + Duration::from_millis((half - into_half).max(1)))
        } else {
            None
        };
        // render_pane re-arms this if a pane is autoscrolling.
        self.next_autoscroll_deadline = None;

        // Lay out the window's pane tree, then render each pane into its rect.
        let layout = self.pane_layout();
        let active_pane = self.active_pane;
        let mut below: Vec<RectInstance> = Vec::new();
        let mut above: Vec<RectInstance> = Vec::new();
        let mut tab_bar: Vec<RectInstance> = Vec::new();
        let mut draws: Vec<PaneDraw> = Vec::with_capacity(layout.len());
        for (pid, rect) in &layout {
            let d = self.render_pane(
                *pid,
                *rect,
                *pid == active_pane,
                blink_on,
                &mut below,
                &mut above,
                &mut tab_bar,
            );
            draws.push(d);
        }

        // Split divider seams drawn on top of pane content.
        collect_dividers(self.root_ref(), self.content_rect(), &mut above);

        // Live preview of a corner-handle gesture: a gold line at the
        // cursor-placed split, or a red wash over a pane about to be removed.
        if let Some(g) = self.split_gesture.as_ref() {
            let dx = self.mouse_pos.0 - g.start.0;
            let dy = self.mouse_pos.1 - g.start.1;
            if let Some((_, r)) = layout.iter().find(|(id, _)| *id == g.pid).copied() {
                match gesture_outcome(dx, dy) {
                    Some(GestureOutcome::Split(dir)) => {
                        let ratio = split_ratio_from_cursor(
                            r,
                            dir,
                            self.mouse_pos.0,
                            self.mouse_pos.1,
                        );
                        let preview = match dir {
                            SplitDir::Horizontal => [
                                r.x,
                                r.y + (r.h - DIVIDER_THICKNESS) * ratio,
                                r.w,
                                DIVIDER_THICKNESS,
                            ],
                            SplitDir::Vertical => [
                                r.x + (r.w - DIVIDER_THICKNESS) * ratio,
                                r.y,
                                DIVIDER_THICKNESS,
                                r.h,
                            ],
                        };
                        above.push(RectInstance {
                            rect: preview,
                            color: TAB_ACTIVE_UNDERLINE,
                        });
                    }
                    Some(GestureOutcome::Remove) => {
                        // Wash the pane the cursor is over — the one that will
                        // be consumed — not the source the handle came from.
                        let (mx, my) = self.mouse_pos;
                        if let Some((_, tr)) = layout
                            .iter()
                            .find(|(id, rr)| {
                                *id != g.pid
                                    && mx >= rr.x
                                    && mx < rr.x + rr.w
                                    && my >= rr.y
                                    && my < rr.y + rr.h
                            })
                            .copied()
                        {
                            above.push(RectInstance {
                                rect: [tr.x, tr.y, tr.w, tr.h],
                                color: REMOVE_PREVIEW_COLOR,
                            });
                        }
                    }
                    None => {}
                }
            }
        }

        // Find bar background — a floating box at the active pane's
        // top-right. The query text is drawn by the tab text renderer.
        let find_bar_origin = if self.find.is_some() {
            let apr = self.active_pane_rect();
            let bx = apr.x + apr.w - FIND_BAR_W - FIND_BAR_MARGIN;
            let by = apr.y + self.tab_bar_height + FIND_BAR_MARGIN;
            above.push(RectInstance {
                rect: [bx - 1.0, by - 1.0, FIND_BAR_W + 2.0, FIND_BAR_H + 2.0],
                color: FIND_BAR_BORDER,
            });
            above.push(RectInstance {
                rect: [bx, by, FIND_BAR_W, FIND_BAR_H],
                color: FIND_BAR_BG,
            });
            Some((bx, by))
        } else {
            None
        };

        // Bell flash: a soft warm overlay over the whole surface. Auto-clears
        // when the deadline passes; a thread already scheduled a wakeup.
        if let Some(until) = self.bell_flash_until {
            if Instant::now() < until {
                above.push(RectInstance {
                    rect: [
                        0.0,
                        0.0,
                        self.surface_config.width as f32,
                        self.surface_config.height as f32,
                    ],
                    color: BELL_COLOR,
                });
            } else {
                self.bell_flash_until = None;
            }
        }

        let resolution = [
            self.surface_config.width as f32,
            self.surface_config.height as f32,
        ];
        // The modal and the context menu share the rects_modal /
        // modal_text_renderer pipelines — they're mutually exclusive in
        // practice and the modal wins if both are somehow set.
        let overlay_rects = if self.modal.is_some() {
            self.build_modal_rects()
        } else {
            self.build_menu_rects()
        };
        self.rects_below.prepare(&self.queue, &below, resolution);
        self.rects_above.prepare(&self.queue, &above, resolution);
        // `tab_bar` gets more entries in phase 2 (block-label highlights),
        // so its `prepare` is deferred to after that pass — uploading
        // here would freeze it before the highlights land.
        self.rects_modal
            .prepare(&self.queue, &overlay_rects, resolution);

        // Modal text preparation — independent renderer so its draw can come
        // after the modal background rects.
        if let Some(modal) = self.modal.as_ref() {
            let surface_w = self.surface_config.width as f32;
            let surface_h = self.surface_config.height as f32;
            let card_x = (surface_w - MODAL_CARD_W) * 0.5;
            let card_y = (surface_h - MODAL_CARD_H) * 0.5;
            let title_color = Color::rgb(235, 235, 245);
            let body_color = Color::rgb(180, 180, 195);
            let cancel_color = Color::rgb(200, 200, 215);
            let confirm_color = Color::rgb(245, 240, 240);
            let inset = 28.0;
            let title_top = card_y + inset;
            let body_top = title_top + MODAL_LINE_H + 8.0;
            let card_bounds = TextBounds {
                left: card_x as i32,
                top: card_y as i32,
                right: (card_x + MODAL_CARD_W) as i32,
                bottom: (card_y + MODAL_CARD_H) as i32,
            };
            let cr = modal.cancel_rect;
            let fr = modal.confirm_rect;
            let areas = [
                TextArea {
                    buffer: &modal.title_buf,
                    left: card_x + inset,
                    top: title_top,
                    scale: 1.0,
                    bounds: card_bounds,
                    default_color: title_color,
                    custom_glyphs: &[],
                },
                TextArea {
                    buffer: &modal.body_buf,
                    left: card_x + inset,
                    top: body_top,
                    scale: 1.0,
                    bounds: card_bounds,
                    default_color: body_color,
                    custom_glyphs: &[],
                },
                TextArea {
                    buffer: &modal.cancel_buf,
                    left: cr.0 + (cr.2 - MODAL_BTN_W * 0.55) * 0.5,
                    top: cr.1 + (cr.3 - MODAL_LINE_H) * 0.5,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: cr.0 as i32,
                        top: cr.1 as i32,
                        right: (cr.0 + cr.2) as i32,
                        bottom: (cr.1 + cr.3) as i32,
                    },
                    default_color: cancel_color,
                    custom_glyphs: &[],
                },
                TextArea {
                    buffer: &modal.confirm_buf,
                    left: fr.0 + (fr.2 - MODAL_BTN_W * 0.55) * 0.5,
                    top: fr.1 + (fr.3 - MODAL_LINE_H) * 0.5,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: fr.0 as i32,
                        top: fr.1 as i32,
                        right: (fr.0 + fr.2) as i32,
                        bottom: (fr.1 + fr.3) as i32,
                    },
                    default_color: confirm_color,
                    custom_glyphs: &[],
                },
            ];
            self.modal_text_renderer
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    areas,
                    &mut self.swash_cache,
                )
                .expect("terminite: modal text prepare failed");
        } else if let Some(menu) = self.context_menu.as_ref() {
            // Context-menu item labels go through the same text renderer.
            let label_color = Color::rgb(225, 225, 235);
            let disabled_color = Color::rgb(110, 110, 125);
            let text_inset = 18.0;
            let areas: Vec<TextArea> = menu
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let row_y = menu.y + i as f32 * MENU_ITEM_H;
                    TextArea {
                        buffer: &item.label_buf,
                        left: menu.x + text_inset,
                        top: row_y + (MENU_ITEM_H - MODAL_LINE_H) * 0.5,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: menu.x as i32,
                            top: row_y as i32,
                            right: (menu.x + MENU_WIDTH) as i32,
                            bottom: (row_y + MENU_ITEM_H) as i32,
                        },
                        default_color: if item.enabled {
                            label_color
                        } else {
                            disabled_color
                        },
                        custom_glyphs: &[],
                    }
                })
                .collect();
            self.modal_text_renderer
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    areas,
                    &mut self.swash_cache,
                )
                .expect("terminite: menu text prepare failed");
        }

        // Per-pane image placements: collected during phase 2 (root is
        // borrowed for the text areas anyway), prepared after the text
        // prep, drawn in the render pass between content and the tab bar.
        let mut texture_instances: Vec<TextureInstance> = Vec::new();
        let mut texture_bgs: Vec<wgpu::BindGroup> = Vec::new();

        // Content text + per-pane tab-bar labels. Phase 2: every pane's
        // buffers are already refreshed, so we can take the immutable
        // borrows the TextAreas need. Content goes through `text_renderer`,
        // tab labels + find bar through `tab_text_renderer`.
        {
            let root = self.root.as_ref().expect("pane tree present");
            let pad = self.pad;
            let line_height = self.line_height;
            let active_color = Color::rgb(230, 230, 240);
            let inactive_color = Color::rgb(140, 140, 160);
            let close_color = Color::rgb(160, 160, 170);
            // Subdued; a block label is chrome, not content.
            let block_label_color = Color::rgb(110, 110, 130);
            let mut content_areas: Vec<TextArea> = Vec::with_capacity(draws.len());
            let mut tab_areas: Vec<TextArea> = Vec::new();
            for d in &draws {
                let pane = root.find(d.pid).expect("drawn pane present");
                let pane_rect = layout
                    .iter()
                    .find(|(id, _)| *id == d.pid)
                    .map(|(_, r)| *r)
                    .expect("drawn pane present in layout");
                let tab_ref = pane.active_tab_ref();
                // Non-shell kinds render from `content_buffer`. If for
                // some reason it's missing (race between kind switch
                // and render), fall back to the empty text_buffer so
                // we don't crash.
                let body_buffer = match tab_ref.kind {
                    TabContentKind::Shell => &tab_ref.text_buffer,
                    _ => tab_ref
                        .content_buffer
                        .as_ref()
                        .unwrap_or(&tab_ref.text_buffer),
                };
                // Data modules scroll their body via `module_scroll_y`.
                // Bounds clip overflow so scrolled-out content doesn't
                // leak past the pane.
                let is_data_module = matches!(tab_ref.kind, TabContentKind::Module(_))
                    && tab_ref.module_pty.is_none();
                let scroll_y = if is_data_module { tab_ref.module_scroll_y } else { 0.0 };
                // When a data-module pane is showing an image (still or
                // animated), suppress the text body — otherwise the
                // placeholder body bleeds through behind the image.
                // Shells with kitty images keep both (text + overlaid
                // image) as before.
                let suppress_text = is_data_module
                    && (tab_ref.image.is_some() || tab_ref.animation.is_some());
                if !suppress_text {
                    // When the module supplied a gutter, content
                    // shifts right by the gutter width. We compute
                    // the gutter width here the same way
                    // render_non_shell_pane did (widest label),
                    // which is cheap and avoids threading it back
                    // through PaneDraw.
                    let metrics = self.pane_metrics(d.pid);
                    let pane_content_w = (d.bounds.right - d.text_left as i32).max(0) as f32;
                    let gutter_w = match tab_ref.module_gutter.as_ref() {
                        Some(lbls) => {
                            let max_chars = lbls
                                .iter()
                                .map(|s| s.chars().count())
                                .max()
                                .unwrap_or(0) as f32;
                            if max_chars > 0.0 {
                                ((max_chars + 1.0) * metrics.cell_advance).min(pane_content_w * 0.5)
                            } else {
                                0.0
                            }
                        }
                        None => 0.0,
                    };
                    let body_left = d.text_left + gutter_w;
                    let body_bounds = TextBounds {
                        left: (body_left as i32).max(d.bounds.left),
                        ..d.bounds
                    };
                    content_areas.push(TextArea {
                        buffer: body_buffer,
                        left: body_left,
                        top: d.text_top - scroll_y,
                        scale: 1.0,
                        bounds: body_bounds,
                        default_color: Color::rgb(DEFAULT_FG.0, DEFAULT_FG.1, DEFAULT_FG.2),
                        custom_glyphs: &[],
                    });
                    // Gutter labels — one TextArea per first-run of
                    // each source line that has a label. We walk
                    // body's layout_runs (so wrap continuations
                    // get no label) and tell glyphon to render
                    // gutter_buffer with `top` shifted so row N of
                    // the gutter buffer ends up at the body's
                    // first-run y for line N, clipped to one row.
                    if let (Some(gbuf), Some(labels)) = (
                        tab_ref.gutter_buffer.as_ref(),
                        tab_ref.module_gutter.as_ref(),
                    ) {
                        let line_h = metrics.line_height;
                        let mut acc = 0.0_f32;
                        let mut prev_line: Option<u32> = None;
                        for run in body_buffer.layout_runs() {
                            let line_i = run.line_i as u32;
                            let is_first = prev_line != Some(line_i);
                            prev_line = Some(line_i);
                            if is_first
                                && (line_i as usize) < labels.len()
                                && !labels[line_i as usize].is_empty()
                            {
                                let row_y = d.text_top + acc - scroll_y;
                                // Shift gutter buffer so its row
                                // line_i aligns with row_y.
                                let g_top = row_y - (line_i as f32) * line_h;
                                let row_bounds = TextBounds {
                                    left: d.text_left as i32,
                                    top: (row_y as i32).max(d.bounds.top),
                                    right: ((d.text_left + gutter_w) as i32)
                                        .min(d.bounds.right),
                                    bottom: ((row_y + line_h) as i32)
                                        .min(d.bounds.bottom),
                                };
                                if row_bounds.right > row_bounds.left
                                    && row_bounds.bottom > row_bounds.top
                                {
                                    content_areas.push(TextArea {
                                        buffer: gbuf,
                                        left: d.text_left,
                                        top: g_top,
                                        scale: 1.0,
                                        bounds: row_bounds,
                                        default_color: Color::rgb(110, 110, 130),
                                        custom_glyphs: &[],
                                    });
                                }
                            }
                            acc += line_h;
                        }
                    }
                }
                // Kind selector label — leftmost in the bar. Looked up
                // by the kind's stable key. If a module was unregistered
                // since the tab last switched to it, the buffer is gone
                // and we just skip rendering the label (the dropdown
                // still works to pick a new kind).
                let active_kind = &pane.active_tab_ref().kind;
                if let Some(label_buf) = self.kind_label_buffers.get(active_kind.key()) {
                    let bar_top = pane_rect.y;
                    let text_top =
                        bar_top + (self.tab_bar_height - self.tab_line_h) / 2.0;
                    let ksw_label = kind_selector_w(self.config.tab_font_size);
                    tab_areas.push(TextArea {
                        buffer: label_buf,
                        left: pane_rect.x + TAB_LABEL_INSET,
                        top: text_top,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: pane_rect.x as i32,
                            top: bar_top as i32,
                            right: (pane_rect.x + ksw_label) as i32,
                            bottom: (bar_top + self.tab_bar_height) as i32,
                        },
                        default_color: active_color,
                        custom_glyphs: &[],
                    });
                }
                for slot in &d.tabs {
                    let tab = &pane.tabs[slot.index];
                    tab_areas.push(TextArea {
                        buffer: &tab.title_buffer,
                        left: slot.label_left,
                        top: slot.text_top,
                        scale: 1.0,
                        bounds: slot.label_bounds,
                        default_color: if slot.is_active {
                            active_color
                        } else {
                            inactive_color
                        },
                        custom_glyphs: &[],
                    });
                    tab_areas.push(TextArea {
                        buffer: &self.close_buffer,
                        left: slot.close_left,
                        top: slot.text_top,
                        scale: 1.0,
                        bounds: slot.close_bounds,
                        default_color: close_color,
                        custom_glyphs: &[],
                    });
                }
                // Pane's image. Scaled to fit the content area (never
                // upscaled). Data-module panes (Preview, etc.) center
                // the image — that's the natural "viewer" framing.
                // Shell panes keep the top-left placement that kitty
                // graphics emitters expect for inline display. Clone
                // is cheap — wgpu BindGroup is ref-counted internally.
                //
                // For animated images we pick the current frame here
                // and scale against the animation's envelope (max
                // width/height across frames) so the layout doesn't
                // wobble between frames of different sizes.
                let img_info: Option<(&TextureImage, u32, u32)> =
                    if let Some(anim) = tab_ref.animation.as_ref() {
                        Some((anim.current_frame(), anim.width, anim.height))
                    } else if let Some(img) = tab_ref.image.as_ref() {
                        Some((img, img.width, img.height))
                    } else {
                        None
                    };
                if let Some((tex, nw_u, nh_u)) = img_info {
                    let ox = pane_rect.x + pad.left;
                    let oy = pane_rect.y + self.tab_bar_height + pad.top;
                    let max_w = (pane_rect.x + pane_rect.w - ox - pad.right).max(1.0);
                    let max_h =
                        (pane_rect.y + pane_rect.h - oy - pad.bottom).max(1.0);
                    let nw = nw_u as f32;
                    let nh = nh_u as f32;
                    let scale = (max_w / nw).min(max_h / nh).min(1.0);
                    let sw = nw * scale;
                    let sh = nh * scale;
                    let (x, y) = if is_data_module {
                        (
                            ox + (max_w - sw) * 0.5,
                            oy + (max_h - sh) * 0.5,
                        )
                    } else {
                        (ox, oy)
                    };
                    texture_instances.push(TextureInstance {
                        rect: [x, y, sw, sh],
                    });
                    texture_bgs.push(tex.bind_group().clone());
                }
                // Block IDs (`Bn`) in the pane's left-gutter strip.
                // Coords are session-absolute (`abs = history + cursor.line`
                // at fire time); to find the current screen vl, unwind
                // both the rows that have since scrolled into history and
                // the user's current scroll position.
                // Per-pane scale affects the row stride used for block-
                // label vertical placement — labels track content rows.
                let pane_line_height = self.pane_metrics(d.pid).line_height;
                let y_shift = tab_ref.pixel_offset;
                let (display_offset, history) =
                    tab_ref.live_term.offset_and_history();
                let display_offset = display_offset as i32;
                let history = history as i32;
                let rows = tab_ref.rows as i32;
                let py = pane_rect.y + self.tab_bar_height + pad.top;
                let gutter_left = self.gutter_left;
                // Right-align each label against a fixed anchor just
                // inside the content edge. The label grows leftward as
                // the digit count climbs (B7 → B12 → B323 all end at the
                // same x), and `gutter_left` becomes the minimum-left
                // clip — when a label overruns it (very long ids in a
                // narrow gutter), the leading "B" gets clipped rather
                // than overlapping the line. `gutter_gap` is the space
                // between the label's right edge and the line content.
                let label_right = pane_rect.x + pad.left - self.gutter_gap;
                let label_left_min = pane_rect.x + gutter_left;
                // v_pad + label_line_h are now per-block (labels scale
                // with the pane that owned them at creation time).
                // Reads off the block in the loop below.
                // Visual signal lives in a background highlight behind
                // the label (like an HTML `<mark>`), not in the text
                // color. Text color alone reads as "another shade of
                // gray" — a filled block of color pops unambiguously.
                //   - cursored: bright warm fill, dark text for contrast.
                //   - tagged:   dim cool fill, label color unchanged.
                //   - default:  no fill, subdued label color.
                let cursor_bg: [f32; 4] = [1.0, 0.83, 0.30, 0.95];
                let tagged_bg: [f32; 4] = [0.45, 0.50, 0.65, 0.45];
                let cursor_text = Color::rgb(20, 20, 30);
                let tagged_text = Color::rgb(40, 40, 60);
                let highlight_pad_x = self.highlight_pad_x;
                let highlight_pad_y = self.highlight_pad_y;
                let highlight_offset_y = self.highlight_offset_y;
                let cursor_block_id = tab_ref.blocks.cursor();
                for block in tab_ref.blocks.iter() {
                    let Some(abs) = block.anchor_line() else { continue };
                    let vl = abs - history + display_offset;
                    if vl < 0 || vl >= rows {
                        continue;
                    }
                    let row_top = py + vl as f32 * pane_line_height + y_shift;
                    let label_line_h = block.label_line_h;
                    let v_pad =
                        ((pane_line_height - label_line_h) * 0.5).max(0.0);
                    let top = row_top + v_pad;
                    let left = label_right - block.label_width;
                    let is_cursor = Some(block.id) == cursor_block_id;
                    let bg = if is_cursor {
                        Some(cursor_bg)
                    } else if !block.tags.is_empty() {
                        Some(tagged_bg)
                    } else {
                        None
                    };
                    if let Some(color) = bg {
                        // Highlight clamped to the gutter strip so it
                        // never bleeds into line content. tab_bar rect
                        // layer renders before tab_text_renderer, so the
                        // fill sits behind the label text. The pads +
                        // offset come from config so the box can be
                        // dialed in live without a recompile.
                        let hx = (left - highlight_pad_x).max(pane_rect.x);
                        let hr = (label_right + highlight_pad_x)
                            .min(pane_rect.x + pad.left);
                        let hw = (hr - hx).max(0.0);
                        let hy = top - highlight_pad_y + highlight_offset_y;
                        let hh = label_line_h + highlight_pad_y * 2.0;
                        tab_bar.push(RectInstance {
                            rect: [hx, hy, hw, hh],
                            color,
                        });
                    }
                    let text_color = if is_cursor {
                        cursor_text
                    } else if !block.tags.is_empty() {
                        tagged_text
                    } else {
                        block_label_color
                    };
                    tab_areas.push(TextArea {
                        buffer: &block.label_buffer,
                        left,
                        top,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: label_left_min as i32,
                            top: row_top as i32,
                            right: label_right as i32,
                            bottom: (row_top + line_height) as i32,
                        },
                        default_color: text_color,
                        custom_glyphs: &[],
                    });
                }
            }
            // The find bar's text rides in the tab text renderer.
            if let (Some(find), Some((bx, by))) = (self.find.as_ref(), find_bar_origin) {
                tab_areas.push(TextArea {
                    buffer: &find.bar_buf,
                    left: bx + 16.0,
                    top: by + (FIND_BAR_H - MODAL_LINE_H) * 0.5,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: bx as i32,
                        top: by as i32,
                        right: (bx + FIND_BAR_W) as i32,
                        bottom: (by + FIND_BAR_H) as i32,
                    },
                    default_color: Color::rgb(225, 225, 235),
                    custom_glyphs: &[],
                });
            }
            self.text_renderer
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    content_areas,
                    &mut self.swash_cache,
                )
                .expect("terminite: text prepare failed");
            self.tab_text_renderer
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    tab_areas,
                    &mut self.swash_cache,
                )
                .expect("terminite: tab bar text prepare failed");
        }

        // Upload the tab-bar rects now that phase 2 has pushed any
        // block-label highlights into the same Vec — render order still
        // puts these behind `tab_text_renderer`, so the rect sits behind
        // the label glyphs.
        self.rects_tab_bar
            .prepare(&self.queue, &tab_bar, resolution);

        // Stage the image instance buffer; render happens between content
        // (text + decorations) and the tab bar, so images sit above the
        // cell grid but below per-pane chrome.
        self.texture_renderer
            .prepare(&self.queue, &texture_instances, resolution);

        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self
                    .instance
                    .create_surface(self.window.clone())
                    .expect("terminite: failed to recreate the surface");
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return;
            }
            other => {
                eprintln!("terminite: surface status: {other:?}");
                return;
            }
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("terminite frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(BACKGROUND),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // One full-window scissor — the panes tile the whole surface and
            // every rect / TextArea is already clipped to its own pane box,
            // so no per-pane scissor switching is needed.
            pass.set_scissor_rect(
                0,
                0,
                self.surface_config.width,
                self.surface_config.height,
            );

            self.rects_below.render(&mut pass);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("terminite: text render failed");
            self.rects_above.render(&mut pass);

            // Decoded images, atop the cell grid but below the tab bar.
            self.texture_renderer.render(&mut pass, &texture_bgs);

            // Per-pane tab bars drawn on top of the content.
            self.rects_tab_bar.render(&mut pass);
            self.tab_text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("terminite: tab bar text render failed");

            // Modal and context menu sit on top of *everything* — they
            // share the rects_modal / modal_text_renderer pipelines.
            if self.modal.is_some() || self.context_menu.is_some() {
                self.rects_modal.render(&mut pass);
                self.modal_text_renderer
                    .render(&self.atlas, &self.viewport, &mut pass)
                    .expect("terminite: overlay text render failed");
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        surface_texture.present();
        self.atlas.trim();

        // Frame-time bookkeeping for the stats verb. Sample is the
        // wall-clock interval from the start of this frame through
        // present; close enough to "what the user perceives as
        // frame cost" for debug purposes.
        let dt = frame_start.elapsed().as_secs_f32() * 1000.0;
        if self.frame_samples.len() == FRAME_TIMER_CAP {
            self.frame_samples.pop_front();
        }
        self.frame_samples.push_back(dt);
        self.last_frame_end = Some(Instant::now());
        self.frame_count = self.frame_count.saturating_add(1);
    }

}

// ── moved from mod.rs ───────────────────────────────

impl Renderer {
    /// Emit one pane's tab-bar rects into `out`, and return a label slot per
    /// tab for the text pass. `rect` is the pane's full rect; the bar fills
    /// its top `self.tab_bar_height`. `is_active_pane` gates the gold underline so
    /// exactly one tab bar in the window marks where keystrokes go.
    pub(super) fn build_pane_tab_bar(
        &self,
        pid: PaneId,
        rect: PaneRect,
        is_active_pane: bool,
        out: &mut Vec<RectInstance>,
    ) -> Vec<TabLabelSlot> {
        let pane = self.root_ref().find(pid).expect("pane present");
        let title_widths: Vec<f32> = pane
            .tabs
            .iter()
            .map(|t| measure_title_width(&t.title_buffer))
            .collect();
        let ksw = kind_selector_w(self.config.tab_font_size);
        let layout = pane_tab_layout(
            rect,
            &title_widths,
            pane.active_tab,
            self.tab_min_width,
            self.tab_max_width,
            ksw,
        );
        let bar_top = rect.y;
        // Bar background across the pane's width.
        out.push(RectInstance {
            rect: [rect.x, bar_top, rect.w, self.tab_bar_height],
            color: TAB_INACTIVE_BG,
        });
        // Kind selector — the leftmost element in the bar (Blender area
        // header model). Same bg as inactive tabs, with a separator on
        // its right edge. Click → opens a popover with available
        // kinds. The label text is emitted in render's phase 2.
        out.push(RectInstance {
            rect: [
                rect.x + ksw - 1.0,
                bar_top + 6.0,
                1.0,
                self.tab_bar_height - 12.0,
            ],
            color: TAB_SEPARATOR,
        });
        let text_top = bar_top + (self.tab_bar_height - self.tab_line_h) / 2.0;
        let mut slots = Vec::with_capacity(layout.len());
        for (i, (x, w, is_active)) in layout.iter().enumerate() {
            let (x, w, is_active) = (*x, *w, *is_active);
            out.push(RectInstance {
                rect: [x, bar_top, w, self.tab_bar_height],
                color: if is_active { TAB_ACTIVE_BG } else { TAB_INACTIVE_BG },
            });
            out.push(RectInstance {
                rect: [x + w - 1.0, bar_top + 6.0, 1.0, self.tab_bar_height - 12.0],
                color: TAB_SEPARATOR,
            });
            if is_active {
                // Gold underline only in the focused pane; a dim seam marks
                // the active tab of an unfocused pane.
                out.push(RectInstance {
                    rect: [x + 6.0, bar_top + self.tab_bar_height - 3.0, w - 12.0, 3.0],
                    color: if is_active_pane {
                        TAB_ACTIVE_UNDERLINE
                    } else {
                        TAB_SEPARATOR
                    },
                });
            }
            // Per-tab color band — a thin strip at the top of the tab
            // slot, so it sits above the active-tab underline at the
            // bottom and doesn't fight it. Drawn only when the tab
            // has a non-`none` color picked.
            let tab = &pane.tabs[i];
            if tab.color_idx != 0 {
                out.push(RectInstance {
                    rect: [x + 6.0, bar_top + 2.0, w - 12.0, 3.0],
                    color: palette_color(tab.color_idx),
                });
            }
            let label_left = x + TAB_LABEL_INSET;
            let label_right = (x + w - TAB_CLOSE_WIDTH).max(label_left);
            let close_left = x + w - TAB_CLOSE_WIDTH + 8.0;
            slots.push(TabLabelSlot {
                index: i,
                is_active,
                label_left,
                label_bounds: TextBounds {
                    left: label_left as i32,
                    top: bar_top as i32,
                    right: label_right as i32,
                    bottom: (bar_top + self.tab_bar_height) as i32,
                },
                close_left,
                close_bounds: TextBounds {
                    left: close_left as i32,
                    top: bar_top as i32,
                    right: (x + w) as i32,
                    bottom: (bar_top + self.tab_bar_height) as i32,
                },
                text_top,
            });
        }
        // Bottom border between the bar and the content.
        out.push(RectInstance {
            rect: [rect.x, bar_top + self.tab_bar_height, rect.w, 1.0],
            color: TAB_SEPARATOR,
        });
        // Corner split handle — a "peel" triangle; drag it to split (or,
        // dragged back out, to remove) this pane.
        let grip_active = self.split_gesture.as_ref().map(|g| g.pid) == Some(pid);
        push_split_grip(
            out,
            rect,
            if grip_active {
                TAB_ACTIVE_UNDERLINE
            } else {
                SPLIT_HANDLE_COLOR
            },
        );
        slots
    }

}

// ── helpers moved from mod.rs ──────────────────────

/// Body text for each non-shell content kind. Modules render a
/// placeholder until step 2b lands process spawning + IPC.
pub(super) fn non_shell_body(
    kind: &TabContentKind,
    registry: &crate::modules::Registry,
) -> String {
    match kind {
        TabContentKind::Shell => String::new(),
        TabContentKind::Welcome => "\
welcome to terminite — a terminal for the human + AI pair.

each pane runs a shell (Shell) or some other kind of inhabitant.
the leftmost dropdown in this pane's tab bar switches between them.
this pane is showing the Welcome inhabitant — read-only, static.
pick Shell from the dropdown to drop into a real shell.

two halves of the pair share one surface here. blocks (B1, B2, …)
in the left gutter are command + output units the pair can name.
the AI partner connects to ~/.terminite/socket and gets the same
coordinates you do. see guide/getting-started.md for more."
            .to_string(),
        TabContentKind::Module(id) => match registry.find(id) {
            Some(m) => format!(
                "module: {}  (v{})\nbinary: {}\nwaiting for the module to send its first frame…",
                m.name,
                m.version,
                m.binary.display(),
            ),
            None => format!(
                "module '{id}' is no longer registered.\npick a different kind from the dropdown."
            ),
        },
    }
}

// ── Proto helpers ────────────────────────────────────────────────────────


/// The cosmic-text font family for a config `font_family` string — empty
/// means terminite's built-in monospace default.
pub(super) fn font_family(name: &str) -> Family<'_> {
    if name.is_empty() {
        Family::Monospace
    } else {
        Family::Name(name)
    }
}

/// Build a content `Buffer` for a pane — monospace, one-cell glyph advance,
/// sized to the pane's pixel rect.
#[allow(clippy::too_many_arguments)]
pub(super) fn make_content_buffer(
    font_system: &mut FontSystem,
    cell_advance: f32,
    line_height: f32,
    font_size: f32,
    family: &str,
    w: f32,
    h: f32,
) -> Buffer {
    let mut buf = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buf.set_size(font_system, Some(w.max(1.0)), Some(h.max(1.0)));
    buf.set_monospace_width(font_system, Some(cell_advance));
    buf.set_text(
        font_system,
        "",
        &Attrs::new().family(font_family(family)),
        Shaping::Advanced,
        None,
    );
    buf.shape_until_scroll(font_system, false);
    buf
}

/// Build a `Buffer` for modal-card text at a larger font size.
pub(super) fn make_modal_buffer(font_system: &mut FontSystem, text: &str) -> Buffer {
    let metrics = Metrics::new(MODAL_FONT_SIZE, MODAL_LINE_H);
    let mut buf = Buffer::new(font_system, metrics);
    buf.set_size(font_system, Some(MODAL_CARD_W), Some(MODAL_LINE_H * 3.0));
    let attrs = Attrs::new().family(Family::Monospace);
    buf.set_text(font_system, text, &attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(font_system, false);
    buf
}


pub(super) fn make_title_buffer(
    font_system: &mut FontSystem,
    title: &str,
    font_size: f32,
    line_h: f32,
    max_w: f32,
) -> Buffer {
    let metrics = Metrics::new(font_size, line_h);
    let mut buf = Buffer::new(font_system, metrics);
    // The buffer is sized to twice the max tab width so long titles
    // don't get pre-wrapped — the tab's `TextBounds` clips at display.
    buf.set_size(font_system, Some(max_w * 2.0), Some(line_h));
    let attrs = Attrs::new().family(Family::Monospace);
    buf.set_text(font_system, title, &attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(font_system, false);
    buf
}


pub(super) fn compute_grid_size(
    physical_width: f32,
    physical_height: f32,
    cell_advance: f32,
    line_height: f32,
    pad: Padding,
    tab_bar_height: f32,
) -> (usize, usize) {
    // Full window as a single pane: one tab-bar strip plus per-edge pads.
    let available_w = (physical_width - pad.left - pad.right).max(cell_advance);
    let available_h =
        (physical_height - tab_bar_height - pad.top - pad.bottom).max(line_height);
    let cols = ((available_w / cell_advance) as usize).clamp(2, MAX_GRID_COLS);
    let rows = ((available_h / line_height) as usize).clamp(2, MAX_GRID_ROWS);
    (cols, rows)
}

/// Measure the one-cell advance width of the configured font at the
/// configured size, by shaping an `M` and reading its glyph advance.
pub(super) fn measure_cell_advance(font_system: &mut FontSystem, font_size: f32, family: &str) -> f32 {
    let line_height = font_size * LINE_H_RATIO;
    let mut probe = Buffer::new(font_system, Metrics::new(font_size, line_height));
    probe.set_size(font_system, Some(1000.0), Some(line_height * 2.0));
    probe.set_text(
        font_system,
        "M",
        &Attrs::new().family(font_family(family)),
        Shaping::Advanced,
        None,
    );
    probe.shape_until_scroll(font_system, false);
    probe
        .layout_runs()
        .next()
        .and_then(|run| run.glyphs.first())
        .map(|glyph| glyph.w)
        .unwrap_or(font_size * 0.6)
        // Floor it: a degenerate measurement must never explode the grid.
        .max(2.0)
}

// ── Memory kill-switch ────────────────────────────────────────────────────


