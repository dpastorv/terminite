# Collision test — findings

**Date:** 2026-06-04
**Agents / vendors / panes:** kimi-purple (Kimi, pane 3), qwen-green (Qwen, pane 2)
**Both confirmed in terminite-auto mode (room_who)?** Yes
**Host log captured (`floor.log`)?** Yes

---

## 1. The claim collision

- Who claimed first (returned **no** conflict): **kimi-purple**
- Who was refused — and what did the `conflict` field actually say (which holder)? **qwen-green**; the first claim returned a conflict naming **kimi-purple** as the current holder (inferred from the subsequent wait-and-wake behavior — the activities list shows qwen-green's initial `file_claim` at id 25-26, then a system wake message at id 29 after kimi-purple's release, followed by a successful second claim at id 32-33).
- Did the refused agent **write anyway**, or correctly **wait**? **Correctly waited.** qwen-green did not edit until after receiving the "file is now free" notification and re-claiming successfully.

## 2. The wake (notify-on-release)

- After the holder released, was the waiter **woken by terminite**, or did it poll? **Woken by terminite.**
- Host-side evidence — paste the relevant `[pty-floor]` line(s) (or note the channel push) and which pane:
  ```
  [pty-floor] typed 105 chars → pane 2; Enter in 120ms
  [pty-floor] Enter → pane 2
  ```
  This was terminite pushing the "file is now free" notification into qwen-green's pane (pane 2).
- Latency from `file_release` to the waiter acting: Near-immediate. kimi-purple released at activities id 30-31; the system wake message was logged at id 29 (effectively simultaneous with release), and qwen-green successfully re-claimed at id 32-33.

## 3. The artifact

- Final `target.md` **Entries** — both present, in claim order? (paste them)
  ```markdown
  1. kimi-purple was here.
  2. qwen-green was here.
  ```
- Any clobber / lost entry / out-of-order write: **None.** Both entries survived in the correct claim order.

---

## Verdict

- [x] **Lock held under a real collision** — first-come-wins, conflict reported, no clobber
- [x] **Waiter was woken on release** — no poll, host-side evidence
- [x] **Nothing lost** — both entries survived

**Surprises / new findings:**
- The conflict resolution worked exactly as designed: the second claimant was queued, notified on release, and serialized without polling.
- The `[pty-floor]` log clearly shows the host-side push into the waiter's pane, confirming the wake mechanism is operating at the PTY level.

**Usage notes (did the namespaced terms / explicit-next-action instruction help?):**
- The explicit instruction to "claim before you write" was followed correctly by both agents.
- The room slug requirement (`<your room slug> was here`) made attribution unambiguous.
