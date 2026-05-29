# Lounge validation — empirical check before Rust

**Status: planned, not built.** This folder exists so a fresh Claude
session can build the experiment by reading these files cold, without
re-deriving the scope from a transcript. If you're opening this for
the first time, read [`../../guide/activities-design.md`](../../guide/activities-design.md)
first — that's the design this lab validates.

## Why this exists

The Stage-1 Codex-sees-Codex experiment (2026-05-29) showed the room
model erases actor identity. We wrote `guide/activities-design.md`
proposing the activities layer as the fix. Before pouring 200–300
lines of Rust into terminite proper, we want to know:

1. **Does the protocol shape actually deliver the visibility we
   designed?** Agent A emits → agent B sees → addressing works.
2. **Where's the pull-poll pain?** Empirically measure how stale
   `activities_list` results feel without a push wire. Concrete data
   to decide whether brick 4 push is urgent.
3. **What feels missing once it's running?** A scripted Claude-shaped
   agent in the room surfaces gaps in the design that paper review
   misses.

The point of doing this outside terminite is **isolation**: if the
design breaks, we discover it here, not in the main codebase. If it
holds, we port to Rust with confidence.

## What's being built

```
lab/lounge-validation/
├── README.md          (this file — the plan)
├── FINDINGS.md        (template for results — empty until run)
├── proto_server.py    (mock terminite: Unix socket + ActivityStore)
├── mcp_bridge.py      (stdio MCP server — what each agent spawns;
│                       connects to proto_server via socket)
├── agent_mock.py      (scripted Python agent — emits + reads via MCP)
└── run.sh             (orchestrates: start proto_server, spawn agents,
                       run scenarios, capture timings)
```

Python (not Rust): faster iteration; we're testing the *design*, not
the implementation. If the design is right, porting to Rust is
mechanical. If wrong, we discover here without touching the main app.

### File specifications

**`proto_server.py`** (~200 lines)
- Listens on a Unix socket at `/tmp/lounge-validation.sock`.
- Implements an in-memory `ActivityStore` matching the schema in
  `activities-design.md` §"The model":
  - `Activity { id, parent, actor, agent_name, kind, status, opened_at,
    closed_at, tags }`
  - `ParentRef::Block(BlockId) | ParentRef::Actor(ActorLabel)`
  - `ActivityKind::ToolCall | AgentMessage { from, to, text } |
    UserPrompt`
- Proto verbs (JSON-RPC over the socket):
  - `activities_list { actor?, parent? }` → list
  - `activity_get { id }` → single
  - `activity_emit { kind, title, ... }` → returns new id (this is
    invoked by `mcp_bridge.py`, not by the proto directly)
  - `activity_tag_add { id, tag }` / `activity_tag_remove`
- Caps: `MAX_ACTIVITIES_TOTAL = 10_000`, `MAX_INPUT_BYTES = 65_536`,
  oldest-closed-first eviction. Mirror the discipline from the design
  doc.
- Logs every operation to `proto.log` with timestamps so we can do
  latency math from the file.

**`mcp_bridge.py`** (~100 lines)
- Short-lived per-call (spawned fresh by each agent for each MCP tool
  call) — same architecture as the real terminite mcp bridge.
- Speaks MCP over stdio (JSON-RPC 2.0): `initialize`, `tools/list`,
  `tools/call`, `notifications/initialized`, `ping`.
- Catalog exposed to the agent (mirror the activities-design proposal):
  - `terminite_activities_list` { actor?, parent? }
  - `terminite_activity_get` { id }
  - `terminite_activity_emit` { kind, title, input?, output?,
    to?, parent? }
  - `terminite_activity_tag_add` / `_remove`
- Tool descriptions copy the prose from `activities-design.md` §"Surface"
  verbatim — that prose is part of the design under test (we're
  validating it's self-documenting enough that agents discover the
  tools naturally).
- Bridges every call to `proto_server.py` via the Unix socket.

**`agent_mock.py`** (~150 lines)
- Runnable as either *Codex-shaped* or *Claude-shaped* via a CLI arg
  (`--shape codex|claude`).
- Codex-shaped: emits ~5 tool calls per turn, all visible. Mirrors what
  the real Codex does in an ACP pane.
- Claude-shaped: emits selectively — only the few actions Claude would
  choose to share (file edits, key decisions). Models the opt-in
  shell-hosted path. The selection logic is *the experiment* — we want
  to feel what feels right to emit.
- Both connect to the MCP bridge as if they were an ACP-hosted agent;
  poll `terminite_activities_list` to read what others are doing.
- Configurable poll cadence (default: 500ms) so we can measure pull
  staleness.

**`run.sh`** (~50 lines)
- Boots `proto_server.py` as a background process.
- Runs each scenario below, captures timings, dumps results into
  `runs/<timestamp>/` (one folder per run with logs + summary).
