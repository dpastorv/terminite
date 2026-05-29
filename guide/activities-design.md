# Activities design — the room's fine-grained inner stream

Written 2026-05-29 by Daniel and Claude, during the design sit-down
that followed the Stage-1 Codex-sees-Codex experiment. Codex was the
scout that ran the experiment; the wall it surfaced is what this
document addresses. Captured here before the next build so we
commit to the shape, not to a hurried implementation of it.

## The wall the experiment surfaced

Stage 1 wired terminite's MCP server into ACP `session/new`, so
hosted agents discover terminite's vocabulary automatically. Codex
used the tools without prompting and concluded:

> *"From the Terminite workspace view, I appear to be alone here."*
> — codex/terminite-presence-report.md

The vocabulary worked. The *room* didn't show another actor — only
two identical-looking tabs. Codex fell back to `ps` to find sibling
processes outside terminite's view.

Two walls, named:

- **Wall A — visibility.** Agent actions don't enter the shared
  coordinate space. When an agent does something, the room can't see it.
- **Wall B — notification.** Even if actions were visible, the read is
  still pull. No subscribe-and-broadcast wire.

This document is **Wall A only**. Wall B is brick 4, deferred until
Wall A is shipped and lived in.

## The honest read on what shell-hosted agents look like in the room today

When a human runs `claude` (or `aider`, or any long-running interactive
agent) in a terminite shell tab, OSC 133 forms **one open block** —
B1 — and the entire session lives inside it. Claude does dozens of
file edits, tool calls, and responses. The room sees a single block:

```
B1: claude
```

That is presence, not visibility. "Blocks are the activity stream for
shell-hosted agents" — *false*, for long-running interactive ones.
The block is a container; its contents are opaque.

The roadmap named this in Phase 2:

> *"When a human runs an AI agent (claude, aider) interactively, the
> entire session collapses to one open block. The activity model — per-
> tool-call granularity, with stable coordinate ranges — is named in
> lounge-thesis.md but not yet implemented."*

The activities layer was always meant to address this. It is **not**
a parallel surface to blocks; it is the **fine-grained inner stream**
that lives inside long-running blocks and inside ACP panes alike.

## The model

```
Activity {
  id: u64                      // global, monotonic — "act-N"
  parent: ParentRef            // see below
  actor: ActorLabel            // "B1" | "codex-1" | ...
  agent_name: String           // "Claude" | "Codex" | ...
  kind: ActivityKind           // see below
  status: ActivityStatus       // Pending | InProgress | Completed | Failed
  opened_at: SystemTime
  closed_at: Option<SystemTime>
  tags: Vec<String>            // shared namespace with block tags
}

ParentRef {
  Block(BlockId)               // long-running shell agent (Claude in B1)
  Actor(ActorLabel)            // ACP-hosted (Codex's session)
}

ActivityKind {
  ToolCall      { tool: String, title: String, input: Option<String>,
                  output: Option<String> }
  AgentMessage  { from: ActorLabel, to: Option<ActorLabel>, text: String }
  UserPrompt    { text: String }
}

ActivityStatus { Pending, InProgress, Completed, Failed }
```

### Identity = visible coordinate

Actor identity reuses what's already addressable in the room:

- Shell-hosted long-running agent → actor label is the block ID. The
  agent inhabiting `B1` *is* "B1." If a later block forms (B7), that
  block is its own potential actor.
- ACP-hosted agent → actor label is a session-scoped slug assigned at
  session open. `codex-1`, `codex-2`, `claude-1` (if ever hosted that
  way). Stable for the session's lifetime.

  > NOTE 2026-05-29 (code audit): no slug exists today, but it's cleanly
  > derivable — no wall. `agent_name` arrives in `AcpEvent::Initialized`
  > (from `agentInfo.name`, `acp.rs:570`) and `session_id` in
  > `SessionCreated` (`acp.rs:582`). Assign the slug at the
  > `SessionCreated` handler: `agent_name` lowercased + a per-agent-name
  > counter held on the `Renderer` (so the second Codex becomes `codex-2`).
  > The `AcpSession` is the natural place to cache the assigned slug for
  > the session's lifetime.

There is no separate actor-registry. Identity rides on coordinates that
already exist. Tags reference labels the same way they reference blocks:
`to:B1` addresses the agent in B1; `to:codex-1` addresses that ACP
session.

### Addressability

Activity IDs are globally monotonic — `act-1, act-2, ...` — unique
across the workspace. The display form composes with the parent for
locality:

- `B1.act-7` reads as *"the 7th activity inside block B1"*
- `codex-1.act-3` reads as *"the 3rd activity in codex-1's session"*

The raw `act-7` form is the canonical identifier; the dotted form is a
convenience for humans reading the room. Either resolves to the same
entry.

### Activity types

We commit to three kinds in v1:

- **`ToolCall`** — the canonical agent action. One per logical tool
  invocation, not per streamed chunk. Status moves Pending → InProgress
  → Completed/Failed.
