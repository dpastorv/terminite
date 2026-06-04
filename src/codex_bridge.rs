//! Codex comms-base receiver — the second per-vendor last hop.
//!
//! Unlike claude's channel (terminite code that claude spawns and pushes INTO),
//! codex is reached by terminite acting as a CLIENT of codex's own app-server
//! daemon. The transport (cracked with Daniel, 2026-06-04) is **WebSocket over
//! the daemon's Unix control socket** — JSON-RPC in WS text frames, with the
//! `jsonrpc` field omitted. `codex app-server proxy` relays raw bytes (the WS
//! handshake + frames), it does NOT translate JSON-RPC, so we do the handshake
//! ourselves over a plain UnixStream via `tungstenite` (sync, no async runtime).
//! Ref: github.com/openai/codex codex-rs/app-server/README.md#protocol
//!
//! `terminite codex-bridge` runs alongside a codex launched in `--remote` mode:
//! it subscribes to the room for the codex actor, and on each pushed directed
//! message it `turn/start`s the codex's idle thread — the wake. Acks on success.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use tungstenite::{Message, WebSocket};

// ── codex daemon (WebSocket) ─────────────────────────────────────────────

fn codex_socket_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".codex/app-server-control/app-server-control.sock"))
}

/// Connect to the codex daemon over WebSocket and complete the initialize
/// handshake. Read timeout armed so reads don't block forever.
fn connect_daemon() -> Result<WebSocket<UnixStream>, String> {
    let sock = codex_socket_path().ok_or("no $HOME for the codex socket")?;
    let stream = UnixStream::connect(&sock).map_err(|e| {
        format!("can't connect to codex daemon {} — is `codex app-server daemon start` running? ({e})", sock.display())
    })?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(4)))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    let (mut ws, _resp) = tungstenite::client("ws://localhost/", stream)
        .map_err(|e| format!("ws handshake failed: {e}"))?;
    let init = json!({
        "id": 0, "method": "initialize",
        "params": { "clientInfo": { "name": "terminite", "version": "0.1.0" },
                    "capabilities": { "experimentalApi": true } }
    });
    ws.send(Message::Text(init.to_string().into())).map_err(|e| format!("send initialize: {e}"))?;
    ws.send(Message::Text(json!({ "method": "initialized" }).to_string().into()))
        .map_err(|e| format!("send initialized: {e}"))?;
    Ok(ws)
}

/// Read frames until a response with the given id arrives (skipping the
/// interleaved notifications), or the read times out.
fn read_result(ws: &mut WebSocket<UnixStream>, id: u64) -> Option<Value> {
    for _ in 0..40 {
        match ws.read() {
            Ok(Message::Text(t)) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                        return v.get("result").cloned();
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
    None
}

/// Pick the codex thread to wake: a currently-LOADED (live) session. Verified
/// live — `thread/list` returns *persisted* conversations (mostly `notLoaded`),
/// so the active session a human is sitting in is reached via
/// `thread/loaded/list`, which returns live thread ids (most recent first). We
/// take the first. (With multiple loaded codex sessions the daemon doesn't map
/// actor→thread, so this is best-effort; the clean single-codex case is exact.)
fn find_live_thread(ws: &mut WebSocket<UnixStream>) -> Option<String> {
    ws.send(Message::Text(
        json!({ "id": 1, "method": "thread/loaded/list", "params": {} }).to_string().into(),
    ))
    .ok()?;
    let result = read_result(ws, 1)?;
    result
        .get("data")?
        .as_array()?
        .first()?
        .as_str()
        .map(String::from)
}

/// Wake codex: turn/start the idle thread with `text`. Returns Ok only if a
/// turn was actually started (so the caller acks).
fn wake_codex(text: &str) -> Result<(), String> {
    let mut ws = connect_daemon()?;
    let thread_id = find_live_thread(&mut ws).ok_or("no live codex thread to wake (is codex `--remote` to the daemon?)")?;
    let req = json!({
        "id": 2, "method": "turn/start",
        "params": { "threadId": thread_id,
                    "input": [{ "type": "text", "text": text, "text_elements": [] }] }
    });
    ws.send(Message::Text(req.to_string().into())).map_err(|e| format!("send turn/start: {e}"))?;
    // Confirm the turn started (a turn/start result or a turn/started notice).
    match read_result(&mut ws, 2) {
        Some(_) => Ok(()),
        None => Err("turn/start sent but no confirmation".into()),
    }
}

// ── terminite room (JSON-line proto) ─────────────────────────────────────

fn terminite_socket() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TERMINITE_SOCKET") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".terminite/socket"))
}

/// One-shot request to terminite's socket; returns the parsed response line.
fn troom_call(req: &Value) -> Option<Value> {
    let path = terminite_socket()?;
    let mut stream = UnixStream::connect(&path).ok()?;
    let mut line = req.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).ok()?;
    let mut reader = BufReader::new(stream);
    let mut resp = String::new();
    reader.read_line(&mut resp).ok()?;
    serde_json::from_str(resp.trim()).ok()
}

