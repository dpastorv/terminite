//! Agent Client Protocol — terminite hosts ACP-speaking AI agents in
//! a pane. The agent runs as a subprocess; we speak JSON-RPC 2.0 over
//! stdio per [the ACP spec](https://agentclientprotocol.com/protocol).
//!
//! Bidirectional. We send (`initialize`, `session/new`,
//! `session/prompt`, `session/cancel`). Agent sends `session/update`
//! notifications (text chunks, tool call announcements + status
//! updates, plans). Agent also calls back into us for
//! `fs/read_text_file`, `fs/write_text_file`, and (eventually)
//! `terminal/*` — those are *client methods* we implement and the
//! agent invokes. Permission requests gate everything sensitive.
//!
//! Threads: one writer (drains an mpsc, writes JSON-RPC lines to
//! child stdin), one reader (parses stdout lines, dispatches events
//! via `EventLoopProxy<UserEvent>` to the main render thread), one
//! stderr drainer (logs each line). All three exit cleanly when the
//! child exits or the session drops.
//!
//! Bounded throughout: input channel is sync_channel(256); writer
//! drops on full. Reader caps each line at MAX_LINE_BYTES; oversize
//! lines are skipped with a warn log.

use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread;
use winit::event_loop::EventLoopProxy;

use crate::TabId;
use crate::UserEvent;

// Inbound JSON-RPC message decoding lives in its own module.
mod wire;
use wire::classify_message;

const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const ACP_PROTOCOL_VERSION: u32 = 1;
const CLIENT_NAME: &str = "terminite";

