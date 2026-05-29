//! Model Context Protocol server — terminite as a tool surface for
//! MCP-speaking AI clients.
//!
//! Architecture:
//!
//!   AI client (Claude Code / Desktop / Cursor / …)
//!       spawns: `terminite mcp` (subprocess)
//!         ↓ JSON-RPC 2.0 over stdio
//!       this module
//!         ↓ Unix socket
//!       running terminite window (proto.rs server)
//!
//! Each MCP tool call is translated into a proto request, the
//! response is reshaped into MCP's `content` array, and returned.
//! Stateless — opens a fresh proto connection per call (cheap; the
//! socket is local).
//!
//! Tool descriptions are deliberately written as *partnership
//! onboarding*, not as bare API docs. They tell the AI *when* and
//! *why* to use each verb, so the vocabulary self-advertises. The
//! lounge thesis (`guide/lounge-thesis.md`) names this as the right
//! delivery for the shared vocabulary: the tool palette is the primer.
//!
//! MCP spec: the v1 protocol surface we implement is
//!   - `initialize` — handshake
//!   - `notifications/initialized` — client ack (no response)
//!   - `tools/list` — return the catalog
//!   - `tools/call` — invoke a tool
//!   - `ping` — keepalive
//! Anything else returns a structured method-not-found error.
//!
//! Bounded: each stdin line capped at MAX_LINE_BYTES (1 MB). Each
//! proto socket interaction reads one response line then closes.
//! No threads spawned. No state held beyond the running message
//! handler.

use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

const MAX_LINE_BYTES: usize = 1024 * 1024;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "terminite";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run the MCP server loop until stdin closes. Blocking; intended
/// to be invoked as `terminite mcp` and driven by an AI client's
/// MCP runtime.
pub fn run() -> ExitCode {
    let stdin = io::stdin();
    let mut reader = BufReader::with_capacity(MAX_LINE_BYTES, stdin.lock());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return ExitCode::SUCCESS, // EOF — client closed.
            Ok(_) => {}
            Err(e) => {
                eprintln!("terminite mcp: read failed: {e}");
                return ExitCode::from(1);
            }
        }
        if line.len() > MAX_LINE_BYTES {
            // The reader fills past cap if there's no newline; reject.
            send_err(&mut out, Value::Null, -32600,
                "request line exceeded MAX_LINE_BYTES");
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                send_err(&mut out, Value::Null, -32700,
                    &format!("parse error: {e}"));
                continue;
            }
        };
        handle_request(&mut out, req);
    }
}

fn handle_request(out: &mut impl Write, req: Value) {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => {
            send_result(out, id, json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
            }));
        }
        "notifications/initialized" => {
            // No response expected for notifications.
        }
        "ping" => {
            send_result(out, id, json!({}));
        }
        "tools/list" => {
            send_result(out, id, json!({ "tools": tool_catalog() }));
        }
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(name, &args) {
                Ok(content_text) => {
                    send_result(out, id, json!({
                        "content": [{ "type": "text", "text": content_text }],
                        "isError": false,
                    }));
                }
                Err(msg) => {
                    send_result(out, id, json!({
                        "content": [{ "type": "text", "text": msg }],
                        "isError": true,
                    }));
                }
            }
        }
        _ => {
            send_err(out, id, -32601, &format!("method not found: {method}"));
        }
    }
}

// ── Tool catalog ─────────────────────────────────────────────────────────

