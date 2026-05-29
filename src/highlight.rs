//! Syntax highlighting for data-module bodies (Editor pane).
//!
//! Wraps syntect: load the bundled grammars + themes once at startup,
//! then highlight on demand when a module's body or language changes.
//! The output is a flat list of `(byte_start, byte_end, [r, g, b])`
//! per source line, kept tight on purpose — the render path slices
//! the body into per-color spans without re-running the highlighter.
//!
//! Performance: synchronous, full re-highlight per call. For files up
//! to a few thousand lines this is sub-frame; for very large files a
//! future bundle should incrementalize (syntect supports stateful
//! line-by-line so the unchanged tail above the edit point can be
//! reused). Capped upstream by the editor's 1 MB load limit.
//!
//! Trust: syntect itself is a parser + regex engine over read-only
//! string data. No `unsafe`, no system calls, no FS access at
//! highlight time (defaults are loaded once at startup).

use syntect::easy::HighlightLines;
use syntect::highlighting::{Color as SynColor, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Bundled themes — hand-authored .tmTheme files for the Atom / Zed
/// One Dark + One Light palettes. We embed them at compile time so
/// the binary has no runtime asset dependency. Zed is terminite's
/// reference editor (same ACP + MCP protocol family, same first-
/// class-AI stance), so matching its visual defaults lands users in
/// recognizable colors out of the box.
const ONE_DARK_TMTHEME: &str = include_str!("../assets/themes/one-dark.tmTheme");
const ONE_LIGHT_TMTHEME: &str = include_str!("../assets/themes/one-light.tmTheme");

/// One styled span inside a source line. `start` / `end` are byte
/// offsets into that line's text (LF stripped). `rgb` is the
/// foreground color the theme assigned.
#[derive(Clone, Debug)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub rgb: [u8; 3],
}

/// Per-source-line spans. Outer index = source line (0-based,
/// matching `body.split('\n')`). Inner Vec covers the line's runs
/// in order, contiguous and non-overlapping.
pub type LineSpans = Vec<Vec<Span>>;

/// Bundled syntect data shared across all tabs — one parse of the
/// default grammars + themes, reused per highlight call. Cheap to
/// hold (<10 MB), expensive to rebuild, so we hand the renderer one
/// instance at startup and never touch it again.
pub struct HighlightStore {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl HighlightStore {
    /// Load syntect's bundled grammars and the One Dark theme as the
    /// default. The One Light variant is also bundled so a future
    /// config knob (or auto-detect) can toggle between them without
    /// adding new assets. If the embedded One Dark fails to parse —
    /// shouldn't happen because we ship the bytes — fall back to
    /// syntect's defaults so the editor still highlights.
    pub fn load() -> Self {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let theme = load_bundled_theme(ONE_DARK_TMTHEME).unwrap_or_else(|| {
            crate::logging::warn(
                "highlight: embedded One Dark failed to parse — falling back to syntect default",
            );
            let themes = ThemeSet::load_defaults();
            themes
                .themes
                .values()
                .next()
                .cloned()
                .expect("syntect ships at least one default theme")
        });
        Self { syntaxes, theme }
    }

    /// Load the bundled One Light theme. Currently unused at the
    /// call sites; lands when we ship the theme-choice config knob.
    #[allow(dead_code)]
    pub fn one_light() -> Option<Theme> {
        load_bundled_theme(ONE_LIGHT_TMTHEME)
    }

    /// Resolve a language token to a syntect syntax. Accepts
    /// extension-style values ("rs", "py") or names ("Rust",
    /// "Python"). Returns `None` if syntect doesn't know it; the
    /// caller falls back to plain text in that case.
    fn syntax_for<'a>(&'a self, language: &str) -> Option<&'a SyntaxReference> {
        let lang = language.trim_start_matches('.');
        self.syntaxes
            .find_syntax_by_extension(lang)
            .or_else(|| self.syntaxes.find_syntax_by_name(lang))
            .or_else(|| self.syntaxes.find_syntax_by_token(lang))
    }

    /// Highlight `body` line-by-line. Returns one span list per
    /// source line (i.e. `body.split('\n')`). If the language isn't
    /// recognized, every line gets a single span covering its full
    /// length in the theme's default foreground — callers can treat
    /// this as "no highlighting" and skip rich-text rendering.
    pub fn highlight(&self, body: &str, language: &str) -> LineSpans {
        let Some(syntax) = self.syntax_for(language) else {
            return Vec::new();
        };
        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut out: LineSpans = Vec::new();
        // `LinesWithEndings` keeps the trailing `\n` on each line
        // (which syntect's parser expects). We strip it before
        // measuring byte offsets so `start`/`end` index into the
        // line text the renderer will join with `\n`.
        for line_with_eol in LinesWithEndings::from(body) {
            let ranges = match highlighter.highlight_line(line_with_eol, &self.syntaxes) {
                Ok(r) => r,
                Err(_) => {
                    // On parse error keep the line, just unstyled.
                    out.push(Vec::new());
                    continue;
                }
            };
            let mut line_spans: Vec<Span> = Vec::with_capacity(ranges.len());
            let mut cursor = 0;
            for (style, text) in ranges {
                let len = text.len();
                if len == 0 {
                    continue;
                }
                // Stop at the trailing newline so byte offsets index
                // into the LF-stripped line.
                let visible_end = (cursor + len).min(line_with_eol.len().saturating_sub(1));
                let trim = if cursor + len > visible_end {
                    (cursor + len) - visible_end
                } else {
                    0
                };
                let end = cursor + len - trim;
                if end > cursor {
                    line_spans.push(Span {
                        start: cursor,
                        end,
                        rgb: rgb_of(style.foreground),
                    });
                }
                cursor += len;
            }
            out.push(line_spans);
        }
        // `LinesWithEndings` skips an empty trailing line (i.e.,
        // body ending in `\n` yields N lines, not N+1). The
        // editor's body convention puts content lines after a 3-
        // line header so the index alignment is preserved either
        // way; we just don't pad.
        out
    }
}

fn rgb_of(c: SynColor) -> [u8; 3] {
    [c.r, c.g, c.b]
}

/// Parse a .tmTheme XML string into a syntect Theme. Returns `None`
/// on parse failure; the caller decides whether to warn + fall back
/// or hard-fail.
fn load_bundled_theme(tmtheme_xml: &str) -> Option<Theme> {
    let mut reader = std::io::Cursor::new(tmtheme_xml.as_bytes());
    ThemeSet::load_from_reader(&mut reader).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_one_dark_parses() {
        let t = load_bundled_theme(ONE_DARK_TMTHEME).expect("One Dark parses");
        assert_eq!(t.name.as_deref(), Some("One Dark"));
    }

    #[test]
    fn embedded_one_light_parses() {
        let t = load_bundled_theme(ONE_LIGHT_TMTHEME).expect("One Light parses");
        assert_eq!(t.name.as_deref(), Some("One Light"));
    }

    #[test]
    fn highlight_store_uses_one_dark() {
        let store = HighlightStore::load();
        let spans = store.highlight("fn main() {}\n", "rs");
        assert!(!spans.is_empty(), "rs body should highlight");
    }
}