/// One conversation turn — what gets rendered as a labeled section
/// in the chat pane.
#[derive(Debug, Clone)]
pub enum Turn {
    User {
        text: String,
    },
    Assistant {
        /// Accumulated streamed text. Appended to as
        /// `agent_message_chunk` notifications arrive.
        text: String,
        /// Tool calls fired inside this assistant turn.
        tool_calls: Vec<ToolCall>,
        /// True while the agent is still producing this turn.
        streaming: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: ToolCallStatus,
    /// Aggregated output text (decoded from `content` chunks on
    /// completion).
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// One inbound event from the agent (after reader-thread parsing).
/// Dispatched via `UserEvent::AcpEvent` to the main render thread,
/// which mutates the session's `turns` + asks for a redraw.
#[derive(Debug, Clone)]
pub enum AcpEvent {
    /// `initialize` response received; we're cleared to call
    /// `session/new`.
    Initialized {
        agent_name: String,
        agent_version: Option<String>,
    },
    /// `session/new` response received with a session id; we can
    /// start prompting.
    SessionCreated {
        session_id: String,
    },
    /// `user_message_chunk` notification — the agent's echo of the
    /// most recent user message.
    UserMessageChunk(String),
    /// `agent_message_chunk` — incremental text from the agent.
    AgentMessageChunk(String),
    /// `tool_call` — a new tool call is being announced.
    ToolCallStarted {
        id: String,
        title: String,
        kind: String,
    },
    /// `tool_call_update` — status/content update on a running call.
    ToolCallUpdated {
        id: String,
        status: ToolCallStatus,
        output: Option<String>,
    },
    /// Inbound `session/request_permission` — the agent wants us to
    /// gate something. Main thread renders inline + waits for user
    /// choice, then calls `respond_permission`.
    PermissionRequest {
        request_id: Value,
        tool_call_title: String,
        options: Vec<PermissionOption>,
    },
    /// Inbound `fs/read_text_file` — agent wants to read a file.
    /// Main thread reads (no permission gating on reads in v1 since
    /// the agent already has the cwd; same trust boundary as before),
    /// sends response.
    FsReadRequest {
        request_id: Value,
        path: String,
        line: Option<u32>,
        limit: Option<u32>,
    },
    /// Inbound `fs/write_text_file` — agent wants to write. Main
    /// thread surfaces a permission UI before applying.
    FsWriteRequest {
        request_id: Value,
        path: String,
        content: String,
    },
    /// JSON-RPC error response to one of *our* requests (initialize /
    /// new_session / prompt / cancel).
    ProtocolError(String),
    /// Stderr line from the agent; routed for logging, not display.
    Stderr(String),
    /// Reader / writer / process exited.
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

/// Owns a live ACP session — the spawned child + the writer half of
/// the IO channel. The reader half lives in its own thread and
/// dispatches events via the proxy; nothing else holds it.
pub struct AcpSession {
    pub binary: String,
    pub session_id: Option<String>,
    /// This agent's room slug (`codex-1`), assigned by the host at
    /// `Initialized` and injected into the agent's MCP server env so its
    /// `activity_emit` calls are host-attributed, not self-declared.
    pub slug: Option<String>,
    pub turns: Vec<Turn>,
    /// True between the moment we send `session/prompt` and the
    /// moment the agent stops streaming (no message-chunk in the
    /// last frame). The UI shows a "thinking" indicator while this
    /// is true.
    pub awaiting_response: bool,
    /// Buffer the user is composing before pressing Enter.
    pub draft: String,
    /// Currently-pending permission request, if any. While Some, the
    /// pane intercepts keystrokes for the option keys.
    pub pending_permission: Option<PermissionPrompt>,
    input_tx: SyncSender<String>,
    child: Child,
    next_request_id: u64,
}

#[derive(Debug, Clone)]
pub struct PermissionPrompt {
    pub request_id: Value,
    pub title: String,
    pub options: Vec<PermissionOption>,
}

impl AcpSession {
    /// Spawn an ACP-speaking agent as a subprocess and start the
    /// initialize handshake. Reader + writer + stderr threads are
    /// spawned; events flow to the main thread via the proxy. The
    /// caller waits for `AcpEvent::SessionCreated` before showing the
    /// pane as ready.
    pub fn spawn(
        binary: &str,
        args: &[String],
        tab_id: TabId,
        proxy: EventLoopProxy<UserEvent>,
        cwd: Option<&std::path::Path>,
    ) -> Option<Self> {
        let mut cmd = Command::new(binary);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                crate::logging::warn(&format!(
                    "acp: spawn `{binary}` failed: {e}"
                ));
                return None;
            }
        };

        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let stderr = child.stderr.take()?;

        let (tx, rx) = sync_channel::<String>(256);

        // Writer thread: drain rx → child stdin.
        let writer_proxy = proxy.clone();
        thread::Builder::new()
            .name(format!("acp-writer-{}", tab_id.0))
            .spawn(move || writer_loop(stdin, rx, writer_proxy, tab_id))
            .ok()?;

        // Reader thread: parse stdout lines → AcpEvent → proxy.
        let reader_proxy = proxy.clone();
        thread::Builder::new()
            .name(format!("acp-reader-{}", tab_id.0))
            .spawn(move || reader_loop(stdout, reader_proxy, tab_id))
            .ok()?;

        // Stderr drainer: log each line; if the child dies, this
        // exits naturally on EOF.
        thread::Builder::new()
            .name(format!("acp-stderr-{}", tab_id.0))
            .spawn(move || {
                let r = BufReader::new(stderr);
                for line in r.lines().map_while(Result::ok) {
                    crate::logging::info(&format!(
                        "acp tab {}: stderr: {}",
                        tab_id.0, line
                    ));
                }
            })
            .ok();

        let mut session = AcpSession {
            binary: binary.to_string(),
            session_id: None,
            slug: None,
            turns: Vec::new(),
            awaiting_response: false,
            draft: String::new(),
            pending_permission: None,
            input_tx: tx,
            child,
            next_request_id: 1,
        };
        // Fire the initialize handshake immediately. Response handled
        // by the reader → main loop → caller logic.
        session.send_initialize();
        Some(session)
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    fn send_initialize(&mut self) {
        let id = self.next_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": ACP_PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": { "readTextFile": true, "writeTextFile": true },
                    "terminal": false,
                },
                "clientInfo": {
                    "name": CLIENT_NAME,
                    "title": "Terminite",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            },
        });
        let _ = self.input_tx.try_send(req.to_string());
    }

    /// Called by the main thread after `Initialized` arrives.
    ///
    /// We inject terminite's own MCP server into every session so
    /// the hosted agent sees the room's vocabulary (tabs_list,
    /// blocks_list, cursor_move, tag_add, …) as native tools. This
    /// is the room handing the actor a map — first step toward the
    /// lounge thesis. Without it, each ACP pane is a solo session
    /// in a folder with no awareness of who else is here.
    pub fn send_new_session(&mut self, cwd: &str) {
        let id = self.next_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/new",
            "params": {
                "cwd": cwd,
                "mcpServers": terminite_mcp_servers_array(self.slug.as_deref()),
                // No `permissions` field. The Stage-1 work sent
                // `permissions: { defaultMode: "default" }`; codex-acp maps
                // that through a path that yields `auto` and an internal
                // validator rejects it — `Invalid permissions.defaultMode:
                // auto` — failing session/new. Verified by driving codex-acp
                // standalone: omit the field and session/new succeeds, the
                // adapter defaulting to its own `auto` mode. So we omit it.
            },
        });
        let _ = self.input_tx.try_send(req.to_string());
    }

    /// Send the user's composed draft as a prompt. Pushes a User turn
    /// + a fresh empty Assistant turn so streaming chunks land
    /// somewhere.
    pub fn send_prompt(&mut self) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let text = std::mem::take(&mut self.draft);
        if text.trim().is_empty() {
            self.draft = text;
            return;
        }
        self.turns.push(Turn::User { text: text.clone() });
        self.turns.push(Turn::Assistant {
            text: String::new(),
            tool_calls: Vec::new(),
            streaming: true,
        });
        self.awaiting_response = true;
        let id = self.next_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": text }],
            },
        });
        let _ = self.input_tx.try_send(req.to_string());
    }

    /// Cancel the in-flight prompt. The agent should stop generating
    /// and the current Assistant turn should close gracefully.
    pub fn cancel(&mut self) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let id = self.next_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/cancel",
            "params": { "sessionId": session_id },
        });
        let _ = self.input_tx.try_send(req.to_string());
    }

    /// Reply to a permission request. The `option_id` comes from the
    /// `PermissionPrompt.options` array the UI rendered.
    ///
    /// Wire shape per ACP spec: `result.outcome` is itself an object
    /// with a `outcome` discriminator (`"selected"` | `"cancelled"`)
    /// plus the chosen option's id. Sending a flat `{optionId}` looks
    /// well-formed but the agent silently treats it as a no-op and the
    /// turn hangs.
    pub fn respond_permission(&mut self, request_id: Value, option_id: &str) {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id,
                },
            },
        });
        let _ = self.input_tx.try_send(resp.to_string());
        self.pending_permission = None;
    }

    /// Reply to an `fs/read_text_file` request the agent issued.
    pub fn respond_fs_read(&self, request_id: Value, content: Result<String, String>) {
        let resp = match content {
            Ok(text) => json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": { "content": text },
            }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": { "code": -32000, "message": err },
            }),
        };
        let _ = self.input_tx.try_send(resp.to_string());
    }

    /// Reply to an `fs/write_text_file` request after the user has
    /// approved or denied + the write was attempted.
    pub fn respond_fs_write(&self, request_id: Value, success: Result<(), String>) {
        let resp = match success {
            Ok(()) => json!({ "jsonrpc": "2.0", "id": request_id, "result": {} }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": { "code": -32000, "message": err },
            }),
        };
        let _ = self.input_tx.try_send(resp.to_string());
    }
}

