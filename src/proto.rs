//! Module protocol — terminite serves a Unix socket so out-of-process
//! partners (the AI, future modules) read the block model and subscribe
//! to live events. This is the bridge that makes Phase 2's "shared
//! coordinates" actually shared: the human sees `B7` in the gutter, and
//! through this surface the AI can ask for that same `B7` by name and
//! get its command + output back, structured.
//!
//! Wire: line-delimited JSON. One request per line; one response per
//! line. Subscription events ride the same line stream with `id: 0`.
//!
//! Lifecycle:
//! - Server starts when terminite boots, listens at
//!   `~/.terminite/socket` (or `$TERMINITE_SOCKET` if set).
//! - A stale socket file from a prior crashed run is removed at start.
//! - Single connected client v1 — a new connection replaces any prior;
//!   the prior's subscription is dropped.
//! - The `ProtoServer` handle removes the socket file on `Drop`, which
//!   runs when terminite exits.
//!
//! v1 surface (read-only):
//! - `list_tabs` → `{kind: "tabs", tabs: [{tab_id, title}]}`.
//! - `list_blocks {tab_id}` → `{kind: "blocks", blocks: [BlockInfo]}`.
//! - `get_block {tab_id, block_id}` → `{kind: "block", block: BlockData}`.
//! - `subscribe` → `{kind: "subscribed"}` then a stream of
//!   `{id: 0, kind: "event", event: ...}` lines.
//!
//! All bounds explicit at the source: per-line size, per-subscriber
//! queue depth, drop-on-overflow rather than buffer.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread;

use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

/// Per-line cap. Larger requests get an error response, no buffering.
/// The shape of v1 requests is tiny — this is a defensive ceiling, not
/// the working size.
pub const MAX_LINE_BYTES: usize = 256 * 1024;

/// Outstanding messages (responses + events) per connection. On
/// overflow the subscriber is dropped — terminite can't fall behind on
/// its own render path because a module stalled.
pub const SUB_QUEUE_CAP: usize = 1024;

#[derive(Deserialize, Debug)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A single outbound line — either a response (matches a request `id`)
/// or an event (id `0`). `payload` is flattened so the wire reads as
/// `{"id": 7, "kind": "block", "block": {...}}`.
#[derive(Serialize, Debug)]
pub struct OutMessage {
    pub id: u64,
    #[serde(flatten)]
    pub payload: OutPayload,
}

#[derive(Serialize, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutPayload {
    Tabs {
        tabs: Vec<TabInfo>,
    },
    Blocks {
        blocks: Vec<BlockInfo>,
        /// The block this tab's AI cursor is on, or null.
        cursor: Option<u32>,
    },
    Block {
        block: BlockData,
    },
    /// The export surface — every block of a tab as structured data
    /// (full command + output text), suitable for serialising the
    /// session as a markdown / JSON artifact.
    Export {
        tab_id: u64,
        blocks: Vec<BlockData>,
    },
    Subscribed,
    /// Acknowledgement for write methods (`set_tag`, `remove_tag`,
    /// `cursor_at`, `cursor_clear`). Empty on success.
    Ok,
    /// Single-snapshot of terminite's internal state, for the debug
    /// surface. Bounded — no streaming, no growth.
    Stats(StatsPayload),
    /// All discovered modules. Step 2a — registry only. Step 2b adds
    /// per-module connection state.
    Modules {
        modules: Vec<crate::modules::ModuleManifest>,
    },
    /// The room's activity stream — agent tool calls and messages, in
    /// time order. The lounge's read surface.
    Activities {
        activities: Vec<ActivityInfo>,
    },
    /// Response to `room_join` — the actor terminite assigned (a host-picked
    /// color makes the slug unique). The agent uses this slug thereafter.
    Joined {
        actor: ActorInfo,
    },
    /// The room roster — who is *present* right now (attendance), each with
    /// its host-assigned color. Distinct from `Activities` (what's been done).
    RoomWho {
        actors: Vec<ActorInfo>,
    },
    /// Result of `file_claim`: `conflict` is a *different* actor already in the
    /// file (advisory — the claim still succeeds; the human always wins). `null`
    /// conflict means you took it cleanly.
    FileClaim {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        conflict: Option<String>,
    },
    /// `file_status` — who, if anyone, currently holds a path (within the TTL).
    FileStatus {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        held_by: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        held_seconds_ago: Option<u64>,
    },
    /// `files` — every live claim in the room.
    Files {
        claims: Vec<FileClaimInfo>,
    },
    /// A directed room message PUSHED to a subscribed actor (the comms base's
    /// delivery — see `guide/comms-base.md`). Arrives unsolicited on a
    /// `room_subscribe` connection; the receiver surfaces it into the agent and
    /// acks with `room_ack {message_id}`.
    RoomMessage {
        message_id: u64,
        from: String,
        text: String,
    },
    Error {
        message: String,
    },
    Event(EventPayload),
}

#[derive(Serialize, Debug)]
pub struct StatsPayload {
    pub version: &'static str,
    pub peak_rss_bytes: Option<u64>,
    pub frame: FrameStats,
    pub tabs: Vec<TabStats>,
    pub subscriber_connected: bool,
}

