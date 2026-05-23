//! Kitty graphics protocol — parsing and decoding.
//!
//! An APC payload starting with `G` is a Kitty command:
//! `G<key>=<value>,<key>=<value>,...;<base64-encoded-data>`
//!
//! v1 supports the headline operations: transmit, display, and the
//! combined transmit+display, in PNG, RGBA, or RGB. Chunked transmission
//! (`m=1`), placement (`x/y/w/h/X/Y/z`), and cell sizing (`c/r`) are
//! parsed but not yet acted on by the renderer.
//!
//! Everything here is pure data — no GPU, no storage, no side effects —
//! so the parsing/decoding is unit-tested without a terminal running.
//! System-impact bounds: per-image decoded bytes are capped at
//! [`MAX_IMAGE_BYTES`]; APC bytes themselves are capped upstream by vte.
//!
//! References: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use base64::Engine;
use base64::prelude::BASE64_STANDARD;

/// Decoded-image size cap: a single image can't exceed this many bytes of
/// RGBA in memory. Keeps one rogue image from OOMing the renderer.
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// The Kitty action — what to do with the payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Transmit only — store the image; display it later via a placement.
    Transmit,
    /// Display a previously-transmitted image.
    Display,
    /// Transmit and display in one go — the most common case.
    TransmitDisplay,
    /// Delete (subset thereof; the spec is rich here, we keep it simple).
    Delete,
    /// Query — the shell is asking what we support.
    Query,
}

/// The pixel format of the transmitted payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// `f=100`: PNG bytes (we decode).
    Png,
    /// `f=24`: raw RGB pixels; `width`/`height` must be set.
    Rgb,
    /// `f=32`: raw RGBA pixels; `width`/`height` must be set.
    Rgba,
}

/// A parsed Kitty command. Only the v1 keys are surfaced; the rest is
/// dropped silently so unknown options don't break the parse. `id` and
/// `more_chunks` are parsed now but not yet used (display-by-id and
/// chunked transmission are later commits).
#[derive(Clone, Debug)]
pub struct KittyCommand {
    pub action: Action,
    pub format: Format,
    #[allow(dead_code)]
    pub id: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[allow(dead_code)]
    pub more_chunks: bool,
    pub payload: Vec<u8>,
}

/// Decoded image — tightly packed RGBA, top-left origin.
#[derive(Clone, Debug)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, RGBA8.
    pub rgba: Vec<u8>,
}

impl ImageData {
    /// Decoded byte cost — used by image-store eviction (later commit).
    #[allow(dead_code)]
    pub fn bytes(&self) -> usize {
        self.rgba.len()
    }
}

