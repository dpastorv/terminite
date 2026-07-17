//! User configuration — a small flat TOML file at
//! `~/.config/terminite/config.toml`, re-read whenever the window regains
//! focus (event-driven, no fs-watch dependency: edit the file in another
//! window, switch back to terminite, it applies).
//!
//! The format is a deliberately flat `key = value` subset of TOML — a
//! handful of scalar fields don't justify pulling in a TOML crate right
//! after a leanness audit. A bad config never crashes the terminal:
//! unknown keys and unparseable values are ignored; missing fields fall
//! back to the defaults.

use std::path::PathBuf;

// Bounds on the numeric fields. A terminal is unusable outside these — and,
// more importantly, `font_size` (via the cell grid) and `scrollback` both
// multiply into the per-shell `Term` allocation. One unbounded value there
// is a single multi-gigabyte allocation that OOMs the machine before the
// per-frame RSS kill switch can react. Every numeric field is clamped.
pub(crate) const MIN_FONT_SIZE: f32 = 6.0;
pub(crate) const MAX_FONT_SIZE: f32 = 200.0;
/// Font weight bounds — the `wght` variation axis of the bundled variable
/// fonts (JetBrains Mono spans 100–800; keep a touch of headroom).
pub(crate) const MIN_FONT_WEIGHT: f32 = 100.0;
pub(crate) const MAX_FONT_WEIGHT: f32 = 900.0;
const MAX_PADDING: f32 = 400.0;
const MAX_SCROLLBACK: i64 = 50_000;
const MIN_LINE_HEIGHT: f32 = 0.7;
const MAX_LINE_HEIGHT: f32 = 3.0;
const MIN_TAB_WIDTH: f32 = 24.0;
const MAX_TAB_WIDTH: f32 = 800.0;
const MIN_TAB_FONT_SIZE: f32 = 8.0;
const MAX_TAB_FONT_SIZE: f32 = 96.0;
const MIN_TAB_BAR_HEIGHT: f32 = 16.0;
const MAX_TAB_BAR_HEIGHT: f32 = 200.0;

/// What `\a` (BEL) does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BellStyle {
    /// A brief background flash.
    Visual,
    /// Nothing at all.
    Silent,
}

/// Per-edge inset from the pane's pixel rect to the text grid. Sets the
/// content rectangle on all four sides independently — Daniel asked for
/// this so the block-label gutter has room to breathe on the left without
/// throwing off the other edges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Padding {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

