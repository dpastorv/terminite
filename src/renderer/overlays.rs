//! Modal, context-menu, and find overlays.

use super::*;

impl Renderer {
    /// True while an in-window modal is up — callers (main.rs) should route
    /// keyboard / mouse input to the modal handlers below.
    pub fn has_modal(&self) -> bool {
        self.modal.is_some()
    }

    pub fn modal_cancel(&mut self) {
        self.modal = None;
        self.window.request_redraw();
    }

    /// Confirm the open modal. Returns true if the window should exit.
    pub fn modal_confirm(&mut self) -> bool {
        let Some(modal) = self.modal.take() else { return false };
        self.window.request_redraw();
        match modal.action {
            ModalAction::CloseTab => self.do_close_active_tab(),
            ModalAction::ClosePane => {
                // close_active_pane is guarded against the last pane, so it
                // never signals an exit here.
                let _ = self.close_active_pane();
                false
            }
            ModalAction::CrashNotice => {
                // "View Last Crash" — print the dump to stdout and exit so
                // the shell can capture it (e.g. `terminite 2>&1 | tee crash.log`).
                use crate::crash::last_crash_path;
                if let Some(path) = last_crash_path() {
                    if let Ok(body) = std::fs::read_to_string(&path) {
                        println!("{}", body);
                    }
                }
                true // exit after printing
            }
        }
    }

    /// Mouse click while the modal is up. Returns true if the window should
    /// exit (confirm hit on the last tab).
    pub fn modal_click(&mut self, x: f32, y: f32) -> bool {
        let Some(modal) = self.modal.as_ref() else { return false };
        let in_rect = |r: (f32, f32, f32, f32)| {
            x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
        };
        if in_rect(modal.confirm_rect) {
            return self.modal_confirm();
        }
        if in_rect(modal.cancel_rect) {
            self.modal_cancel();
        }
        false
    }

    /// Open the right-click context menu at a pixel position. Items depend
    /// on context: Copy is enabled only with a selection, Open Link only
    /// appears when the click landed on an OSC 8 hyperlink.
    pub(super) fn open_context_menu(&mut self, x: f32, y: f32) {
        let (line, col) = self.pixel_to_absolute(x, y);
        let link = self.active_tab_mut().live_term.hyperlink_at(line, col);
        let has_selection = self
            .active_tab_ref()
            .selection
            .map(|s| !s.is_empty())
            .unwrap_or(false);

        let mut items: Vec<MenuItem> = Vec::new();
        items.push(MenuItem {
            label_buf: make_modal_buffer(&mut self.font_system, "Copy"),
            action: MenuAction::Copy,
            enabled: has_selection,
            swatch: None,
        });
        items.push(MenuItem {
            label_buf: make_modal_buffer(&mut self.font_system, "Paste"),
            action: MenuAction::Paste,
            enabled: true,
            swatch: None,
        });
        if let Some(uri) = link {
            items.push(MenuItem {
                label_buf: make_modal_buffer(&mut self.font_system, "Open Link"),
                action: MenuAction::OpenLink(uri),
                enabled: true,
                swatch: None,
            });
        }
        items.push(MenuItem {
            label_buf: make_modal_buffer(&mut self.font_system, "Select All"),
            action: MenuAction::SelectAll,
            enabled: true,
            swatch: None,
        });

        // Color items — apply to the active tab + active pane. Each
        // click cycles one step through the shared palette. The label
        // shows the *current* setting so the user knows what state
        // they're in before clicking.
        let tab_color_name =
            palette_name(self.active_tab_ref().color_idx);
        let pane_bg_name =
            palette_name(self.active_pane_ref().bg_idx);
        items.push(MenuItem {
            label_buf: make_modal_buffer(
                &mut self.font_system,
                &format!("Tab color: {tab_color_name}  ▸"),
            ),
            action: MenuAction::SubmenuTabColor,
            enabled: true,
            swatch: None,
        });
        items.push(MenuItem {
            label_buf: make_modal_buffer(
                &mut self.font_system,
                &format!("Pane bg: {pane_bg_name}  ▸"),
            ),
            action: MenuAction::SubmenuPaneBg,
            enabled: true,
            swatch: None,
        });
        let pane_scale_pct = (self.active_pane_ref().font_scale * 100.0).round() as i32;
        items.push(MenuItem {
            label_buf: make_modal_buffer(
                &mut self.font_system,
                &format!("Pane scale: {pane_scale_pct}%  ▸"),
            ),
            action: MenuAction::SubmenuPaneScale,
            enabled: true,
            swatch: None,
        });

        // Keep the menu fully on-screen.
        let h = items.len() as f32 * MENU_ITEM_H;
        let mx = x
            .min(self.surface_config.width as f32 - MENU_WIDTH - 4.0)
            .max(0.0);
        let my = y
            .min(self.surface_config.height as f32 - h - 4.0)
            .max(0.0);
        self.context_menu = Some(ContextMenu {
            x: mx,
            y: my,
            items,
            hovered: None,
        });
        self.window.request_redraw();
    }

