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
    /// Load syntect's bundled grammars and pick a default theme.
    /// We use "Solarized (dark)" for readability + decent contrast
    /// on terminite's dark background; users can drop in their own
    /// `.tmTheme` files via a future config knob.
    pub fn load() -> Self {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        // Falls back to the first available theme if the named one
        // isn't bundled. syntect ships ~5 by default; "Solarized
        // (dark)" is one of them.
        let theme = themes
            .themes
            .get("Solarized (dark)")
            .or_else(|| themes.themes.get("base16-ocean.dark"))
            .or_else(|| themes.themes.values().next())
            .cloned()
            .expect("syntect ships at least one default theme");
        Self { syntaxes, theme }
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