impl Drop for AcpSession {
    fn drop(&mut self) {
        // Polite-then-kill so the reader/writer threads exit when
        // their pipes close.
        let _ = self.child.kill();
        let _ = self.child.wait();
        crate::logging::info(&format!("acp: session torn down ({})", self.binary));
    }
}

/// Build the `mcpServers` array for `session/new`. We inject one
/// entry: terminite's own MCP server (`terminite mcp`), spawned as a
/// stdio subprocess by the agent. The agent then sees `tabs_list`,
/// `blocks_list`, `cursor_move`, `tag_add`, … as native tools — the
/// room's vocabulary, available to whoever enters.
///
/// Resolution uses `std::env::current_exe()` so it tracks whichever
/// terminite binary is currently running (debug, installed, .app
/// bundle). If we can't resolve the path (rare), we fall back to an
/// empty array — agent still works, just without room awareness.
fn terminite_mcp_servers_array(slug: Option<&str>) -> Value {
    let Ok(path) = std::env::current_exe() else {
        return json!([]);
    };
    // Pass the agent's room slug as a command arg to its MCP server, so its
    // `activity_emit` calls are host-attributed (the agent spawns the command
    // exactly as specified — it can't claim another identity). We use `args`
    // rather than `env` because the env array shape is adapter-fragile (it
    // broke session/new); `args` is the field that already works.
    let args = match slug {
        Some(s) => json!(["mcp", "--actor", s]),
        None => json!(["mcp"]),
    };
    json!([{
        "name": "terminite",
        "command": path.to_string_lossy(),
        "args": args,
        "env": [],
    }])
}

// ── Threads ─────────────────────────────────────────────────────────────

fn writer_loop(
    mut stdin: ChildStdin,
    rx: Receiver<String>,
    proxy: EventLoopProxy<UserEvent>,
    tab_id: TabId,
) {
    while let Ok(msg) = rx.recv() {
        if stdin.write_all(msg.as_bytes()).is_err() {
            break;
        }
        if stdin.write_all(b"\n").is_err() {
            break;
        }
        if stdin.flush().is_err() {
            break;
        }
    }
    let _ = proxy.send_event(UserEvent::AcpEvent {
        tab_id,
        event: AcpEvent::Shutdown,
    });
}