    /// Color-picker submenu — the palette as swatches, click sets directly
    /// (drilled into from "Tab color" / "Pane bg"). `pane_bg` picks the target.
    fn open_color_picker(&mut self, x: f32, y: f32, pane_bg: bool) {
        let mut items: Vec<MenuItem> = Vec::new();
        for (idx, (name, color)) in COLOR_PALETTE.iter().enumerate() {
            let action = if pane_bg {
                MenuAction::SetPaneBg(idx as u8)
            } else {
                MenuAction::SetTabColor(idx as u8)
            };
            // Index 0 is the transparent "none" entry — no swatch to draw.
            let swatch = if idx == 0 { None } else { Some(*color) };
            items.push(MenuItem {
                label_buf: make_modal_buffer(&mut self.font_system, &format!("   {name}")),
                action,
                enabled: true,
                swatch,
            });
        }
        self.place_menu(items, x, y);
    }

    /// Pane-scale (zoom) submenu — the presets as a list, click sets directly.
    fn open_scale_picker(&mut self, x: f32, y: f32) {
        let mut items: Vec<MenuItem> = Vec::new();
        for scale in PANE_SCALE_PRESETS {
            let pct = (scale * 100.0).round() as i32;
            items.push(MenuItem {
                label_buf: make_modal_buffer(&mut self.font_system, &format!("   {pct}%")),
                action: MenuAction::SetPaneScale(*scale),
                enabled: true,
                swatch: None,
            });
        }
        self.place_menu(items, x, y);
    }

    /// Anchor a freshly-built menu on-screen and show it.
    fn place_menu(&mut self, items: Vec<MenuItem>, x: f32, y: f32) {
        let h = items.len() as f32 * MENU_ITEM_H;
        let mx = x
            .min(self.surface_config.width as f32 - MENU_WIDTH - 4.0)
            .max(0.0);
        let my = y
            .min(self.surface_config.height as f32 - h - 4.0)
            .max(0.0);
        self.context_menu = Some(ContextMenu { x: mx, y: my, items, hovered: None });
        self.window.request_redraw();
    }

    /// Open the kind-selector dropdown for one pane. Anchored at the
    /// bottom-left of that pane's selector slot, so it falls open like
    /// a normal dropdown rather than appearing where the cursor was.
    pub(super) fn open_kind_dropdown(&mut self, pid: PaneId, prect: PaneRect) {
        let current = self
            .root_ref()
            .find(pid)
            .map(|p| p.active_tab_ref().kind.clone())
            .unwrap_or(TabContentKind::Shell);

        // Menu: built-ins first, then every discovered module in
        // registry order.
        let mut entries: Vec<(TabContentKind, String)> = vec![
            (TabContentKind::Shell, "Shell".to_string()),
            (TabContentKind::Welcome, "Welcome".to_string()),
        ];
        for m in self.modules.list() {
            entries.push((TabContentKind::Module(m.id.clone()), m.name.clone()));
        }

        let mut items: Vec<MenuItem> = entries
            .into_iter()
            .map(|(kind, name)| {
                let label = if kind == current {
                    format!("• {name}")
                } else {
                    format!("  {name}")
                };
                MenuItem {
                    label_buf: make_modal_buffer(&mut self.font_system, &label),
                    action: MenuAction::SetTabKind { pane: pid, kind },
                    enabled: true,
                    swatch: None,
                }
            })
            .collect();
        // Trailing "Open modules folder…" — reveals the install
        // directory in Finder so the user can drop a new module in
        // without touching the CLI. fs-watch picks up the drop and
        // refreshes this dropdown automatically.
        items.push(MenuItem {
            label_buf: make_modal_buffer(
                &mut self.font_system,
                "  Open modules folder…",
            ),
            action: MenuAction::OpenModulesFolder,
            enabled: crate::modules::modules_dir().is_some(),
            swatch: None,
        });
        let h = items.len() as f32 * MENU_ITEM_H;
        let mx = prect.x.max(0.0);
        let my = (prect.y + self.tab_bar_height)
            .min(self.surface_config.height as f32 - h - 4.0)
            .max(0.0);
        self.context_menu = Some(ContextMenu {
            x: mx,
            y: my,
            items,
            hovered: None,
        });
        self.window.request_redraw();
    }

    pub fn has_context_menu(&self) -> bool {
        self.context_menu.is_some()
    }

    pub fn dismiss_context_menu(&mut self) {
        self.context_menu = None;
        self.window.request_redraw();
    }

    /// Item index under a pixel position, or None if outside the menu.
    pub(super) fn context_menu_at(&self, x: f32, y: f32) -> Option<usize> {
        let menu = self.context_menu.as_ref()?;
        if x < menu.x || x >= menu.x + MENU_WIDTH || y < menu.y {
            return None;
        }
        let idx = ((y - menu.y) / MENU_ITEM_H) as usize;
        (idx < menu.items.len()).then_some(idx)
    }

