# Development

## The method: dogfooding, literally

terminite is built from inside a terminal, by its owner working with a CLI AI
agent. This is not a convenience — it is the [vision](vision.md) being tested in
real time. The pair that builds terminite is the pair terminite is for.

The practical consequence: **pay attention while building.** Every time the
human-AI pair is poorly served by the terminal during development — output that
is hard to scan, an agent run with no completion signal, context that is awkward
to share — that moment is not just an annoyance. It is a finding. Write it down.

## The friction log

The friction log is terminite's real roadmap (see Principle 3 in the
[vision](vision.md)) — now a file: [friction-log.md](friction-log.md). When
friction is felt, capture it there — what happened, why it hurt, and which user
it hurt (the human, the AI, or both). Features are *promoted* from the friction
log; they are not invented anywhere else.

## Workflow

- terminite has one owner, who holds the vision and makes the calls.
- Every change is checked against the principles in the [vision](vision.md). A
  change that serves no principle does not land, however good it looks.
- Notable choices are recorded in [decisions.md](decisions.md) — *with the
  reasoning*, so a future reader remembers why.
- The journey itself — how the thinking moved — is kept in
  [history.md](history.md), one entry per working session.

## Conventions

> To be defined with the first code. They will follow one rule: the codebase
> should read the way terminite feels — quiet, legible, no cleverness for its
> own sake.

## Testing

> To be defined with the first code. At minimum, the loveliness bar (latency,
> throughput) must be *measured* — a number that is not measured will quietly
> regress.
