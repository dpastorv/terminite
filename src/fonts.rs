//! Bundled monospace fonts, embedded in the binary so terminite always has a
//! true fixed-pitch font — and box-drawing glyphs that actually fill the cell —
//! regardless of what the host system has installed. All five are OFL-licensed
//! (license files alongside the ttfs in `assets/fonts/`).

use glyphon::FontSystem;

macro_rules! font_bytes {
    ($p:expr) => {
        include_bytes!(concat!("../assets/fonts/", $p)) as &[u8]
    };
}

/// (family name, ttf bytes). The family name must match the font's internal
/// name table so `Family::Name(..)` resolves to it. Variable-weight files
/// (JetBrains/Fira/Roboto) cover Regular + Bold from one face.
const BUNDLED: &[(&str, &[u8])] = &[
    ("JetBrains Mono", font_bytes!("JetBrainsMono.ttf")),
    ("Fira Code", font_bytes!("FiraCode.ttf")),
    ("DM Mono", font_bytes!("DMMono-Regular.ttf")),
    ("PT Mono", font_bytes!("PTMono-Regular.ttf")),
    ("Roboto Mono", font_bytes!("RobotoMono.ttf")),
];

/// The font terminite ships with by default — a clean fixed-pitch with solid
/// box-drawing, identical on every machine.
pub const DEFAULT_FAMILY: &str = "JetBrains Mono";

/// The bundled family names, in pick order — for live cycling and config help.
pub fn families() -> &'static [&'static str] {
    &["JetBrains Mono", "Fira Code", "DM Mono", "PT Mono", "Roboto Mono"]
}

/// Embed every bundled font into the FontSystem's database. Call once, right
/// after `FontSystem::new()` (which has already pulled in the system fonts).
pub fn load_bundled(font_system: &mut FontSystem) {
    let db = font_system.db_mut();
    for (_, bytes) in BUNDLED {
        db.load_font_data(bytes.to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundled font's bytes parse AND its family name matches what we
    /// advertise — so `Family::Name(<our name>)` actually resolves to it. This
    /// is the check fonttools couldn't give us: if a name is off (e.g. "PTMono"
    /// vs "PT Mono"), this fails instead of silently falling back at runtime.
    #[test]
    fn bundled_families_resolve() {
        let mut fs = FontSystem::new();
        load_bundled(&mut fs);
        let db = fs.db();
        for fam in families() {
            let found = db
                .faces()
                .any(|f| f.families.iter().any(|(n, _)| n == fam));
            assert!(found, "bundled family not found in db: {fam}");
        }
    }
}
