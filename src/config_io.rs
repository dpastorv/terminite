//! Read-modify-write for `~/.config/terminite/config.toml`.
//!
//! Uses `toml_edit` so user comments / whitespace / key ordering
//! survive a `set` from the config pane. New keys are appended at
//! the bottom of the file; existing keys are mutated in place.
//!
//! Path resolution mirrors `Config::load`: prefers
//! `$XDG_CONFIG_HOME/terminite/config.toml`, else
//! `$HOME/.config/terminite/config.toml`. Returns `None` when
//! neither var is set (treated as "no config file possible" — the
//! config pane will surface the error).

use std::io::Read;
use std::path::PathBuf;
use toml_edit::{value, DocumentMut, Formatted, Item, Value};

/// Hard cap on `config.toml`. ~256 KB is two orders of magnitude past
/// any realistic terminite config; anything larger is almost certainly
/// a runaway editor or a stray cat. We refuse to load past this so a
/// pathological file can't OOM the main thread when the config pane
/// requests a snapshot.
pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;

pub fn config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("terminite");
        p.push("config.toml");
        return Some(p);
    }
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("terminite");
    p.push("config.toml");
    Some(p)
}

/// Read the existing config file as a `DocumentMut`. Returns an
/// empty document if the file is absent (so a fresh install can
/// still write its first key).
pub fn read_document() -> std::io::Result<DocumentMut> {
    let Some(path) = config_path() else {
        return Ok(DocumentMut::new());
    };
    let f = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DocumentMut::new()),
        Err(e) => return Err(e),
    };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    if size > MAX_CONFIG_BYTES {
        return Err(std::io::Error::other(format!(
            "config: refusing to read {} bytes > cap {}",
            size, MAX_CONFIG_BYTES,
        )));
    }
    let mut text = String::with_capacity(size as usize);
    f.take(MAX_CONFIG_BYTES).read_to_string(&mut text)?;
    text.parse::<DocumentMut>().map_err(std::io::Error::other)
}

/// Set one key. `value` is converted to the matching toml type;
/// `Value::Null` is rejected (caller validated already).
pub fn set_key(doc: &mut DocumentMut, name: &str, value: &serde_json::Value) -> Result<(), String> {
    let item = json_to_toml(value).ok_or_else(|| {
        format!("config_set: unsupported value for key `{name}`")
    })?;
    doc[name] = item;
    Ok(())
}

fn json_to_toml(v: &serde_json::Value) -> Option<Item> {
    match v {
        serde_json::Value::Bool(b) => Some(value(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(value(i))
            } else if let Some(f) = n.as_f64() {
                Some(value(f))
            } else {
                None
            }
        }
        serde_json::Value::String(s) => Some(Item::Value(Value::String(Formatted::new(s.clone())))),
        _ => None,
    }
}

/// Atomic write to the same path read_document() used. Creates
/// parent directories as needed.
pub fn write_document(doc: &DocumentMut) -> std::io::Result<()> {
    let Some(path) = config_path() else {
        return Err(std::io::Error::other("no HOME or XDG_CONFIG_HOME"));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = doc.to_string();
    let tmp = path.with_extension("toml.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_comments_on_set() {
        let src = "# top comment\nfont_size = 28\n# trailing\n";
        let mut doc: DocumentMut = src.parse().expect("parse");
        set_key(&mut doc, "font_size", &serde_json::json!(32)).expect("set");
        let out = doc.to_string();
        assert!(out.contains("# top comment"), "lost top comment: {out:?}");
        assert!(out.contains("font_size = 32"), "value not set: {out:?}");
        assert!(out.contains("# trailing"), "lost trailing comment: {out:?}");
    }

    #[test]
    fn appends_missing_key() {
        let src = "font_size = 28\n";
        let mut doc: DocumentMut = src.parse().expect("parse");
        set_key(&mut doc, "cursor_blink", &serde_json::json!(false)).expect("set");
        let out = doc.to_string();
        assert!(out.contains("cursor_blink = false"));
    }
}
