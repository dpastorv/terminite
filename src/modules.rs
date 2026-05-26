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

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;

use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::{TabId, UserEvent};

/// Per-connection write queue depth for the input pipe to a module.
/// Drop-on-overflow rather than buffer-forever, matching the proto
/// subscriber pattern.
const MODULE_INPUT_QUEUE_CAP: usize = 1024;
/// Max bytes terminite will read from a single module stdout line —
/// defensive ceiling so a runaway module can't grow our buffers
/// without bound.
const MODULE_MAX_LINE_BYTES: usize = 256 * 1024;

/// What protocol terminite uses to talk to a module process.
///
/// - `Data` — line-delimited JSON over stdio. Module pushes `set_text`
///   frames; terminite renders through its glyphon text path. Right
///   shape for: log tailers, viewers, AI chat, the debug pane.
/// - `Tty` — module gets a PTY, draws via terminal escape sequences,
///   terminite renders through the same vte/alacritty path shells
///   use. Right shape for: file managers (yazi), editors (helix,
///   nvim), monitors (htop, btop), git UIs (lazygit) — anything with
///   a real TUI.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModuleKind {
    #[default]
    Data,
    Tty,
}

/// One module's parsed manifest.
#[derive(Clone, Debug, Serialize)]
pub struct ModuleManifest {
    /// The directory name (the module's identifier in the registry).
    pub id: String,
    /// Human-readable name shown in the dropdown.
    pub name: String,
    /// Free-form version string.
    pub version: String,
    /// Absolute path to the executable terminite will spawn.
    pub binary: PathBuf,
    /// Optional one-line summary.
    pub description: String,
    /// Wire protocol — data (JSON over stdio) or tty (PTY). Defaults
    /// to `data` for backward compatibility with existing manifests.
    pub kind: ModuleKind,
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
    let mut kind = ModuleKind::Data;

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
            "kind" => {
                kind = match value.to_ascii_lowercase().as_str() {
                    "tty" => ModuleKind::Tty,
                    "data" => ModuleKind::Data,
                    other => {
                        return Err(format!(
                            "unknown `kind` value `{other}` (expected `data` or `tty`)"
                        ));
                    }
                };
            }
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
        kind,
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

// ── ModuleSession ─────────────────────────────────────────────────────
//
// A live module: the spawned child process plus its IO threads. Step
// 2b. Step 2c (later) will expand the wire vocabulary.
//
// Wire format (line-delimited JSON, both directions):
//
//   terminite → module
//     {"kind":"init",  "tab_id":N}
//     {"kind":"input", "bytes":"…"}   (utf-8 string of the keystroke)
//     {"kind":"close"}
//
//   module → terminite
//     {"kind":"set_text",  "body":"…"}
//     {"kind":"set_image", "path":"/abs/file.png"}
//     {"kind":"publish_focus", "path":"/abs/file"}
//     {"kind":"log",       "message":"…"}
//
// set_text and set_image are exclusive — the most recent of the two
// wins on screen, and switching either clears the other so panes
// don't end up with a stale half of the previous content showing.

/// Cursor position in a module-rendered body. 0-indexed source line
/// + column, where the line is the body string split on `\n`.
#[derive(Deserialize, Debug, Clone, Copy)]
pub struct CursorPos {
    pub line: u32,
    pub col: u32,
}

/// Message a module sends to terminite. Reader thread parses each line
/// of the module's stdout into one of these and forwards via
/// `UserEvent::ModuleMessage`.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModuleMessage {
    /// Replace the rendered body with this text. Clears any image
    /// the module had previously asked us to show.
    ///
    /// Optional `scroll_to_line` asks the host to ensure that
    /// 0-indexed source line is visible after applying the body. If
    /// the line is already on screen the scroll position is left
    /// alone — that way a user wheel-scroll isn't fought by every
    /// module re-render. If omitted, scroll resets to 0 (old
    /// behavior — preview wants this, nav doesn't).
    ///
    /// Optional `cursor` places the host's terminal cursor (same
    /// shape + color as a shell cursor) at a 0-indexed (line, col)
    /// in the body. Modules that need a visible cursor (Editor)
    /// should use this instead of injecting their own glyph — that
    /// way (a) the cursor looks identical across panes and (b) the
    /// body string is stable as the cursor moves, which skips a
    /// full buffer reshape on every keystroke. `None` means "no
    /// cursor on this pane" (Preview, Nav, …).
    ///
    /// Optional `dim_left_cols` paints the first N columns of every
    /// body row in a dim color (typeset over the same buffer with
    /// split bounds — no extra shaping). Editor uses this for the
    /// line-number gutter; Preview / Nav leave it `None`.
    ///
    /// Optional `highlight_line` paints a subtle background rect
    /// across that 0-indexed source line. Nav uses it for the
    /// current selection row; Editor uses it for the cursor row.
    SetText {
        body: String,
        #[serde(default)]
        scroll_to_line: Option<u32>,
        #[serde(default)]
        cursor: Option<CursorPos>,
        #[serde(default)]
        dim_left_cols: Option<u32>,
        #[serde(default)]
        highlight_line: Option<u32>,
    },
    /// Render the file at `path` as the pane's content. PNG only in
    /// v1; the host returns a `log` line if decode fails so the
    /// module can recover (typically by sending `set_text` with a
    /// fallback message). Clears any prior text body.
    SetImage { path: String },
    /// Module-side info log; lands in terminite's regular log.
    Log { message: String },
    /// Announce a file/directory the user just focused. terminite
    /// remembers the path and broadcasts a `focus` event to every
    /// other module session so paired views (nav + preview) can
    /// react. Lean v1 of cross-pane signaling — one event type,
    /// global broadcast.
    PublishFocus { path: String },
}