    /// Resolve a click while the menu is up: run the hit item's action (if
    /// enabled), then dismiss. A click anywhere just dismisses.
    pub(super) fn context_menu_click(&mut self, x: f32, y: f32) {
        let hit = self.context_menu_at(x, y);
        let Some(menu) = self.context_menu.take() else { return };
        self.window.request_redraw();
        let Some(idx) = hit else { return };
        if !menu.items[idx].enabled {
            return;
        }
        match &menu.items[idx].action {
            MenuAction::Copy => self.copy_selection(),
            MenuAction::Paste => self.paste(),
            MenuAction::OpenLink(uri) => open_uri(uri),
            MenuAction::SelectAll => self.select_all(),
            MenuAction::SetTabKind { pane, kind } => {
                let pane = *pane;
                let kind = kind.clone();
                self.set_tab_kind(pane, kind);
            }
            // Parents drill into a picker submenu (don't close the menu).
            MenuAction::SubmenuTabColor => self.open_color_picker(menu.x, menu.y, false),
            MenuAction::SubmenuPaneBg => self.open_color_picker(menu.x, menu.y, true),
            MenuAction::SubmenuPaneScale => self.open_scale_picker(menu.x, menu.y),
            // Leaves set the value directly.
            MenuAction::SetTabColor(idx) => {
                self.active_tab_mut().color_idx = *idx;
                self.window.request_redraw();
            }
            MenuAction::SetPaneBg(idx) => {
                self.active_pane_mut().bg_idx = *idx;
                self.window.request_redraw();
            }
            MenuAction::SetPaneScale(scale) => {
                let pid = self.active_pane;
                self.apply_pane_scale(pid, *scale);
            }
            MenuAction::OpenModulesFolder => {
                if let Some(dir) = crate::modules::modules_dir() {
                    // Ensure it exists so `open` doesn't bounce
                    // (a fresh install may not have created it yet).
                    let _ = std::fs::create_dir_all(&dir);
                    // open_uri only accepts http(s)/file://mailto —
                    // wrap the absolute path in file:// so the path
                    // takes the macOS `open` route rather than being
                    // silently dropped by the scheme allowlist.
                    open_uri(&format!("file://{}", dir.to_string_lossy()));
                }
            }
        }
    }

    /// Build the rect instances for the context menu (background, border,
    /// hovered-item highlight).
    pub(super) fn build_menu_rects(&self) -> Vec<RectInstance> {
        let Some(menu) = self.context_menu.as_ref() else {
            return Vec::new();
        };
        let h = menu.items.len() as f32 * MENU_ITEM_H;
        let border = 1.0;
        let mut rects = vec![
            RectInstance {
                rect: [
                    menu.x - border,
                    menu.y - border,
                    MENU_WIDTH + 2.0 * border,
                    h + 2.0 * border,
                ],
                color: MENU_BORDER,
            },
            RectInstance {
                rect: [menu.x, menu.y, MENU_WIDTH, h],
                color: MENU_BG,
            },
        ];
        if let Some(hov) = menu.hovered {
            if menu.items[hov].enabled {
                rects.push(RectInstance {
                    rect: [
                        menu.x,
                        menu.y + hov as f32 * MENU_ITEM_H,
                        MENU_WIDTH,
                        MENU_ITEM_H,
                    ],
                    color: MENU_HOVER_BG,
                });
            }
        }
        // Colour swatches for the picker submenu — drawn at the row's left
        // edge; the rows' leading spaces keep the label clear of it.
        for (i, item) in menu.items.iter().enumerate() {
            if let Some(color) = item.swatch {
                let row_y = menu.y + i as f32 * MENU_ITEM_H;
                rects.push(RectInstance {
                    rect: [menu.x + 14.0, row_y + (MENU_ITEM_H - 18.0) * 0.5, 18.0, 18.0],
                    color,
                });
            }
        }
        rects
    }

    // ── Find ──────────────────────────────────────────────────────────────

    pub fn has_find(&self) -> bool {
        self.find.is_some()
    }

    pub fn open_find(&mut self) {
        let bar_buf = make_modal_buffer(&mut self.font_system, "Find:");
        self.find = Some(FindState {
            query: String::new(),
            bar_buf,
            matches: Vec::new(),
            current: 0,
        });
        self.window.request_redraw();
    }

    pub fn close_find(&mut self) {
        self.find = None;
        self.window.request_redraw();
    }

    pub fn find_input(&mut self, ch: char) {
        if let Some(find) = self.find.as_mut() {
            find.query.push(ch);
        }
        self.rerun_search();
    }

    pub fn find_backspace(&mut self) {
        if let Some(find) = self.find.as_mut() {
            find.query.pop();
        }
        self.rerun_search();
    }

    pub fn find_next(&mut self) {
        if let Some(find) = self.find.as_mut() {
            if !find.matches.is_empty() {
                find.current = (find.current + 1) % find.matches.len();
            }
        }
        self.rebuild_find_bar();
        self.scroll_to_current_match();
        self.window.request_redraw();
    }