/// Parse an APC payload as a Kitty command. Returns `None` for payloads
/// that aren't Kitty (don't start with `G`) or that are malformed.
pub fn parse_kitty(apc: &[u8]) -> Option<KittyCommand> {
    let rest = apc.strip_prefix(b"G")?;
    // The control segment ends at the first `;`; everything after is the
    // base64 payload. A command with no payload (e.g. Display by id, or
    // Query) has no `;`.
    let (control, payload_b64) = match memchr_first(rest, b';') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &b""[..]),
    };

    // Defaults that match the Kitty spec.
    let mut action = Action::TransmitDisplay;
    let mut format = Format::Rgba;
    let mut id: Option<u32> = None;
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;
    let mut more_chunks = false;
    let mut had_action = false;
    let mut had_format = false;

    for pair in control.split(|&b| b == b',') {
        if pair.is_empty() {
            continue;
        }
        let eq = memchr_first(pair, b'=')?;
        let key = &pair[..eq];
        let val = &pair[eq + 1..];
        // Single-byte keys only in the v1 subset.
        if key.len() != 1 {
            continue;
        }
        match key[0] {
            b'a' => {
                had_action = true;
                action = match val {
                    b"t" => Action::Transmit,
                    b"T" => Action::TransmitDisplay,
                    b"p" => Action::Display,
                    b"d" => Action::Delete,
                    b"q" => Action::Query,
                    _ => return None,
                };
            }
            b'f' => {
                had_format = true;
                format = match val {
                    b"100" => Format::Png,
                    b"32" => Format::Rgba,
                    b"24" => Format::Rgb,
                    _ => return None,
                };
            }
            b'i' => id = parse_u32(val),
            b's' => width = parse_u32(val),
            b'v' => height = parse_u32(val),
            b'm' => more_chunks = val == b"1",
            _ => {
                // Unknown / unsupported v1 key — silently ignored so a
                // sender's display-control options don't fail the parse.
            }
        }
    }
    let _ = (had_action, had_format); // both have safe defaults

    let payload = if payload_b64.is_empty() {
        Vec::new()
    } else {
        // Tolerate whitespace: macOS `base64` line-wraps by default and
        // shells happily slice newlines into payloads. The Kitty spec
        // doesn't allow whitespace, but in practice it's everywhere.
        let cleaned: Vec<u8> = payload_b64
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        BASE64_STANDARD.decode(&cleaned).ok()?
    };

    Some(KittyCommand {
        action,
        format,
        id,
        width,
        height,
        more_chunks,
        payload,
    })
}

/// Decode a Kitty payload to RGBA pixels. Returns `None` on a bad payload
/// or one that would exceed [`MAX_IMAGE_BYTES`] decoded.
pub fn decode_image(
    format: Format,
    width: Option<u32>,
    height: Option<u32>,
    payload: &[u8],
) -> Option<ImageData> {
    match format {
        Format::Png => decode_png(payload),
        Format::Rgba => {
            let (w, h) = (width?, height?);
            let need = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
            if need != payload.len() || need > MAX_IMAGE_BYTES {
                return None;
            }
            Some(ImageData { width: w, height: h, rgba: payload.to_vec() })
        }
        Format::Rgb => {
            let (w, h) = (width?, height?);
            let need = (w as usize).checked_mul(h as usize)?.checked_mul(3)?;
            if need != payload.len() {
                return None;
            }
            let pixels = (w as usize).checked_mul(h as usize)?;
            if pixels.checked_mul(4)? > MAX_IMAGE_BYTES {
                return None;
            }
            let mut rgba = Vec::with_capacity(pixels * 4);
            for chunk in payload.chunks_exact(3) {
                rgba.extend_from_slice(chunk);
                rgba.push(0xFF);
            }
            Some(ImageData { width: w, height: h, rgba })
        }
    }
}