/// Resolved configuration. Cheap to clone; the renderer holds one and
/// swaps it on reload.
#[derive(Clone)]
pub struct Config {
    /// Monospace font family. Empty means terminite's built-in default.
    /// Startup-applied — changing it needs a relaunch.
    pub font_family: String,
    /// Text size in pixels. Startup-applied.
    pub font_size: f32,
    /// Content font weight (the variable font's `wght` axis): 400 = Regular,
    /// 500–600 = heavier stems (crisper small text on low-DPI screens).
    /// Startup-applied.
    pub font_weight: f32,
    /// Window background colour (RGB). Hot-reloaded on focus-gain — set it as a
    /// hex string in config (`background = "#1a1b26"`), click back in, it applies.
    pub background: (u8, u8, u8),
    /// Draw the `Bn` block-ID labels in the left gutter. Off by default — the
    /// block model is still tracked from OSC 133, but nothing references blocks
    /// yet and the labels' anchors can desync across reflow/focus. Hot-reloaded.
    pub show_block_labels: bool,
    /// A faint tint over the focused pane's content so it's easy to tell which
    /// pane has keyboard focus. `#rrggbbaa` (alpha = strength). Hot-reloaded.
    pub focus_tint: (u8, u8, u8, u8),
    /// Default text colour (`#rrggbb`). Hot-reloaded.
    pub foreground: (u8, u8, u8),
    /// Cursor colour (`#rrggbbaa`). Hot-reloaded.
    pub cursor_color: (u8, u8, u8, u8),
    /// Selection highlight colour (`#rrggbbaa`). Hot-reloaded.
    pub selection_color: (u8, u8, u8, u8),
    /// Treat the Option/Alt key as Meta: `Opt+<char>` sends `ESC`+char
    /// (the readline / zsh convention — `Opt+f`/`b`/`d`/`.` etc.) instead
    /// of typing the macOS special glyph. On by default for shell line
    /// editing; turn off to type accented characters via Option.
    /// Hot-reloaded.
    pub option_as_meta: bool,
    /// Per-edge inset from the pane rect to the text grid. Hot-reloaded
    /// on focus-gain: edit the config in a side pane, click back into
    /// terminite, the new pad takes effect immediately.
    pub padding: Padding,
    /// Block-label inset from the pane's left edge. The label is
    /// right-aligned against the content edge, so this is the minimum
    /// left position it can occupy before clipping. Hot-reloaded.
    pub gutter_left: f32,
    /// Space between the block label's right edge and the start of the
    /// line content. Pushes the label further off the line without
    /// touching the content rect. Hot-reloaded.
    pub gutter_gap: f32,
    /// Horizontal padding on the cursor / tag highlight rect around the
    /// label glyphs. Hot-reloaded.
    pub highlight_pad_x: f32,
    /// Vertical padding on the highlight rect. Hot-reloaded.
    pub highlight_pad_y: f32,
    /// Vertical nudge applied to the highlight rect after padding. Use
    /// to dial in the visual centering since glyph caps don't always
    /// sit dead-center in `LABEL_LINE_H`. Hot-reloaded.
    pub highlight_offset_y: f32,
    /// Multiplier on the font's natural line height. 1.0 = no change.
    /// Smaller packs lines tighter; larger spreads them out.
    /// Hot-reloaded — each tab's buffer metrics update on focus-gain.
    pub line_height: f32,
    /// Per-tab label width clamps inside each pane's tab bar. Tabs shrink
    /// uniformly from `tab_max_width` toward `tab_min_width` as more
    /// open. Hot-reloaded.
    pub tab_min_width: f32,
    pub tab_max_width: f32,
    /// Tab-label text size in pixels. Startup-applied — pre-shapes the
    /// title buffers at this size.
    pub tab_font_size: f32,
    /// Height of each pane's tab bar strip in pixels. Startup-applied;
    /// drives the grid math and where content begins.
    pub tab_bar_height: f32,
    /// Blink the cursor while the window is focused.
    pub cursor_blink: bool,
    /// What the bell does.
    pub bell_style: BellStyle,
    /// Scrollback lines kept per shell. Applied to tabs created after a
    /// reload; existing tabs keep the size they were born with.
    pub scrollback: usize,
    /// The comms base's delivery: when on (default), a directed room message is
    /// PUSHED to its addressee's receiver (the room is alive — agents sharing a
    /// window reach each other). Off = record-only (messages wait in the log
    /// until read), the pre-comms behavior. The human's opt-out.
    pub comms_delivery: bool,

    /// How the PTY floor decides whether to inject a room message into an agent
    /// pane. `"focus"` (default) — the floor holds while the human has focus on
    /// that pane (shared-experience: you work in the pane, don't interrupt).
    /// `"typing"` — only holds while the human is actively typing (older
    /// behavior, lets wakes land while you watch).
    pub inject_policy: String, // "focus" or "typing"
}

/// One row of `schema()` — a known config key with its type, default,
/// hot-reload semantics, and a short description. Used by the proto
/// `config` verb (and the future config pane) to introspect what
/// knobs exist without grepping the source. Keep this in sync when
/// adding a new field above; the test enforces a presence baseline.
#[allow(dead_code)]
pub struct ConfigKey {
    pub name: &'static str,
    pub kind: ConfigKind,
    pub default: ConfigValue,
    pub hot_reload: bool,
    pub doc: &'static str,
}

#[derive(Clone, Copy)]
pub enum ConfigKind {
    Float,
    Int,
    String,
    Bool,
    Enum,
}