    pub fn find_prev(&mut self) {
        if let Some(find) = self.find.as_mut() {
            if !find.matches.is_empty() {
                find.current = if find.current == 0 {
                    find.matches.len() - 1
                } else {
                    find.current - 1
                };
            }
        }
        self.rebuild_find_bar();
        self.scroll_to_current_match();
        self.window.request_redraw();
    }

    /// Re-run the search for the current query and reset to the first match.
    pub(super) fn rerun_search(&mut self) {
        let query = match self.find.as_ref() {
            Some(f) => f.query.clone(),
            None => return,
        };
        let matches = self.active_tab_mut().live_term.search(&query);
        if let Some(find) = self.find.as_mut() {
            find.matches = matches;
            find.current = 0;
        }
        self.rebuild_find_bar();
        self.scroll_to_current_match();
        self.window.request_redraw();
    }

    pub(super) fn rebuild_find_bar(&mut self) {
        let text = match self.find.as_ref() {
            Some(f) if f.query.is_empty() => "Find:".to_string(),
            Some(f) if f.matches.is_empty() => {
                format!("Find: {}   no matches", f.query)
            }
            Some(f) => {
                format!("Find: {}   {}/{}", f.query, f.current + 1, f.matches.len())
            }
            None => return,
        };
        let buf = make_modal_buffer(&mut self.font_system, &text);
        if let Some(find) = self.find.as_mut() {
            find.bar_buf = buf;
        }
    }

    pub(super) fn scroll_to_current_match(&mut self) {
        let target = self
            .find
            .as_ref()
            .and_then(|f| f.matches.get(f.current).copied());
        if let Some((line, _, _)) = target {
            let rows = self.grid_rows;
            self.active_tab_mut()
                .live_term
                .scroll_to_line(line, rows);
        }
    }

    pub(super) fn open_modal(
        &mut self,
        action: ModalAction,
        title: String,
        body: String,
        cancel: &str,
        confirm: &str,
    ) {
        let title_buf = make_modal_buffer(&mut self.font_system, &title);
        let body_buf = make_modal_buffer(&mut self.font_system, &body);
        let cancel_buf = make_modal_buffer(&mut self.font_system, cancel);
        let confirm_buf = make_modal_buffer(&mut self.font_system, confirm);
        self.modal = Some(Modal {
            action,
            title_buf,
            body_buf,
            cancel_buf,
            confirm_buf,
            cancel_rect: (0.0, 0.0, 0.0, 0.0),
            confirm_rect: (0.0, 0.0, 0.0, 0.0),
        });
        self.window.request_redraw();
    }

    // ── Command palette ───────────────────────────────────────────────────

    pub fn has_palette(&self) -> bool {
        self.palette.is_some()
    }

    /// Open the command palette (Cmd+Shift+P): a filterable list of every
    /// action + its shortcut, so the app's commands (including the novel
    /// split/join gestures) are discoverable and runnable by name.
    pub fn open_palette(&mut self) {
        let items: Vec<PaletteItem> = PALETTE_COMMANDS
            .iter()
            .map(|(label, hint, action)| {
                // Left-pad the label so the shortcut hint trails at a roughly
                // consistent column. Proportional font, so it's approximate —
                // readable, not pixel-aligned.
                let text = format!("{label:<24}{hint}");
                PaletteItem {
                    label_buf: make_modal_buffer(&mut self.font_system, &text),
                    search: label.to_lowercase(),
                    action: *action,
                }
            })
            .collect();
        let filtered = (0..items.len()).collect();
        let prompt_buf = make_modal_buffer(&mut self.font_system, "\u{203a} ");
        self.palette = Some(PaletteState {
            query: String::new(),
            prompt_buf,
            items,
            filtered,
            selected: 0,
        });
        self.window.request_redraw();
    }

    pub fn close_palette(&mut self) {
        self.palette = None;
        self.window.request_redraw();
    }

    pub fn palette_input(&mut self, ch: char) {
        if let Some(p) = self.palette.as_mut() {
            p.query.push(ch);
        }
        self.palette_refilter();
    }

    pub fn palette_backspace(&mut self) {
        if let Some(p) = self.palette.as_mut() {
            p.query.pop();
        }
        self.palette_refilter();
    }

    /// Move the selection up or down within the filtered list (wraps).
    pub fn palette_move(&mut self, down: bool) {
        if let Some(p) = self.palette.as_mut() {
            let n = p.filtered.len();
            if n == 0 {
                return;
            }
            p.selected = if down {
                (p.selected + 1) % n
            } else if p.selected == 0 {
                n - 1
            } else {
                p.selected - 1
            };
        }
        self.window.request_redraw();
    }

