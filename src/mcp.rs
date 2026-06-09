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
//! lounge thesis names this as the right
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

/// This server's room actor slug, passed via `terminite mcp --actor <slug>`
/// when terminite wires it into an agent CLI. Host-assigned; the agent
/// can't override it. `None` when run standalone (CLI emit is rejected).
static ACTOR: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Run the MCP server loop until stdin closes. Blocking; intended
/// to be invoked as `terminite mcp` and driven by an AI client's
/// MCP runtime. `actor` is the host-assigned room slug, if any.
pub fn run(actor: Option<String>) -> ExitCode {
    // Presence: hold one connection open for our whole lifetime. We join the
    // room with our base name (type[+profile], e.g. `claude-gut`); terminite
    // assigns a unique color and hands back the slug we go by (`claude-gut-
    // blue`). The held connection *is* our attendance — when this process
    // exits (stdin EOF), it closes and terminite drops us from the roster.
    // `_presence` is bound for the whole function so the stream stays open.
    let _presence: Option<UnixStream> = match actor.as_deref() {
        Some(base) => match join_room(base) {
            Ok((stream, slug)) => {
                let _ = ACTOR.set(slug);
                Some(stream)
            }
            Err(e) => {
                eprintln!("terminite mcp: room join failed ({e}); using base name");
                let _ = ACTOR.set(base.to_string());
                None
            }
        },
        None => None,
    };
    let stdin = io::stdin();
    let mut reader = BufReader::with_capacity(MAX_LINE_BYTES, stdin.lock());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();

    loop {
        match crate::io_util::read_capped_line(&mut reader, MAX_LINE_BYTES, &mut line) {
            Ok(0) => return ExitCode::SUCCESS, // EOF — client closed.
            Ok(_) => {}
            Err(e) => {
                eprintln!("terminite mcp: read failed: {e}");
                return ExitCode::from(1);
            }
        }
        if line.len() > MAX_LINE_BYTES {
            // Bounded reader cut the line at the cap; drop it and resync at
            // the next newline.
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

/// Run as a Claude Code **channel** — a receiver in the comms base. Spawned BY
/// claude (`claude --channels server:lounge`), it subscribes to terminite's room
/// and pushes each directed message into claude's *running* session as a
/// `notifications/claude/channel` event — the wake. Receive-only: claude replies
/// through its existing lounge MCP tools. Auto-acks each message it surfaces.
///
/// The MCP loop runs on the main thread and answers `initialize` immediately
/// (declaring the channel capability) — the subscribe + bridge happen on a
/// background thread so a slow room-join never stalls claude's handshake. The
/// bridge exits on socket EOF; the process exits on stdin EOF (claude closing),
/// which closes the subscribe socket → terminite drops the subscriber. No
/// long-lived state, one bounded thread.
pub fn run_channel() -> ExitCode {
    let stdout = std::sync::Arc::new(std::sync::Mutex::new(io::stdout()));

    // Background: resolve our room slug (the actor in our pane — claude's lounge
    // MCP joined as e.g. claude-blue), subscribe, and bridge pushes → events.
    let bridge_out = stdout.clone();
    std::thread::spawn(move || {
        let pane = std::env::var("TERMINITE_PANE").ok().and_then(|s| s.parse::<u64>().ok());
        let Some(slug) = resolve_pane_slug(pane) else { return };
        let Some(path) = socket_path() else { return };
        let Ok(mut sub) = UnixStream::connect(&path) else { return };
        let req = json!({ "id": 1, "method": "room_subscribe", "params": { "actor": slug } });
        let mut line = req.to_string();
        line.push('\n');
        if sub.write_all(line.as_bytes()).is_err() {
            return;
        }
        channel_bridge(sub, bridge_out);
    });

    // Main: the MCP stdio loop. Minimal — a channel, not a tool server.
    let stdin = io::stdin();
    let mut reader = BufReader::with_capacity(MAX_LINE_BYTES, stdin.lock());
    let mut line = String::new();
    loop {
        match crate::io_util::read_capped_line(&mut reader, MAX_LINE_BYTES, &mut line) {
            Ok(0) => return ExitCode::SUCCESS, // claude closed stdin → exit.
            Ok(n) if n > MAX_LINE_BYTES => continue, // over-long → drop, resync
            Ok(_) => {}
            Err(_) => return ExitCode::from(1),
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let Ok(mut out) = stdout.lock() else { return ExitCode::from(1) };
        match method {
            "initialize" => send_result(&mut *out, id, json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "experimental": { "claude/channel": {} } },
                "serverInfo": { "name": "terminite-channel", "version": SERVER_VERSION },
            })),
            "notifications/initialized" => {}
            "ping" => send_result(&mut *out, id, json!({})),
            "tools/list" => send_result(&mut *out, id, json!({ "tools": [] })),
            _ => send_err(&mut *out, id, -32601, &format!("method not found: {method}")),
        }
    }
}

/// The channel's background bridge: read room-message pushes off the subscribe
/// socket and surface each into claude as a `notifications/claude/channel`,
/// auto-acking it. Exits on socket EOF.
fn channel_bridge(sub: UnixStream, out: std::sync::Arc<std::sync::Mutex<io::Stdout>>) {
    let mut reader = BufReader::new(sub);
    let mut line = String::new();
    loop {
        match crate::io_util::read_capped_line(&mut reader, MAX_LINE_BYTES, &mut line) {
            Ok(0) => break,                          // socket EOF
            Ok(n) if n > MAX_LINE_BYTES => continue, // over-long → drop, resync
            Ok(_) => {}
            Err(_) => break,
        }
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
        if v.get("kind").and_then(|k| k.as_str()) != Some("room_message") {
            continue;
        }
        let from = v.get("from").and_then(|x| x.as_str()).unwrap_or("");
        let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/claude/channel",
            "params": {
                "content": format!("{from}: {text}"),
                "meta": { "from": from },
            },
        });
        if let Ok(mut o) = out.lock() {
            let mut s = notif.to_string();
            s.push('\n');
            let _ = o.write_all(s.as_bytes());
            let _ = o.flush();
        }
        // Auto-ack (a separate one-shot keeps the subscribe socket read-only).
        if let Some(mid) = v.get("message_id").and_then(|x| x.as_u64()) {
            let _ = proto_call(json!({ "id": 1, "method": "room_ack", "params": { "message_id": mid } }));
        }
    }
}