fn reader_loop(
    stdout: ChildStdout,
    proxy: EventLoopProxy<UserEvent>,
    tab_id: TabId,
) {
    let mut reader = BufReader::with_capacity(MAX_LINE_BYTES, stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(n) if n > MAX_LINE_BYTES => {
                crate::logging::warn(&format!(
                    "acp tab {}: skipping oversize line ({n} bytes)",
                    tab_id.0
                ));
                continue;
            }
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Cheap structural gate before invoking serde_json — the
        // ACP wire is strict JSON object per line. During npx-driven
        // first-run installs the agent's stdout is *flooded* with
        // human-readable progress lines (npm warnings, download
        // progress, …); parsing each one + logging the failure
        // saturated CPU in the reader + log mutex and starved the
        // main thread. Drop anything that doesn't start with `{`
        // without allocating.
        if !trimmed.starts_with('{') {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue, // silently drop malformed frames
        };
        // Diagnostic: log inbound methods (skip session/update — high
        // volume during streaming). Helps us see whether the agent is
        // sending us session/request_permission, fs/read_text_file,
        // etc. when something looks broken on screen.
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            if method != "session/update" {
                crate::logging::info(&format!(
                    "acp tab {}: rx method={method}",
                    tab_id.0
                ));
            }
        }
        for event in classify_message(&msg) {
            let _ = proxy.send_event(UserEvent::AcpEvent {
                tab_id,
                event,
            });
        }
    }
    let _ = proxy.send_event(UserEvent::AcpEvent {
        tab_id,
        event: AcpEvent::Shutdown,
    });
}

/// A baked-in list of common ACP-speaking agents. Each preset names
/// the binary terminite spawns + the args it passes. Most modern AI
/// agents don't speak ACP natively yet — they ship as separate
/// adapter packages that Zed (and now terminite) drive over stdio.
///
/// We invoke each adapter via `npx` so:
///   - Users who haven't installed the adapter still get a one-time
///     auto-download (cached afterward).
///   - Users who have `npm install -g`'d the adapter get the cached
///     binary instantly.
///   - There's no per-OS binary path to maintain.
///
/// Trade-off: requires `npx` (i.e. Node/npm) on PATH. Documented in
/// getting-started.md under the AI partner section.
pub struct AgentPreset {
    pub display_name: &'static str,
    pub binary: &'static str,
    pub default_args: &'static [&'static str],
}

pub fn presets() -> &'static [AgentPreset] {
    &[
        AgentPreset {
            display_name: "Claude Code",
            binary: "npx",
            default_args: &["-y", "@zed-industries/claude-agent-acp"],
        },
        AgentPreset {
            display_name: "Codex",
            binary: "npx",
            default_args: &["-y", "@zed-industries/codex-acp"],
        },
        AgentPreset {
            display_name: "Gemini",
            binary: "npx",
            default_args: &["-y", "@google/gemini-cli", "--experimental-acp"],
        },
    ]
}

/// Check whether the preset's launcher binary is on PATH. Used by
/// the dropdown to gray out presets whose adapter can't be reached.
/// For npx-driven presets this just confirms Node is installed; the
/// adapter package itself is fetched on first run.
pub fn resolve_preset(preset: &AgentPreset) -> Option<String> {
    if which(preset.binary).is_some() {
        Some(preset.binary.to_string())
    } else {
        None
    }
}

fn which(binary: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)] // Reserved for richer ContentBlock parsing in v2.
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_agent_message_chunk() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_abc",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "hello" },
                }
            }
        });
        let events = classify_message(&msg);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AcpEvent::AgentMessageChunk(s) => assert_eq!(s, "hello"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn classifies_tool_call_announce_and_update() {
        let announce = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_abc",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "call_001",
                    "title": "Run tests",
                    "kind": "shell",
                    "status": "pending"
                }
            }
        });
        let events = classify_message(&announce);
        assert!(matches!(events[0], AcpEvent::ToolCallStarted { .. }));

        let update = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_abc",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call_001",
                    "status": "completed",
                    "content": [
                        { "type": "content", "content": { "type": "text", "text": "ok" } }
                    ]
                }
            }
        });
        let events = classify_message(&update);
        match &events[0] {
            AcpEvent::ToolCallUpdated { id, status, output } => {
                assert_eq!(id, "call_001");
                assert!(matches!(status, ToolCallStatus::Completed));
                assert_eq!(output.as_deref(), Some("ok"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn classifies_initialize_and_new_session_responses() {
        let init = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {
                "protocolVersion": 1,
                "agentInfo": { "name": "OpenClaw", "version": "1.2.3" }
            }
        });
        let events = classify_message(&init);
        match &events[0] {
            AcpEvent::Initialized { agent_name, agent_version } => {
                assert_eq!(agent_name, "OpenClaw");
                assert_eq!(agent_version.as_deref(), Some("1.2.3"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
        let new_sess = serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": { "sessionId": "sess_xyz" }
        });
        match &classify_message(&new_sess)[0] {
            AcpEvent::SessionCreated { session_id } => assert_eq!(session_id, "sess_xyz"),
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