#[derive(Clone)]
#[allow(dead_code)]
pub enum ConfigValue {
    Float(f32),
    Int(i64),
    String(&'static str),
    Bool(bool),
}

/// Return the full set of known config keys. The order matches the
/// struct fields above. Stable: callers (proto verb, config pane)
/// iterate this list and trust the names. Currently unused at the
/// call sites; the next bundle (config pane) wires it through proto.
#[allow(dead_code)]
pub fn schema() -> Vec<ConfigKey> {
    let k = |name, kind, default, hot_reload, doc| ConfigKey {
        name, kind, default, hot_reload, doc,
    };
    vec![
        k("font_family", ConfigKind::String,
          ConfigValue::String(crate::fonts::DEFAULT_FAMILY), false,
          "Monospace family. Bundled: JetBrains Mono, Fira Code, DM Mono, PT Mono, Roboto Mono. Empty = platform default."),
        k("font_size", ConfigKind::Float, ConfigValue::Float(28.0), false,
          "Content font size in pixels. Startup-applied."),
        k("font_weight", ConfigKind::Float, ConfigValue::Float(400.0), false,
          "Content font weight (100–900). 400 = Regular; try 500–600 for crisper text on low-DPI monitors. Startup-applied."),
        k("background", ConfigKind::String, ConfigValue::String("#0a0a0f"), true,
          "Window background colour, hex (#rrggbb). Hot-reloaded."),
        k("show_block_labels", ConfigKind::Bool, ConfigValue::Bool(false), true,
          "Draw the Bn block-ID labels in the left gutter. Hot-reloaded."),
        k("focus_tint", ConfigKind::String, ConfigValue::String("#ffffff01"), true,
          "Tint over the focused pane, hex #rrggbbaa (alpha = strength). Hot-reloaded."),
        k("foreground", ConfigKind::String, ConfigValue::String("#dcdcdc"), true,
          "Default text colour, hex #rrggbb. Hot-reloaded."),
        k("cursor_color", ConfigKind::String, ConfigValue::String("#ffc850b4"), true,
          "Cursor colour, hex #rrggbbaa. Hot-reloaded."),
        k("selection_color", ConfigKind::String, ConfigValue::String("#5275bf59"), true,
          "Selection highlight colour, hex #rrggbbaa. Hot-reloaded."),
        k("option_as_meta", ConfigKind::Bool, ConfigValue::Bool(true), true,
          "Treat Option/Alt as Meta: Opt+<char> sends ESC+char (readline Opt+f/b/d/.) instead of a special glyph. Off = type accented chars. Hot-reloaded."),
        k("padding_left", ConfigKind::Float, ConfigValue::Float(55.0), true,
          "Inset between the pane edge and content (left)."),
        k("padding_right", ConfigKind::Float, ConfigValue::Float(24.0), true,
          "Inset between the pane edge and content (right)."),
        k("padding_top", ConfigKind::Float, ConfigValue::Float(16.0), true,
          "Inset between the pane edge and content (top)."),
        k("padding_bottom", ConfigKind::Float, ConfigValue::Float(16.0), true,
          "Inset between the pane edge and content (bottom)."),
        k("gutter_left", ConfigKind::Float, ConfigValue::Float(8.0), true,
          "Left edge of the block-label gutter strip."),
        k("gutter_gap", ConfigKind::Float, ConfigValue::Float(8.0), true,
          "Gap between gutter labels and content."),
        k("highlight_pad_x", ConfigKind::Float, ConfigValue::Float(4.0), true,
          "Horizontal pad around block-label highlight rects."),
        k("highlight_pad_y", ConfigKind::Float, ConfigValue::Float(2.0), true,
          "Vertical pad around block-label highlight rects."),
        k("highlight_offset_y", ConfigKind::Float, ConfigValue::Float(0.0), true,
          "Y-offset for block-label highlight (fine-tune)."),
        k("line_height", ConfigKind::Float, ConfigValue::Float(1.0), true,
          "Multiplier on the cell line height."),
        k("tab_min_width", ConfigKind::Float, ConfigValue::Float(140.0), true,
          "Minimum per-tab label width (px)."),
        k("tab_max_width", ConfigKind::Float, ConfigValue::Float(360.0), true,
          "Maximum per-tab label width (px)."),
        k("tab_font_size", ConfigKind::Float, ConfigValue::Float(18.0), false,
          "Tab-label font size in pixels. Startup-applied."),
        k("tab_bar_height", ConfigKind::Float, ConfigValue::Float(44.0), false,
          "Per-pane tab-bar strip height in pixels."),
        k("cursor_blink", ConfigKind::Bool, ConfigValue::Bool(true), true,
          "Blink the cursor while the window is focused."),
        k("bell_style", ConfigKind::Enum, ConfigValue::String("visual"), true,
          "How to handle \\a (bell). One of: visual, none."),
        k("scrollback", ConfigKind::Int, ConfigValue::Int(10_000), false,
          "Scrollback lines per shell. Applied to new tabs only."),
        k("comms_delivery", ConfigKind::Bool, ConfigValue::Bool(true), true,
          "Push directed room messages to their addressee (the room is alive). \
           Off = record-only."),
        k("inject_policy", ConfigKind::String, ConfigValue::String("focus"), true,
          "When the PTY floor injects a room message into an agent pane. \
           \"focus\" = hold while the human has focus on that pane (default). \
           \"typing\" = only hold while actively typing (older behavior, lets \
           wakes land while you watch)."),
    ]
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: crate::fonts::DEFAULT_FAMILY.to_string(),
            font_size: 28.0,
            font_weight: 400.0,
            background: crate::palette::BACKGROUND_RGB,
            show_block_labels: false,
            focus_tint: (255, 255, 255, 1),
            foreground: crate::palette::DEFAULT_FG,
            cursor_color: (255, 200, 80, 180),
            selection_color: (82, 117, 191, 89),
            option_as_meta: true,
            // Defaults dialed in via the hot-reload loop — Daniel's
            // tuned values land more breathing room around the content
            // and a noticeable gap between the block label and the line.
            padding: Padding { left: 55.0, right: 24.0, top: 16.0, bottom: 16.0 },
            gutter_left: 8.0,
            gutter_gap: 8.0,
            highlight_pad_x: 4.0,
            highlight_pad_y: 2.0,
            highlight_offset_y: 0.0,
            line_height: 1.0,
            tab_min_width: 140.0,
            tab_max_width: 360.0,
            tab_font_size: 18.0,
            tab_bar_height: 44.0,
            cursor_blink: true,
            bell_style: BellStyle::Visual,
            scrollback: 10_000,
            comms_delivery: true,
            inject_policy: "focus".to_string(), // hold while focused on the pane
        }
    }
}

