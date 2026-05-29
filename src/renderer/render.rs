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
                        above.push(RectInstance {
                            rect: [r.x, r.y, r.w, r.h],
                            color: REMOVE_PREVIEW_COLOR,
                        });
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
                // Data modules + Agent panes scroll their body via
                // `module_scroll_y`. Bounds clip overflow so
                // scrolled-out content doesn't leak past the pane.
                let is_data_module = (matches!(tab_ref.kind, TabContentKind::Module(_))
                    && tab_ref.module_pty.is_none())
                    || matches!(tab_ref.kind, TabContentKind::Agent(_));
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

    /// Render one pane into its rect: draw its own tab bar, then tick
    /// autoscroll, snapshot the active tab, refresh its text buffer, and emit
    /// clipped background / selection / cursor / decoration rects. Returns
    /// where to place the content + tab-label text (phase 2 in `render`).
    #[allow(clippy::too_many_arguments)]
    /// Render path for a pane whose active tab is *not* a shell. Builds
    /// (or rebuilds) the tab's `content_buffer` to fit the current pane
    /// rect, then returns a `PaneDraw` pointing into it. Welcome lives
    /// here in Bundle 6 step 1; future built-in kinds slot in alongside.
    pub(super) fn render_non_shell_pane(
        &mut self,
        pid: PaneId,
        rect: PaneRect,
        tab_slots: Vec<TabLabelSlot>,
        kind: TabContentKind,
        blink_on: bool,
        below: &mut Vec<RectInstance>,
    ) -> PaneDraw {
        let metrics = self.pane_metrics(pid);
        let pad = self.pad;
        let px = rect.x + pad.left;
        let py = rect.y + self.tab_bar_height + pad.top;
        let content_w = (rect.w - pad.left - pad.right).max(1.0);
        let content_h = (rect.h - self.tab_bar_height - pad.top - pad.bottom)
            .max(metrics.line_height);

        // Build / refresh content_buffer if needed. For modules,
        // prefer the live session's body (what the module asked us
        // to render); for Agent panes, render the ACP turn list;
        // fall back to the kind's static placeholder.
        let body = {
            let tab_opt = self
                .root
                .as_ref()
                .and_then(|n| n.find(pid))
                .map(|p| p.active_tab_ref());
            let acp_body = tab_opt
                .and_then(|t| t.acp_session.as_ref())
                .map(render_acp_body);
            let session_body = tab_opt
                .and_then(|t| t.module_session.as_ref())
                .map(|s| s.body.clone())
                .filter(|s| !s.is_empty());
            acp_body
                .or(session_body)
                .unwrap_or_else(|| non_shell_body(&kind, &self.modules))
        };
        // Gutter width — content shifts right by this when the
        // active module supplied gutter labels (Editor's line
        // numbers). Computed from the widest non-empty label;
        // capped at content_w/2 to keep an over-eager gutter from
        // squeezing content off-screen.
        let gutter_w = {
            let labels = self
                .root
                .as_ref()
                .and_then(|n| n.find(pid))
                .and_then(|p| p.active_tab_ref().module_gutter.as_ref())
                .cloned();
            match labels {
                Some(lbls) => {
                    let max_chars = lbls
                        .iter()
                        .map(|s| s.chars().count())
                        .max()
                        .unwrap_or(0) as f32;
                    if max_chars > 0.0 {
                        ((max_chars + 1.0) * metrics.cell_advance).min(content_w * 0.5)
                    } else {
                        0.0
                    }
                }
                None => 0.0,
            }
        };
        let body_w = (content_w - gutter_w).max(1.0);
        let font_size = metrics.font_size;
        let line_height = metrics.line_height;
        let family = self.font_family.clone();
        let tab = self
            .root
            .as_mut()
            .and_then(|n| n.find_mut(pid))
            .map(|p| p.active_tab_mut());
        if let Some(tab) = tab {
            let needs_build = tab.content_buffer.is_none();
            if needs_build {
                let mut buf = Buffer::new(
                    &mut self.font_system,
                    Metrics::new(font_size, line_height),
                );
                // Width-only constraint — glyphon wraps to body
                // width but lays out every line. Height = None
                // means long bodies don't get truncated; the render
                // path applies `module_scroll_y` + clipping bounds
                // to show only what fits.
                buf.set_size(&mut self.font_system, Some(body_w), None);
                let attrs = Attrs::new().family(font_family(&family));
                // When syntect produced per-source-line spans, build
                // the rich-text input by walking the body's lines
                // in parallel with the spans and emitting (text,
                // attrs.color) chunks. Anything outside a span (e.g.
                // header lines, gaps) falls back to default color.
                if let Some(highlights) = tab.module_highlights.as_ref() {
                    let mut runs: Vec<(String, Color)> = Vec::new();
                    let default_color = Color::rgb(DEFAULT_FG.0, DEFAULT_FG.1, DEFAULT_FG.2);
                    let mut first_line = true;
                    for (line_idx, line) in body.split('\n').enumerate() {
                        if !first_line {
                            runs.push(("\n".to_string(), default_color));
                        }
                        first_line = false;
                        let spans = highlights.get(line_idx);
                        match spans {
                            Some(spans) if !spans.is_empty() => {
                                let mut cursor = 0usize;
                                for span in spans {
                                    if span.start > cursor && span.start <= line.len() {
                                        runs.push((
                                            line[cursor..span.start].to_string(),
                                            default_color,
                                        ));
                                    }
                                    let s = span.start.min(line.len());
                                    let e = span.end.min(line.len());
                                    if e > s {
                                        runs.push((
                                            line[s..e].to_string(),
                                            Color::rgb(span.rgb[0], span.rgb[1], span.rgb[2]),
                                        ));
                                    }
                                    cursor = e.max(cursor);
                                }
                                if cursor < line.len() {
                                    runs.push((line[cursor..].to_string(), default_color));
                                }
                            }
                            _ => {
                                runs.push((line.to_string(), default_color));
                            }
                        }
                    }
                    buf.set_rich_text(
                        &mut self.font_system,
                        runs.iter().map(|(t, c)| (t.as_str(), attrs.clone().color(*c))),
                        &attrs,
                        Shaping::Advanced,
                        None,
                    );
                } else {
                    buf.set_text(
                        &mut self.font_system,
                        &body,
                        &attrs,
                        Shaping::Advanced,
                        None,
                    );
                }
                buf.shape_until_scroll(&mut self.font_system, false);
                tab.content_buffer = Some(buf);
            } else if let Some(buf) = tab.content_buffer.as_mut() {
                // Keep the buffer width matched to the (post-gutter)
                // content area. Height stays unbounded so all lines
                // are laid out for scrolling.
                buf.set_size(&mut self.font_system, Some(body_w), None);
            }
            // Build / refresh gutter buffer when present. We shape
            // it as a tall single-column buffer; the render path
            // pulls per-line slices by adjusting TextArea.top.
            if tab.module_gutter.is_some() && tab.gutter_buffer.is_none() {
                let joined = tab
                    .module_gutter
                    .as_ref()
                    .map(|g| g.join("\n"))
                    .unwrap_or_default();
                let mut gbuf = Buffer::new(
                    &mut self.font_system,
                    Metrics::new(font_size, line_height),
                );
                gbuf.set_size(&mut self.font_system, Some(gutter_w), None);
                let attrs = Attrs::new().family(font_family(&family));
                gbuf.set_text(
                    &mut self.font_system,
                    &joined,
                    &attrs,
                    Shaping::Advanced,
                    None,
                );
                gbuf.shape_until_scroll(&mut self.font_system, false);
                tab.gutter_buffer = Some(gbuf);
            } else if let Some(gbuf) = tab.gutter_buffer.as_mut() {
                gbuf.set_size(&mut self.font_system, Some(gutter_w), None);
            }
            if tab.module_gutter.is_none() {
                tab.gutter_buffer = None;
            }
            // Clamp module_scroll_y so a pane shrink or a shorter
            // body can't leave us scrolled past the end. The render
            // pass subtracts this from text_top to scroll content.
            if let Some(buf) = tab.content_buffer.as_ref() {
                let total_h = buf.layout_runs().count() as f32 * line_height;
                let max_scroll = (total_h - content_h).max(0.0);
                // Honor a pending ensure_visible from set_text:
                // adjust scroll just enough to bring the target line
                // into the visible window. "Just enough" means we
                // leave the user's wheel position alone when the
                // target is already on screen.
                if let Some(line) = tab.pending_ensure_visible.take() {
                    let target_top = line as f32 * line_height;
                    let target_bottom = target_top + line_height;
                    if target_top < tab.module_scroll_y {
                        tab.module_scroll_y = target_top;
                    } else if target_bottom > tab.module_scroll_y + content_h {
                        tab.module_scroll_y = target_bottom - content_h;
                    }
                }
                if tab.module_scroll_y > max_scroll {
                    tab.module_scroll_y = max_scroll;
                }
                if tab.module_scroll_y < 0.0 {
                    tab.module_scroll_y = 0.0;
                }
            }
        }

        // Optional row highlight (Nav's current entry, Editor's
        // cursor row). Painted below text/cursor so glyphs read
        // cleanly on top.
        if let Some(tab) = self
            .root
            .as_ref()
            .and_then(|n| n.find(pid))
            .map(|p| p.active_tab_ref())
        {
            if let Some(hline) = tab.module_highlight_line {
                // Walk all layout runs that belong to this source
                // line — wraps continue the highlight cleanly across
                // every visual segment. Each qualifying run pushes
                // its own clipped rect so the band stays inside the
                // pane's content box even when partially scrolled.
                if let Some(buf) = tab.content_buffer.as_ref() {
                    let mut acc = 0.0_f32;
                    let highlight_left = px + gutter_w;
                    let highlight_w = (content_w - gutter_w).max(0.0);
                    for run in buf.layout_runs() {
                        if run.line_i as u32 == hline {
                            let hy = py + acc - tab.module_scroll_y;
                            let top = hy.max(py);
                            let bot = (hy + line_height).min(py + content_h);
                            if bot > top {
                                below.push(RectInstance {
                                    rect: [highlight_left, top, highlight_w, bot - top],
                                    color: [1.0, 200.0 / 255.0, 80.0 / 255.0, 0.10],
                                });
                            }
                        }
                        acc += line_height;
                    }
                }
            }
        }

        // Host-rendered cursor for data modules that asked for one
        // (Editor). Uses the same color the shell cursor uses + the
        // same blink — same look as anywhere else in terminite.
        // Cells are sized at this pane's metrics; we compute the
        // cursor's source-line y, subtract module_scroll_y, and clip
        // to the content rect via a bounds check.
        if let Some(tab) = self
            .root
            .as_ref()
            .and_then(|n| n.find(pid))
            .map(|p| p.active_tab_ref())
        {
            if let Some((cline, ccol)) = tab.module_cursor {
                // Walk all layout runs belonging to source line
                // `cline`, tracking how many glyph columns of that
                // line have already been laid out. The first run
                // whose [cols_so_far .. cols_so_far + run_cols]
                // range contains the cursor column is the wrap
                // segment we draw on. If the cursor falls past the
                // last run's end (cursor at end of line / off the
                // tail), we clamp to the last segment.
                let mut chosen: Option<(f32, usize, usize)> = None; // (y, cols_consumed, run_cols)
                if let Some(buf) = tab.content_buffer.as_ref() {
                    let mut acc_y = 0.0_f32;
                    let mut cols_so_far = 0usize;
                    let mut prev_line: Option<u32> = None;
                    for run in buf.layout_runs() {
                        let line_i = run.line_i as u32;
                        if prev_line != Some(line_i) {
                            cols_so_far = 0;
                        }
                        if line_i == cline {
                            let run_cols = run.glyphs.len();
                            let after = cols_so_far + run_cols;
                            // Track latest run on this line so an
                            // over-end cursor lands on the final
                            // wrap rather than off-screen.
                            chosen = Some((acc_y, cols_so_far, run_cols));
                            if (ccol as usize) <= after {
                                break;
                            }
                            cols_so_far = after;
                        }
                        prev_line = Some(line_i);
                        acc_y += line_height;
                    }
                }
                if let Some((line_y, cols_consumed, run_cols)) = chosen {
                    let cy = py + line_y - tab.module_scroll_y;
                    let row_top = cy;
                    let row_bottom = cy + line_height;
                    let visible = row_bottom > py && row_top < py + content_h;
                    if visible && blink_on {
                        let cell_advance = metrics.cell_advance;
                        let col_in_run = ((ccol as usize)
                            .saturating_sub(cols_consumed))
                            .min(run_cols) as f32;
                        let cx = px + gutter_w + col_in_run * cell_advance;
                        let crect = [
                            cx - CURSOR_PAD_X,
                            cy - CURSOR_PAD_Y,
                            cell_advance + 2.0 * CURSOR_PAD_X,
                            line_height + 2.0 * CURSOR_PAD_Y,
                        ];
                        let cl = crect[0].max(px + gutter_w);
                        let ct = crect[1].max(py);
                        let cr = (crect[0] + crect[2]).min(px + content_w);
                        let cb = (crect[1] + crect[3]).min(py + content_h);
                        if cr > cl && cb > ct {
                            below.push(RectInstance {
                                rect: [cl, ct, cr - cl, cb - ct],
                                color: CURSOR_COLOR,
                            });
                        }
                    }
                }
            }
        }

        PaneDraw {
            pid,
            text_left: px,
            text_top: py,
            bounds: TextBounds {
                left: px as i32,
                top: py as i32,
                right: (rect.x + rect.w - pad.right) as i32,
                bottom: (rect.y + rect.h - pad.bottom) as i32,
            },
            tabs: tab_slots,
        }
    }

    pub(super) fn render_pane(
        &mut self,
        pid: PaneId,
        rect: PaneRect,
        is_active: bool,
        blink_on: bool,
        below: &mut Vec<RectInstance>,
        above: &mut Vec<RectInstance>,
        tab_bar: &mut Vec<RectInstance>,
    ) -> PaneDraw {
        let metrics = self.pane_metrics(pid);
        let cell_advance = metrics.cell_advance;
        let line_height = metrics.line_height;
        let pad = self.pad;
        // This pane's own tab bar fills the top strip of its rect.
        let tab_slots = self.build_pane_tab_bar(pid, rect, is_active, tab_bar);

        // Per-pane background tint — pushed first into `below` so it
        // sits beneath everything else. Low alpha keeps text legible;
        // the palette color stays recognisable as a hint, not a wash.
        let pane_bg_idx = self
            .root
            .as_ref()
            .and_then(|n| n.find(pid))
            .map(|p| p.bg_idx)
            .unwrap_or(0);
        if pane_bg_idx != 0 {
            let [r, g, b, _] = palette_color(pane_bg_idx);
            below.push(RectInstance {
                rect: [
                    rect.x,
                    rect.y + self.tab_bar_height,
                    rect.w,
                    rect.h - self.tab_bar_height,
                ],
                color: [r, g, b, 0.18],
            });
        }

        // Non-shell content kinds short-circuit the whole shell render
        // path. The Welcome card (and future built-in kinds) lives in
        // `content_buffer`, built lazily here.
        let active_kind = self
            .root
            .as_ref()
            .and_then(|n| n.find(pid))
            .map(|p| p.active_tab_ref().kind.clone())
            .unwrap_or(TabContentKind::Shell);
        // TTY modules render through the same path shells use (they
        // draw via terminal escape sequences, parsed by alacritty).
        // Only data modules + Welcome short-circuit to a static body.
        let is_tty_module = match &active_kind {
            TabContentKind::Module(id) => self
                .modules
                .find(id)
                .map(|m| m.kind == crate::modules::ModuleKind::Tty)
                .unwrap_or(false),
            _ => false,
        };
        if active_kind != TabContentKind::Shell && !is_tty_module {
            return self.render_non_shell_pane(
                pid, rect, tab_slots, active_kind, blink_on, below,
            );
        }

        // Content origin and clip box — below this pane's tab bar, inset
        // on all four sides by the configured padding.
        let px = rect.x + pad.left;
        let py = rect.y + self.tab_bar_height + pad.top;
        let box_l = px;
        let box_t = py;
        let box_r = rect.x + rect.w - pad.right;
        let box_b = rect.y + rect.h - pad.bottom;
        // Clip a rect to this pane's content box; `None` if fully outside.
        // Hides the extra row above the pane until it scrolls into view, and
        // keeps one pane's rects out of its neighbour.
        let clip = |r: [f32; 4]| -> Option<[f32; 4]> {
            let nx = r[0].max(box_l);
            let ny = r[1].max(box_t);
            let nr = (r[0] + r[2]).min(box_r);
            let nb = (r[1] + r[3]).min(box_b);
            if nr <= nx || nb <= ny {
                None
            } else {
                Some([nx, ny, nr - nx, nb - ny])
            }
        };

        // ── Autoscroll tick (only a drag-selecting tab has a direction) ──
        let autoscroll_dir = self
            .root
            .as_mut()
            .expect("pane tree present")
            .find_mut(pid)
            .expect("pane present")
            .active_tab_ref()
            .autoscroll_dir;
        if let Some(dir) = autoscroll_dir {
            {
                let tab = self
                    .root
                    .as_mut()
                    .expect("pane tree present")
                    .find_mut(pid)
                    .expect("pane present")
                    .active_tab_mut();
                tab.active_term_mut().scroll(TermScroll::Delta(dir));
                let (after, _history) = tab.active_term().offset_and_history();
                let (c, r) = (tab.cols, tab.rows);
                if let Some(sel) = tab.selection.as_mut() {
                    let edge = if dir > 0 {
                        (-(after as i32), 0)
                    } else {
                        (r as i32 - 1 - after as i32, c.saturating_sub(1))
                    };
                    sel.extend_to(edge.0, edge.1);
                }
            }
            self.next_autoscroll_deadline =
                Some(Instant::now() + Duration::from_millis(AUTOSCROLL_TICK_MS));
        }

        // ── Snapshot the pane's active tab ──
        let Snapshot {
            text_runs,
            bg_runs,
            deco_runs,
            link_runs,
            cursor_line,
            cursor_col,
            cursor_shape,
            cursor_blinking,
            has_extra_row,
        } = self
            .root
            .as_mut()
            .expect("pane tree present")
            .find_mut(pid)
            .expect("pane present")
            .active_tab_mut()
            .active_term_mut()
            .snapshot();
        let _ = cursor_blinking;

        // ── Refresh the active tab's text buffer if its content changed ──
        {
            let tab = self
                .root
                .as_mut()
                .expect("pane tree present")
                .find_mut(pid)
                .expect("pane present")
                .active_tab_mut();
            let stale = tab.buffer_dirty || text_runs != tab.last_text_runs;
            if stale {
                let default_attrs =
                    Attrs::new().family(font_family(&self.font_family));
                tab.text_buffer.set_rich_text(
                    &mut self.font_system,
                    text_runs.iter().map(|(text, style)| {
                        let mut attrs = default_attrs.clone().color(style.color);
                        if style.bold {
                            attrs = attrs.weight(Weight::BOLD);
                        }
                        if style.italic {
                            attrs = attrs.style(Style::Italic);
                        }
                        (text.as_str(), attrs)
                    }),
                    &default_attrs,
                    Shaping::Advanced,
                    None,
                );
                tab.text_buffer
                    .shape_until_scroll(&mut self.font_system, false);
                tab.last_text_runs = text_runs;
                tab.buffer_dirty = false;
            }
        }

        // ── Active tab geometry reads ──
        let (y_shift, selection, display_offset, cols, rows) = {
            let tab = self
                .root
                .as_mut()
                .expect("pane tree present")
                .find_mut(pid)
                .expect("pane present")
                .active_tab_mut();
            (
                tab.pixel_offset,
                tab.selection,
                tab.active_term().offset_and_history().0 as i32,
                tab.cols,
                tab.rows,
            )
        };

        // ── Background runs ──
        for run in &bg_runs {
            if let Some(rc) = clip([
                px + run.start_col as f32 * cell_advance,
                py + run.line as f32 * line_height + y_shift,
                run.width as f32 * cell_advance,
                line_height,
            ]) {
                below.push(RectInstance {
                    rect: rc,
                    color: color_to_floats(run.color),
                });
            }
        }

        // ── Selection highlight: one rect per row, absolute Line coords
        // converted to viewport rows via display_offset. vl = -1 is allowed
        // so the highlight enters smoothly from the top during scroll. ──
        if let Some(sel) = selection {
            if !sel.is_empty() {
                let ((s_line, s_col), (e_line, e_col)) = sel.normalized();
                for abs_line in s_line..=e_line {
                    let vl = abs_line + display_offset;
                    if vl < -1 || vl >= rows as i32 {
                        continue;
                    }
                    let col_start = if abs_line == s_line { s_col } else { 0 };
                    let col_end_raw = if abs_line == e_line { e_col + 1 } else { cols };
                    let col_end = col_end_raw.min(cols);
                    let col_start = col_start.min(cols);
                    if col_start >= col_end {
                        continue;
                    }
                    if let Some(rc) = clip([
                        px + col_start as f32 * cell_advance,
                        py + vl as f32 * line_height + y_shift,
                        (col_end - col_start) as f32 * cell_advance,
                        line_height,
                    ]) {
                        below.push(RectInstance { rect: rc, color: SELECTION_COLOR });
                    }
                }
            }
        }

        // ── Find-match highlights (active pane's search only) ──
        if is_active {
            if let Some(find) = self.find.as_ref() {
                for (i, &(line, cs, ce)) in find.matches.iter().enumerate() {
                    let vl = line + display_offset;
                    if vl < -1 || vl >= rows as i32 {
                        continue;
                    }
                    let color = if i == find.current {
                        FIND_CURRENT_COLOR
                    } else {
                        FIND_MATCH_COLOR
                    };
                    if let Some(rc) = clip([
                        px + cs as f32 * cell_advance,
                        py + vl as f32 * line_height + y_shift,
                        (ce - cs + 1) as f32 * cell_advance,
                        line_height,
                    ]) {
                        below.push(RectInstance { rect: rc, color });
                    }
                }
            }
        }

        // ── Cursor (last in `below` so it sits on top of selection/bgs) ──
        let cursor_visible = !matches!(cursor_shape, CursorShapeKind::Hidden);
        if cursor_visible && blink_on {
            let cx = px + cursor_col as f32 * cell_advance;
            let cy_base = py + (cursor_line.max(0) as f32) * line_height + y_shift;
            let (crect, is_hollow) = match cursor_shape {
                CursorShapeKind::Block | CursorShapeKind::HollowBlock => (
                    [
                        cx - CURSOR_PAD_X,
                        cy_base - CURSOR_PAD_Y,
                        cell_advance + 2.0 * CURSOR_PAD_X,
                        line_height + 2.0 * CURSOR_PAD_Y,
                    ],
                    matches!(cursor_shape, CursorShapeKind::HollowBlock),
                ),
                CursorShapeKind::Beam => ([cx, cy_base, 2.0, line_height], false),
                CursorShapeKind::Underline => {
                    ([cx, cy_base + line_height - 2.0, cell_advance, 2.0], false)
                }
                CursorShapeKind::Hidden => ([0.0; 4], false),
            };
            if is_hollow {
                let [x, y, w, h] = crect;
                let t = 1.5;
                for edge in [
                    [x, y, w, t],
                    [x, y + h - t, w, t],
                    [x, y, t, h],
                    [x + w - t, y, t, h],
                ] {
                    if let Some(rc) = clip(edge) {
                        below.push(RectInstance { rect: rc, color: CURSOR_COLOR });
                    }
                }
            } else if let Some(rc) = clip(crect) {
                below.push(RectInstance { rect: rc, color: CURSOR_COLOR });
            }
        }

        // ── Decorations (underline / double underline / strikeout) ──
        for run in &deco_runs {
            let x = px + run.start_col as f32 * cell_advance;
            let w = run.width as f32 * cell_advance;
            let line_y = py + run.line as f32 * line_height + y_shift;
            let (y, h) = match run.kind {
                DecorationKind::Underline | DecorationKind::DoubleUnderline => {
                    (line_y + line_height - UNDERLINE_THICKNESS, UNDERLINE_THICKNESS)
                }
                DecorationKind::Strikeout => (
                    line_y + line_height / 2.0 - STRIKEOUT_THICKNESS / 2.0,
                    STRIKEOUT_THICKNESS,
                ),
            };
            let color = color_to_floats(run.color);
            if let Some(rc) = clip([x, y, w, h]) {
                above.push(RectInstance { rect: rc, color });
            }
            if matches!(run.kind, DecorationKind::DoubleUnderline) {
                if let Some(rc) =
                    clip([x, y - DOUBLE_UNDERLINE_GAP, w, UNDERLINE_THICKNESS])
                {
                    above.push(RectInstance { rect: rc, color });
                }
            }
        }

        // ── OSC 8 hyperlink underlines ──
        for run in &link_runs {
            let x = px + run.start_col as f32 * cell_advance;
            let w = run.width as f32 * cell_advance;
            let line_y = py + run.line as f32 * line_height + y_shift;
            if let Some(rc) = clip([
                x,
                line_y + line_height - UNDERLINE_THICKNESS,
                w,
                UNDERLINE_THICKNESS,
            ]) {
                above.push(RectInstance { rect: rc, color: LINK_UNDERLINE_COLOR });
            }
        }

        // The buffer's top sits one line up when an extra row is present; the
        // y_shift slides it into view as pixel_offset grows.
        let text_top = if has_extra_row {
            py - line_height + y_shift
        } else {
            py + y_shift
        };
        PaneDraw {
            pid,
            text_left: px,
            text_top,
            bounds: TextBounds {
                left: box_l as i32,
                top: box_t as i32,
                right: box_r as i32,
                bottom: box_b as i32,
            },
            tabs: tab_slots,
        }
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
