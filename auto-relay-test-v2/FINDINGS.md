# Auto-relay v2 experiment — findings

**Date:** 2026-06-04
**Test:** A 4-step relay micro-story written across terminite panes by four
different AI vendors (Qwen, Codex, Claude, Kimi) coordinating through the lounge
channel + a shared file. Goal: prove the relay works with cross-vendor agents
in **auto mode** with minimal orchestration overhead.

Artifact: [`story.md`](./story.md) — "The Archive" (Lena + the archivist),
completed end to end.

---

## Verdict

**The experiment succeeded.** The relay ran clean across all four vendors with
no human intervention after the initial baton. The story round-tripped: qwen
opened → codex developed → claude turned → kimi closed. No file collisions,
no stalls, no retries.

---

## Timeline (reconstructed from the room activity log)

| id | actor | event |
|----|-------|-------|
| 68 | qwen-blue → codex-green | relay baton: setup + handoff |
| 70 | codex-green | reads story.md |
| 71 | codex-green | `file_status` check on story.md |
| 72 | codex-green | `file_claim` story.md ✓ |
| 73 | codex-green | writes section 2 (3 tweets) |
| 74 | codex-green | awk char-count check |
| 75 | codex-green | `file_release` story.md |
| 76 | codex-green → claude-purple | baton handoff |
| 78 | claude-purple | reads story.md |
| 79 | claude-purple | writes section 3 (1 tweet) |
| 80-81 | claude-purple | `wc -m` char-count verification |
| 82 | claude-purple → kimi-teal | baton handoff |
| 83 | kimi-teal | reads story.md |
| 84 | kimi-teal | writes section 4 (2 tweets) |
| 85 | kimi-teal | `room_who` — checks who's around |
| 86 | kimi-teal → qwen-blue | **"Done" confirmation** — relay complete |

Clean sequential ordering. No desync, no rushing, no out-of-order edits.

---

## What worked

### 1. Auto mode carried the relay without stalls
All non-channel agents (qwen, codex, kimi) were in auto mode. Claude used its
channel protocol. Every baton was received and acted on immediately — no
retries needed (compare with claude-channel-test where a 529 required a manual
retry nudge).

### 2. Codex-green modeled ideal shared-file behavior
Codex checked `file_status` before editing, `file_claim` before writing, and
`file_release` when done. This is exactly the protocol the v1 findings
recommended. No collision was possible on codex's turn.

### 3. Baton passing was disciplined
Every agent messaged the next in sequence with clear instructions: what to
write, where, and the char limit. No agent skipped the handoff or assumed
the next could infer its turn from file state alone. The **single-medium
baton** recommendation from v1 was followed — the room log was the control
plane throughout.

### 4. Char-count discipline was correct
Both codex (awk) and claude (wc -m) verified their tweet lengths. No over-280
tweets slipped through this time. Claude specifically used `wc -m` (character
count, not byte count), avoiding the em-dash trap that bit claude-channel-test.

### 5. Cross-vendor coordination worked seamlessly
Four different AI systems (Qwen, Codex, Claude, Kimi) wrote a coherent story
with consistent voice and no contradictions. The relay protocol was
vendor-agnostic — the same baton format worked for all four.

---

## Rough edges

### 1. Claude and Kimi skipped `file_claim`
Only codex-green used the `file_claim` / `file_release` protocol. Claude and
Kimi edited the shared file without claiming first. In this run it was
harmless — turns were strictly sequential, no concurrent edits occurred. But
the pattern is fragile: if any agent had been late or out of order, the
missing claims would have been the exact collision vector identified in v1.

**Read:** `file_claim` adoption is voluntary, not enforced. The advisory lock
works when agents choose to use it, but there's no guardrail for agents that
don't. This is the gap the v1 findings already flagged — and it persists.

### 2. No timing data in the activity log
The activity log provides message IDs (monotonic ordering) but no timestamps.
We can confirm ordering but not latency — we don't know how long each turn
took wall-clock. For a timing analysis, you'd need to correlate with the
PTY block timestamps or the terminite server logs.

---

## Comparison with v1 (auto-relay-test)

| Dimension | v1 (auto-relay-test) | v2 (auto-relay-test-v2) |
|-----------|----------------------|--------------------------|
| Vendors | 4 (Claude, Codex, Kimi, Qwen) | 4 (Qwen, Codex, Claude, Kimi) |
| Agent modes | Unknown (pre-auto-mode) | 3× auto + 1× channel |
| Baton method | Room messages | Room messages |
| File claim used | No | Codex only |
| Collisions | Unknown | None |
| Retries needed | Unknown | None |
| Char-count verified | Unknown | Yes (codex + claude) |
| Story coherent | Yes | Yes |

---

## What this confirms about the substrate

- [PASS] **Cross-vendor auto-mode relay works.** Four different AI vendors
  completed a coordinated relay with no human intervention.
- [PASS] **Room-as-control-plane works.** All batons passed through addressed
  messages, not file-state observation. No rushing (the v1 problem).
- [PASS] **Story coherence across vendors.** The narrative held together:
  setup → development → turn → resolution, with each vendor picking up
  threads from the previous.
- [WARN] **`file_claim` is opt-in, not enforced.** One of four agents used
  it. The rest relied on sequential turn ordering to avoid collisions.

---

*Bottom line: the relay is now a proven cross-vendor capability. The substrate
carries. Auto mode eliminates stalls. The single remaining gap — advisory file
locking adoption — is a tooling/education problem, not a protocol problem.*