/// Find the room slug of the actor present in `pane` (our own pane), retrying a
/// few times — claude's lounge MCP may still be joining when the channel starts.
fn resolve_pane_slug(pane: Option<u64>) -> Option<String> {
    let pane = pane?;
    for _ in 0..10 {
        if let Ok(resp) = proto_call(json!({ "id": 1, "method": "room_who" })) {
            if let Ok(v) = serde_json::from_str::<Value>(&resp) {
                if let Some(actors) = v.get("actors").and_then(|a| a.as_array()) {
                    for a in actors {
                        if a.get("pane").and_then(|p| p.as_u64()) == Some(pane) {
                            if let Some(slug) = a.get("slug").and_then(|s| s.as_str()) {
                                return Some(slug.to_string());
                            }
                        }
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    None
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
        json!({
            "name": "terminite_activities_list",
            "description":
                "What's been happening in the room. Returns agent messages (and, later, tool calls) in time order. Filter by `actor` to see one agent's activity; pass `to` with your own room id to read messages addressed to you (broadcasts are excluded from a `to` filter). Use this to see who else is here and what they've said.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "actor": { "type": "string", "description": "Only this actor's activities, e.g. \"codex-1\"." },
                    "to": { "type": "string", "description": "Only messages addressed to this actor — your inbox." },
                    "kind": { "type": "string", "description": "Filter by kind, e.g. \"agent_message\"." },
                },
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_activity_emit",
            "description":
                "Post a message to the room. Address it (set `to` to a room id, e.g. \"codex-2\") or broadcast (omit `to`). You are identified automatically — your room id is stamped by the host, you can't post as someone else. A DIRECTED message is delivered to its recipient (pushed to its receiver, or typed into its pane when it's idle) and the response returns a `message_id` — track its fate with `terminite_message_status` (queued / delivered / floor_typed / read / cancelled / gave_up), or retract it with `terminite_message_cancel` if it's gone stale before it lands. Use `terminite_activities_list` to read what others posted to you.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["agent_message"], "description": "Currently only \"agent_message\"." },
                    "to": { "type": "string", "description": "Recipient's room id; omit to broadcast to the room." },
                    "text": { "type": "string", "description": "The message body." },
                },
                "required": ["kind", "text"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_room_who",
            "description":
                "Who is present in the room right now — every agent (and the human) currently connected, each with its host-assigned color id, even if they haven't said anything yet. Use this to see who you're sharing the session with. This is *attendance*; `terminite_activities_list` is what's been *done*.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_file_status",
            "description":
                "Before you edit a file the room might share, check whether another agent is currently working in it. Returns who holds it (if anyone) and how many seconds ago they last said so. Use this to avoid clobbering a peer's in-progress edit.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path of the file to check." },
                },
                "required": ["path"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_file_claim",
            "description":
                "Declare that you are about to work on a file, so other agents don't clobber it. ADVISORY — it never blocks you and the human always wins; it just makes \"someone is in this file\" visible. If a different agent already holds it, the response's `conflict` names them so you can coordinate or yield first. Claim before you Edit/Write a shared file; the claim expires on its own if you go idle. You are identified automatically.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path you're about to work on." },
                },
                "required": ["path"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_file_release",
            "description":
                "Release a file you claimed with terminite_file_claim, so the room knows it's free again. Optional — claims also expire on a timer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to release." },
                },
                "required": ["path"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_status",
            "description":
                "Set your terminite ROOM MODE — how the room may engage you. (These are terminite room modes, NOT your CLI's own auto/yolo/accept-edits mode; don't conflate them.) Four states: \"busy\" (terminite-busy) before a long, uninterruptible stretch (build, multi-step refactor, deploy) — the room will NOT wake you mid-process; directed messages queue and wait. \"available\" (terminite-available) when you're back at your prompt and ready. \"auto\" (terminite-auto) ONLY when the human asks you to enter terminite-auto mode — it's the fast lane: you give standing consent to be driven, so the room delivers messages promptly instead of waiting for you to look idle. While in terminite-auto you MUST cooperate: treat an injected room message as a live instruction and act on it; keep your turns short and responsive; if you hit something genuinely atomic, set \"busy\" first and \"available\" after — the brake still works. \"normal\" (terminite-normal) leaves the fast lane. Your mode shows in terminite_room_who. Everything expires on its own if you forget. You are identified automatically.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "state": {
                        "type": "string",
                        "enum": ["busy", "available", "auto", "normal"],
                        "description": "\"busy\" = hold my messages; \"available\" = deliver now; \"auto\" = fast lane, drive me promptly (only when the human asks); \"normal\" = leave the fast lane.",
                    },
                },
                "required": ["state"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_message_status",
            "description":
                "Did my message land? Pass the `message_id` that `terminite_activity_emit` returned, and get its delivery fate: \"queued\" (recorded, waiting for a delivery path), \"delivered\" (pushed to the recipient's live receiver), \"floor_typed\" (typed into its pane), \"read\" (the recipient confirmed it — processed), \"cancelled\" (you retracted it), or \"gave_up\" (re-delivery exhausted, it never acted). Absence of a reply is NOT evidence — check this instead of assuming.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message_id": { "type": "integer", "description": "The id terminite_activity_emit returned for the message." },
                },
                "required": ["message_id"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_outbox",
            "description":
                "Your sent directed messages and what happened to each — a glanceable receipt list (message_id, recipient, state, preview). Use it to spot anything stuck \"queued\" or \"gave_up\" (it never reached the recipient) without checking ids one by one. You are identified automatically.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "terminite_message_cancel",
            "description":
                "Retract a message you sent before it lands — for when the situation moved on and a stale instruction shouldn't reach the recipient. Pass the `message_id`. Works while it's still \"queued\" (pulled from the recipient's inbox, never delivered) or in the brief window after it was typed into their pane but before it submitted (the typed text is erased). Too late once it's \"delivered\"/\"read\". You can only cancel your OWN messages.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message_id": { "type": "integer", "description": "The id of your message to retract." },
                },
                "required": ["message_id"],
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
        "terminite_room_who" => proto_call(json!({
            "id": 1, "method": "room_who"
        })),
        "terminite_activities_list" => {
            let mut params = serde_json::Map::new();
            for key in ["actor", "to", "kind"] {
                if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
                    params.insert(key.to_string(), json!(v));
                }
            }
            proto_call(json!({ "id": 1, "method": "activities_list", "params": params }))
        }
        "terminite_activity_emit" => {
            let kind = require_str(args, "kind")?;
            let text = require_str(args, "text")?;
            let to = args.get("to").and_then(|v| v.as_str());
            // Host-attributed: the actor is whoever terminite spawned this MCP
            // server as (via --actor), never anything the agent can choose.
            let actor = ACTOR.get().cloned().unwrap_or_default();
            proto_call(json!({
                "id": 1, "method": "activity_emit",
                "params": { "actor": actor, "kind": kind, "to": to, "text": text }
            }))
        }
        "terminite_message_status" => {
            let message_id = require_int(args, "message_id")?;
            proto_call(json!({
                "id": 1, "method": "room_message_status",
                "params": { "message_id": message_id }
            }))
        }
        "terminite_outbox" => {
            let actor = ACTOR.get().cloned().unwrap_or_default();
            proto_call(json!({
                "id": 1, "method": "room_outbox", "params": { "actor": actor }
            }))
        }
        "terminite_message_cancel" => {
            let message_id = require_int(args, "message_id")?;
            // Sender-scoped: the host checks this actor authored the message.
            let actor = ACTOR.get().cloned().unwrap_or_default();
            proto_call(json!({
                "id": 1, "method": "room_message_cancel",
                "params": { "actor": actor, "message_id": message_id }
            }))
        }
        "terminite_file_status" => {
            let path = require_str(args, "path")?;
            proto_call(json!({
                "id": 1, "method": "file_status", "params": { "path": path }
            }))
        }
        "terminite_file_claim" => {
            let path = require_str(args, "path")?;
            let actor = ACTOR.get().cloned().unwrap_or_default();
            proto_call(json!({
                "id": 1, "method": "file_claim", "params": { "actor": actor, "path": path }
            }))
        }
        "terminite_file_release" => {
            let path = require_str(args, "path")?;
            let actor = ACTOR.get().cloned().unwrap_or_default();
            proto_call(json!({
                "id": 1, "method": "file_release", "params": { "actor": actor, "path": path }
            }))
        }
        "terminite_status" => {
            let state = require_str(args, "state")?;
            let actor = ACTOR.get().cloned().unwrap_or_default();
            proto_call(json!({
                "id": 1, "method": "room_status", "params": { "actor": actor, "state": state }
            }))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

// ── Proto socket bridge ──────────────────────────────────────────────────

/// Open a held presence connection and join the room. Returns the open stream
/// — keep it alive; its lifetime is our attendance — and the host-assigned
/// slug to go by. terminite sends nothing more on this connection; it just
/// blocks reading it, and sees EOF (→ leave) when we exit.
fn join_room(base: &str) -> Result<(UnixStream, String), String> {
    let path = socket_path()
        .ok_or_else(|| "no socket path — set $TERMINITE_SOCKET or $HOME".to_string())?;
    let mut stream = UnixStream::connect(&path).map_err(|e| {
        format!("can't connect to {} — is terminite running? ({e})", path.display())
    })?;
    // Tell terminite which pane we're in (if we're in one) so it can tint it.
    let mut params = json!({ "base": base });
    if let Some(pane) = std::env::var("TERMINITE_PANE").ok().and_then(|s| s.parse::<u64>().ok()) {
        params["pane"] = json!(pane);
    }
    let mut line = json!({ "id": 1, "method": "room_join", "params": params }).to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).map_err(|e| format!("join write: {e}"))?;
    // Read the Joined response off a clone so the original stays open + held.
    let clone = stream.try_clone().map_err(|e| format!("join clone: {e}"))?;
    let mut reader = BufReader::new(clone);
    let mut resp = String::new();
    reader.read_line(&mut resp).map_err(|e| format!("join read: {e}"))?;
    let parsed: Value = serde_json::from_str(resp.trim())
        .map_err(|e| format!("invalid join response: {e}"))?;
    if parsed.get("kind").and_then(|k| k.as_str()) == Some("error") {
        let msg = parsed.get("message").and_then(|m| m.as_str()).unwrap_or("(no message)");
        return Err(format!("terminite error: {msg}"));
    }
    let slug = parsed
        .get("actor")
        .and_then(|a| a.get("slug"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| "join response missing actor.slug".to_string())?
        .to_string();
    Ok((stream, slug))
}

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
