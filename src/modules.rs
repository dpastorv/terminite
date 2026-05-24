//! Module registry. A *module* is an out-of-process program terminite
//! hosts as the content of a pane. Step 2a of Bundle 6 lands the
//! manifest format + discovery + listing; step 2b spawns the process
//! and wires the IPC channel.
//!
//! Layout on disk:
//!
//! ```
//! ~/.terminite/modules/<id>/manifest.toml   # required, declares the module
//! ~/.terminite/modules/<id>/...             # the module's own files
//! ```
//!
//! The directory name is the module's `id` — short, kebab-case, what
//! the dropdown shows. Override the modules dir with
//! `$TERMINITE_MODULES_DIR`.

use std::path::PathBuf;

use serde::Serialize;

/// One module's parsed manifest.
#[derive(Clone, Debug, Serialize)]
pub struct ModuleManifest {
    /// The directory name (the module's identifier in the registry).
    pub id: String,
    /// Human-readable name shown in the dropdown.
    pub name: String,
    /// Free-form version string.
    pub version: String,
    /// Absolute path to the executable terminite will spawn. Step 2b
    /// uses this; step 2a just surfaces it for `module list`.
    pub binary: PathBuf,
    /// Optional one-line summary.
    pub description: String,
}

/// All discovered modules. Built at startup; rebuilt on demand.
#[derive(Default, Clone)]
pub struct Registry {
    manifests: Vec<ModuleManifest>,
}

impl Registry {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Scan `~/.terminite/modules/` (or `$TERMINITE_MODULES_DIR`) and
    /// return a fresh registry. Malformed manifests are skipped with a
    /// log; the rest still register. Never errors — a missing modules
    /// dir simply returns an empty registry.
    pub fn discover() -> Self {
        let Some(dir) = modules_dir() else {
            return Self::empty();
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Self::empty();
        };
        let mut manifests = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let id = match path.file_name().and_then(|n| n.to_str()) {
                Some(s) if !s.starts_with('.') => s.to_string(),
                _ => continue,
            };
            let manifest_path = path.join("manifest.toml");
            let Ok(text) = std::fs::read_to_string(&manifest_path) else {
                crate::logging::warn(&format!(
                    "module {id}: manifest.toml missing or unreadable, skipping"
                ));
                continue;
            };
            match parse_manifest(&id, &path, &text) {
                Ok(m) => manifests.push(m),
                Err(e) => {
                    crate::logging::warn(&format!(
                        "module {id}: manifest parse failed ({e}), skipping"
                    ));
                }
            }
        }
        // Stable, alphabetical order — the dropdown reads predictable.
        manifests.sort_by(|a, b| a.id.cmp(&b.id));
        Self { manifests }
    }

    pub fn list(&self) -> &[ModuleManifest] {
        &self.manifests
    }

    pub fn find(&self, id: &str) -> Option<&ModuleManifest> {
        self.manifests.iter().find(|m| m.id == id)
    }
}

fn modules_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TERMINITE_MODULES_DIR") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".terminite/modules"))
}

/// Parse the small subset of TOML we need: `key = "value"` lines, `#`
/// comments, no tables or arrays. Same flavor as `src/config.rs` —
/// kept hand-rolled to keep the dep tree shallow.
fn parse_manifest(
    id: &str,
    dir: &std::path::Path,
    text: &str,
) -> Result<ModuleManifest, String> {
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut binary: Option<String> = None;
    let mut description = String::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let key = line[..eq].trim();
        let value_part = line[eq + 1..].trim();
        let value = match strip_quoted(value_part) {
            Some(s) => s,
            None => continue,
        };
        match key {
            "name" => name = Some(value),
            "version" => version = Some(value),
            "binary" => binary = Some(value),
            "description" => description = value,
            _ => {} // ignore unknown keys
        }
    }

    let name = name.unwrap_or_else(|| id.to_string());
    let version = version.unwrap_or_else(|| "0.0.0".to_string());
    let binary_raw = binary.ok_or_else(|| "missing required key `binary`".to_string())?;
    let binary_path = PathBuf::from(&binary_raw);
    let binary = if binary_path.is_absolute() {
        binary_path
    } else {
        dir.join(&binary_path)
    };
    Ok(ModuleManifest {
        id: id.to_string(),
        name,
        version,
        binary,
        description,
    })
}

/// Pull the value out of `"foo"` or `'foo'`. Returns `None` for
/// anything that isn't a quoted string. We don't accept bare values
/// — manifest fields are all strings.
fn strip_quoted(s: &str) -> Option<String> {
    for q in ['"', '\''] {
        if let Some(rest) = s.strip_prefix(q) {
            let end = rest.find(q)?;
            return Some(rest[..end].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parses_a_full_manifest() {
        let text = r#"
# A hello-world module.
name = "Hello"
version = "0.1.0"
binary = "./bin/hello"
description = "Prints hi."
"#;
        let m = parse_manifest("hello", Path::new("/modules/hello"), text).unwrap();
        assert_eq!(m.id, "hello");
        assert_eq!(m.name, "Hello");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.binary, PathBuf::from("/modules/hello/bin/hello"));
        assert_eq!(m.description, "Prints hi.");
    }

    #[test]
    fn missing_binary_is_an_error() {
        let text = r#"name = "x""#;
        assert!(parse_manifest("x", Path::new("/m"), text).is_err());
    }

    #[test]
    fn unknown_keys_ignored() {
        let text = r#"
name = "X"
binary = "./x"
something_else = "ignored"
"#;
        let m = parse_manifest("x", Path::new("/m"), text).unwrap();
        assert_eq!(m.name, "X");
    }

    #[test]
    fn absolute_binary_kept() {
        let text = r#"
name = "X"
binary = "/abs/path/to/x"
"#;
        let m = parse_manifest("x", Path::new("/m"), text).unwrap();
        assert_eq!(m.binary, PathBuf::from("/abs/path/to/x"));
    }
}
