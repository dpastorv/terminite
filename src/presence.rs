//! The room roster — who is currently present, and their color.
//!
//! Presence is **attendance, not activity**: an actor is "here" from the
//! moment its faculty's MCP connection joins until that connection drops —
//! whether or not it has said anything. (Activity, in `activities.rs`, is the
//! separate stream of what's been *done*.)
//!
//! Identity is host-assigned, because terminite is the one process that sees
//! every connection and so is the only authority that can guarantee
//! uniqueness. The agent supplies only a **base** (its type, plus the profile
//! it was installed into — e.g. `claude-gut`); terminite assigns a unique
//! **color** from a fixed palette, and the visible slug is `<base>-<color>`
//! (`claude-gut-blue`). The color is the discriminator: an agent can't name
//! itself (every `claude` is identical) and a user-chosen name can't be
//! unique (you could spawn two), so the host picks the color. The same hue
//! names the actor in the room and tints its pane on screen.
//!
//! Colors **recycle** on leave, so the palette caps *concurrent* presence,
//! not lifetime; for a real room (a handful of agents) it never binds. If it
//! ever does, the slug stays unique via a numeric suffix.

// Wired into the proto room_join / room_who path and the renderer tint in
// follow-up increments; this allow covers the not-yet-called surface until
// then.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock milliseconds — the stamp for presence liveness. Monotonicity
/// isn't required (a small clock skew just nudges a linger window); we only
/// ever compare two of these for an elapsed-since.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// How long an actor stays in the roster after its connection drops. The floor
/// for CLIs that DON'T hold their MCP server open across calls (e.g. agy /
/// Antigravity spawns it per tool call): each call joins → acts → disconnects,
/// so without a grace window such an actor would never be "present" when
/// another agent polls. Held-connection CLIs (claude/codex) don't rely on this
/// — they stay live in `by_conn`. An actor that truly goes idle past the TTL
/// expires honestly. Bounded: at most one lingering entry per pane.
const LINGER_TTL_MS: u64 = 180_000;

/// One palette color: a human name + its RGB, so the same hue can both name
/// an actor in the room (`claude-blue`) and tint its pane on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorColor {
    pub name: &'static str,
    pub rgb: (u8, u8, u8),
}

/// The actor color palette — distinct, legible hues (One Dark family + a warm
/// pair). Order is allocation preference. Recycled on leave.
pub const ACTOR_PALETTE: &[ActorColor] = &[
    ActorColor { name: "blue", rgb: (97, 175, 239) },
    ActorColor { name: "green", rgb: (152, 195, 121) },
    ActorColor { name: "purple", rgb: (198, 120, 221) },
    ActorColor { name: "teal", rgb: (86, 182, 194) },
    ActorColor { name: "yellow", rgb: (229, 192, 123) },
    ActorColor { name: "red", rgb: (224, 108, 117) },
    ActorColor { name: "orange", rgb: (216, 140, 79) },
    ActorColor { name: "pink", rgb: (229, 132, 188) },
];

/// One present actor.
#[derive(Clone, Debug)]
pub struct Presence {
    /// Visible host-assigned id, e.g. `claude-gut-blue`.
    pub slug: String,
    /// What the agent supplied — type[+profile], e.g. `claude-gut`.
    pub base: String,
    pub color: ActorColor,
    /// The tab/pane the agent is running in (`TERMINITE_PANE`), if it told
    /// us — lets terminite tint that pane in this actor's color. `None` when
    /// the agent connected from outside a terminite pane.
    pub pane: Option<u64>,
    /// Monotonic join order, for stable display.
    pub seq: u64,
}

/// An actor whose connection has dropped but whose pane is still recent — kept
/// in the roster until `LINGER_TTL_MS` elapses, so a per-call CLI stays present
/// between its calls.
#[derive(Clone, Debug)]
struct Lingering {
    presence: Presence,
    last_seen_ms: u64,
}

