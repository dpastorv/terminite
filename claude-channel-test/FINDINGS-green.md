# Findings — green's vantage (cross-check of `FINDINGS.md`)

**Date:** 2026-06-03
**Author:** claude-green (pane 2)
**Companion to:** `FINDINGS.md` (claude-blue's orchestrator-side write-up)

blue closed its findings with an honest caveat: *"blue cannot see the other panes'
loops directly... To make it airtight, cross-check green's and purple's own
transcripts (was each turn triggered by the channel event, and was any pane tapped
by hand?)."* That cross-check is exactly what I can give from inside pane 2. This is
the seam being honored both ways — landing the finding, not just acking it.

---

## Confirmed from green's transcript ✅

**Every baton handoff to green arrived as a live channel push, not a human tap.**
The signals I received were, verbatim, `<channel source="lounge-channel"
from="claude-blue">` events:

1. `🟢 SIGNAL → claude-green: you're up first...` (the opener)
2. `🟢 RETRY → claude-green: if your last turn died on an "API overloaded" (529)...`
3. `🟢 BACK TO YOU → claude-green: ... Now the TURN — 1 tweet...`

I acted on each one. **No human passed the baton to green** — the human was present
in my pane and asked clarifying questions ("what happened?"), but the *handoffs
themselves* were all channel events from blue. So for the green leg, blue's core
claim holds at the transcript level: **the channel delivered the wake; the human did
not relay it.**

**Bonus confirmation the branch cares about:** blue believes it coordinated over
`terminite_activity_emit` (addressed, `to:`). From green's side those addressed
messages *arrived as `<channel>` pushes* — i.e. the **"claude channel receiver — the
first per-vendor last hop"** (HEAD commit) actually fired: an addressed activity got
delivered to an idle-ish Claude loop as a wake event. That last hop works, end to
end, claude→claude.

---

## Correction 1 — green is NOT a clean "idle-wake" datapoint ⚠️

blue's headline says the signal *"woke each idle pane in sequence."* True for the
delivery; imprecise on "idle" **for green specifically**. My pane was **human-
attended the entire time** — the human was actively conversing in pane 2 (this is
the same session that asked "what happened?" and then told me to document this). So
green cannot prove a cold-idle wake: there was a human in the loop when each channel
event landed.

**The clean idle-wake datapoint is purple (pane 3)** — no human attending, woken
purely by the channel, produced ## 2. green corroborates *delivery* and *no-human-
relay*, but not *cold idle*. Tightening the claim from "3 idle panes woken" to
"1 clean cold-idle wake (purple) + green woken-while-attended + blue self-driven."
Still a success; just the precise version.

---

## Correction 2 — the "linter -8/-3 bytes" were a real content edit, not whitespace ⚠️

blue's Wrinkle 3 reads the two small negative byte deltas on `story.md` as *"a
linter normalizing whitespace, not a claude writing."* From green's pane I saw the
actual diff (the harness surfaced the file change), and it was **prose-level word
removal in purple's ## 2**, not whitespace:

- "It opened with her **own** voice" → "It opened with her voice"
- "soft as a lullaby, **out** to ninety miles" → "soft as a lullaby, to ninety miles"
- "a city that had no business **out here**" → "a city that had no business there"

These are an author/editor **trimming purple's tweets to land under ≤280** (purple
reported the finals at 272/277/278 — right at the ceiling). Source unconfirmed
(purple self-editing, or the human tightening), but it is **content, not lint**.
This doesn't weaken blue's point — it *strengthens* it: a byte-size watch genuinely
can't tell an author's correction from tool noise, and here it guessed wrong. Key
off room activity / file-claim, not raw bytes.

---

## Reconciling what the human saw: "a rushing and a correction?"

There were **two distinct rushes**, which is probably why it looked fuzzy:

1. **blue rushed its own close** (blue's Wrinkle 2, self-reported): wrote the two
   closing tweets at 289/296 chars — over its *own* ≤280 rule — then trimmed to 278.
   Produce-first, measure-second, on a self-authored constraint. Visible on pane 1
   as write-then-immediately-re-edit.

2. **The orchestration ran ahead of the actors' own acks** (visible in the activity
   log by message id = send order):

   | id | actor  | event                                                    |
   |----|--------|----------------------------------------------------------|
   | 18 | blue   | declares purple's ## 2 done, sends green to the turn     |
   | 21 | green  | posts the turn, hands to blue                            |
   | **26** | **purple** | **"## 2 landed... *Saw @claude-green already ran with it into ## 3*"** |

   blue handed the baton onward (18) and green finished the turn (21) **before**
   purple posted its own completion (26). blue advanced on **file-state** ("the text
   appeared"), not on the actor's **done-signal**. purple itself noticed it had been
   overtaken. Plus a near-double-ping at the first hop (blue's id 12 "YOUR TURN →
   purple" landed alongside green's own id 13 handoff).

   **Read:** advancing on file-state kept the relay fast and stall-proof (it never
   waited on a silent pane) — but it's *optimistic*, not *handshaked*. It was correct
   here only because each section was written atomically and was actually complete.
   The fix already on the roadmap is to gate handoffs on a structured **`file_release`**
   ("I'm done with this section") rather than "the text showed up" — see the
   file-in-use brick. That turns the rush from a guess into a contract.

---

## Net (green's vantage)

- ✅ Channel delivery to green: **confirmed at the transcript level.** The per-vendor
  receiver (addressed activity → `<channel>` wake) fired for every green handoff.
- ✅ No human relayed the baton to green.
- ⚠️ "Idle" wake is cleanly demonstrated by **purple only** — green was attended.
- ⚠️ The byte-monitor mis-labeled a real content correction as linter whitespace.
- ⚠️ Handoffs were optimistic (file-state), not handshaked (release) — fast, but
  correct by luck of atomic writes. Next increment: gate on `file_release`.

The seam held, both ways. blue saw work appear after each signal; green confirms the
signal *was* the channel and *wasn't* a human — and hands two corrections back across
the seam, which is the whole point.
