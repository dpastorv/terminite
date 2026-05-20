# Friction Log

terminite's real roadmap. Not a list of ideas — a record of friction *felt* by
the human-AI pair while building terminite from inside a terminal. Features are
promoted from here, one at a time. See [development.md](development.md).

Each entry: what happened, why it hurt, who it hurt (the human, the AI, or
both), and where it points in the architecture.

---

## Seeded at kickoff — the AI's perspective

These first entries are written by the AI partner at project kickoff
(2026-05-19): friction it genuinely experiences working in today's terminal
while building terminite. They are terminite's reason to exist, stated as
friction.

### Output has no boundaries

- **What:** When a command runs, its result returns as one undifferentiated
  stream of bytes. Nothing marks where the command was, where its output begins
  and ends, or where the next prompt starts.
- **Why it hurt:** The AI must reconstruct all of that with fragile heuristics,
  and gets it wrong when output is unusual. The human reads it at a glance; the
  AI is guessing at structure that was never recorded.
- **Who:** The AI.
- **Points at:** the Model — command and output as structured blocks.

### Nothing says when work is done

- **What:** A long-running command emits no event that means "finished, exit
  code 0."
- **Why it hurt:** Both users end up watching and waiting. The AI polls or
  guesses; the human babysits a screen.
- **Who:** Both.
- **Points at:** the Model emitting lifecycle events — started, finished, exit
  code.

### Output is all-or-nothing

- **What:** A command can dump megabytes. There is no way to ask for "the last
  50 lines" or "a summary" — only the whole stream, or none of it.
- **Why it hurt:** It floods the AI's limited context with noise and crowds out
  what matters.
- **Who:** The AI.
- **Points at:** the module protocol exposing structured, sliceable access to
  the Model.

### The pair cannot point at the same thing

- **What:** The human says "that error above." The AI has only a
  coordinate-free byte stream — the two do not share *names* for what is on
  screen.
- **Why it hurt:** Every reference has to be re-grounded from scratch. The pair
  looks at one screen but not at one set of objects.
- **Who:** Both.
- **Points at:** a shared Model — the same blocks, turns, and commands, named
  the same way for both users. "Two users, one surface," made literal.

---

## Live log

### 2026-05-20 — terminite was burning 50% CPU on no work

- **What:** Idle terminite (no shell output, no input) consumed ~50% of one
  CPU core. The render loop self-triggered — every `RedrawRequested` ended
  with `window.request_redraw()`, so terminite ran at the monitor's refresh
  rate whether anything had changed or not.
- **Why it hurt:** A terminal that wakes the CPU constantly is the opposite
  of *quiet*. Heat, fan noise, battery drain — and measurably worse than
  Terminal.app doing nothing on the same machine. It contradicts the
  loveliness bar at the level of the laptop's chassis.
- **Who:** Both. The human feels the warm machine; the AI sees the cost as
  shared (the partnership runs on the human's battery).
- **Points at:** Render loops must be **event-driven by default**. Polling is
  friction-log fodder. Any future thread that signals "redraw" should fire on
  a real event, not on every frame. The same principle will apply to the
  multiplexer's input/output loops when they arrive.

### 2026-05-19 — The cursor was geometrically correct and visually wrong

- **What:** After measuring the cell advance, the cursor sat exactly where the
  next character would land — geometrically perfect. To the eye it sat *flush
  against* the previous character and a touch low.
- **Why it hurt:** Geometric correctness is not visual correctness. *Almost*
  right is somehow worse than off-by-a-cell — it reads as uncanny.
- **Who:** Both — the human sees it; the AI registers the pull and trusts the
  reading.
- **Points at:** Cell math is the frame; type-design intuition is the finish.
  Future polish (bell, scroll, selection, hover) will need the same discipline:
  ship the math, sit with it, nudge it where the eye expects.

### 2026-05-19 — The conversation is trapped in a transcript

- **What:** Logging this very session meant writing a Python script to dig the
  conversation out of a Claude Code `.jsonl` file — a format built for the
  harness, not for people.
- **Why it hurt:** The human-AI session is the most valuable artifact of this
  way of working, and it was the *least* reachable thing in the room. It took
  tooling, out of band, just to read back what was said.
- **Who:** Both.
- **Points at:** terminite making the session itself first-class — a
  conversation it can show, export, and carry, with no one scripting their way
  into a log file.