/// Workspace-global presence roster — one per `Renderer`, like
/// `ActivityStore`. Keyed by the proto connection id so a dropped connection
/// removes exactly its actor.
#[derive(Default)]
pub struct Roster {
    by_conn: HashMap<u64, Presence>,
    /// Actors whose connection dropped but whose presence still lingers (keyed
    /// by pane). This is the floor for CLIs that don't hold the MCP connection
    /// open: presence = a live connection OR a pane seen within `LINGER_TTL_MS`.
    /// A re-join on the pane promotes it back to live (removes it here).
    lingering: HashMap<u64, Lingering>,
    /// The last color each pane wore, kept across disconnect. An agent that
    /// reconnects to the same pane reclaims its color instead of drifting to
    /// first-free — the pane is the stable identity, the socket connection is
    /// not. (Found by claude-green: color keyed by join order flickers on
    /// reconnect; the fix is to key stability on the pane.)
    pane_colors: HashMap<u64, &'static str>,
    next_seq: u64,
}

impl Roster {
    pub fn new() -> Self {
        Self::default()
    }

    /// A connection joined with `base` (and, if it's in a terminite pane, the
    /// pane id). Assigns the first free palette color (recycled colors
    /// included), builds a unique slug, records presence, and returns it so the
    /// caller can hand the slug + color back to the agent.
    pub fn join(&mut self, conn_id: u64, base: &str, pane: Option<u64>) -> Presence {
        let used: HashSet<&str> = self.by_conn.values().map(|p| p.color.name).collect();
        // Prefer the color this pane last wore, if it's still free — so an
        // agent reconnecting to the same pane keeps its color (pane is the
        // stable identity). Otherwise first free; if the palette is exhausted,
        // the front (slug stays unique via the suffix dedup below).
        let preferred = pane
            .and_then(|p| self.pane_colors.get(&p).copied())
            .filter(|name| !used.contains(name))
            .and_then(|name| ACTOR_PALETTE.iter().find(|c| c.name == name).copied());
        let color = preferred
            .or_else(|| ACTOR_PALETTE.iter().find(|c| !used.contains(c.name)).copied())
            .unwrap_or(ACTOR_PALETTE[0]);
        if let Some(p) = pane {
            self.pane_colors.insert(p, color.name);
            // This pane is live again — drop any lingering presence for it.
            self.lingering.remove(&p);
        }

        let mut slug = format!("{base}-{}", color.name);
        let existing: HashSet<&str> = self.by_conn.values().map(|p| p.slug.as_str()).collect();
        if existing.contains(slug.as_str()) {
            let mut n = 2;
            while existing.contains(format!("{slug}-{n}").as_str()) {
                n += 1;
            }
            slug = format!("{slug}-{n}");
        }

        let seq = self.next_seq;
        self.next_seq += 1;
        let presence = Presence {
            slug,
            base: base.to_string(),
            color,
            pane,
            seq,
        };
        self.by_conn.insert(conn_id, presence.clone());
        presence
    }

    /// A connection dropped. If the actor was in a pane, its presence **lingers**
    /// (so a per-call CLI stays present between calls) until `LINGER_TTL_MS`
    /// elapses or a re-join promotes it back to live. A paneless actor (joined
    /// from outside a terminite pane) has no anchor, so it leaves at once.
    /// Returns the departed presence (None if the connection never joined).
    pub fn leave(&mut self, conn_id: u64, now_ms: u64) -> Option<Presence> {
        let presence = self.by_conn.remove(&conn_id)?;
        if let Some(pane) = presence.pane {
            self.lingering.insert(
                pane,
                Lingering { presence: presence.clone(), last_seen_ms: now_ms },
            );
        }
        Some(presence)
    }

    /// Everyone present now, in join order: live connections, plus lingering
    /// actors whose pane was seen within `LINGER_TTL_MS` (and isn't already
    /// live). Stale lingerers are filtered out — presence stays honest.
    pub fn present(&self, now_ms: u64) -> Vec<Presence> {
        let live_panes: HashSet<u64> = self.by_conn.values().filter_map(|p| p.pane).collect();
        let mut v: Vec<Presence> = self.by_conn.values().cloned().collect();
        for (pane, l) in &self.lingering {
            if now_ms.saturating_sub(l.last_seen_ms) < LINGER_TTL_MS && !live_panes.contains(pane) {
                v.push(l.presence.clone());
            }
        }
        v.sort_by_key(|p| p.seq);
        v
    }

