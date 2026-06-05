//! Config get/set bridge to hosted tabs.

use super::*;

impl Renderer {
    /// Build a `config` event for the requesting tab — schema +
    /// the live Config's current values — and push it down the
    /// module's stdin.
    pub(super) fn send_config_to_tab(&self, tab_id: TabId) {
        let schema = crate::config::schema();
        let mut keys = Vec::with_capacity(schema.len());
        for key in &schema {
            let current = self.config_current_value(key);
            keys.push(serde_json::json!({
                "name": key.name,
                "kind": match key.kind {
                    crate::config::ConfigKind::Float => "float",
                    crate::config::ConfigKind::Int => "int",
                    crate::config::ConfigKind::String => "string",
                    crate::config::ConfigKind::Bool => "bool",
                    crate::config::ConfigKind::Enum => "enum",
                },
                "default": match &key.default {
                    crate::config::ConfigValue::Float(f) => serde_json::json!(f),
                    crate::config::ConfigValue::Int(i) => serde_json::json!(i),
                    crate::config::ConfigValue::String(s) => serde_json::json!(s),
                    crate::config::ConfigValue::Bool(b) => serde_json::json!(b),
                },
                "current": current,
                "hot_reload": key.hot_reload,
                "doc": key.doc,
            }));
        }
        let payload = serde_json::json!({
            "kind": "config",
            "config_path": crate::config_io::config_path()
                .map(|p| p.to_string_lossy().to_string()),
            "keys": keys,
        });
        let payload_str = payload.to_string();
        let mut tabs: Vec<&Tab> = Vec::new();
        if let Some(root) = self.root.as_ref() {
            root.all_tabs(&mut tabs);
        }
        if let Some(tab) = tabs.into_iter().find(|t| t.id == tab_id) {
            if let Some(sess) = tab.module_session.as_ref() {
                sess.send_config_event(&payload_str);
            }
        }
    }

    /// Map a schema entry to the live Config's current value as a
    /// JSON literal — keeps the mapping in one place so add-a-new-
    /// field also lands a current-value lookup.
    pub(super) fn config_current_value(&self, key: &crate::config::ConfigKey) -> serde_json::Value {
        let c = &self.config;
        let hex3 = |(r, g, b): (u8, u8, u8)| format!("#{r:02x}{g:02x}{b:02x}");
        let hex4 =
            |(r, g, b, a): (u8, u8, u8, u8)| format!("#{r:02x}{g:02x}{b:02x}{a:02x}");
        match key.name {
            "font_family" => serde_json::json!(&c.font_family),
            "font_size" => serde_json::json!(c.font_size),
            "background" => serde_json::json!(hex3(c.background)),
            "show_block_labels" => serde_json::json!(c.show_block_labels),
            "focus_tint" => serde_json::json!(hex4(c.focus_tint)),
            "foreground" => serde_json::json!(hex3(c.foreground)),
            "cursor_color" => serde_json::json!(hex4(c.cursor_color)),
            "selection_color" => serde_json::json!(hex4(c.selection_color)),
            "comms_delivery" => serde_json::json!(c.comms_delivery),
            "padding_left" => serde_json::json!(c.padding.left),
            "padding_right" => serde_json::json!(c.padding.right),
            "padding_top" => serde_json::json!(c.padding.top),
            "padding_bottom" => serde_json::json!(c.padding.bottom),
            "gutter_left" => serde_json::json!(c.gutter_left),
            "gutter_gap" => serde_json::json!(c.gutter_gap),
            "highlight_pad_x" => serde_json::json!(c.highlight_pad_x),
            "highlight_pad_y" => serde_json::json!(c.highlight_pad_y),
            "highlight_offset_y" => serde_json::json!(c.highlight_offset_y),
            "line_height" => serde_json::json!(c.line_height),
            "tab_min_width" => serde_json::json!(c.tab_min_width),
            "tab_max_width" => serde_json::json!(c.tab_max_width),
            "tab_font_size" => serde_json::json!(c.tab_font_size),
            "tab_bar_height" => serde_json::json!(c.tab_bar_height),
            "cursor_blink" => serde_json::json!(c.cursor_blink),
            "bell_style" => serde_json::json!(match c.bell_style {
                crate::config::BellStyle::Visual => "visual",
                crate::config::BellStyle::Silent => "none",
            }),
            "scrollback" => serde_json::json!(c.scrollback),
            _ => serde_json::Value::Null,
        }
    }

    /// Validate + write a config_set request through. Reloads the
    /// in-memory Config and applies hot-reload-eligible changes
    /// immediately; startup-only changes wait for a relaunch (the
    /// schema entry's `hot_reload` flag tells the module which is
    /// which so it can warn the user).
    pub(super) fn apply_config_set(
        &mut self,
        name: &str,
        value: &serde_json::Value,
    ) -> Result<(), String> {
        let schema = crate::config::schema();
        let key = schema
            .iter()
            .find(|k| k.name == name)
            .ok_or_else(|| format!("unknown key `{name}`"))?;
        // Type check against the declared kind. Be permissive on
        // numeric coercion: an int literal is fine for a float field.
        let ok = match key.kind {
            crate::config::ConfigKind::Float => value.is_number(),
            crate::config::ConfigKind::Int => value.is_i64() || value.is_u64(),
            crate::config::ConfigKind::String => value.is_string(),
            crate::config::ConfigKind::Bool => value.is_boolean(),
            crate::config::ConfigKind::Enum => value.is_string(),
        };
        if !ok {
            return Err(format!("type mismatch for `{name}`"));
        }
        let mut doc = crate::config_io::read_document().map_err(|e| e.to_string())?;
        crate::config_io::set_key(&mut doc, name, value)?;
        crate::config_io::write_document(&doc).map_err(|e| e.to_string())?;
        // Reload + apply. `Config::load` already pulls from disk
        // via the same path used here. `apply_live_layout` is the
        // existing hot-reload path the focus handler uses.
        self.config = crate::config::Config::load();
        self.apply_live_layout();
        self.window.request_redraw();
        Ok(())
    }

}
