//! The activity Model — the room's fine-grained inner stream.
//!
//! An activity is one addressable thing an agent did or said: a tool call,
//! or a message (directed at another actor, or broadcast to the room). The
//! store is workspace-global (one per `Renderer`, NOT per-tab like
//! `BlockStore`) because the whole point is cross-pane visibility — an agent
//! in one pane seeing what an agent in another did.
//!
//! Identity rides on the visible coordinate: `actor` is the host-assigned
//! session slug (`codex-1`, ...), never self-declared. Ids are global and
//! monotonic (`act-N`). This module is the substrate for the lounge router
//! (see `guide/lounge-experiment.md`); it knows nothing about routing — it
//! just records and queries.

// The message path (emit_message / list / get) is wired and live. The
// ToolCall "see" half (ToolCall kind + update_status) is built but not yet
// emitted — automatic tool-call emission (via agent-CLI hooks) is a
// fast-follow. This allow
// covers only that not-yet-exercised path; remove it when the see-half lands.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::time::SystemTime;

/// Workspace-wide cap. Oldest *closed* activity evicts first. Each entry is
/// small (a few strings), so this bounds memory at a few MB worst case.
pub const MAX_ACTIVITIES: usize = 10_000;
/// Cap on a message's text / a tool call's title. Truncated past this.
pub const MAX_TEXT_LEN: usize = 8 * 1024;
pub const MAX_TITLE_LEN: usize = 256;

pub type ActivityId = u64;