#[derive(Serialize, Debug)]
pub struct FrameStats {
    pub frames_observed: u64,
    pub recent_samples: usize,
    pub avg_ms: f32,
    pub p99_ms: f32,
    pub max_ms: f32,
}

#[derive(Serialize, Debug)]
pub struct TabStats {
    pub tab_id: u64,
    pub title: String,
    pub cols: usize,
    pub rows: usize,
    pub blocks: usize,
    pub open_block: Option<u32>,
    pub cursor_block: Option<u32>,
    pub has_image: bool,
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventPayload {
    BlockOpened {
        tab_id: u64,
        block_id: u32,
    },
    BlockClosed {
        tab_id: u64,
        block_id: u32,
        exit_code: Option<i32>,
    },
}

#[derive(Serialize, Debug)]
pub struct TabInfo {
    pub tab_id: u64,
    pub title: String,
}

/// One activity on the wire. `actor` + `id` are the visible coordinate
/// (`codex-1.act-7`). `to`/`text` are present only for messages.
#[derive(Serialize, Debug)]
pub struct ActivityInfo {
    pub id: u64,
    pub actor: String,
    pub agent_name: String,
    pub kind: String,
    pub status: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// One live file claim on the wire — who is working in which path, and how
/// long ago they last said so.
#[derive(Serialize, Debug)]
pub struct FileClaimInfo {
    pub path: String,
    pub actor: String,
    pub seconds_ago: u64,
}

/// One present actor on the wire. `slug` is the host-assigned id
/// (`claude-gut-blue`); `base` is what the agent supplied (`claude-gut`);
/// `color`/`rgb` are the host-picked palette color that both names and tints.
#[derive(Serialize, Debug)]
pub struct ActorInfo {
    pub slug: String,
    pub base: String,
    pub color: String,
    pub rgb: [u8; 3],
    /// The pane the actor is in (`TERMINITE_PANE`), if it told us. `null`
    /// means it connected from outside a terminite pane — or its env didn't
    /// carry the var through, which is the thing to check if a tint is missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane: Option<u64>,
}

#[derive(Serialize, Debug)]
pub struct BlockInfo {
    pub id: u32,
    pub exit_code: Option<i32>,
    pub prompt_line: Option<i32>,
    pub command_end_line: Option<i32>,
    pub output_start_line: Option<i32>,
    pub output_end_line: Option<i32>,
    pub tags: Vec<String>,
}

#[derive(Serialize, Debug)]
pub struct BlockData {
    #[serde(flatten)]
    pub info: BlockInfo,
    pub command: String,
    pub output: String,
}

/// Server handle. Holds the socket file path so it can be cleaned up on
/// Drop. The accept thread and any connection threads are detached;
/// terminite exit kills them via process teardown.
pub struct ProtoServer {
    socket_path: PathBuf,
}

impl ProtoServer {
    /// Bind the socket and spawn the accept loop. Returns `None` if the
    /// socket can't be set up (no `HOME`, bind failed, …) — a missing
    /// IPC surface degrades gracefully rather than crashing terminite.
    pub fn start(proxy: EventLoopProxy<UserEvent>) -> Option<ProtoServer> {
        let socket_path = socket_path()?;
        if let Some(parent) = socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // A prior crashed run may have left the socket file in place;
        // remove it so the bind below doesn't `EADDRINUSE`.
        let _ = std::fs::remove_file(&socket_path);
        let listener = match UnixListener::bind(&socket_path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("terminite: proto bind failed: {e}");
                return None;
            }
        };
        // 0600 — only the current user can connect.
        if let Ok(meta) = std::fs::metadata(&socket_path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(&socket_path, perms);
        }
        thread::Builder::new()
            .name("terminite-proto".into())
            .spawn(move || accept_loop(listener, proxy))
            .ok()?;
        Some(ProtoServer { socket_path })
    }
}

impl Drop for ProtoServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TERMINITE_SOCKET") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".terminite/socket"))
}

/// Per-connection id. Lets the renderer map a `room_join` to the exact
/// connection and drop that actor's presence when the connection closes.
static CONN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// PID of the process on the other end of a Unix-socket connection (macOS
/// `LOCAL_PEERPID`). `None` if it can't be determined.
#[cfg(target_os = "macos")]
fn peer_pid(stream: &UnixStream) -> Option<i32> {
    use std::os::unix::io::AsRawFd;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let r = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (r == 0 && pid > 0).then_some(pid)
}

#[cfg(not(target_os = "macos"))]
fn peer_pid(_stream: &UnixStream) -> Option<i32> {
    None
}

fn accept_loop(listener: UnixListener, proxy: EventLoopProxy<UserEvent>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let conn_id = CONN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Tell the main thread a new connection arrived so it
                // drops any prior subscriber slot (v1 = single client).
                let _ = proxy.send_event(UserEvent::ProtoConnect);
                let p = proxy.clone();
                if thread::Builder::new()
                    .name("terminite-proto-conn".into())
                    .spawn(move || handle_connection(conn_id, stream, p))
                    .is_err()
                {
                    eprintln!("terminite: proto failed to spawn connection thread");
                }
            }
            Err(e) => {
                eprintln!("terminite: proto accept failed: {e}");
            }
        }
    }
}

