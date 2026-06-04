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

impl Renderer {
    /// A new module connected — drop any prior subscriber (v1 = single
    /// client; the new one wins).
    pub fn handle_proto_connect(&mut self) {
        self.proto_subscriber = None;
    }

    /// A proto connection closed — clear the subscriber slot and drop the
    /// connection's room presence (if it had joined). `room_who` reflects the
    /// departure immediately.
    pub fn handle_proto_disconnect(&mut self, conn_id: u64) {
        self.proto_subscriber = None;
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
                self.proto_subscriber = Some(out.clone());
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
                if actor.is_empty() {
                    crate::proto::OutPayload::Error {
                        message: "room_subscribe: missing actor".into(),
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
        let to = params.get("to").and_then(|v| v.as_str()).map(String::from);
        let agent_name = agent_name_from_slug(actor);
        let id = self
            .activities
            .emit_message(actor.to_string(), agent_name, to.clone(), text.clone());
        // The comms base: a directed message becomes deliverable. Queue it as
        // pending (un-consumed) for catch-up, then push now if a receiver is
        // live. Records stay the read fallback either way.
        if let Some(target) = to {
            let q = self.pending.entry(target.clone()).or_default();
            q.push(id);
            if q.len() > PENDING_CAP {
                let excess = q.len() - PENDING_CAP;
                q.drain(0..excess);
            }
            self.push_room_message(&target, id, actor, &text);
        }
        crate::proto::OutPayload::Ok
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
    /// the comms base — `guide/comms-base.md`.
    fn push_room_message(&mut self, target: &str, message_id: u64, from: &str, text: &str) {
        // The human's opt-out: delivery off → record-only (the message still
        // queued as pending, so toggling back on catches up). The single choke
        // point for both direct push and catch-up.
        if !self.config.comms_delivery {
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
        let dead = match self.room_subscribers.get(target) {
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
            None => false,
        };
        if dead {
            self.room_subscribers.remove(target);
        }
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
        crate::proto::OutPayload::FileClaim { path: path.to_string(), conflict }
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
                .map(presence_to_info)
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
        let Some(out) = self.proto_subscriber.as_ref() else { return };
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
    }
}

/// Cosmetic display name from a slug: `codex-1` → `Codex`.
fn agent_name_from_slug(slug: &str) -> String {
    let base = slug.split('-').next().unwrap_or(slug);
    let mut chars = base.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => slug.to_string(),
    }
}


