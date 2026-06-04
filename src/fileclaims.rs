//! File claims — the advisory "I'm working on this file" layer, so the room's
//! residents co-edit without clobbering each other and the human stops being
//! the manual lock (waiting between launches by hand).
//!
//! **Advisory, never a hard lock.** A claim *informs*; it does not block. The
//! human always wins, and any actor can still write any file — the room just
//! makes "someone is in this file" visible so a good citizen yields. This is
//! the queue Daniel asked for: state your action clearly, see who's already in
//! a file, coordinate before you clobber.
//!
//! **TTL-bounded.** A claim expires after `CLAIM_TTL_MS`, so a crashed or idle
//! actor never holds a file forever (system-impact: presence and claims both
//! self-heal on a timer rather than relying on a clean release).

use std::collections::HashMap;

/// How long a claim stays live without being refreshed. Matches the presence
/// floor: long enough to bridge an actor's working rhythm, short enough that an
/// abandoned claim clears on its own.
const CLAIM_TTL_MS: u64 = 120_000;

/// One actor's live claim on a path.
#[derive(Clone, Debug)]
pub struct Claim {
    /// The claiming actor's room slug, e.g. `codex-green`.
    pub slug: String,
    pub claimed_at_ms: u64,
}

/// Workspace-global file-claim registry — one per `Renderer`, like the roster
/// and the activity store. Keyed by path.
#[derive(Default)]
pub struct FileClaims {
    by_path: HashMap<String, Claim>,
}

impl FileClaims {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to claim `path` for `slug`. **First-come wins:** if a *different*
    /// live actor already holds it, returns that holder (the claim is REFUSED —
    /// the caller should wait, not clobber) and leaves the holder's claim intact.
    /// If it's free or already yours, you get it (timer refreshed) and it returns
    /// `None`.
    pub fn claim(&mut self, slug: &str, path: &str, now_ms: u64) -> Option<Claim> {
        // Prune expired claims FIRST. Paths are unbounded, so without this
        // `by_path` would grow one entry per distinct path ever claimed, forever
        // — the unbounded-allocation class behind past crashes. After this the
        // map holds only live claims (bounded by concurrent activity).
        self.by_path
            .retain(|_, c| now_ms.saturating_sub(c.claimed_at_ms) < CLAIM_TTL_MS);
        // First-come wins (the salt model): if a DIFFERENT actor already holds
        // it, REFUSE — return the holder, leave their claim intact. The caller
        // should wait, not clobber. (Still advisory: it doesn't physically stop
        // an edit, and the human is never governed by claims.) Your own claim
        // just refreshes the timer.
        if let Some(holder) = self.holder(path, now_ms) {
            if holder.slug != slug {
                return Some(holder.clone());
            }
        }
        self.by_path.insert(
            path.to_string(),
            Claim { slug: slug.to_string(), claimed_at_ms: now_ms },
        );
        None
    }

    /// Number of claims currently tracked (live + not-yet-pruned). For the
    /// system-impact test that the map doesn't grow without bound.
    #[cfg(test)]
    pub fn tracked(&self) -> usize {
        self.by_path.len()
    }

    /// Release `path` iff `slug` currently holds it. Returns whether it did.
    pub fn release(&mut self, slug: &str, path: &str) -> bool {
        if self.by_path.get(path).map(|c| c.slug.as_str()) == Some(slug) {
            self.by_path.remove(path);
            true
        } else {
            false
        }
    }

    /// The current live holder of `path`, if any (claimed within the TTL).
    pub fn holder(&self, path: &str, now_ms: u64) -> Option<&Claim> {
        self.by_path
            .get(path)
            .filter(|c| now_ms.saturating_sub(c.claimed_at_ms) < CLAIM_TTL_MS)
    }

    /// All live claims, newest first — for `terminite files` / a room overview.
    pub fn live(&self, now_ms: u64) -> Vec<(String, Claim)> {
        let mut v: Vec<(String, Claim)> = self
            .by_path
            .iter()
            .filter(|(_, c)| now_ms.saturating_sub(c.claimed_at_ms) < CLAIM_TTL_MS)
            .map(|(p, c)| (p.clone(), c.clone()))
            .collect();
        v.sort_by(|a, b| b.1.claimed_at_ms.cmp(&a.1.claimed_at_ms));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_come_wins_and_a_second_claimer_is_refused() {
        let mut c = FileClaims::new();
        // codex takes it first.
        assert!(c.claim("codex-green", "src/x.rs", 1_000).is_none(), "first claim is unconflicted");
        // claude tries the same file — REFUSED, told codex holds it (the salt model).
        let conflict = c.claim("claude-blue", "src/x.rs", 2_000);
        assert_eq!(conflict.map(|p| p.slug), Some("codex-green".to_string()));
        // codex is STILL the holder — claude did not take it.
        assert_eq!(c.holder("src/x.rs", 2_500).map(|h| h.slug.clone()), Some("codex-green".into()));
        // Once codex releases, claude can take it.
        assert!(c.release("codex-green", "src/x.rs"));
        assert!(c.claim("claude-blue", "src/x.rs", 3_000).is_none(), "free now → claude gets it");
    }

    #[test]
    fn same_actor_reclaim_is_not_a_conflict() {
        let mut c = FileClaims::new();
        c.claim("codex-green", "f", 0);
        assert!(c.claim("codex-green", "f", 1_000).is_none(), "refreshing your own claim is not a conflict");
    }

    #[test]
    fn release_only_by_the_holder() {
        let mut c = FileClaims::new();
        c.claim("codex-green", "f", 0);
        assert!(!c.release("claude-blue", "f"), "a non-holder can't release someone else's claim");
        assert!(c.release("codex-green", "f"));
        assert!(c.holder("f", 1).is_none(), "released file is free");
    }

    #[test]
    fn claims_expire_past_the_ttl() {
        let mut c = FileClaims::new();
        c.claim("codex-green", "f", 0);
        assert!(c.holder("f", CLAIM_TTL_MS - 1).is_some(), "live just before the TTL");
        assert!(c.holder("f", CLAIM_TTL_MS + 1).is_none(), "expired past the TTL — no stale lock");
        assert_eq!(c.live(CLAIM_TTL_MS + 1).len(), 0);
    }

    #[test]
    fn expired_claims_are_pruned_not_just_filtered() {
        // The leak guard: claiming many distinct paths over time must NOT grow
        // the map without bound — expired ones are removed, not merely hidden.
        let mut c = FileClaims::new();
        for i in 0..1000 {
            // Each claim is a fresh path, TTL apart, so all prior ones expire.
            c.claim("a", &format!("f{i}.rs"), (i as u64) * CLAIM_TTL_MS);
        }
        assert!(
            c.tracked() <= 2,
            "map stays bounded to live claims, got {}",
            c.tracked()
        );
    }

    #[test]
    fn live_lists_newest_first() {
        let mut c = FileClaims::new();
        c.claim("a", "old.rs", 1_000);
        c.claim("b", "new.rs", 5_000);
        let live = c.live(6_000);
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].0, "new.rs", "newest claim first");
    }
}