    /// The assigned slug for a connection — used to attribute its emits.
    pub fn slug_for(&self, conn_id: u64) -> Option<&str> {
        self.by_conn.get(&conn_id).map(|p| p.slug.as_str())
    }

    /// The color of the actor present in `pane`, if any — the renderer's tint
    /// lookup. Falls back to a lingering actor so a per-call CLI's tab keeps its
    /// tint between calls. First match wins (one agent per pane in practice).
    pub fn color_for_pane(&self, pane: u64) -> Option<ActorColor> {
        self.by_conn
            .values()
            .find(|p| p.pane == Some(pane))
            .map(|p| p.color)
            .or_else(|| self.lingering.get(&pane).map(|l| l.presence.color))
    }

    /// The slug of the actor present in `pane`, if any — used to attribute a
    /// tool call the see-half hook reports from that pane. Falls back to a
    /// lingering actor so a per-call CLI's tool calls still attribute correctly
    /// between its connections.
    pub fn slug_for_pane(&self, pane: u64) -> Option<String> {
        self.by_conn
            .values()
            .find(|p| p.pane == Some(pane))
            .map(|p| p.slug.clone())
            .or_else(|| self.lingering.get(&pane).map(|l| l.presence.slug.clone()))
    }

    pub fn is_empty(&self) -> bool {
        self.by_conn.is_empty()
    }

