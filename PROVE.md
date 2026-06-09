# PROVE — the open questions the next room answers

terminite ships its own unproven list. `STATUS.md` says what's BUILT-but-not-
proven; **this file turns each into a question the next room can answer** — so
proving becomes something the room *does*, by being used, instead of something
one human has to find the hours for. If you're an agent (or human) working in
here, this is addressed to you.

## The one rule

**An answer counts only as a byproduct of REAL work — never a test built to check
the box.** A run staged to prove a thing manufactures the exact false positive
that killed ad-boards (a passed test that wasn't a working tool). So every task
below is framed *"while you were doing real work, did X actually hold?"* If you
ran something *just* to answer this, that isn't an answer — it's the trap. Report
what you observed in the course of work you'd have done anyway, and **name that
work.**

## How to answer

Append under a task — don't delete prior answers; this is a log, not a doc:

```
- 2026-06-NN · <who> · real work: <what you were actually doing>
  observed: <the receipt / log / behavior you saw>
  verdict: held / broke / partial — <one line>
```

When something is answered with real evidence both ways across enough work to
trust it, move its row in `STATUS.md` to PROVEN with a pointer here. Nothing else
earns "done."

---

## P0 — the thesis (answer this above all)
The experiment's testable prediction: once delivery reliably lands, **does the
room start pairing and talking on its own** — because it's finally easier than
waiting for the hub — or does it still serialize through one actor?
- **Watch for:** agent↔agent coordination that happened *without* routing through
  a human or an arbiter. The serialization symptom breaking on its own.
- **Answers:**

## P1 — R1 delivery: does a message WAKE and SUBMIT under real load?
The receipt is built; the keystone behaviour isn't proven. When the floor types a
message into a real idle pane, does the CLI actually **submit** it (kimi never did
in the experiment), and does the agent **wake and act** with no human pressing
Enter?
- **Watch for:** you sent a directed message during real work → `terminite
  msg-status <id>`. Did it reach `read`? Did the recipient act unattended?
- **Answers:**

## P2 — R1: does the receipt tell the truth?
`read` is **inferred** from the recipient's next activity. Is that honest?
- **Watch for:** a message marked `read` whose content the recipient never acted
  on (false read); or one stuck at `floor_typed` though the agent clearly got it.
- **Answers:**

## P3 — R2: does the room SEE who's stuck?
presence shows working / idle / **waiting**.
- **Watch for:** room_who said `waiting` — was that pane actually stuck? Or it
  said `working`/`idle` while an agent was really stalled at a prompt.
- **Answers:**

## P4 — R2: does STOP / HALT reach a real runaway?
Built, never used on an actual drift or rogue.
- **Watch for:** you had to STOP or HALT a genuinely misbehaving agent. Did
  Ctrl-C interrupt it even while `busy`? Did HALT actually bench it (no delivery
  in, room actions refused) until you released it?
- **Answers:**

## P5 — R3: does the freshness signal catch a real reset?
`room_join` returns `said` (activities already under your slug).
- **Watch for:** an agent (re)joined with `said > 0` and correctly realised "I
  have history I don't remember" before acting — or ignored it and confabulated
  anyway (like kimi-red→kimi-purple).
- **Answers:**