- Scenarios (each maps to an experiment below):
  - `scenario_e1_two_codex_shaped`
  - `scenario_e2_codex_plus_claude_shaped`
  - `scenario_e3_agent_to_agent_addressing`
  - `scenario_e4_latency_sweep`
  - `scenario_e5_claude_shape_introspection`
- Tears down cleanly on exit (kills proto_server, removes socket).

## Experiments

Each experiment has a goal, a procedure, and explicit pass/fail criteria.
The fresh Claude session running this should update `FINDINGS.md` after
each run.

### E1 — Emission round-trip (the basic claim)

**Goal:** Validate `agent_a.emit() → agent_b.activities_list()` shows
the emitted activity, with the actor label preserved.

**Procedure:**
1. Spawn two `agent_mock.py --shape codex` instances (call them
   `codex-1`, `codex-2`).
2. `codex-1` emits 10 `ToolCall` activities (`read_file`, `bash`, etc).
3. `codex-2` polls `activities_list`.
4. Verify: `codex-2` sees all 10, attributed to `actor: "codex-1"`,
   in time order.

**Pass:** all 10 visible, correct attribution, time order preserved.
**Fail:** missing entries, wrong attribution, out-of-order — any of
those invalidates the design's identity-via-coordinate claim.

**Capture:** emit→visible latency (median, p95).

### E2 — Codex + Claude-shaped coexistence

**Goal:** Two agent shapes (high-volume Codex + selective Claude) coexist
without one swamping the other.

**Procedure:**
1. Spawn one `--shape codex` (emits ~5/turn) and one `--shape claude`
   (emits ~1/turn).
2. Run for 50 turns each.
3. Both poll the other.

**Pass:** both can see the other's stream; neither overwhelms the
ActivityStore; eviction (if triggered) drops oldest closed first, not
the wrong shape's activities.

**Capture:** activities-per-actor at end, store size, eviction count.

### E3 — Agent-to-agent message addressing

**Goal:** `ActivityKind::AgentMessage { to: Some(actor) }` actually
delivers and is filterable.

**Procedure:**
1. `codex-1` emits an `AgentMessage` with `to: "codex-2"`, text
   `"look at act-3"`.
2. `codex-2` polls `activities_list` and filters for `kind:
   AgentMessage AND to: self`.
3. `codex-2` responds with another `AgentMessage to: codex-1`.

**Pass:** both directions deliver; filtering by addressee works; the
addressed agent can distinguish "for me" from "for the room"
(`to: None` broadcast).

**Capture:** does the "broadcast vs addressed" distinction feel
natural or forced? Document in FINDINGS.

### E4 — Polling latency sweep (the brick 4 question)

**Goal:** Concrete data on whether push (brick 4) is urgent or whether
polling at sensible cadence is enough.

**Procedure:**
1. Single emitter, single poller.
2. Sweep poll cadence: 100ms, 250ms, 500ms, 1s, 2s, 5s.
3. For each cadence, measure: median latency from emit to poller-sees,
   p95 latency, CPU usage of the poller, message rate from the proto.

**Pass:** there's a cadence (probably 250–500ms) where latency feels
"live" without burning CPU. If even 100ms feels laggy, push is urgent.
If 2s feels fine, brick 4 can wait longer.

**Capture:** the table. This becomes the answer when someone next asks
"do we need push yet."

### E5 — Claude-shape introspection (the gap experiment)

**Goal:** Surface what's missing in the design for shell-hosted Claude
opt-in emission.

**Procedure:**
1. Run `agent_mock.py --shape claude` alone (no peer).
2. Watch what it chooses to emit across a scripted "work session"
   (edit 3 files, run 5 commands, make 1 decision).
3. Compare against what would be useful for a peer agent to see.

**Pass:** the agent's natural emission choices look like a usable
visibility trace. Gaps surface as design questions for the next
session.

**Capture:** sample emissions, what felt right to share, what felt
awkward, what categories of action didn't fit the three `ActivityKind`s
cleanly.

## How to run

```bash
cd lab/lounge-validation
./run.sh                  # all scenarios, dumps to runs/<timestamp>/
./run.sh e1               # single scenario
./run.sh e4 --cadence 250ms,500ms,1s  # custom params
```

Results land in `runs/<timestamp>/summary.md` plus per-scenario log
files. The Claude running this should manually transcribe the summary
into `FINDINGS.md` with their commentary.

## When this lab gets archived

The lab is **scaffolding, not infrastructure.** Once the activities
brick is built in terminite proper and the regression test from
`activities-design.md` §"What success looks like" passes against the
real implementation, this folder can be moved to `lab/_archive/` (or
deleted, with a git tag for posterity). It's not meant to live forever.

## Trail markers

- **Design under test:** [`../../guide/activities-design.md`](../../guide/activities-design.md)
- **Wall this is built to address:** [`../../codex/terminite-presence-report.md`](../../codex/terminite-presence-report.md)
- **Session that produced this plan:** `guide/history.md` 2026-05-29 entry
- **Standing direction guards:** memory `terminite-acp-is-approximation`,
  `feedback-additive-not-forcing` (don't drift while building this).