impl Config {
    /// The config path — the single resolver shared with the config-pane
    /// writer, so the renderer reads exactly the file the pane edits.
    /// `$XDG_CONFIG_HOME/terminite/config.toml` if set, else
    /// `~/.config/terminite/config.toml`. (Previously the loader was
    /// `$HOME`-only while the writer honored XDG — they could diverge.)
    pub fn path() -> Option<PathBuf> {
        crate::config_io::config_path()
    }

    /// Load from the standard path. A missing file, an unreadable file, or
    /// any unparseable field falls back to the default — never an error.
    pub fn load() -> Self {
        let mut cfg = Config::default();
        if let Some(path) = Config::path() {
            if path.exists() {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    cfg.apply(&text);
                }
            } else {
                // First run: write a fully self-documenting config so the file
                // explains every available key + default, instead of leaving the
                // user to discover them from the source.
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, documented_default());
            }
        }
        cfg
    }

    fn apply(&mut self, text: &str) {
        for (key, val) in parse_flat_toml(text) {
            match key.as_str() {
                "font_family" => {
                    if let Value::Str(s) = &val {
                        self.font_family = s.clone();
                    }
                }
                "font_size" => {
                    if let Some(n) = val.as_f32() {
                        if n.is_finite() {
                            self.font_size = n.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                        }
                    }
                }
                "font_weight" => {
                    if let Some(n) = val.as_f32() {
                        if n.is_finite() {
                            self.font_weight = n.clamp(MIN_FONT_WEIGHT, MAX_FONT_WEIGHT);
                        }
                    }
                }
                "background" => {
                    if let Value::Str(s) = &val {
                        if let Some(rgb) = parse_hex_color(s) {
                            self.background = rgb;
                        }
                    }
                }
                "padding_left" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.padding.left = n.clamp(0.0, MAX_PADDING);
                    }
                }
                "padding_right" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.padding.right = n.clamp(0.0, MAX_PADDING);
                    }
                }
                "padding_top" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.padding.top = n.clamp(0.0, MAX_PADDING);
                    }
                }
                "padding_bottom" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.padding.bottom = n.clamp(0.0, MAX_PADDING);
                    }
                }
                "gutter_left" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.gutter_left = n.clamp(0.0, MAX_PADDING);
                    }
                }
                "gutter_gap" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.gutter_gap = n.clamp(0.0, MAX_PADDING);
                    }
                }
                "highlight_pad_x" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.highlight_pad_x = n.clamp(0.0, MAX_PADDING);
                    }
                }
                "highlight_pad_y" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.highlight_pad_y = n.clamp(0.0, MAX_PADDING);
                    }
                }
                "highlight_offset_y" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        // Signed nudge; clamp to a small range so a
                        // bad value can't pull the rect off-screen.
                        self.highlight_offset_y = n.clamp(-100.0, 100.0);
                    }
                }
                "line_height" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.line_height = n.clamp(MIN_LINE_HEIGHT, MAX_LINE_HEIGHT);
                    }
                }
                "tab_min_width" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.tab_min_width = n.clamp(MIN_TAB_WIDTH, MAX_TAB_WIDTH);
                    }
                }
                "tab_max_width" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.tab_max_width = n.clamp(MIN_TAB_WIDTH, MAX_TAB_WIDTH);
                    }
                }
                "tab_font_size" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.tab_font_size =
                            n.clamp(MIN_TAB_FONT_SIZE, MAX_TAB_FONT_SIZE);
                    }
                }
                "tab_bar_height" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.tab_bar_height =
                            n.clamp(MIN_TAB_BAR_HEIGHT, MAX_TAB_BAR_HEIGHT);
                    }
                }
                "cursor_blink" => {
                    if let Value::Bool(b) = val {
                        self.cursor_blink = b;
                    }
                }
                "show_block_labels" => {
                    if let Value::Bool(b) = val {
                        self.show_block_labels = b;
                    }
                }
                "option_as_meta" => {
                    if let Value::Bool(b) = val {
                        self.option_as_meta = b;
                    }
                }
                "focus_tint" => {
                    if let Value::Str(s) = &val {
                        if let Some(rgba) = parse_hex_rgba(s) {
                            self.focus_tint = rgba;
                        }
                    }
                }
                "foreground" => {
                    if let Value::Str(s) = &val {
                        if let Some(rgb) = parse_hex_color(s) {
                            self.foreground = rgb;
                        }
                    }
                }
                "cursor_color" => {
                    if let Value::Str(s) = &val {
                        if let Some(rgba) = parse_hex_rgba(s) {
                            self.cursor_color = rgba;
                        }
                    }
                }
                "selection_color" => {
                    if let Value::Str(s) = &val {
                        if let Some(rgba) = parse_hex_rgba(s) {
                            self.selection_color = rgba;
                        }
                    }
                }
                "comms_delivery" => {
                    if let Value::Bool(b) = val {
                        self.comms_delivery = b;
                    }
                }
                "inject_policy" => {
                    if let Value::Str(s) = &val {
                        let s = s.trim().to_lowercase();
                        if s == "focus" || s == "typing" {
                            self.inject_policy = s;
                        }
                    }
                }
                "bell_style" => {
                    if let Value::Str(s) = &val {
                        self.bell_style = match s.as_str() {
                            "none" | "silent" => BellStyle::Silent,
                            // "audible" isn't implemented yet — fall back
                            // to the visual bell rather than going silent.
                            _ => BellStyle::Visual,
                        };
                    }
                }
                "scrollback" => {
                    if let Value::Int(n) = val {
                        self.scrollback = n.clamp(0, MAX_SCROLLBACK) as usize;
                    }
                }
                _ => {}
            }
        }
    }
}

