# The lounge experiment — terminite as the autonomous bus

Written 2026-05-29. This is the swing at the actual thesis: not nicer
copy-paste, but **an underlying comms layer that makes several agents
work together without the human in the middle.** It is an *experiment*,
on purpose. If it lands, it's the reason terminite exists. If it doesn't,
we mark precisely why and fall back to the known-good app.

Read first: `lounge-thesis.md`, `activities-design.md`, and
`lab/lounge-validation/FINDINGS.md`.

## The bet, in one paragraph

terminite is the ACP **host** — it's the thing that sends prompts to each
agent and reads their events. So terminite can be the **router**: when
codex-1 emits a message to codex-2, *terminite delivers it into codex-2's
session as a prompt.* The human isn't the message bus — **terminite is.**
That's the one thing panes+copy-paste and Zed/VS-Code ACP can't do: Zed
spawns one agent on a task; the lounge is N agents coordinating through a
shared room with the human out of the loop after kickoff.

## What we are actually testing (the unknowns — this is why it's an experiment)

The lab validated the *plumbing* (emit / see / address) with scripted
agents and polling. It never tested autonomous coordination. These are
the open questions, and a clear answer to them — yes **or** no — is the
deliverable:

1. **Will a real agent *send*?** The lab proved agents *discover and read*
   the room tools from prose. Choosing to emit a *directed message to
   coordinate* is a new behavior. Unknown.
2. **Is turn-quantized routing good enough?** ACP is one-prompt-one-turn;
   terminite can't interrupt mid-turn, so it delivers at the target's
   next idle. Does that feel like coordination or like lag?
3. **Do they converge or degenerate?** Two agents prompting each other can
   loop ("thanks!"/"you're welcome!"), drift, or burn tokens without
   producing work. Does real coordinated output emerge?

The win condition is **B (build it) answers these**, instead of building
around them.

## Control model — non-negotiable, ships with the router (not after)

An autonomous agent-to-agent loop is unbounded by default. Token/turn
runaway is this project's new "unbounded allocation" — same discipline as
the three crashes. The router does **not** go live without all of:

- **Off by default.** Routing only happens when the human turns the
  lounge **on** (a proto verb / key). Off → `emit` just records; nothing
  is delivered. Agents run exactly as today.
- **Human kicks off.** The human seeds the room with a goal prompt. The
  agents never start themselves.
- **Turn budget.** A hard cap on routed deliveries per session (default
  small, e.g. 12). On reaching it, routing **pauses** and terminite tells
  the human "N exchanges, budget reached — resume?" Never silent, never
  unbounded.
- **Hard stop.** One key / verb halts all routing immediately. Typing
  into a pane directly also interrupts that agent.
- **Loop guards.** No self-delivery (message to self ignored). Message
  size + per-agent inbox depth capped. One delivery per target idle, FIFO.
- **Cost is visible.** Each routed delivery is a real ACP turn = tokens.
  The budget is the cost guard; surface the count.

If any guard isn't in place, the router stays off. This is the part we do
*not* cut.

## What we build

Substrate (shared with the known-good app, so not wasted if the verdict
is no):
- `ActivityStore` (workspace-global) holding `AgentMessage` activities.
- `terminite_activity_emit { kind: agent_message, to, text }` + a read
  verb. Host-attributed actor (the caller's slug, never self-declared).
- Actor slugs (`codex-1`, `codex-2`) assigned at session open.

The novel core:
- `AcpEvent::TurnEnded` — the idle signal (audit NOTE: terminite drops the
  `stopReason` today; the router needs "agent finished its turn" to know
  when to deliver). Insertion point: `acp.rs::classify_response`.
- **The router** — per-target inbox queue; on `emit` to X, enqueue; on X's
  `TurnEnded` while the lounge is on and budget remains, dequeue one and
  `session/prompt` it into X with lounge framing:
  > *"[Message from codex-1] <text>. You're in a shared workspace with
  > other agents; reply via terminite_activity_emit if useful, or say so
  > if nothing's needed."*
- The control model above, wired to the router.

## Build steps — one commit each, `cargo build` + `cargo test` green at every step

1. **`src/activities.rs`** — `ActivityStore` + `AgentMessage`, caps,
   eviction, monotonic ids. Standalone, unit-tested (ids; eviction;
   `to:self` filter excludes broadcasts — the E3 invariant).
2. **Store on `Renderer` + actor slugs** at `SessionCreated`
   (`renderer/acp.rs`; `AcpSession` gains `slug`).
3. **`emit` + read over proto + MCP tools** — `activity_emit` /
   `activities_list` proto verbs (`renderer/proto.rs`, `proto.rs`
   `OutPayload`), `terminite_activity_emit` / `terminite_activities_list`
   in `mcp.rs` (descriptions verbatim from `activities-design.md`).
   *Lounge still off — emit only records. Verify via `terminite activities`
   CLI that two panes' emits land in one store with correct attribution.*
4. **`AcpEvent::TurnEnded`** — emit it from `classify_response` on a
   prompt result; handle it in `handle_acp_event` (mark session idle).
   No routing yet — just prove we detect idle reliably (log it).
5. **The router + full control model** — inbox queues, deliver-on-idle via
   `session/prompt`, lounge on/off, turn budget, hard stop, loop guards.
   This is the heart; it ships *with* every guardrail.
6. **The experiment run + findings** — below. Write the verdict honestly.

## The experiment — protocol and verdict

**Setup (Daniel, `cargo run`):** two Codex ACP panes. Turn the lounge on.
Kick off codex-1 with a goal that *needs* the other agent, e.g.:
> *"You're codex-1 in a shared room with codex-2. Draft a small API for
> X. Send codex-2 a message asking it to review for bugs, then
> incorporate what it sends back. Coordinate through the room."*

Then **take your hands off** and watch terminite route between them.

**Watch for:**
- Does codex-1 *emit* to codex-2 unprompted-by-you?
- Does terminite *deliver* it (codex-2's pane shows the injected turn)?
- Does codex-2 *act and reply*?
- Do they *converge* on better output, or loop / stall / drift?
- Token/turn count vs budget.

**Verdict — both outcomes are valid deliverables:**
- **Lands:** the round-trip produces coordinated work you didn't shuttle.
  Then we harden it (Wall B real-time, more actors) — the lounge is real.
- **Doesn't land:** record *exactly* which unknown broke it (didn't send /
  routing felt wrong / degenerated), in `FINDINGS` and `history.md`. We
  then fall back to the known-good app (single-agent-on-task, fixed) and
  we *know* the lounge's limit instead of guessing. That is a real result.

## Resource / safety discipline (system-impact pass)

- The control model above *is* the resource discipline for routing.
- `ActivityStore`: `MAX_ACTIVITIES_TOTAL` ≈ 10_000, oldest-closed eviction,
  message text capped, every numeric clamped at source.
- No new threads/processes. Atomic-write persistence like `blocks.rs`.
- Off-by-default means the experiment can't run except when a human
  deliberately starts it — the strongest guard of all.
