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

/// Workspace-global presence roster — one per `Renderer`, like
/// `ActivityStore`. Keyed by the proto connection id so a dropped connection
/// removes exactly its actor.
#[derive(Default)]
pub struct Roster {
    by_conn: HashMap<u64, Presence>,
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
        // First free color; if the palette is exhausted, reuse from the front
        // (slug stays unique via the suffix dedup below).
        let color = ACTOR_PALETTE
            .iter()
            .find(|c| !used.contains(c.name))
            .copied()
            .unwrap_or(ACTOR_PALETTE[0]);

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

    /// A connection dropped — remove its actor, freeing the color. Returns the
    /// departed presence (None if the connection never joined the room).
    pub fn leave(&mut self, conn_id: u64) -> Option<Presence> {
        self.by_conn.remove(&conn_id)
    }

    /// Everyone present now, in join order.
    pub fn present(&self) -> Vec<Presence> {
        let mut v: Vec<Presence> = self.by_conn.values().cloned().collect();
        v.sort_by_key(|p| p.seq);
        v
    }

    /// The assigned slug for a connection — used to attribute its emits.
    pub fn slug_for(&self, conn_id: u64) -> Option<&str> {
        self.by_conn.get(&conn_id).map(|p| p.slug.as_str())
    }

    /// The color of the actor present in `pane`, if any — the renderer's tint
    /// lookup. First match wins (one agent per pane in practice).
    pub fn color_for_pane(&self, pane: u64) -> Option<ActorColor> {
        self.by_conn
            .values()
            .find(|p| p.pane == Some(pane))
            .map(|p| p.color)
    }

    /// The slug of the actor present in `pane`, if any — used to attribute a
    /// tool call the see-half hook reports from that pane.
    pub fn slug_for_pane(&self, pane: u64) -> Option<String> {
        self.by_conn
            .values()
            .find(|p| p.pane == Some(pane))
            .map(|p| p.slug.clone())
    }

    pub fn is_empty(&self) -> bool {
        self.by_conn.is_empty()
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
        r.leave(1); // frees blue
        let c = r.join(3, "kimi", None); // should reclaim blue, the first free
        assert_eq!(c.color.name, "blue");
    }

    #[test]
    fn present_is_join_ordered_and_excludes_the_departed() {
        let mut r = Roster::new();
        r.join(1, "claude", None);
        r.join(2, "codex", None);
        r.leave(1);
        let present = r.present();
        assert_eq!(present.len(), 1);
        assert_eq!(present[0].base, "codex");
        assert!(r.slug_for(1).is_none());
        assert!(r.slug_for(2).is_some());
    }

    #[test]
    fn color_for_pane_finds_the_actor_in_that_pane() {
        let mut r = Roster::new();
        let a = r.join(1, "claude", Some(3));
        r.join(2, "codex", Some(5));
        assert_eq!(r.color_for_pane(3).map(|c| c.name), Some(a.color.name));
        assert_eq!(r.color_for_pane(5).map(|c| c.name), Some("green"));
        assert!(r.color_for_pane(99).is_none(), "no actor in an empty pane");
        r.leave(1);
        assert!(r.color_for_pane(3).is_none(), "departed actor frees its pane");
    }

    #[test]
    fn slug_stays_unique_when_palette_exhausts() {
        let mut r = Roster::new();
        // Fill the whole palette with the same base, then one more.
        for conn in 0..ACTOR_PALETTE.len() as u64 {
            r.join(conn, "claude", None);
        }
        let extra = r.join(999, "claude", None);
        let slugs: HashSet<String> = r.present().iter().map(|p| p.slug.clone()).collect();
        assert_eq!(slugs.len(), ACTOR_PALETTE.len() + 1, "every slug is unique");
        assert!(extra.slug.ends_with("-2"), "exhaustion suffixes for uniqueness: {}", extra.slug);
    }
}