    /// Recompute the filtered set for the current query and rebuild the
    /// prompt line. Case-insensitive substring, matching find's simplicity.
    pub(super) fn palette_refilter(&mut self) {
        let (query, filtered) = match self.palette.as_ref() {
            Some(p) => {
                let q = p.query.to_lowercase();
                let f: Vec<usize> = p
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| q.is_empty() || it.search.contains(&q))
                    .map(|(i, _)| i)
                    .collect();
                (p.query.clone(), f)
            }
            None => return,
        };
        let prompt = make_modal_buffer(&mut self.font_system, &format!("\u{203a} {query}"));
        if let Some(p) = self.palette.as_mut() {
            p.filtered = filtered;
            p.selected = 0;
            p.prompt_buf = prompt;
        }
        self.window.request_redraw();
    }

    /// Run the selected command and close the palette. Returns true if the
    /// action should exit the app (Quit, or Close that shut the last pane) —
    /// the caller owns the event loop.
    pub fn palette_execute(&mut self) -> bool {
        let action = self.palette.as_ref().and_then(|p| {
            p.filtered.get(p.selected).map(|&i| p.items[i].action)
        });
        self.close_palette();
        let Some(action) = action else { return false };
        use PaletteAction::*;
        match action {
            NewTab => self.new_tab(),
            SplitRight => self.split_active(SplitDir::Vertical, 0.5),
            SplitDown => self.split_active(SplitDir::Horizontal, 0.5),
            NextTab => self.next_tab(),
            PrevTab => self.prev_tab(),
            Find => self.open_find(),
            ClearScrollback => self.clear_scrollback(),
            SelectAll => self.select_all(),
            ZoomIn => self.zoom_by(2.0),
            ZoomOut => self.zoom_by(-2.0),
            ZoomReset => self.zoom_reset(),
            CycleFont => self.cycle_font(true),
            ScrollTop => self.scroll_to_edge(true),
            ScrollBottom => self.scroll_to_edge(false),
            FocusLeft => self.focus_dir(-1.0, 0.0),
            FocusRight => self.focus_dir(1.0, 0.0),
            FocusUp => self.focus_dir(0.0, -1.0),
            FocusDown => self.focus_dir(0.0, 1.0),
            CloseTab => return self.close_active_tab(),
            Stop => self.governance_stop(),
            Halt => self.governance_halt(),
            Release => self.governance_release(),
            RoomWho => self.show_room_who(),
            RoomFiles => self.show_room_files(),
            Quit => return true,
        }
        false
    }

    /// Palette box geometry: `(x, y, first_visible_idx, visible_count)`.
    /// Shared by the rect and text passes so they never drift. `None` when
    /// the palette is closed.
    pub(super) fn palette_layout(&self) -> Option<(f32, f32, usize, usize)> {
        let p = self.palette.as_ref()?;
        let total = p.filtered.len();
        let first = if p.selected >= PALETTE_MAX_ROWS {
            p.selected - PALETTE_MAX_ROWS + 1
        } else {
            0
        };
        let visible = total.saturating_sub(first).min(PALETTE_MAX_ROWS);
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let x = ((surface_w - PALETTE_WIDTH) * 0.5).max(4.0);
        let y = (surface_h * 0.16).max(8.0);
        Some((x, y, first, visible))
    }

    pub(super) fn build_palette_rects(&self) -> Vec<RectInstance> {
        let Some((x, y, first, visible)) = self.palette_layout() else {
            return Vec::new();
        };
        let Some(p) = self.palette.as_ref() else { return Vec::new() };
        // Query row on top, then the visible command rows.
        let h = PALETTE_ROW_H * (1 + visible) as f32;
        let border = 1.0;
        let mut rects = vec![
            RectInstance {
                rect: [x - border, y - border, PALETTE_WIDTH + 2.0 * border, h + 2.0 * border],
                color: MENU_BORDER,
            },
            RectInstance {
                rect: [x, y, PALETTE_WIDTH, h],
                color: MENU_BG,
            },
        ];
        // Highlight the selected row (offset by the query row + scroll window).
        if visible > 0 {
            let sel_row = p.selected - first;
            rects.push(RectInstance {
                rect: [
                    x,
                    y + PALETTE_ROW_H * (1 + sel_row) as f32,
                    PALETTE_WIDTH,
                    PALETTE_ROW_H,
                ],
                color: MENU_HOVER_BG,
            });
        }
        rects
    }

}

// ── moved from mod.rs ───────────────────────────────

/// What the user is being asked to confirm. Generalized so we can reuse the
/// same modal for future yes/no decisions.
#[derive(Debug)]
pub(super) enum ModalAction {
    CloseTab,
    ClosePane,
    /// Crash notice — shown at startup when a recent crash is detected.
    CrashNotice,
}

// ── Display settings overlay — zoom controls ─────────────

pub(super) const DISPLAY_SETTINGS_W: f32 = 360.0;
pub(super) const DISPLAY_SETTINGS_BG: [f32; 4] = [0.10, 0.10, 0.13, 1.0];
pub(super) const DISPLAY_SETTINGS_BORDER: [f32; 4] = [0.20, 0.20, 0.26, 1.0];
/// Button hit boxes: (x, y, w, h) relative to surface origin.
pub(super) type HitBox = (f32, f32, f32, f32);

