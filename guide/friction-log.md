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

_New entries land here as friction is felt during development._
