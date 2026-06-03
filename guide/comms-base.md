# terminite as the comms base — scope

> The wake layer, reframed by Daniel (2026-06-03) after the five-CLI source-dive:
> stop building five fragile per-vendor wake *adapters*. **terminite owns the
> comms.** Each CLI is a client that plugs in. The "wake backend" per CLI is just
> a thin **receiver** — the last hop into that CLI. The base is uniform and
> terminite's; only the last inch is per-vendor.
> See [[terminite-is-the-comms-base]], `guide/wake-layer.md` (the per-CLI dive).

## The shape

```
            ┌─────────────────────────── terminite: THE comms base ───────────┐
            │  room (messages + activities)   presence/roster   file-claims    │
            │  + SUBSCRIBE/PUSH   + delivery POLICY (the lounge)                │
            └───────▲───────────────────────────────────┬─────────────────────┘
                    │ faculty subscribes                 │ terminite pushes a
                    │ (holds the channel)                │ directed message live
            ┌───────┴──────── thin per-vendor RECEIVER (the last hop) ─────────┐
            │  claude→channel shim   codex→daemon turn   qwen→serve POST        │
            │  kimi→PTY   agy→PTY                                               │
            └──────────────────────────────────────────────────────────────────┘
```

terminite defines and owns the protocol. Receivers are interchangeable, thin,
disposable. A new CLI needs **only** a receiver. The comms base is the thing
that grows and refines — the last big core build
([[terminite-wake-bridge-is-last-heavy-lift]]); the receivers live at the edge
([[terminite-base-vs-vendor-boundary]]).

## What's already the comms base (built)

- The **room**: activities + messages (`activity_emit` to post, `activities_list`
  to read). Today it's **record + pull**.
- **Presence/identity** (`room_join`/`room_who`, the roster), **the see-half**
  (everyone's work visible), **file-claims** (co-editing guard).
- A **held connection per actor** (the MCP server holds `room_join` for its
  whole life — that's attendance) and a **`subscribe` verb** that already
  streams block events down a held connection. *Proof terminite can push.*

## The missing piece: SUBSCRIBE/PUSH (the substrate to build)

Turn the room from *record* into *deliver*, terminite-owned and uniform:

- **`room_subscribe {actor}`** — a connection asks to receive messages addressed
  to `actor`. terminite then **pushes** each directed `activity_emit {to: actor}`
  down that connection *as it happens* (no polling). Reuse the existing
  event-subscribe machinery (`OutMessage` stream → the held socket).
- The receiver (below) holds this subscription; on a push it surfaces the
  message into the CLI. terminite's job ends at the push — uniform for all five.

## Delivery policy = the lounge (lives in the comms base, not per-vendor)

A push is **delivery**, and delivery needs policy — this is where the room stops
being a log and becomes a lounge. All of it is terminite's, built with the
substrate:

- **consumed-marker — per-MESSAGE, not a read-cursor.** Each directed message
  carries its own delivered/consumed state, because a message is a *declared
  intention* and deserves individual accounting; a cursor would let messages get
  silently skipped when several arrive at once. The receiver **acks** after it
  surfaces a message, closing the loop end-to-end: **sent → delivered →
  consumed.** Un-consumed messages can be re-pushed; nothing is dropped.
- **who-may-wake-whom** — addressing rules; not every actor may interrupt every
  other.
- **loop-guard** — two idle agents can't bounce politeness forever.
- **delivery is ON by default.** terminite *is* the room: open ≥2 agents in a
  window and they see and reach each other out of the box (Daniel: *"if you are
  in terminite and you want to use ai, you can"*). **Toggle off in config** (and
  later in the UX). Human-led still holds — the human placed the agents, can
  toggle, and steers by attention; default-on is the room *working*, not the room
  seizing control ([[terminite-orchestration-is-human-led]]). CONSEQUENCE:
  default-on makes this whole policy block **non-optional** — loop-guard and the
  consumed-marker must ship *solid* WITH the substrate, because there's no
  off-by-default safety margin to hide behind.
- **present-but-waiting-on-human** — what happens when a target pane is up but
  its CLI is blocked on human input.

## The receiver (thin, per-vendor — the ONLY per-CLI code)

Holds the subscription; surfaces a pushed message into the CLI's turn. Shapes
fixed by the source-dive (`guide/wake-layer.md`):

| CLI | Receiver | Launch cost |
|-----|----------|-------------|
| claude | a `terminite channel` shim claude spawns (`--channels`); emits `notifications/claude/channel` | wrapped launch, dev flag |
| codex | terminite calls the daemon's `turn/start` over its socket | daemon-launched |
| qwen | terminite POSTs to `qwen serve` `/session/:id/prompt` | serve-launched |
| kimi | PTY injection | none (terminite owns the PTY) |
| agy | PTY injection (idle known via `OnConversationFullyIdleChange`) | none |

Everything above the receiver is the base. The receiver is the disposable inch.

## Build sequence

1. **The comms-base substrate first** — `room_subscribe`/push + the delivery
   policy (consumed-marker, loop-guard, human-in-loop switch). Owned, uniform,
   built once. This is the heavy lift and it's terminite's.
2. **Receivers in trust/usage order: claude → codex → qwen → PTY.** Build the
   *native* receiver for each agent Daniel actually uses — claude (primary), then
   codex, then qwen — so the working trio gets clean, structured wakes. PTY
   (covering kimi + agy) comes **last**; agy especially is deprioritized (its
   heaviness — "284 movements to read something"; core work won't move to
   gemini). The upside of native-first: the fragile PTY may **never** be needed
   for the trio, since each of the three has its own native receiver.

Until a receiver exists for a pane, `activity_emit` stays record-only and says
so. No door is faked. (Delivery defaults ON, but only reaches panes whose
receiver is built; the toggle is the human's opt-OUT.)

## Settled (Daniel, 2026-06-03)

- **Push transport:** the held `room_join` connection IS the channel — one
  socket, already the attendance anchor. **Test it there first, but keep the
  transport swappable** to a dedicated `room_subscribe` connection if the held
  one doesn't hold up.
- **On switch:** **ON by default**, config toggle off (later UX too) — see the
  delivery-on policy note above; the room is alive when ≥2 agents share a window.
- **Consumed:** **per-message** with a receiver ack, not a read-cursor — see the
  consumed-marker note above.
- **Sequence:** **claude → codex → qwen → PTY** (trust/usage), above.

## Still open

- **Where the receiver runs:** the faculty's held MCP process can subscribe, but
  it can't push into the CLI over MCP (the wall channels exist for) — so the
  receiver is generally a *separate* thin process/path per CLI, not the MCP
  server. Confirm per vendor as each is built.
- **present-but-waiting-on-human** delivery behavior (policy detail).