/// The tool definitions advertised to MCP clients. Descriptions are
/// the onboarding — they tell the AI when + why + how to use each
/// verb, not just what it does. Edit with care: each one is a slot
/// in the shared vocabulary.
fn tool_catalog() -> Vec<Value> {
    vec![
        json!({
            "name": "terminite_tabs_list",
            "description":
                "List the open tabs in the user's terminite window. Each tab has a numeric id and a title (often the foreground process). Run this at session start to orient: which tabs exist, which one is the human likely working in.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_blocks_list",
            "description":
                "List the numbered blocks in a tab. A block is one shell command + its output, marked via OSC 133, labeled B1/B2/B3… in the gutter. Both you and the human refer to blocks by id (e.g. \"B7\") — when you say B7 in chat, the human sees the gutter label. Use this to see what's been happening, find a block to discuss, or pick up where you left off.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab_id": { "type": "integer", "description": "Tab id from terminite_tabs_list." },
                },
                "required": ["tab_id"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_block_get",
            "description":
                "Read one block's command + full output. Use after blocks_list when you need the actual content of B7 (e.g., to read the error from a failed test).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab_id": { "type": "integer" },
                    "block_id": { "type": "integer", "description": "The N in BN." },
                },
                "required": ["tab_id", "block_id"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_cursor_move",
            "description":
                "Place your AI cursor on a block. The human sees this as a warm-amber highlight on that block's gutter label — the partnership signal for \"I'm reading B7 right now.\" Move it when your focus shifts. One slot per tab; calling this again replaces the prior position.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab_id": { "type": "integer" },
                    "block_id": { "type": "integer" },
                },
                "required": ["tab_id", "block_id"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_cursor_clear",
            "description":
                "Release your AI cursor from a tab. Use when you're done looking, switching tabs, or wrapping up.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab_id": { "type": "integer" },
                },
                "required": ["tab_id"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_tag_add",
            "description":
                "Attach a short label to a block. Persists in the layout — survives the session, visible to other actors in the lounge. Use for marking something for review (\"needs-review\"), claiming a block (\"looking-now\"), or leaving a note that's tied to a runtime artifact rather than source code.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab_id": { "type": "integer" },
                    "block_id": { "type": "integer" },
                    "tag": { "type": "string", "description": "Short label (≤ ~32 chars works well)." },
                },
                "required": ["tab_id", "block_id", "tag"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_tag_remove",
            "description":
                "Remove a tag from a block. Use to retract a marker or clean up after a review.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab_id": { "type": "integer" },
                    "block_id": { "type": "integer" },
                    "tag": { "type": "string" },
                },
                "required": ["tab_id", "block_id", "tag"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_block_export",
            "description":
                "Export a tab's blocks as a markdown artifact (commands + outputs, optionally from a starting block id). Use to share what just happened with the human, or to capture a session's work as a record.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab_id": { "type": "integer" },
                    "since_block_id": {
                        "type": "integer",
                        "description": "Optional — start exporting from this block id onward."
                    },
                },
                "required": ["tab_id"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_modules_list",
            "description":
                "List installed terminite modules (nav / preview / editor / config / etc.). Modules are the extension surface — each one becomes a selectable pane kind. Useful when planning which panes the human might already have open.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_modules_reload",
            "description":
                "Re-discover modules from disk (~/.terminite/modules/). Run after installing or removing a module so the dropdown picks it up without relaunching terminite.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_stats",
            "description":
                "Internal-state snapshot — frame timings, peak memory, per-tab block counts. Mostly useful when you're debugging terminite itself; not needed for everyday partnership work.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
    ]
}

// ── Tool dispatch ────────────────────────────────────────────────────────

fn call_tool(name: &str, args: &Value) -> Result<String, String> {
    match name {
        "terminite_tabs_list" => proto_call(json!({
            "id": 1, "method": "list_tabs"
        })),
        "terminite_blocks_list" => {
            let tab_id = require_int(args, "tab_id")?;
            proto_call(json!({
                "id": 1, "method": "list_blocks", "params": { "tab_id": tab_id }
            }))
        }
        "terminite_block_get" => {
            let tab_id = require_int(args, "tab_id")?;
            let block_id = require_int(args, "block_id")?;
            proto_call(json!({
                "id": 1, "method": "get_block",
                "params": { "tab_id": tab_id, "block_id": block_id }
            }))
        }
        "terminite_cursor_move" => {
            let tab_id = require_int(args, "tab_id")?;
            let block_id = require_int(args, "block_id")?;
            proto_call(json!({
                "id": 1, "method": "cursor_at",
                "params": { "tab_id": tab_id, "block_id": block_id }
            }))
        }
        "terminite_cursor_clear" => {
            let tab_id = require_int(args, "tab_id")?;
            proto_call(json!({
                "id": 1, "method": "cursor_clear", "params": { "tab_id": tab_id }
            }))
        }
        "terminite_tag_add" => {
            let tab_id = require_int(args, "tab_id")?;
            let block_id = require_int(args, "block_id")?;
            let tag = require_str(args, "tag")?;
            proto_call(json!({
                "id": 1, "method": "set_tag",
                "params": { "tab_id": tab_id, "block_id": block_id, "tag": tag }
            }))
        }
        "terminite_tag_remove" => {
            let tab_id = require_int(args, "tab_id")?;
            let block_id = require_int(args, "block_id")?;
            let tag = require_str(args, "tag")?;
            proto_call(json!({
                "id": 1, "method": "remove_tag",
                "params": { "tab_id": tab_id, "block_id": block_id, "tag": tag }
            }))
        }
        "terminite_block_export" => {
            let tab_id = require_int(args, "tab_id")?;
            let mut params = serde_json::Map::new();
            params.insert("tab_id".to_string(), json!(tab_id));
            if let Some(since) = args.get("since_block_id").and_then(|v| v.as_i64()) {
                params.insert("since_block_id".to_string(), json!(since));
            }
            proto_call(json!({
                "id": 1, "method": "export_tab", "params": Value::Object(params)
            }))
        }
        "terminite_modules_list" => proto_call(json!({
            "id": 1, "method": "list_modules"
        })),
        "terminite_modules_reload" => proto_call(json!({
            "id": 1, "method": "reload_modules"
        })),
        "terminite_stats" => proto_call(json!({
            "id": 1, "method": "stats"
        })),
        other => Err(format!("unknown tool: {other}")),
    }
}

// ── Proto socket bridge ──────────────────────────────────────────────────

fn proto_call(req: Value) -> Result<String, String> {
    let path = socket_path().ok_or_else(|| {
        "no socket path — set $TERMINITE_SOCKET or $HOME".to_string()
    })?;
    let mut stream = UnixStream::connect(&path).map_err(|e| {
        format!("can't connect to {} — is terminite running? ({e})", path.display())
    })?;
    let mut req_line = req.to_string();
    req_line.push('\n');
    stream.write_all(req_line.as_bytes()).map_err(|e| format!("socket write: {e}"))?;
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).map_err(|e| format!("socket read: {e}"))?;
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return Err("empty response from terminite".to_string());
    }
    // Parse the proto response, then reshape: if it's an error, surface
    // as a tool error; otherwise pretty-print the payload for the AI.
    let parsed: Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("invalid response from terminite: {e}"))?;
    if parsed.get("kind").and_then(|k| k.as_str()) == Some("error") {
        let msg = parsed
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("(no message)");
        return Err(format!("terminite error: {msg}"));
    }
    // Drop the JSON-RPC id wrapper; the AI cares about the payload.
    let payload = parsed
        .as_object()
        .map(|o| {
            let mut clone = o.clone();
            clone.remove("id");
            Value::Object(clone)
        })
        .unwrap_or(parsed);
    Ok(serde_json::to_string_pretty(&payload)
        .unwrap_or_else(|_| "(unserializable)".to_string()))
}

fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TERMINITE_SOCKET") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".terminite/socket"))
}

// ── arg helpers ──────────────────────────────────────────────────────────

fn require_int(args: &Value, key: &str) -> Result<i64, String> {
    args.get(key)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("missing or non-integer argument: {key}"))
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing or non-string argument: {key}"))
}

// ── JSON-RPC response helpers ────────────────────────────────────────────

fn send_result(out: &mut impl Write, id: Value, result: Value) {
    let msg = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    write_line(out, msg);
}

fn send_err(out: &mut impl Write, id: Value, code: i64, message: &str) {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    write_line(out, msg);
}

fn write_line(out: &mut impl Write, msg: Value) {
    let line = msg.to_string();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_catalog_has_core_tools() {
        let tools = tool_catalog();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        for expected in [
            "terminite_tabs_list",
            "terminite_blocks_list",
            "terminite_block_get",
            "terminite_cursor_move",
            "terminite_cursor_clear",
            "terminite_tag_add",
            "terminite_tag_remove",
            "terminite_block_export",
            "terminite_modules_list",
            "terminite_modules_reload",
            "terminite_stats",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn every_tool_has_description_and_schema() {
        for t in tool_catalog() {
            assert!(t.get("description").and_then(|d| d.as_str()).is_some(),
                "tool missing description: {t}");
            assert!(t.get("inputSchema").is_some(),
                "tool missing inputSchema: {t}");
        }
    }
}