/// Resolve the codex room slug: the actor in our pane (`TERMINITE_PANE`), else
/// the first actor whose base is `codex`. Retries — the lounge MCP may still be
/// joining when the bridge starts.
fn resolve_codex_slug() -> Option<String> {
    let pane = std::env::var("TERMINITE_PANE").ok().and_then(|s| s.parse::<u64>().ok());
    for _ in 0..10 {
        if let Some(v) = troom_call(&json!({ "id": 1, "method": "room_who" })) {
            if let Some(actors) = v.get("actors").and_then(|a| a.as_array()) {
                // Prefer the actor in our pane; else any codex.
                let pick = actors
                    .iter()
                    .find(|a| pane.is_some() && a.get("pane").and_then(|p| p.as_u64()) == pane)
                    .or_else(|| actors.iter().find(|a| a.get("base").and_then(|b| b.as_str()) == Some("codex")));
                if let Some(slug) = pick.and_then(|a| a.get("slug").and_then(|s| s.as_str())) {
                    return Some(slug.to_string());
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    None
}

/// `terminite codex-bridge` — subscribe to the room for the codex actor and
/// `turn/start` its idle thread on each pushed message. Run alongside a codex
/// launched in `--remote` mode (so its conversation is a daemon thread).
pub fn run_codex_bridge() -> ExitCode {
    let Some(slug) = resolve_codex_slug() else {
        eprintln!("codex bridge: couldn't find the codex room actor (is codex in a terminite pane with the lounge faculty?)");
        return ExitCode::from(1);
    };
    let Some(path) = terminite_socket() else {
        eprintln!("codex bridge: no terminite socket");
        return ExitCode::from(1);
    };
    let Ok(mut sub) = UnixStream::connect(&path) else {
        eprintln!("codex bridge: can't connect to terminite — is it running?");
        return ExitCode::from(1);
    };
    let mut line = json!({ "id": 1, "method": "room_subscribe", "params": { "actor": slug } }).to_string();
    line.push('\n');
    if sub.write_all(line.as_bytes()).is_err() {
        eprintln!("codex bridge: subscribe failed");
        return ExitCode::from(1);
    }
    eprintln!("codex bridge: waking {slug} on room messages (via the codex app-server daemon)");
    let reader = BufReader::new(sub);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v.get("kind").and_then(|k| k.as_str()) != Some("room_message") {
            continue;
        }
        let from = v.get("from").and_then(|x| x.as_str()).unwrap_or("");
        let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
        let msg = format!("[terminite room — message from {from}] {text}");
        match wake_codex(&msg) {
            Ok(()) => {
                if let Some(mid) = v.get("message_id").and_then(|x| x.as_u64()) {
                    let _ = troom_call(&json!({ "id": 1, "method": "room_ack", "params": { "message_id": mid } }));
                }
            }
            // Don't ack: codex was busy / unreachable → terminite re-delivers.
            Err(e) => eprintln!("codex bridge: {e}"),
        }
    }
    ExitCode::SUCCESS
}