/// A scalar value from the flat-TOML parse.
enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Value {
    /// A numeric value as `f32`, accepting either an integer or a float.
    fn as_f32(&self) -> Option<f32> {
        match self {
            Value::Int(i) => Some(*i as f32),
            Value::Float(f) => Some(*f as f32),
            _ => None,
        }
    }
}

/// Parse a flat `key = value` subset of TOML: no tables, no arrays. `#`
/// starts a comment; values may be double/single-quoted strings,
/// integers, or `true` / `false`.
/// Parse a `#rrggbb` (or bare `rrggbb`) hex colour. Returns None on anything
/// malformed — the caller keeps the prior/default colour.
fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ))
}

/// Render a self-documenting default config from `schema()` — every key with
/// its description, hot-reload note, and default value. Written on first run and
/// printed by `terminite config`, so the available knobs are discoverable
/// without grepping the source.
pub fn documented_default() -> String {
    let mut out = String::from(
        "# terminite config. Each key below is shown COMMENTED with its default.\n\
         # To change one: uncomment its line (remove the `# ` before the key) and\n\
         # edit the value — or just add `key = value` anywhere. Your line always\n\
         # wins; the commented defaults are only documentation.\n\
         # [hot-reload] applies when you click back into terminite; [startup]\n\
         # needs a relaunch. Bad values are ignored.\n\n",
    );
    for key in schema() {
        let when = if key.hot_reload { "hot-reload" } else { "startup" };
        let val = match key.default {
            ConfigValue::Float(f) => format!("{f:?}"),
            ConfigValue::Int(i) => format!("{i}"),
            ConfigValue::Bool(b) => format!("{b}"),
            ConfigValue::String(s) => format!("\"{s}\""),
        };
        // Default shown commented — documentation, not an active assignment, so
        // a user's own line (anywhere) is never overridden by the default below.
        out.push_str(&format!("# {}  [{when}]\n# {} = {val}\n\n", key.doc, key.name));
    }
    out
}