/// What an activity *is*. `ToolCall` is the automatic "see" half (emitted
/// from agent-CLI tool-call hooks); `AgentMessage` is the "talk" half (emitted
/// explicitly by an agent via `terminite_activity_emit`).
#[derive(Clone, Debug)]
pub enum ActivityKind {
    ToolCall { tool: String },
    /// `to: None` is a room broadcast; `to: Some(slug)` is directed.
    AgentMessage { to: Option<String>, text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ActivityStatus {
    /// A terminal status closes the activity (stamps `closed_at`).
    fn is_terminal(self) -> bool {
        matches!(self, ActivityStatus::Completed | ActivityStatus::Failed)
    }
}

/// One recorded activity. `actor` + `id` are the visible coordinate
/// (`codex-1.act-7`); everything else is payload.
#[derive(Clone, Debug)]
pub struct Activity {
    pub id: ActivityId,
    pub actor: String,
    pub agent_name: String,
    pub kind: ActivityKind,
    pub status: ActivityStatus,
    pub title: String,
    pub opened_at: SystemTime,
    pub closed_at: Option<SystemTime>,
}

impl Activity {
    /// Wire-friendly kind tag, used by the proto/MCP filter layer.
    pub fn kind_str(&self) -> &'static str {
        match self.kind {
            ActivityKind::ToolCall { .. } => "tool_call",
            ActivityKind::AgentMessage { .. } => "agent_message",
        }
    }

    /// The addressee of a directed message, if this is one. `None` for tool
    /// calls and for broadcast messages.
    pub fn message_to(&self) -> Option<&str> {
        match &self.kind {
            ActivityKind::AgentMessage { to, .. } => to.as_deref(),
            _ => None,
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed_at.is_some()
    }

    /// The body of a message activity (for re-delivery), `None` for tool calls.
    pub fn message_text(&self) -> Option<&str> {
        match &self.kind {
            ActivityKind::AgentMessage { text, .. } => Some(text),
            _ => None,
        }
    }
}

/// Workspace-global store of activities, oldest-first by id.
pub struct ActivityStore {
    items: VecDeque<Activity>,
    next_id: ActivityId,
}

impl Default for ActivityStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityStore {
    pub fn new() -> Self {
        Self {
            items: VecDeque::new(),
            next_id: 1,
        }
    }

    /// Record an activity. Caps text/title, stamps `closed_at` if the status
    /// is terminal, evicts past the cap. Returns the assigned id.
    pub fn emit(
        &mut self,
        actor: impl Into<String>,
        agent_name: impl Into<String>,
        kind: ActivityKind,
        status: ActivityStatus,
        title: impl Into<String>,
    ) -> ActivityId {
        let id = self.next_id;
        self.next_id += 1;
        let now = SystemTime::now();
        let closed_at = status.is_terminal().then_some(now);
        self.items.push_back(Activity {
            id,
            actor: actor.into(),
            agent_name: agent_name.into(),
            kind: cap_kind(kind),
            status,
            title: truncate(&title.into(), MAX_TITLE_LEN),
            opened_at: now,
            closed_at,
        });
        self.evict();
        id
    }

    /// Convenience for the "talk" path: an agent message is complete the
    /// moment it's sent. `to: None` broadcasts to the room.
    pub fn emit_message(
        &mut self,
        actor: impl Into<String>,
        agent_name: impl Into<String>,
        to: Option<String>,
        text: String,
    ) -> ActivityId {
        let title = match &to {
            Some(t) => format!("→ {t}"),
            None => "→ room".to_string(),
        };
        self.emit(
            actor,
            agent_name,
            ActivityKind::AgentMessage { to, text },
            ActivityStatus::Completed,
            title,
        )
    }

    /// Move an activity's status (e.g. a tool call Pending → Completed).
    /// Stamps `closed_at` when it reaches a terminal status. Returns `false`
    /// if the id is unknown.
    pub fn update_status(&mut self, id: ActivityId, status: ActivityStatus) -> bool {
        let Some(a) = self.items.iter_mut().find(|a| a.id == id) else {
            return false;
        };
        a.status = status;
        if status.is_terminal() && a.closed_at.is_none() {
            a.closed_at = Some(SystemTime::now());
        }
        true
    }

    pub fn get(&self, id: ActivityId) -> Option<&Activity> {
        self.items.iter().find(|a| a.id == id)
    }

    /// Every activity, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Activity> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Filtered view. All filters are AND-ed; `None` means "don't filter".
    /// `to: Some(slug)` returns **only directed messages to that slug** —
    /// broadcasts (`to: None`) and tool calls are excluded. That is the E3
    /// inbox invariant: "for me" is distinguishable from "for the room".
    pub fn list(
        &self,
        actor: Option<&str>,
        to: Option<&str>,
        kind: Option<&str>,
    ) -> Vec<&Activity> {
        self.items
            .iter()
            .filter(|a| {
                actor.is_none_or(|x| a.actor == x)
                    && kind.is_none_or(|k| a.kind_str() == k)
                    && to.is_none_or(|t| a.message_to() == Some(t))
            })
            .collect()
    }

    /// Evict oldest **closed** first; only drop an open activity as a last
    /// resort (store full of in-flight work — shouldn't happen at this cap).
    fn evict(&mut self) {
        while self.items.len() > MAX_ACTIVITIES {
            if let Some(pos) = self.items.iter().position(|a| a.is_closed()) {
                self.items.remove(pos);
            } else {
                self.items.pop_front();
            }
        }
    }
}

/// Truncate at a char boundary with a visible marker, so a runaway payload
/// can't blow the cap. Bytes, not chars, since the cap is about memory.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [{} bytes truncated]", &s[..end], s.len() - end)
}

/// Apply the text cap to whatever string a kind carries.
fn cap_kind(kind: ActivityKind) -> ActivityKind {
    match kind {
        ActivityKind::AgentMessage { to, text } => ActivityKind::AgentMessage {
            to,
            text: truncate(&text, MAX_TEXT_LEN),
        },
        ActivityKind::ToolCall { tool } => ActivityKind::ToolCall {
            tool: truncate(&tool, MAX_TITLE_LEN),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(s: &mut ActivityStore, from: &str, to: Option<&str>, text: &str) -> ActivityId {
        s.emit_message(from, "Codex", to.map(String::from), text.to_string())
    }

    #[test]
    fn ids_are_ascending_and_global() {
        let mut s = ActivityStore::new();
        let a = msg(&mut s, "codex-1", None, "hi");
        let b = msg(&mut s, "codex-2", None, "yo");
        assert_eq!((a, b), (1, 2));
    }

    #[test]
    fn a_message_is_closed_on_send() {
        let mut s = ActivityStore::new();
        let id = msg(&mut s, "codex-1", Some("codex-2"), "look at this");
        let a = s.get(id).unwrap();
        assert_eq!(a.status, ActivityStatus::Completed);
        assert!(a.is_closed());
        assert_eq!(a.message_to(), Some("codex-2"));
    }

    #[test]
    fn to_self_filter_returns_directed_and_excludes_broadcast() {
        // The E3 invariant, now in real code.
        let mut s = ActivityStore::new();
        msg(&mut s, "codex-1", Some("codex-2"), "for you");
        msg(&mut s, "codex-1", None, "for the room"); // broadcast
        msg(&mut s, "codex-1", Some("codex-3"), "for someone else");

        let inbox = s.list(None, Some("codex-2"), Some("agent_message"));
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].message_to(), Some("codex-2"));
    }

    #[test]
    fn actor_filter_scopes_to_one_agent() {
        let mut s = ActivityStore::new();
        msg(&mut s, "codex-1", None, "a");
        msg(&mut s, "codex-2", None, "b");
        msg(&mut s, "codex-1", None, "c");
        assert_eq!(s.list(Some("codex-1"), None, None).len(), 2);
        assert_eq!(s.list(Some("codex-2"), None, None).len(), 1);
    }

    #[test]
    fn tool_call_status_transitions_and_closes() {
        let mut s = ActivityStore::new();
        let id = s.emit(
            "codex-1",
            "Codex",
            ActivityKind::ToolCall { tool: "read_file".into() },
            ActivityStatus::Pending,
            "Read src/main.rs",
        );
        assert!(!s.get(id).unwrap().is_closed());
        assert_eq!(s.get(id).unwrap().kind_str(), "tool_call");
        assert!(s.update_status(id, ActivityStatus::Completed));
        assert!(s.get(id).unwrap().is_closed());
        assert!(!s.update_status(9999, ActivityStatus::Completed)); // unknown id
    }

    #[test]
    fn eviction_drops_oldest_closed_first() {
        let mut s = ActivityStore::new();
        for i in 0..(MAX_ACTIVITIES + 5) {
            msg(&mut s, "codex-1", None, &format!("m{i}"));
        }
        assert_eq!(s.len(), MAX_ACTIVITIES);
        // The first five ids aged out; the store starts at id 6.
        assert!(s.get(1).is_none());
        assert!(s.get(5).is_none());
        assert!(s.get(6).is_some());
    }

    #[test]
    fn open_activities_survive_when_closed_ones_can_evict() {
        let mut s = ActivityStore::new();
        // One open (in-flight) tool call up front...
        let open = s.emit(
            "codex-1",
            "Codex",
            ActivityKind::ToolCall { tool: "bash".into() },
            ActivityStatus::InProgress,
            "long build",
        );
        // ...then overflow the cap with closed messages.
        for i in 0..(MAX_ACTIVITIES + 5) {
            msg(&mut s, "codex-2", None, &format!("m{i}"));
        }
        assert_eq!(s.len(), MAX_ACTIVITIES);
        // The open one outlived the eviction (closed dropped first).
        assert!(s.get(open).is_some());
        assert_eq!(s.get(open).unwrap().status, ActivityStatus::InProgress);
    }

    #[test]
    fn oversized_text_is_truncated() {
        let mut s = ActivityStore::new();
        let huge = "x".repeat(MAX_TEXT_LEN + 1000);
        let id = msg(&mut s, "codex-1", None, &huge);
        if let ActivityKind::AgentMessage { text, .. } = &s.get(id).unwrap().kind {
            assert!(text.len() < MAX_TEXT_LEN + 100);
            assert!(text.contains("bytes truncated"));
        } else {
            panic!("expected AgentMessage");
        }
    }
}
