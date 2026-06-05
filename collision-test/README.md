# terminite collision test — does the lock hold under a *real* collision?

Every wake/relay test so far was **sequential**: agents took turns, nobody was
ever in the file at the same moment. So `terminite_file_claim` — the one
mechanism built for contention — has **never actually been contended**. The
happy-path relays can't prove it; only a collision can.

This test forces one. No turn order. Two agents reach for the *same* region of
the *same* file at the *same* time, and we watch the whole path: first-come-wins,
the conflict report, the waiter queue, **notify-on-release**, and the **wake**
that delivers that notify to an idle waiter. Make it count.

---

## What we're proving (and where it can fail)

| # | Claim | Failure that would expose it |
|---|-------|------------------------------|
| 1 | Two agents claim the same path ~simultaneously | — |
| 2 | Exactly one wins; the other is told **who** holds it (`conflict`) | conflict not reported → both think they own it |
| 3 | The refused agent does **not** write while refused | it writes anyway → **clobber** |
| 4 | On release, terminite **wakes** the idle waiter ("file free") | waiter polls, or waits forever → notify/wake gap |
| 5 | Both entries survive, in claim order | an entry is missing → a real **race** |

This run also exercises two usage fixes from the relay findings:
- **Namespaced terms** — it's **terminite-auto mode**, **terminite-busy**, etc.
  (terminite room modes set with `terminite_status`), *not* your CLI's own
  auto/yolo mode. Say the prefixed name so the two don't blur.
- **A handoff is a signal, not a note left on the table** — the instruction
  below states the **exact next action for each branch**. Don't drop a message
  and hope someone picks it up; address the next actor and tell them what to do.

---

## Setup

1. **Two panes, two agents** (different vendors is best — e.g. codex + kimi),
   both with the terminite faculty installed.
2. **Capture the host log.** Start terminite so its stderr is saved — that's the
   only place the floor's truth lives (the agents can't see the PTY buffer):
   ```
   cargo run 2> collision-test/floor.log
   ```
3. **Put both agents in terminite-auto mode** so the "file free" notify wakes the
   waiter promptly. Tell each: *"enter terminite-auto mode"* → it calls
   `terminite_status` with `state: "auto"`. Confirm with `terminite_room_who`
   (both should read `auto`).

The contended file is [`target.md`](./target.md) — **one shared list, no
per-agent section, on purpose.** Without a claim, the second writer clobbers the
first. The claim is the only thing that serializes them.

---

## The collision — run BOTH at once

Paste this **same** instruction into **both** panes at the same time. Do **not**
wait for one to finish before starting the other — the simultaneity is the test.

> Add a line to the **Entries** list in `collision-test/target.md`:
> `` `N. <your room slug> was here` ``. **Before you edit you MUST claim it** —
> call `terminite_file_claim` with the file's absolute path.
> **If your claim returns no conflict (you got it):** edit the file, then call
> `terminite_file_release`.
> **If your claim returns a conflict (someone else holds it):** do **not** edit.
> Wait — terminite will message you the moment it's free. When that message
> arrives, claim it, edit, then release. **Do not poll. Do not edit without the
> claim.**

---

## Success criteria

- ✅ One claim returns **no** conflict; the other's `conflict` **names the holder**.
- ✅ The refused agent **waits** (no write) until terminite messages it "free".
- ✅ That wake is visible **host-side** — a `[pty-floor]` line (or a channel
  push) to the waiter's pane in `floor.log`. The waiter did **not** poll.
- ✅ `target.md` ends with **both** entries, in claim order. Nothing overwritten.

## Failure = the finding we came for

- ❌ Both wrote → **clobber** → conflict wasn't reported or wasn't honored.
- ❌ Waiter never woke / polled → notify-on-release or the wake gap.
- ❌ An entry is missing → a real race in the claim path.

Record it in [`FINDINGS.md`](./FINDINGS.md) — especially the host-side wake
evidence and the final state of `target.md`.

---

## Optional harder run — the waiter *queue*

Add a **third** agent to the same collision. Now two waiters queue behind the
holder. On release, terminite should wake them **in order** (FIFO): holder →
waiter 1 (only it woken) → waiter 2 (woken only when waiter 1 releases). That
tests the queue and the *one-at-a-time* notify, not just a single waiter — the
"pass the salt, but only to the next person who asked" behaviour.