- **`AgentMessage`** — the completed message at end-of-turn (not per
  streamed chunk). Carries `to: Option<ActorLabel>`: `None` means
  broadcast to the room; `Some` means addressed. This is the
  agent-to-agent channel.
- **`UserPrompt`** — the human's prompt that started a turn. Recorded so
  other actors can see *what was asked*, not just what was done.

We deliberately do **not** ship a streaming-chunks-as-activities model.
That blows volume out for no clarity gain. The conversation streams in
the chat pane as before; activities lock the completed events.

> NOTE 2026-05-29 (from lab E5, see `lab/lounge-validation/FINDINGS.md`):
> The scripted Claude-shape session surfaced one action that fits none of
> the three kinds cleanly — a **decision**. It's not a `ToolCall` (nothing
> invoked), not a `UserPrompt`, and not really an `AgentMessage` (addressed
> to no one). It was forced into `AgentMessage { to: None }`, which works but
> muddies the agent-to-agent channel with non-messages. Recommendation
> (Daniel's call, not yet settled): **don't add a 4th kind** — instead treat
> a decision as a broadcast message carrying a `decision` tag, reusing the
> shared tag namespace. Safe and lean; stays filterable. Revisit only if the
> convention bites.

## Emission paths

Two paths, both producing the same `Activity` shape:

### ACP-hosted agents — automatic

terminite already catches `AcpEvent::ToolCallStarted`,
`ToolCallUpdated`, `AgentMessageChunk`, `UserMessageChunk` in
`renderer.rs::handle_acp_event` to drive the chat pane. The same
hooks push to `ActivityStore` as a side-effect:

- `ToolCallStarted` → new `Activity::ToolCall` with status `Pending`
- `ToolCallUpdated` with status change → mutate the existing activity
- End of `AgentMessageChunk` stream (we see another event type) →
  finalize `Activity::AgentMessage`
- `UserPrompt` → activity is created when our send-prompt path fires,
  not when the agent echoes it back

> NOTE 2026-05-29 (code audit): the `ToolCall` path is fully backed —
> `ToolCallStarted`/`ToolCallUpdated` are already caught
> (`renderer.rs:4347-4371`). But the `AgentMessage` finalize has **no
> backing event today.** The turn-complete signal *does* arrive — it's the
> `stopReason` in the `session/prompt` response — but `classify_response`
> (`acp.rs:568`) only extracts `agentInfo`/`sessionId` and **drops the
> rest**, returning empty for a prompt result. So "we see another event
> type" is currently false. Net-new work, small and contained: add
> `AcpEvent::TurnEnded { stop_reason }`, emit it from `classify_response`
> when the prompt result has neither `agentInfo` nor `sessionId`, and
> finalize the open `AgentMessage` on it. ~5 lines + one enum variant +
> one match arm. Don't assume the event exists — it doesn't yet.

The agent doesn't have to know. The room records what it observes.

### Shell-hosted agents — opt-in via MCP

For long-running shell agents (Claude in your terminal, aider, future
CLIs), terminite has no view inside the process. We add an MCP tool:

```
terminite_activity_emit {
  kind: "tool_call" | "agent_message" | "user_prompt",
  title: String,
  to?: ActorLabel,            // agent_message only
  input?: String,
  output?: String,
}
```

When an agent wants its work to be visible — a file edit it just did,
a decision it just made, a signal to another agent — it calls the tool.
Otherwise it stays opaque, the same as today. The granularity is the
agent's choice.

Tool description (the AI sees this):

> *"Surface an action you just took so other actors in the room can see
> it. Use this for file edits, important decisions, signals to other
> agents, or anything you want addressable as `B?.act-N`. Calling this
> is opt-in; you remain otherwise opaque to the room until you do."*

This preserves the **additive, not forcing** principle: terminite is
an enhancement to the pair, not a replacement. Claude (or any CLI
agent) keeps running the way it runs. The room gains visibility only
when the agent chooses to share.

## Surface — proto verbs + MCP tools

**Proto (Unix socket):**
- `activities_list { tab_id?: u64, actor?: String }` → list of `Activity`
- `activity_get { id: u64 }` → single `Activity`
- `activity_tag_add { id: u64, tag: String }` / `activity_tag_remove`

**MCP (catalog additions):**
- `terminite_activities_list` — *"What's been happening in the room.
  Returns agent tool calls, messages, and human prompts in time order.
  Filter by actor label to see one agent's actions; omit to see all."*
- `terminite_activity_get`
- `terminite_activity_tag_add` / `_remove`
- `terminite_activity_emit` — as above

Block tags + activity tags share **one namespace**. `"to:codex-1"`
attaches the same meaning whether the target is `B7` or `act-42`.

## Resource discipline

System-impact pass already earned:

- `MAX_ACTIVITIES_TOTAL` — workspace-wide cap (~10,000). Eviction:
  oldest closed first; pinned activities (tagged with an explicit pin)
  survive longer.
- `MAX_INPUT_BYTES` / `MAX_OUTPUT_BYTES` per activity (e.g. 64 KB each).
  Truncate at limit with a `... [N bytes truncated]` marker.
- `MAX_ACTIVITY_TAGS` per entry (~32).
- Atomic write to disk on a flush schedule; same shape as block
  persistence.

## What success looks like — the regression test

The Stage-1 experiment is the regression test. After this brick ships:

1. Spawn two ACP Codex panes side by side.
2. Tell Codex A: *"Run a few tool calls. Then check who else is here."*
3. Codex B calls `terminite_activities_list` → sees Codex A's tool calls
   with `actor: "codex-1"`.
4. Codex B can address Codex A: `terminite_activity_emit { kind:
   "agent_message", to: "codex-1", text: "I see act-3 — interesting" }`.
5. Codex A polls and sees the addressed message.

That's the "Codex sees Codex" milestone the scout's report identified.

For shell-hosted: when Claude runs in a shell tab and calls
`terminite_activity_emit` for a real edit, Codex sees `B1.act-N` and
can reference it.

## What's deliberately not in this design

- **Wall B (push)** — subscribe-and-broadcast wire. Brick 4. Without
  it the read stays pull; that's accepted for v1.
- **Cross-session backfill** — when an ACP session re-opens, it starts
  fresh. Prior activities persist on disk but the session sees an empty
  recent timeline. History queries (later) can backfill.
- **Multi-human / cross-room** — single human, single room. The shape
  doesn't preclude wider models; we just don't build for them yet.
- **CRDT for shared editing** — also brick 4 / 5.
- **Forcing shell agents to emit** — the room enhances; it does not
  require. An agent that emits nothing is fine; it stays as opaque as
  it is today. See `feedback-additive-not-forcing` memory.

## Open questions tracked for later

1. **Unified room view** — should there be a `terminite_room_list` MCP
   tool that merges blocks + activities chronologically? Probably yes
   eventually, but as a view on top of the two stores. Defer.
2. **Tag namespace conflicts** — block `B1` and activity `act-1` share
   tag space. Both could carry `"reviewed"`. That's likely fine; they're
   different objects under different verbs. Re-examine if it bites.
3. **Activity authorship attribution** — when terminite synthesizes an
   ACP-side activity, the agent didn't directly emit it; terminite did.
   For shell-hosted opt-in, the agent did emit it. Do we mark this
   distinction (`source: Observed | Emitted`)? Lean: no, the activity is
   the truth either way. Worth revisiting if it matters downstream.
4. **Identity stability across re-opens** — if a Claude CLI session
   exits and a new one starts in the same block, is that the same actor
   or a new one? Lean: new actor; block stays, but the inhabitant
   changed. Worth noting in any UI that shows actor history.

## The shape the brick ships in

Bounded scope for the build pass:

1. `ActivityStore` parallel to `BlockStore` — same persistence shape,
   same caps discipline.

   > NOTE 2026-05-29 (code audit, "does the structure hold for terminite
   > itself"): "parallel to `BlockStore`" is true for *discipline* (caps,
   > eviction, persistence) but **false for location, and the difference
   > bites silently.** `BlockStore` lives on `Tab` — it is *per-tab*
   > (`renderer.rs:195`, accessed as `tab.blocks.iter()` at ~3632).
   > Activities exist precisely for *cross-pane* visibility (Codex in tab 2
   > seeing `B1`'s activity in tab 1), so `ActivityStore` must live on the
   > **`Renderer`, workspace-global** — alongside `proto_subscriber`
   > (`renderer.rs:1403`) — with the monotonic `act-N` counter there. An
   > implementer who reads "parallel to `BlockStore`" and puts it on `Tab`
   > breaks the entire reason activities exist, with no compile error to
   > catch it. Build it workspace-global.
2. ACP emission wiring in `renderer.rs::handle_acp_event` — additive
   side-effects, no change to existing rendering.
3. `terminite_activity_emit` MCP tool — new tool in the catalog, no
   change to existing tools.
4. `activities_list`/`activity_get`/`activity_tag_*` proto verbs.
5. CLI wrappers (`terminite activities`, `activity show`, etc.).
6. Tests: an ActivityStore unit suite + the regression test above
   integrated into the manual smoketest.

What we will resist adding to this pass:
- Per-actor cursors (brick 4)
- Multi-cursor presence rendering in the gutter (brick 4)
- Subscription / push (brick 4)
- Activity-block linkage as a separate join structure (just use
  `parent`)

## Coda

The Codex `codex/terminite-presence-report.md` is the document this
brick is built to answer. When the brick ships, re-running that same
prompt should produce a report that includes another visible actor and
their actions — not just identical tab titles. The wall is real and
crossable, and this design is what we think crossing it cleanly looks
like.

*"You stay B1. You always stay B1. Activities are the sub-events
inside the block you inhabit."* — Daniel, the realignment that
produced this doc.