/// Display settings overlay: shows current zoom level and + / - / Reset buttons.
pub(super) struct DisplaySettingsOverlay {
    pub(super) title_buf: Buffer,
    /// "Zoom: 100%" or similar.
    pub(super) zoom_buf: Buffer,
    /// Display info: resolution, DPI, scale factor, suggestion.
    pub(super) display_buf: Buffer,
    /// Button hit boxes in surface coordinates.
    pub(super) btn_in: HitBox,
    pub(super) btn_out: HitBox,
    pub(super) btn_reset: HitBox,
}

impl Renderer {
    /// Open the display settings overlay card.
    pub(crate) fn open_display_settings(&mut self) {
        let zoom = self.current_zoom_pct();
        let title = "Display Settings".to_string();
        let zoom_text = format!("Zoom: {}%", zoom);

        // Display info: scale factor, resolution, suggested zoom.
        let scale = self.scale_factor;
        let surface_w = self.surface_config.width;
        let surface_h = self.surface_config.height;
        let logical_w = (surface_w as f32 / scale) as i32;
        let logical_h = (surface_h as f32 / scale) as i32;
        let dpi = (scale * 96.0).round() as i32;
        let suggestion = if scale > 1.5 {
            "Consider zooming out for sharper text"
        } else if scale < 1.1 && (surface_w > 2500 || surface_h > 1400) {
            "Consider zooming in for readability"
        } else {
            "Display looks good"
        };

        let title_buf = make_modal_buffer(&mut self.font_system, &title);
        let zoom_buf = make_modal_buffer(&mut self.font_system, &zoom_text);
        let display_info = format!(
            "{}×{} @ {}dpi (scale {:.1}×)\n{}",
            logical_w, logical_h, dpi, scale, suggestion
        );
        let display_buf = make_modal_buffer(&mut self.font_system, &display_info);

        // Compute button hit boxes — centered below the text.
        let card_w = DISPLAY_SETTINGS_W;
        let card_h = MODAL_CARD_H;
        let card_x = (surface_w as f32 - card_w) * 0.5;
        let card_y = (surface_h as f32 - card_h) * 0.5;
        let btn_h = 36.0;
        let btn_w = 80.0;
        let gap = 12.0;
        let btn_y = card_y + MODAL_CARD_H - btn_h - 24.0;
        let total_btns_w = btn_w * 3.0 + gap * 2.0;
        let start_x = card_x + (card_w - total_btns_w) * 0.5;

        self.display_settings = Some(DisplaySettingsOverlay {
            title_buf,
            zoom_buf,
            display_buf,
            btn_in: (start_x, btn_y, btn_w, btn_h),
            btn_out: (start_x + btn_w + gap, btn_y, btn_w, btn_h),
            btn_reset: (start_x + (btn_w + gap) * 2.0, btn_y, btn_w, btn_h),
        });
        self.window.request_redraw();
    }

    /// Close the display settings overlay.
    pub(crate) fn close_display_settings(&mut self) {
        self.display_settings = None;
        self.window.request_redraw();
    }

    /// Is the display settings overlay currently open?
    pub(crate) fn has_display_settings(&self) -> bool {
        self.display_settings.is_some()
    }

    /// Current zoom level as a percentage of base font size.
    pub(super) fn current_zoom_pct(&self) -> i32 {
        if self.base_font_size == 0.0 {
            return 100;
        }
        ((self.font_size / self.base_font_size) * 100.0).round() as i32
    }

    /// Hit-test display settings buttons at a surface coordinate. Returns
    /// which button was clicked, if any.
    pub(crate) fn hit_display_settings(&self, x: f32, y: f32) -> Option<&'static str> {
        let ds = self.display_settings.as_ref()?;
        let in_rect = |r: HitBox| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
        if in_rect(ds.btn_in) {
            Some("zoom_in")
        } else if in_rect(ds.btn_out) {
            Some("zoom_out")
        } else if in_rect(ds.btn_reset) {
            Some("zoom_reset")
        } else {
            None
        }
    }
}


// ── moved from mod.rs ───────────────────────────────

/// An action invoked from the right-click context menu.
pub(super) enum MenuAction {
    Copy,
    Paste,
    OpenLink(String),
    SelectAll,
    /// Switch the active tab of the menu's pane to a different content
    /// kind. Bundle 6 step 1 — the dropdown surface.
    SetTabKind {
        pane: PaneId,
        kind: TabContentKind,
    },
    /// Open a picker submenu (a list of swatches / values) instead of
    /// cycling — drilled into from the parent row.
    SubmenuTabColor,
    SubmenuPaneBg,
    SubmenuPaneScale,
    /// Leaf picks from those submenus — set directly, no cycling.
    SetTabColor(u8),
    SetPaneBg(u8),
    SetPaneScale(f32),
    /// Reveal `~/.terminite/modules/` in Finder so the user can
    /// drop a new module in. fs-watch picks it up automatically;
    /// no CLI dance needed.
    OpenModulesFolder,
}

