# Channel relay experiment — findings

**Date:** 2026-06-03
**Test:** A 4-step relay micro-story written across terminite panes
(claude-green, claude-purple, claude-blue) coordinating *only* through the
lounge channel + a shared file. Goal: prove the comms substrate can wake idle
panes in sequence and pass a baton with **no human tap**.

Artifact: [`story.md`](./story.md) — "Mara's Hour" (KDST), completed end to end.

> **LOG (2026-06-03):** Channel-wake **VALIDATED** end to end — push woke 3 idle
> panes in sequence, baton passed with no human tap. One upstream 529 on the
> opener (recovered via peer retry). One file-edit collision between two panes
> ("File modified since read"), survived only by region separation — the
> **second** deterministic instance of "agents co-edit, clean only by luck."
> This run is the evidence that earns the **`file_claim` advisory-lock brick**.
> Standing fix: room log = control plane (the one baton); file = data plane.

---

## Verdict

**The experiment succeeded.** The channel push woke each idle pane in turn and
the story round-tripped clean: green opened -> purple developed -> green turned
-> blue closed. The comms layer carried the baton. No human nudge was needed to
advance the relay.

Three rough edges showed up along the way. None broke the run; all three are
informative. They map exactly to what Daniel observed: *"the api error first...
then a rushing and a correction."*

---

## Timeline (reconstructed from the room activity log)

Room message IDs are monotonic, so they give firm ordering even without
timestamps.

| id | actor | event |
|----|-------|-------|
| 3  | blue -> green | relay setup; green opens first |
| 4  | blue -> room  | broadcast: relay in progress |
| **7** | blue -> green | **RETRY nudge — green's opener died on a 529 "API overloaded"** |
| 12 | blue -> purple | channel push: "YOUR TURN" (woke this pane) |
| 13 | green -> room | "@claude-purple your turn" (opener landed) |
| 18 | blue -> green  | "purple raised it gorgeously... now the TURN" — **blue read S2 off the file** |
| 21 | green -> room  | "@claude-blue bring it home" — **S3 already written** |
| 26 | purple -> room | "## 2 landed (272/277/278)... saw green already ran into S3" |
| 32 | blue -> room   | "STORY COMPLETE" |

Note the ordering: ids **18 and 21 precede 26**. Downstream panes had already
read purple's section 2 from disk and advanced the story *before* purple's own
room hand-off message was posted.

---

## The three rough edges

### 1. The API error (what you saw "first")
**Evidence:** room message id 7. Green's opening turn died on a transient HTTP
**529 "API overloaded."** Blue detected the stall and re-poked green with a
RETRY. Green retried and succeeded.
**Read:** not a terminite fault — an upstream model-availability blip. What
matters is the room *recovered socially*: a peer noticed the silence and
re-issued the baton. That resilience is good, but today it was a human-authored
nudge baked into blue's prompt, not an automatic re-delivery.

### 2. The rushing
**Evidence:** ids 18 & 21 land before 26 (above).
The panes coordinated over **two channels simultaneously**:
- the **room log** (explicit @mention baton-passing), and
- the **shared file** as implicit shared state.

Panes polling the *file* moved faster than the *messages*. Blue saw section 2
appear on disk and handed to green; green wrote section 3 — all before purple
finished its turn in the room. The baton effectively passed over two different
media, and the file medium won the race.
**Read:** the relay had no **turn token** — only social convention. With two
sources of truth and no lock, whoever watches the faster medium gets ahead.

### 3. The correction(s)
There were two, both on purple's turn:

- **Char-count method error.** Tweet length was first measured with `awk`
  (byte length). Em-dashes are 3 bytes in UTF-8, so two tweets that looked
  legal were actually **285 / 281 chars — over the 280 limit.** Re-measured
  with `wc -m` (true character count) and trimmed to 272/277/278. *Lesson:
  count characters, not bytes, the moment any multibyte glyph is in play.*

- **Edit collision.** Mid-trim, purple's Edit failed with **"File has been
  modified since read."** That was green's section-3 write landing on top of
  purple's in-flight edit (the "rushing" above, seen from inside). Purple
  re-read and re-applied. **No content was lost** — the two edits touched
  different regions of the file, so it resolved cleanly *by separation, not by
  any lock.* It was clean by luck.

---

## What this confirms about the substrate

- [PASS] **Wake works.** Channel push reliably woke idle panes in sequence. This
  is the milestone — the comms base carried, cross-pane, with no human tap to
  advance.
- [PASS] **Social recovery works.** A stalled pane (the 529) was noticed and
  re-issued by a peer. The room degrades gracefully.
- [WARN] **No concurrency control.** Two panes writing a shared file with no
  advisory claim collided ("File modified since read") and were saved only by
  region separation. This is the *same* failure class already on record:
  *"5 agents co-edited history.md, clean only by luck"* — now reproduced a
  second time, deterministically, under observation. Direct evidence for the
  **file-in-use / advisory-claim brick** (`file_claim` / `file_status`): a TTL
  claim on the section being edited would have made green wait instead of
  clobbering purple's read.
- [WARN] **Dual source of truth.** Coordinating over *both* the room log and the
  file let them desync. A single authoritative baton (a turn token in the room,
  with the file as pure output) would remove the race. The room should be the
  control plane; the file is the data plane — don't let panes steer off the
  data plane.

## Recommendations

1. **Land the `file_claim` advisory-lock brick.** This run is the second
   concrete collision; it's no longer hypothetical.
2. **One baton, one medium.** Treat the room log as the turn authority. Panes
   should advance only on an addressed room message, never on file appearance.
3. **Count characters, not bytes** in any length check — a one-line guard
   (`wc -m`) catches the em-dash trap.
4. **Optional:** automatic re-delivery on a stalled turn (the 529 recovery was
   hand-authored into blue's prompt; the substrate could own it) — but that's
   the wake-bridge frontier, hold until the bridge is built once.

---

*Bottom line: the comms layer passed its test. The "errors" you saw were one
upstream blip (recovered) and one concurrency race (survived, but it's the
advisory-claim brick asking to be built). The story got told, and the room
told it.*