fn decode_png(data: &[u8]) -> Option<ImageData> {
    let decoder = png::Decoder::new(data);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    let w = info.width;
    let h = info.height;
    // Reject huge images before allocating.
    let need_rgba = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    if need_rgba > MAX_IMAGE_BYTES {
        return None;
    }
    let buf_size = reader.output_buffer_size();
    let mut buf = vec![0u8; buf_size];
    let frame = reader.next_frame(&mut buf).ok()?;
    buf.truncate(frame.buffer_size());
    let rgba = match frame.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(need_rgba);
            for chunk in buf.chunks_exact(3) {
                out.extend_from_slice(chunk);
                out.push(0xFF);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(need_rgba);
            for chunk in buf.chunks_exact(2) {
                let g = chunk[0];
                out.extend_from_slice(&[g, g, g, chunk[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(need_rgba);
            for &g in buf.iter() {
                out.extend_from_slice(&[g, g, g, 0xFF]);
            }
            out
        }
        // Indexed PNGs need a palette expansion the `png` crate's basic
        // path doesn't apply for us; treat as unsupported for v1.
        png::ColorType::Indexed => return None,
    };
    Some(ImageData { width: w, height: h, rgba })
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    std::str::from_utf8(s).ok()?.parse::<u32>().ok()
}

fn memchr_first(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_kitty() {
        assert!(parse_kitty(b"").is_none());
        assert!(parse_kitty(b"hello").is_none());
        // iTerm2 inline-image OSC payload, not Kitty.
        assert!(parse_kitty(b"]1337;File=").is_none());
    }

    #[test]
    fn parses_transmit_display_png() {
        // a=T (default), f=100 (PNG), tiny base64-encoded "PNG?" stand-in.
        let cmd = parse_kitty(b"Gf=100,a=T;UE5HPw==").unwrap();
        assert_eq!(cmd.action, Action::TransmitDisplay);
        assert_eq!(cmd.format, Format::Png);
        assert_eq!(cmd.payload, b"PNG?");
        assert_eq!(cmd.id, None);
    }

    #[test]
    fn defaults_when_keys_missing() {
        // No keys → defaults: TransmitDisplay + RGBA.
        let cmd = parse_kitty(b"G;").unwrap();
        assert_eq!(cmd.action, Action::TransmitDisplay);
        assert_eq!(cmd.format, Format::Rgba);
        assert!(cmd.payload.is_empty());
    }

    #[test]
    fn parses_ids_and_dims() {
        let cmd = parse_kitty(b"Ga=T,f=32,i=42,s=4,v=2,m=1;").unwrap();
        assert_eq!(cmd.action, Action::TransmitDisplay);
        assert_eq!(cmd.format, Format::Rgba);
        assert_eq!(cmd.id, Some(42));
        assert_eq!(cmd.width, Some(4));
        assert_eq!(cmd.height, Some(2));
        assert!(cmd.more_chunks);
    }

    #[test]
    fn rejects_bad_action_or_format() {
        assert!(parse_kitty(b"Ga=zzz;").is_none());
        assert!(parse_kitty(b"Gf=999;").is_none());
    }

    #[test]
    fn rejects_bad_base64() {
        assert!(parse_kitty(b"Ga=T,f=100;not-base64!!!").is_none());
    }

    #[test]
    fn tolerates_whitespace_in_base64() {
        // macOS `base64` wraps at 76 chars; the payload may include
        // newlines. Both should still decode to the same bytes.
        let no_ws = parse_kitty(b"Gf=100,a=T;UE5HPw==").unwrap();
        let with_nl = parse_kitty(b"Gf=100,a=T;UE5H\nPw==").unwrap();
        let with_spaces = parse_kitty(b"Gf=100,a=T;UE5H Pw==").unwrap();
        assert_eq!(no_ws.payload, b"PNG?");
        assert_eq!(with_nl.payload, b"PNG?");
        assert_eq!(with_spaces.payload, b"PNG?");
    }

    #[test]
    fn decode_rgba_round_trip() {
        // 2x1 RGBA: red, green pixels.
        let pixels = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let img = decode_image(Format::Rgba, Some(2), Some(1), &pixels).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 1);
        assert_eq!(img.rgba, pixels);
    }

    #[test]
    fn decode_rgb_expands_alpha() {
        // 1x1 RGB blue → RGBA with alpha 0xFF.
        let img = decode_image(Format::Rgb, Some(1), Some(1), &[0, 0, 255]).unwrap();
        assert_eq!(img.rgba, vec![0, 0, 255, 0xFF]);
    }

    #[test]
    fn decode_rejects_size_mismatch() {
        // Claims 4x1 RGBA (16 bytes) but only 8 bytes supplied.
        assert!(decode_image(Format::Rgba, Some(4), Some(1), &[0; 8]).is_none());
    }

    #[test]
    fn decode_rejects_oversize() {
        // 4096x4096 RGBA would be 64 MB — past MAX_IMAGE_BYTES.
        let big: usize = 4096 * 4096 * 4;
        assert!(big > MAX_IMAGE_BYTES);
        assert!(decode_image(Format::Rgba, Some(4096), Some(4096), &vec![0; big]).is_none());
    }
}