/// One row in the context menu.
pub(super) struct MenuItem {
    pub(super) label_buf: Buffer,
    pub(super) action: MenuAction,
    pub(super) enabled: bool,
    /// A colour swatch drawn at the row's left edge (the color-picker
    /// submenus). `None` for ordinary text rows.
    pub(super) swatch: Option<[f32; 4]>,
}

/// Right-click context menu — a small overlay anchored at the cursor.
pub(super) struct ContextMenu {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) items: Vec<MenuItem>,
    /// Index of the item under the cursor, for hover highlight.
    pub(super) hovered: Option<usize>,
}

pub(super) const MENU_WIDTH: f32 = 320.0;
pub(super) const MENU_ITEM_H: f32 = 40.0;
pub(super) const MENU_BG: [f32; 4] = [0.12, 0.12, 0.15, 1.0];
pub(super) const MENU_BORDER: [f32; 4] = [0.20, 0.20, 0.26, 1.0];
pub(super) const MENU_HOVER_BG: [f32; 4] = [0.22, 0.30, 0.46, 1.0];

// ── Command palette ─────────────────────────────────────────────────────

pub(super) const PALETTE_WIDTH: f32 = 560.0;
pub(super) const PALETTE_ROW_H: f32 = MENU_ITEM_H;
pub(super) const PALETTE_MAX_ROWS: usize = 10;

#[derive(Clone, Copy)]
pub(super) enum PaletteAction {
    NewTab,
    SplitRight,
    SplitDown,
    CloseTab,
    NextTab,
    PrevTab,
    Find,
    ClearScrollback,
    SelectAll,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    CycleFont,
    ScrollTop,
    ScrollBottom,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    /// STOP — Ctrl-C into the focused actor's pane (bypasses busy).
    Stop,
    /// HALT — interrupt + quarantine the focused actor.
    Halt,
    /// RELEASE — lift a HALT on the focused actor.
    Release,
    /// Room who — show the presence roster in the status line.
    RoomWho,
    /// Room files — show active file claims in the status line.
    RoomFiles,
    Quit,
}

/// One command in the palette: its rendered label buffer, a lowercased
/// search key, and the action to run.
pub(super) struct PaletteItem {
    pub label_buf: Buffer,
    pub search: String,
    pub action: PaletteAction,
}

pub(super) struct PaletteState {
    pub query: String,
    pub prompt_buf: Buffer,
    pub items: Vec<PaletteItem>,
    /// Indices into `items` matching the current query, in display order.
    pub filtered: Vec<usize>,
    /// Index into `filtered` (not `items`) of the highlighted row.
    pub selected: usize,
}

/// (label, shortcut hint, action). Order is the display order at empty query.
/// Symbols: ⌘ super, ⇧ shift, ⌥ option, ↑↓←→ arrows.
pub(super) const PALETTE_COMMANDS: &[(&str, &str, PaletteAction)] = &[
    ("New Tab", "\u{2318}T", PaletteAction::NewTab),
    ("Split Right", "\u{2318}D", PaletteAction::SplitRight),
    ("Split Down", "\u{2318}\u{21e7}D", PaletteAction::SplitDown),
    ("Close Tab / Pane", "\u{2318}W", PaletteAction::CloseTab),
    ("Next Tab", "\u{2318}\u{21e7}]", PaletteAction::NextTab),
    ("Previous Tab", "\u{2318}\u{21e7}[", PaletteAction::PrevTab),
    ("Find", "\u{2318}F", PaletteAction::Find),
    ("Clear Scrollback", "\u{2318}K", PaletteAction::ClearScrollback),
    ("Select All", "\u{2318}A", PaletteAction::SelectAll),
    ("Zoom In", "\u{2318}+", PaletteAction::ZoomIn),
    ("Zoom Out", "\u{2318}-", PaletteAction::ZoomOut),
    ("Reset Zoom", "\u{2318}0", PaletteAction::ZoomReset),
    ("Cycle Font", "\u{2318}\u{21e7}F", PaletteAction::CycleFont),
    ("Scroll to Top", "\u{2318}\u{2191}", PaletteAction::ScrollTop),
    ("Scroll to Bottom", "\u{2318}\u{2193}", PaletteAction::ScrollBottom),
    ("Focus Pane Left", "\u{2318}\u{2325}\u{2190}", PaletteAction::FocusLeft),
    ("Focus Pane Right", "\u{2318}\u{2325}\u{2192}", PaletteAction::FocusRight),
    ("Focus Pane Up", "\u{2318}\u{2325}\u{2191}", PaletteAction::FocusUp),
    ("Focus Pane Down", "\u{2318}\u{2325}\u{2193}", PaletteAction::FocusDown),
    // ── Governance (human-only) ──────────────────────────────────
    ("STOP — interrupt focused actor", "", PaletteAction::Stop),
    ("HALT — quarantine focused actor", "", PaletteAction::Halt),
    ("RELEASE — lift HALT on focused actor", "", PaletteAction::Release),
    // ── Room substrate ───────────────────────────────────────────
    ("Room Who — presence roster", "", PaletteAction::RoomWho),
    ("Room Files — active claims", "", PaletteAction::RoomFiles),
    ("Quit", "\u{2318}Q", PaletteAction::Quit),
];

