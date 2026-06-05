//! Color palette and ANSI color resolution.
//!
//! Maps `vte::ansi::Color` (Named / Indexed / Spec) into glyphon's `Color`
//! through a One Dark–family 16-color palette, the standard xterm 6×6×6 cube,
//! and the 24-step grayscale ramp.

use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};
use glyphon::Color;

pub const BACKGROUND_RGB: (u8, u8, u8) = (10, 10, 15);
pub const DEFAULT_FG: (u8, u8, u8) = (220, 220, 220);

/// Sixteen-color palette tuned in the One Dark family. Indices 0–7 are the
/// base ANSI colors, 8–15 the bright variants.
pub const PALETTE_16: [(u8, u8, u8); 16] = [
    (40, 44, 52),
    (224, 108, 117),
    (152, 195, 121),
    (229, 192, 123),
    (97, 175, 239),
    (198, 120, 221),
    (86, 182, 194),
    (171, 178, 191),
    (92, 99, 112),
    (224, 108, 117),
    (152, 195, 121),
    (229, 192, 123),
    (97, 175, 239),
    (198, 120, 221),
    (86, 182, 194),
    (220, 223, 228),
];

const fn half(rgb: (u8, u8, u8)) -> (u8, u8, u8) {
    (rgb.0 / 2, rgb.1 / 2, rgb.2 / 2)
}

pub fn resolve_color(color: AnsiColor, fg: (u8, u8, u8)) -> Color {
    let (r, g, b) = match color {
        AnsiColor::Spec(rgb) => (rgb.r, rgb.g, rgb.b),
        AnsiColor::Named(name) => named_rgb(name, fg),
        AnsiColor::Indexed(idx) => indexed_rgb(idx),
    };
    Color::rgb(r, g, b)
}

pub fn named_rgb(name: NamedColor, fg: (u8, u8, u8)) -> (u8, u8, u8) {
    match name {
        NamedColor::Black => PALETTE_16[0],
        NamedColor::Red => PALETTE_16[1],
        NamedColor::Green => PALETTE_16[2],
        NamedColor::Yellow => PALETTE_16[3],
        NamedColor::Blue => PALETTE_16[4],
        NamedColor::Magenta => PALETTE_16[5],
        NamedColor::Cyan => PALETTE_16[6],
        NamedColor::White => PALETTE_16[7],
        NamedColor::BrightBlack => PALETTE_16[8],
        NamedColor::BrightRed => PALETTE_16[9],
        NamedColor::BrightGreen => PALETTE_16[10],
        NamedColor::BrightYellow => PALETTE_16[11],
        NamedColor::BrightBlue => PALETTE_16[12],
        NamedColor::BrightMagenta => PALETTE_16[13],
        NamedColor::BrightCyan => PALETTE_16[14],
        NamedColor::BrightWhite => PALETTE_16[15],
        NamedColor::DimBlack => half(PALETTE_16[0]),
        NamedColor::DimRed => half(PALETTE_16[1]),
        NamedColor::DimGreen => half(PALETTE_16[2]),
        NamedColor::DimYellow => half(PALETTE_16[3]),
        NamedColor::DimBlue => half(PALETTE_16[4]),
        NamedColor::DimMagenta => half(PALETTE_16[5]),
        NamedColor::DimCyan => half(PALETTE_16[6]),
        NamedColor::DimWhite => half(PALETTE_16[7]),
        NamedColor::DimForeground => half(fg),
        NamedColor::BrightForeground => (255, 255, 255),
        NamedColor::Background => BACKGROUND_RGB,
        NamedColor::Cursor => (255, 200, 80),
        // Foreground and any unlisted name fall back to the configured fg.
        _ => fg,
    }
}

pub fn indexed_rgb(idx: u8) -> (u8, u8, u8) {
    if (idx as usize) < PALETTE_16.len() {
        return PALETTE_16[idx as usize];
    }
    if idx < 232 {
        let levels: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let i = (idx - 16) as usize;
        return (levels[i / 36], levels[(i / 6) % 6], levels[i % 6]);
    }
    let level = 8 + (idx - 232) * 10;
    (level, level, level)
}

/// Halve a color's intensity — used for the cell DIM flag.
pub fn dim_color(c: Color) -> Color {
    Color::rgba(c.r() / 2, c.g() / 2, c.b() / 2, c.a())
}

/// Convert a glyphon Color to GPU-ready floats in [0, 1].
pub fn color_to_floats(c: Color) -> [f32; 4] {
    [
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
        c.a() as f32 / 255.0,
    ]
}