    /// The pane an actor (by slug) is in, if any — so the PTY floor knows which
    /// terminal to type a room message into. Checks live then lingering.
    pub fn pane_for_slug(&self, slug: &str) -> Option<u64> {
        self.by_conn
            .values()
            .find(|p| p.slug == slug)
            .and_then(|p| p.pane)
            .or_else(|| {
                self.lingering
                    .values()
                    .find(|l| l.presence.slug == slug)
                    .and_then(|l| l.presence.pane)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_distinct_colors_per_connection() {
        let mut r = Roster::new();
        let a = r.join(1, "claude", None);
        let b = r.join(2, "claude", None);
        assert_ne!(a.color.name, b.color.name, "two claudes get different colors");
        assert_ne!(a.slug, b.slug);
        assert_eq!(a.slug, "claude-blue");
        assert_eq!(b.slug, "claude-green");
    }

    #[test]
    fn base_carries_the_profile() {
        let mut r = Roster::new();
        let p = r.join(1, "claude-gut", None);
        assert_eq!(p.slug, "claude-gut-blue");
        assert_eq!(p.base, "claude-gut");
    }

    #[test]
    fn leaving_frees_the_color_for_reuse() {
        let mut r = Roster::new();
        let a = r.join(1, "claude", None); // blue
        let _b = r.join(2, "codex", None); // green
        assert_eq!(a.color.name, "blue");
        r.leave(1, 0); // paneless → leaves at once, frees blue
        let c = r.join(3, "kimi", None); // should reclaim blue, the first free
        assert_eq!(c.color.name, "blue");
    }

    #[test]
    fn present_is_join_ordered_and_excludes_the_departed() {
        let mut r = Roster::new();
        r.join(1, "claude", None);
        r.join(2, "codex", None);
        r.leave(1, 0); // paneless → gone immediately (no anchor to linger on)
        let present = r.present(0);
        assert_eq!(present.len(), 1);
        assert_eq!(present[0].base, "codex");
        assert!(r.slug_for(1).is_none());
        assert!(r.slug_for(2).is_some());
    }

    #[test]
    fn per_call_cli_lingers_in_pane_between_connections() {
        // agy's shape: it joins, acts, and disconnects on every call (it doesn't
        // hold the MCP socket). Without lingering it would never be present when
        // another agent polls. With it, the pane keeps it present across the gap.
        let mut r = Roster::new();
        let a = r.join(1, "agy", Some(2)); // agy-... in pane 2
        assert_eq!(a.pane, Some(2));
        r.leave(1, 1_000); // connection drops at t=1s
        // Shortly after (well within the TTL) another agent polls: agy is here.
        let present = r.present(2_000);
        assert_eq!(present.len(), 1, "agy lingers in pane 2 after disconnect");
        assert_eq!(present[0].base, "agy");
        // Tint + attribution survive the gap too.
        assert_eq!(r.color_for_pane(2).map(|c| c.name), Some(a.color.name));
        assert_eq!(r.slug_for_pane(2), Some(a.slug.clone()));
        // It re-joins on its next call (new conn_id, same pane) → promoted live,
        // same color (pane is the stable identity).
        let a2 = r.join(3, "agy", Some(2));
        assert_eq!(a2.color.name, a.color.name);
        assert_eq!(r.present(2_500).len(), 1, "no duplicate: live replaces lingering");
    }

    #[test]
    fn lingering_presence_expires_past_the_ttl() {
        let mut r = Roster::new();
        r.join(1, "agy", Some(2));
        r.leave(1, 0);
        assert_eq!(r.present(LINGER_TTL_MS - 1).len(), 1, "present just before TTL");
        assert_eq!(r.present(LINGER_TTL_MS + 1).len(), 0, "gone once idle past TTL");
    }

    #[test]
    fn color_for_pane_finds_the_actor_in_that_pane() {
        let mut r = Roster::new();
        let a = r.join(1, "claude", Some(3));
        r.join(2, "codex", Some(5));
        assert_eq!(r.color_for_pane(3).map(|c| c.name), Some(a.color.name));
        assert_eq!(r.color_for_pane(5).map(|c| c.name), Some("green"));
        assert!(r.color_for_pane(99).is_none(), "no actor in an empty pane");
        r.leave(1, 1_000); // claude's connection drops
        // The tint LINGERS so a per-call CLI's tab doesn't flicker between calls.
        assert_eq!(
            r.color_for_pane(3).map(|c| c.name),
            Some(a.color.name),
            "tint lingers after disconnect"
        );
        // A new actor taking that pane takes over the tint.
        let d = r.join(9, "qwen", Some(3));
        assert_eq!(
            r.color_for_pane(3).map(|c| c.name),
            Some(d.color.name),
            "a re-used pane shows the new actor's color"
        );
    }

    #[test]
    fn reconnect_to_same_pane_keeps_its_color() {
        // green's bug: an agent in pane 5 gets a color; another agent churns
        // in/out; the pane-5 agent reconnects (new conn_id) and must come back
        // the same color, not drift to first-free.
        let mut r = Roster::new();
        let a = r.join(1, "claude", Some(5)); // blue (first free)
        let b = r.join(2, "codex", Some(9)); // green
        assert_eq!(a.color.name, "blue");
        assert_eq!(b.color.name, "green");
        r.leave(1, 0); // pane 5 disconnects — blue frees
        let c = r.join(3, "kimi", Some(7)); // would grab blue (first free) by join order
        assert_eq!(c.color.name, "blue", "blue is free, kimi takes it");
        // pane 5's claude reconnects on a new conn_id. blue is now taken by
        // kimi, so it can't reclaim — but the point is it doesn't steal kimi's;
        // it gets a fresh free color, and pane 5 now remembers *that*.
        let a2 = r.join(4, "claude", Some(5));
        assert_ne!(a2.color.name, "blue");
        // Now drop and rejoin pane 5 with its color free — it must reclaim.
        let kept = a2.color.name;
        r.leave(4, 0);
        let a3 = r.join(5, "claude", Some(5));
        assert_eq!(a3.color.name, kept, "pane 5 reclaims the color it last wore");
    }

    #[test]
    fn slug_stays_unique_when_palette_exhausts() {
        let mut r = Roster::new();
        // Fill the whole palette with the same base, then one more.
        for conn in 0..ACTOR_PALETTE.len() as u64 {
            r.join(conn, "claude", None);
        }
        let extra = r.join(999, "claude", None);
        let slugs: HashSet<String> = r.present(0).iter().map(|p| p.slug.clone()).collect();
        assert_eq!(slugs.len(), ACTOR_PALETTE.len() + 1, "every slug is unique");
        assert!(extra.slug.ends_with("-2"), "exhaustion suffixes for uniqueness: {}", extra.slug);
    }
}