// Find bar — a floating box at the top-right of the content area.
pub(super) const FIND_BAR_W: f32 = 420.0;
pub(super) const FIND_BAR_H: f32 = 48.0;
pub(super) const FIND_BAR_MARGIN: f32 = 16.0;
pub(super) const FIND_BAR_BG: [f32; 4] = [0.12, 0.12, 0.15, 1.0];
pub(super) const FIND_BAR_BORDER: [f32; 4] = [0.20, 0.20, 0.26, 1.0];

/// In-progress incremental search over the active tab's scrollback.
pub(super) struct FindState {
    pub(super) query: String,
    /// Text buffer for the find bar (`⌕ query    N/M`), rebuilt on change.
    pub(super) bar_buf: Buffer,
    /// Absolute `(line, col_start, col_end)` matches, top-to-bottom.
    pub(super) matches: Vec<(i32, usize, usize)>,
    /// Index of the current (accented) match.
    pub(super) current: usize,
}

/// In-window modal dialog. Built when the user attempts to do something
/// destructive while a non-trivial process is running.
pub(super) struct Modal {
    pub(super) action: ModalAction,
    pub(super) title_buf: Buffer,
    pub(super) body_buf: Buffer,
    pub(super) cancel_buf: Buffer,
    pub(super) confirm_buf: Buffer,
    /// Hit boxes computed at layout time (origin x, y, w, h). Live for the
    /// frame; updated each render.
    pub(super) cancel_rect: (f32, f32, f32, f32),
    pub(super) confirm_rect: (f32, f32, f32, f32),
}

// ── File claims overlay — in-window card showing active claims ──

pub(super) const FILE_CLAIMS_W: f32 = 480.0;
pub(super) const FILE_CLAIMS_BG: [f32; 4] = [0.10, 0.10, 0.13, 1.0];
pub(super) const FILE_CLAIMS_BORDER: [f32; 4] = [0.20, 0.20, 0.26, 1.0];

/// In-progress file-claims overlay (Room Files). A small card showing
/// active claims keyed by path, rendered above content but below the modal.
pub(super) struct FileClaimsOverlay {
    /// Rendered title: "Room Files" or "Room Who — N actor(s)".
    pub(super) title_buf: Buffer,
    /// Body text: one line per item.
    pub(super) body_buf: Buffer,
}

impl Renderer {
    /// Close the file-claims overlay (Room Who / Room Files).
    pub(crate) fn close_file_claims(&mut self) {
        self.claims_overlay = None;
        self.window.request_redraw();
    }

    /// Is the file-claims overlay currently open?
    pub(crate) fn has_file_claims(&self) -> bool {
        self.claims_overlay.is_some()
    }
}


// ── moved from mod.rs ───────────────────────────────

impl Renderer {
    /// Compute the modal's card + button rectangles for the current surface
    /// size. Also updates the cached hit-boxes on the open modal so mouse
    /// clicks resolve to the correct button.
    pub(super) fn build_modal_rects(&mut self) -> Vec<RectInstance> {
        let modal = match self.modal.as_mut() {
            Some(m) => m,
            None => return Vec::new(),
        };
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let card_x = (surface_w - MODAL_CARD_W) * 0.5;
        let card_y = (surface_h - MODAL_CARD_H) * 0.5;
        let btn_y = card_y + MODAL_CARD_H - MODAL_BTN_H - 18.0;
        let gap = 16.0;
        let confirm_x = card_x + MODAL_CARD_W - MODAL_BTN_W - 18.0;
        let cancel_x = confirm_x - MODAL_BTN_W - gap;
        modal.cancel_rect = (cancel_x, btn_y, MODAL_BTN_W, MODAL_BTN_H);
        modal.confirm_rect = (confirm_x, btn_y, MODAL_BTN_W, MODAL_BTN_H);

        let border = 1.5;
        vec![
            // Dim full-surface overlay.
            RectInstance {
                rect: [0.0, 0.0, surface_w, surface_h],
                color: MODAL_BG_DIM,
            },
            // Card border (drawn slightly larger; card bg covers the interior).
            RectInstance {
                rect: [
                    card_x - border,
                    card_y - border,
                    MODAL_CARD_W + 2.0 * border,
                    MODAL_CARD_H + 2.0 * border,
                ],
                color: MODAL_CARD_BORDER,
            },
            RectInstance {
                rect: [card_x, card_y, MODAL_CARD_W, MODAL_CARD_H],
                color: MODAL_CARD_BG,
            },
            // Cancel button.
            RectInstance {
                rect: [cancel_x, btn_y, MODAL_BTN_W, MODAL_BTN_H],
                color: MODAL_BTN_BG,
            },
            // Confirm button (warm red — destructive emphasis).
            RectInstance {
                rect: [confirm_x, btn_y, MODAL_BTN_W, MODAL_BTN_H],
                color: MODAL_BTN_CONFIRM_BG,
            },
        ]
    }

}
