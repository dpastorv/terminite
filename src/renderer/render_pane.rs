//! Per-pane rendering — shell and non-shell pane content assembly.

use super::*;

impl Renderer {
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
        // to render); fall back to the kind's static placeholder.
        let body = {
            let tab_opt = self
                .root
                .as_ref()
                .and_then(|n| n.find(pid))
                .map(|p| p.active_tab_ref());
            let session_body = tab_opt
                .and_then(|t| t.module_session.as_ref())
                .map(|s| s.body.clone())
                .filter(|s| !s.is_empty());
            session_body
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