/// Parse a `#rrggbbaa` (or bare `rrggbbaa`) hex colour with alpha. Returns None
/// on anything malformed.
fn parse_hex_rgba(s: &str) -> Option<(u8, u8, u8, u8)> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 8 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
        u8::from_str_radix(&h[6..8], 16).ok()?,
    ))
}

fn parse_flat_toml(text: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let key = line[..eq].trim().to_string();
        if key.is_empty() {
            continue;
        }
        if let Some(val) = parse_value(line[eq + 1..].trim()) {
            out.push((key, val));
        }
    }
    out
}

fn parse_value(rhs: &str) -> Option<Value> {
    // Quoted string — double or single.
    for q in ['"', '\''] {
        if let Some(rest) = rhs.strip_prefix(q) {
            let end = rest.find(q)?;
            return Some(Value::Str(rest[..end].to_string()));
        }
    }
    // Bare value — drop any trailing `# comment`.
    match rhs.split('#').next().unwrap_or("").trim() {
        "" => None,
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        bare => bare
            .parse::<i64>()
            .ok()
            .map(Value::Int)
            .or_else(|| bare.parse::<f64>().ok().map(Value::Float)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_lists_known_keys() {
        // Presence baseline — bumping when a new config key is added
        // is the reminder to also extend schema(). The exact set isn't
        // load-bearing, only that the key list grows monotonically.
        let s = schema();
        let names: std::collections::HashSet<&str> =
            s.iter().map(|k| k.name).collect();
        for expected in [
            "font_size", "cursor_blink", "bell_style", "scrollback",
            "tab_font_size", "tab_bar_height", "line_height",
        ] {
            assert!(names.contains(expected), "schema missing: {expected}");
        }
    }

    #[test]
    fn defaults_when_absent() {
        let mut c = Config::default();
        c.apply("");
        assert!(c.cursor_blink);
        assert_eq!(c.bell_style, BellStyle::Visual);
        assert_eq!(c.scrollback, 10_000);
    }

    #[test]
    fn parses_fields_and_ignores_junk() {
        let mut c = Config::default();
        c.apply(
            "# a comment\n\
             cursor_blink = false\n\
             bell_style = \"none\"   # trailing comment\n\
             scrollback = 5000\n\
             unknown_key = 42\n\
             malformed line without equals\n",
        );
        assert!(!c.cursor_blink);
        assert_eq!(c.bell_style, BellStyle::Silent);
        assert_eq!(c.scrollback, 5000);
    }

    #[test]
    fn bad_values_keep_defaults() {
        let mut c = Config::default();
        c.apply("cursor_blink = maybe\nscrollback = lots\n");
        assert!(c.cursor_blink);
        assert_eq!(c.scrollback, 10_000);
    }

    #[test]
    fn audible_falls_back_to_visual() {
        let mut c = Config::default();
        c.apply("bell_style = 'audible'\n");
        assert_eq!(c.bell_style, BellStyle::Visual);
    }

    #[test]
    fn metric_fields() {
        let mut c = Config::default();
        c.apply(
            "font_family = \"JetBrains Mono\"\n\
             font_size = 16\n\
             padding_left = 18.5\n\
             padding_right = 6\n\
             padding_top = 4\n\
             padding_bottom = 4\n\
             gutter_left = 8\n\
             gutter_gap = 6\n\
             highlight_pad_x = 5\n\
             highlight_pad_y = 3\n\
             highlight_offset_y = -2\n\
             line_height = 1.25\n",
        );
        assert_eq!(c.font_family, "JetBrains Mono");
        assert_eq!(c.font_size, 16.0); // integer accepted as a float
        assert_eq!(c.padding.left, 18.5);
        assert_eq!(c.padding.right, 6.0);
        assert_eq!(c.padding.top, 4.0);
        assert_eq!(c.padding.bottom, 4.0);
        assert_eq!(c.gutter_left, 8.0);
        assert_eq!(c.gutter_gap, 6.0);
        assert_eq!(c.highlight_pad_x, 5.0);
        assert_eq!(c.highlight_pad_y, 3.0);
        assert_eq!(c.highlight_offset_y, -2.0);
        assert_eq!(c.line_height, 1.25);
    }

    #[test]
    fn out_of_range_metrics_are_clamped() {
        // Out-of-range numbers clamp to the safe bounds — they can't be
        // allowed to drive the Term grid allocation to OOM.
        let mut c = Config::default();
        c.apply(
            "font_size = 2\nscrollback = 9999999\n\
             padding_left = 99999\nline_height = 0.1\n",
        );
        assert_eq!(c.font_size, 6.0);
        assert_eq!(c.scrollback, 50_000);
        assert_eq!(c.padding.left, 400.0);
        assert_eq!(c.line_height, 0.7);
        // A non-numeric value is ignored entirely — default kept.
        let mut c = Config::default();
        c.apply("padding_left = huge\nline_height = wide\n");
        assert_eq!(c.padding.left, 55.0);
        assert_eq!(c.line_height, 1.0);
    }

    #[test]
    fn old_padding_key_is_ignored() {
        // The single `padding` key was dropped — a stale config with it
        // should leave defaults intact rather than half-apply.
        let mut c = Config::default();
        c.apply("padding = 24\n");
        assert_eq!(c.padding.left, 55.0);
        assert_eq!(c.padding.right, 24.0);
    }
}