/// One accepted connection: a reader on this thread parses requests and
/// hands them to the main thread via `UserEvent`; a writer thread
/// drains the response/event channel back out to the socket. Two
/// threads per connection so subscription events can flow while the
/// reader is blocked on the next request.
fn handle_connection(conn_id: u64, stream: UnixStream, proxy: EventLoopProxy<UserEvent>) {
    // The PID of the connecting process (the MCP server / CLI client). Lets the
    // renderer place an agent in its pane by walking this PID's ancestry to a
    // pane shell — pane detection that survives a CLI scrubbing TERMINITE_PANE
    // (codex does). Captured once; the connection is one process for its life.
    let peer_pid = peer_pid(&stream);
    let (out_tx, out_rx) = sync_channel::<OutMessage>(SUB_QUEUE_CAP);
    let writer_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let writer = thread::Builder::new()
        .name("terminite-proto-writer".into())
        .spawn(move || writer_loop(writer_stream, out_rx))
        .ok();

    let mut br = BufReader::with_capacity(MAX_LINE_BYTES, stream);
    let mut buf = String::new();
    loop {
        buf.clear();
        match br.read_line(&mut buf) {
            Ok(0) => break, // EOF
            Ok(_) => {
                if buf.len() > MAX_LINE_BYTES {
                    let _ = out_tx.try_send(OutMessage {
                        id: 0,
                        payload: OutPayload::Error {
                            message: format!(
                                "request exceeded MAX_LINE_BYTES ({MAX_LINE_BYTES})"
                            ),
                        },
                    });
                    break;
                }
                let trimmed = buf.trim_end();
                if trimmed.is_empty() {
                    continue;
                }
                let req: Request = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = out_tx.try_send(OutMessage {
                            id: 0,
                            payload: OutPayload::Error {
                                message: format!("parse: {e}"),
                            },
                        });
                        continue;
                    }
                };
                if proxy
                    .send_event(UserEvent::ProtoRequest {
                        conn_id,
                        peer_pid,
                        request: req,
                        out: out_tx.clone(),
                    })
                    .is_err()
                {
                    // EventLoop closed — terminite is exiting.
                    break;
                }
            }
            Err(_) => break,
        }
    }
    drop(out_tx);
    let _ = proxy.send_event(UserEvent::ProtoDisconnect { conn_id });
    if let Some(w) = writer {
        let _ = w.join();
    }
}

fn writer_loop(mut stream: UnixStream, rx: Receiver<OutMessage>) {
    while let Ok(msg) = rx.recv() {
        let line = match serde_json::to_string(&msg) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if stream.write_all(line.as_bytes()).is_err() {
            break;
        }
        if stream.write_all(b"\n").is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_response_shape() {
        let msg = OutMessage {
            id: 7,
            payload: OutPayload::Tabs {
                tabs: vec![TabInfo {
                    tab_id: 0,
                    title: "main".into(),
                }],
            },
        };
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains("\"id\":7"), "{s}");
        assert!(s.contains("\"kind\":\"tabs\""), "{s}");
        assert!(s.contains("\"tab_id\":0"), "{s}");
        assert!(s.contains("\"title\":\"main\""), "{s}");
    }

    #[test]
    fn event_shape() {
        let msg = OutMessage {
            id: 0,
            payload: OutPayload::Event(EventPayload::BlockClosed {
                tab_id: 0,
                block_id: 7,
                exit_code: Some(0),
            }),
        };
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains("\"id\":0"), "{s}");
        assert!(s.contains("\"kind\":\"event\""), "{s}");
        assert!(s.contains("\"event\":\"block_closed\""), "{s}");
        assert!(s.contains("\"block_id\":7"), "{s}");
        assert!(s.contains("\"exit_code\":0"), "{s}");
    }

    #[test]
    fn error_shape() {
        let msg = OutMessage {
            id: 9,
            payload: OutPayload::Error {
                message: "no such tab".into(),
            },
        };
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains("\"id\":9"), "{s}");
        assert!(s.contains("\"kind\":\"error\""), "{s}");
        assert!(s.contains("\"message\":\"no such tab\""), "{s}");
    }

    #[test]
    fn request_parse() {
        let req: Request = serde_json::from_str(
            r#"{"id":1,"method":"get_block","params":{"tab_id":0,"block_id":7}}"#,
        )
        .unwrap();
        assert_eq!(req.id, 1);
        assert_eq!(req.method, "get_block");
        assert_eq!(req.params.get("tab_id").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(req.params.get("block_id").and_then(|v| v.as_u64()), Some(7));
    }

    #[test]
    fn request_parse_no_params() {
        let req: Request = serde_json::from_str(r#"{"id":2,"method":"list_tabs"}"#).unwrap();
        assert_eq!(req.id, 2);
        assert_eq!(req.method, "list_tabs");
        assert!(req.params.is_null());
    }
}
