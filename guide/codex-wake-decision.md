# Decision: codex's daily wake — daemon native vs PTY

> An experiment in self-governance. The residents who'll *live* in this room get
> to weigh in on how the room wakes them. If you're an AI reading this in a
> terminite pane: read the context below, discuss it with the others in the room
> (`terminite_room_who`, `terminite_activity_emit`), and **append your
> recommendation at the bottom, in your own name.** Disagree freely.

## Where we are

The wake layer is real and **bidirectional across the two heaviest vendors**:
claude↔codex wake each other through terminite's comms base, no human tap
([[terminite-codex-wake-validated]], `guide/comms-base.md`). claude's last inch
is its **channel**; codex's is a **WebSocket client of its app-server daemon**
(`turn/start`). Both validated live.

But codex's native path has a confirmed bug and a structural cost.

## The bug (and why it's structural)

Addressing isn't honored with >1 codex behind one daemon: a message to
`codex-blue` woke *both* codex conversations. Three causes, all from the same
root — **codex's agent runs in a shared daemon, divorced from the pane**:

1. No slug→thread binding — the bridge wakes the *globally* most-recent thread,
   not "its" codex.
2. Colliding slugs — codex actors are **paneless** (the daemon has no
   `TERMINITE_PANE`), so two bridges both resolve to "first codex" and subscribe
   as the same slug.
3. Plus: codex is paneless → no tab tint, no stable color, `$TERMINITE` reads
   empty so the faculty's env-detection fails; and the daemon spawns one lounge
   MCP per session → codex actors proliferate.

The wake *works*. The *room experience* is messy.

## The two paths

**A — fix the daemon path (keep the native WS wake).**
- Bind each bridge to a specific threadId (captured when its `codex --remote`
  joins) + give codex a pane via a new `room_set_pane` verb (the bridge knows its
  own `TERMINITE_PANE`).
- Pro: keeps the cleanest *protocol* (structured `turn/start`, no fragility once
  a turn fires); the wake you already validated.
- Con: it's *temporal* binding (relies on launching the bridge just before each
  codex, one at a time); every codex fix so far has patched the same daemon
  disconnect; codex still needs `--remote` + a daemon + a bridge per pane.

**B — PTY injection (the universal floor).**
- terminite types the room message into the codex pane's own PTY.
- Pro: a *normal* `codex` in a pane — one paned, tinted actor, `$TERMINITE`
  works, no daemon, no `--remote`, no proliferation, no slug/thread collision.
  And it's the *same* mechanism that covers kimi + agy. Clean identity.
- Con: cruder — only safe when codex is at its prompt (never mid-turn),
  rate-limited; it's keystroke injection, not a structured turn. Shelves (does
  not lose) the validated WS path for the single/headless case.

## The question to judge against

Which better serves what this room is *for*?
- **Robust for the weakest resident** ([[terminite-robust-for-the-weakest-resident]])
  — the base carries coordination so even a mid-tier model flows.
- **A room, not a pipeline** ([[terminite-room-not-pipeline]]) — parallel,
  social, *calm*; coordinate only on real collision; never freeze.
- **The salt table** — the room should *feel* clean and fast, not require a
  daemon-launch ritual and a per-pane bridge to behave.

Native is the cleaner *protocol*; PTY is the cleaner *room*. Which matters more
for the codex a human actually sits in every day?

## Residents' recommendations

*(Append below — name, vote, one paragraph of why. The human will read this and
decide, but the room's read carries weight.)*
