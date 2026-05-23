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
const MIN_FONT_SIZE: f32 = 6.0;
const MAX_FONT_SIZE: f32 = 200.0;
const MAX_PADDING: f32 = 400.0;
const MAX_SCROLLBACK: i64 = 50_000;
const MIN_LINE_HEIGHT: f32 = 0.7;
const MAX_LINE_HEIGHT: f32 = 3.0;

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
    /// Multiplier on the font's natural line height. 1.0 = no change.
    /// Smaller packs lines tighter; larger spreads them out.
    /// Hot-reloaded — each tab's buffer metrics update on focus-gain.
    pub line_height: f32,
    /// Blink the cursor while the window is focused.
    pub cursor_blink: bool,
    /// What the bell does.
    pub bell_style: BellStyle,
    /// Scrollback lines kept per shell. Applied to tabs created after a
    /// reload; existing tabs keep the size they were born with.
    pub scrollback: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: 28.0,
            // Defaults: enough room on the left for a gutter strip wider
            // than `B999` at the chrome font, plus breathing space between
            // the label and the content. Right/top/bottom stay modest.
            padding: Padding { left: 40.0, right: 12.0, top: 8.0, bottom: 8.0 },
            gutter_left: 10.0,
            gutter_gap: 4.0,
            line_height: 1.0,
            cursor_blink: true,
            bell_style: BellStyle::Visual,
            scrollback: 10_000,
        }
    }
}

impl Config {
    /// The standard config path, `~/.config/terminite/config.toml`.
    pub fn path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".config/terminite/config.toml"))
    }

    /// Load from the standard path. A missing file, an unreadable file, or
    /// any unparseable field falls back to the default — never an error.
    pub fn load() -> Self {
        let mut cfg = Config::default();
        if let Some(text) = Config::path().and_then(|p| std::fs::read_to_string(p).ok()) {
            cfg.apply(&text);
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
                "line_height" => {
                    if let Some(n) = val.as_f32().filter(|v| v.is_finite()) {
                        self.line_height = n.clamp(MIN_LINE_HEIGHT, MAX_LINE_HEIGHT);
                    }
                }
                "cursor_blink" => {
                    if let Value::Bool(b) = val {
                        self.cursor_blink = b;
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
        assert_eq!(c.padding.left, 40.0);
        assert_eq!(c.line_height, 1.0);
    }

    #[test]
    fn old_padding_key_is_ignored() {
        // The single `padding` key was dropped — a stale config with it
        // should leave defaults intact rather than half-apply.
        let mut c = Config::default();
        c.apply("padding = 24\n");
        assert_eq!(c.padding.left, 40.0);
        assert_eq!(c.padding.right, 12.0);
    }
}
