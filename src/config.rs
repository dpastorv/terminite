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

/// What `\a` (BEL) does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BellStyle {
    /// A brief background flash.
    Visual,
    /// Nothing at all.
    Silent,
}

/// Resolved configuration. Cheap to clone; the renderer holds one and
/// swaps it on reload.
#[derive(Clone)]
pub struct Config {
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
                        self.scrollback = n.max(0) as usize;
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
    Bool(bool),
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
        bare => bare.parse::<i64>().ok().map(Value::Int),
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
}