/// A spawned module process plus its IO state.
pub struct ModuleSession {
    pub manifest_id: String,
    /// Latest body the module asked us to render. Empty until the
    /// module sends its first `set_text`.
    pub body: String,
    /// Sender for input lines (terminite → module stdin). Bounded;
    /// overflow drops the keystroke rather than blocking the main
    /// thread.
    input_tx: SyncSender<String>,
    /// Owned so Drop can kill the child if it didn't exit cleanly.
    child: Child,
}

impl ModuleSession {
    /// Spawn the module's binary; start reader + writer threads;
    /// send the initial `init` command. Returns `None` on spawn
    /// failure — terminite shows the registration placeholder instead.
    pub fn spawn(
        manifest: &ModuleManifest,
        tab_id: TabId,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Option<Self> {
        let mut child = match Command::new(&manifest.binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                crate::logging::warn(&format!(
                    "module {}: spawn failed ({e})",
                    manifest.id
                ));
                return None;
            }
        };

        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let stderr = child.stderr.take();

        let (input_tx, input_rx) = sync_channel::<String>(MODULE_INPUT_QUEUE_CAP);

        // Writer thread: drain input_rx → child stdin. Exits when
        // input_tx is dropped or the pipe breaks.
        let writer_id = manifest.id.clone();
        thread::Builder::new()
            .name(format!("terminite-mod-w-{}", manifest.id))
            .spawn(move || writer_loop(stdin, input_rx, writer_id))
            .ok();

        // Reader thread: read JSON lines from child stdout; forward
        // each to the main thread as a UserEvent. Exits on EOF.
        let reader_id = manifest.id.clone();
        let reader_proxy = proxy.clone();
        thread::Builder::new()
            .name(format!("terminite-mod-r-{}", manifest.id))
            .spawn(move || reader_loop(stdout, tab_id, reader_id, reader_proxy))
            .ok();

        // Stderr drainer: log each line. Without this, a chatty
        // module's stderr would block on a full pipe.
        if let Some(stderr) = stderr {
            let err_id = manifest.id.clone();
            thread::Builder::new()
                .name(format!("terminite-mod-e-{}", manifest.id))
                .spawn(move || {
                    let reader = BufReader::with_capacity(MODULE_MAX_LINE_BYTES, stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        crate::logging::warn(&format!("module {err_id} stderr: {line}"));
                    }
                })
                .ok();
        }

        // Send the init command — module knows its tab id.
        let init = format!(r#"{{"kind":"init","tab_id":{}}}"#, tab_id.0);
        let _ = input_tx.try_send(init);

        crate::logging::info(&format!(
            "module {}: spawned (tab_id {})",
            manifest.id, tab_id.0
        ));

        Some(ModuleSession {
            manifest_id: manifest.id.clone(),
            body: String::new(),
            input_tx,
            child,
        })
    }

    /// Forward a keystroke (or any user-typed bytes) to the module's
    /// stdin. Overflow drops the keystroke; the user retries.
    pub fn send_input(&self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let escaped = json_escape(&text);
        let msg = format!(r#"{{"kind":"input","bytes":"{escaped}"}}"#);
        let _ = self.input_tx.try_send(msg);
    }

    /// Notify the module that a path was focused somewhere else in
    /// terminite. Paired views (nav + preview + editor) use this to
    /// react to each other without knowing about each other directly.
    pub fn send_focus(&self, path: &str) {
        let escaped = json_escape(path);
        let msg = format!(r#"{{"kind":"focus","path":"{escaped}"}}"#);
        let _ = self.input_tx.try_send(msg);
    }

    /// Notify the module that a shell pane's cwd changed (OSC 7).
    /// Nav uses this to optionally follow the shell; other modules
    /// can ignore. Fires on actual change only (the host dedupes
    /// before broadcasting), so the wire stays quiet.
    pub fn send_cwd(&self, path: &str) {
        let escaped = json_escape(path);
        let msg = format!(r#"{{"kind":"cwd","path":"{escaped}"}}"#);
        let _ = self.input_tx.try_send(msg);
    }

    /// Report a left-click in the module's content area. `line` is
    /// the 0-indexed source line of the body (the host translates
    /// from the visual layout-run index it computed off the pixel
    /// position), `col` is the visual column, and `count` is the
    /// multi-click index (1 = single, 2 = double, 3 = triple).
    /// Same MULTI_CLICK_WINDOW the shell selection path uses. Nav
    /// treats count=2 as "activate" (cd into / focus); editor
    /// treats count=1 as cursor reposition.
    pub fn send_click(&self, line: u32, col: u32, count: u8) {
        let msg = format!(
            r#"{{"kind":"click","line":{line},"col":{col},"count":{count}}}"#
        );
        let _ = self.input_tx.try_send(msg);
    }
}

impl Drop for ModuleSession {
    fn drop(&mut self) {
        // Best-effort polite close, then force-kill so the child can't
        // outlive the tab. Reader + writer threads exit on pipe
        // closure.
        let _ = self.input_tx.try_send(r#"{"kind":"close"}"#.to_string());
        // Give the module a small moment to exit on its own — then
        // kill if needed.
        let _ = self.child.kill();
        let _ = self.child.wait();
        crate::logging::info(&format!("module {}: torn down", self.manifest_id));
    }
}

fn writer_loop(
    mut stdin: std::process::ChildStdin,
    rx: Receiver<String>,
    module_id: String,
) {
    while let Ok(msg) = rx.recv() {
        if stdin.write_all(msg.as_bytes()).is_err() {
            break;
        }
        if stdin.write_all(b"\n").is_err() {
            break;
        }
    }
    let _ = module_id;
}

fn reader_loop(
    stdout: std::process::ChildStdout,
    tab_id: TabId,
    module_id: String,
    proxy: EventLoopProxy<UserEvent>,
) {
    let reader = BufReader::with_capacity(MODULE_MAX_LINE_BYTES, stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.len() > MODULE_MAX_LINE_BYTES {
            crate::logging::warn(&format!(
                "module {module_id}: line exceeded MODULE_MAX_LINE_BYTES, dropping"
            ));
            continue;
        }
        match serde_json::from_str::<ModuleMessage>(line.trim()) {
            Ok(msg) => {
                if proxy
                    .send_event(UserEvent::ModuleMessage { tab_id, msg })
                    .is_err()
                {
                    break;
                }
            }
            Err(e) => {
                crate::logging::warn(&format!(
                    "module {module_id}: bad JSON line ({e}): {line}"
                ));
            }
        }
    }
    crate::logging::info(&format!("module {module_id}: stdout closed"));
}

/// Minimal JSON string escape — matches `proto_client::json_escape`.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
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
