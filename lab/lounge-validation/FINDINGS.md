# Findings — lounge validation lab

Live observations log. One section per experiment run. Append; do not
overwrite. The Claude session that runs the experiment writes here.

Format: each entry has the date, the experiment ID (E1–E5), what was
expected, what was observed, what was surprising, and what (if
anything) needs to change in the design before the brick is built in
terminite proper.

If something we observe here **breaks** an assumption in
`../../guide/activities-design.md`, flag it in the design doc with an
inline `> NOTE 2026-MM-DD: ...` and reference the FINDINGS entry. We
want the design doc to evolve from the empirical data, not from re-
reading paper alone.

---

## Template (copy this when starting a run)

### YYYY-MM-DD · E? — <one-line title>

**Run:** `runs/<timestamp>/`
**Hypothesis:** what we expected before the run
**Result:** Pass / Fail / Mixed
**Observed:**
- key data point 1
- key data point 2
- ...

**Surprises:**
- anything we didn't predict (positive or negative)

**Design implications:**
- does anything in `activities-design.md` need to change?
- new questions raised?

**Notes for the next runner:**
- what to try next, what to watch for

---

### 2026-05-29 · E1 — emission round-trip

**Run:** `runs/20260529-133927/`
**Hypothesis:** `agent_a.emit() → agent_b.activities_list()` shows all
emitted activities, attributed to the emitter, in time order.
**Result:** **Pass**
**Observed:**
- codex-2 saw 10/10 of codex-1's emissions; order preserved (id == time order).
- Attribution was host-assigned: codex-1's label rode on the activity even
  though the agent never passed its own name — the proto stamped it from the
  connection's `hello`. The "identity = visible coordinate" claim holds *and*
  is forgery-resistant: an agent can't emit as someone else.
- emit→seen latency: median 137ms, p95 236ms at a 250ms poll cadence.

**Surprises:**
- None mechanically. The forgery-resistance was the satisfying part — it's
  stronger than the design doc claims. Worth keeping when porting to Rust:
  the actor label must come from the session terminite assigned, never from
  the tool arguments.

**Design implications:** none. The core claim is validated.

---

### 2026-05-29 · E2 — coexistence + eviction

**Run:** `runs/20260529-133927/`
**Hypothesis:** a high-volume Codex-shaped emitter and a selective
Claude-shaped emitter coexist; eviction (when the cap is hit) drops
oldest-closed-first and doesn't preferentially erase the quiet actor.
**Result:** **Pass**
**Observed:**
- Emitted codex-1=250, B1(claude)=50 against a deliberately small cap of 120.
- 180 evictions, oldest-closed-first. Final store: codex-1=103, B1=17, total=120.
- **Both actors survived eviction.** The quiet actor wasn't wiped — its share
  of the survivors (17/120 ≈ 14%) tracks its share of recent emissions, which
  is the honest behavior. Eviction is recency-based, not actor-based.

**Surprises:**
- The selective agent's *older* emissions do age out under sustained pressure
  from a noisy peer. With a real 10,000 cap this is a non-issue, but it names
  a real dynamic: in a loud room, a quiet agent's early work scrolls off first.
  The design's "pin" tag is the escape hatch and it worked (pinned items were
  excluded from eviction). Document that pinning is how you keep a decision
  alive in a noisy room.

**Design implications:** none required. Confirms the caps discipline. Note
added below re: pinning as the durability mechanism.

---

### 2026-05-29 · E3 — agent-to-agent addressing

**Run:** `runs/20260529-133927/`
**Hypothesis:** `AgentMessage { to: Some(actor) }` delivers both directions
and `to:self` filtering distinguishes "for me" from broadcast.
**Result:** **Pass**
**Observed:**
- Both directions delivered. codex-1 received from codex-2 and vice-versa.
- `activities_list { kind: agent_message, to: <self> }` returned only addressed
  messages; broadcasts (`to:None`) were correctly excluded from the addressed
  filter.
- The dotted reference form worked end-to-end in the message body:
  `"yes, I see codex-1.act-2"` — agents can point at each other's activities
  by coordinate.

**Surprises:**
- The broadcast-vs-addressed distinction felt natural for *messages*. It only
  felt forced when used for a *decision* (see E5) — which isn't a message at
  all. The channel itself is clean; the strain is using it for non-messages.

**Design implications:** none for addressing. The strain is a kind-coverage
question, raised in E5.

---

### 2026-05-29 · E4 — polling latency sweep (the brick-4 question)

**Run:** `runs/20260529-133927/`
**Hypothesis:** there's a poll cadence that feels live without burning CPU;
push (brick 4) is or isn't urgent based on the numbers.
**Result:** **Mixed → actionable**

| poll cadence | median lat | p95 lat | poller CPU (6s) | polls |
|---|---|---|---|---|
| 100ms | 51ms | 96ms | 0.040s | 58 |
| 250ms | 149ms | 247ms | 0.030s | 24 |
| 500ms | 295ms | 496ms | 0.030s | 12 |
| 1000ms | 541ms | 877ms | 0.020s | 6 |
| 2000ms | 1057ms | 1793ms | 0.020s | 3 |
| 5000ms | 2660ms | 4486ms | 0.020s | 2 |

**Observed:**
- Latency is mechanically `≈ cadence/2` median, `≈ cadence` p95. Socket+store
  round-trip is sub-millisecond — the poll interval *is* the latency.
- CPU is negligible everywhere. Even 100ms cadence cost 0.04s over 6s ≈ 0.7%
  of one core. Polling is cheap.

**Surprises:**
- The result is partly tautological (of course pull latency ≈ cadence), but it
  settles the brick-4 question on **performance** grounds: **push is NOT
  urgent for latency or CPU.** 250–500ms polling is live-enough and free.

**Design implications:**
- The real argument for push (brick 4) is **not** performance — it's
  **attention**. A scripted mock polls on a fixed loop; a *real* agent only
  polls between turns. An agent deep in a 30-second tool call won't notice it
  was addressed until it surfaces. So push matters for "tap me on the shoulder
  mid-work," not for throughput. The lab can't measure that with scripted
  agents — flag it as the thing the real-Codex follow-up should probe.
- **Recommendation:** ship Wall A (activities) with pull-only polling as
  designed. Defer push. Revisit push only when a real agent reports missing a
  message because it was mid-turn — not before.

---

### 2026-05-29 · E5 — claude-shape introspection (the gap experiment)

**Run:** `runs/20260529-133927/`
**Hypothesis:** a selective Claude-shaped agent's natural emission choices
form a usable visibility trace; gaps in the three `ActivityKind`s surface.
**Result:** **Mixed — one real gap found**
**Observed:**
- Of 9 scripted actions, the agent emitted 5 and stayed opaque on 4 (`ls`,
  `git status`, `cd`, a local scratch edit). The opt-in path worked: noise
  stayed invisible; shareworthy actions (real edits, test runs, the decision)
  surfaced. The resulting trace reads like a usable summary of the session.

**Surprises / the gap:**
- **A *decision* does not fit the three kinds.** It's not a `ToolCall` (nothing
  was invoked), not a `UserPrompt`, and not really an `AgentMessage` (it's
  addressed to no one — it's a state-marker, not communication). It got forced
  into `AgentMessage { to: None }`. That *works*, but it muddies the
  agent-to-agent channel with things that aren't messages, and a peer filtering
  for "messages to me or the room" gets decisions mixed in with greetings.

**Design implications — flagged inline in `activities-design.md`:**
- The gap is real, but the fix should stay **safe and lean**. Three options
  considered; recommendation is (b):
  - (a) Add a 4th kind `Decision`/`Note`. Rejected — the design deliberately
    committed to exactly three kinds; adding one casually is the scope drift
    the project guards against.
  - **(b) Convention over schema: a decision is a broadcast `AgentMessage`
    tagged `decision`.** Zero new schema, uses the existing shared tag
    namespace, stays filterable (`activities_list` could filter by tag later).
    This is the additive, lean move. **Recommended.**
  - (c) Rename `AgentMessage`'s broadcast mode to "room note" conceptually and
    document that `to:None` serves decisions. Weakest — it's just (b) without
    the filterable tag.
- Decision deferred to Daniel: this is exactly the "felt shape" call the
  partnership reserves for him. The lab's job was to surface it, not settle it.

---

### 2026-05-29 · Real-Codex follow-up — discoverability (precision 2)

**Run:** `runs/real-codex-*/` · driver: `real_codex_followup.sh`
**Hypothesis:** the design's load-bearing claim — "the vocabulary has to be
self-evident at the protocol layer" — holds for a *real* agent: dropped in
cold with no tool hints, Codex discovers `terminite_activities_list` from its
prose and uses it to answer "who else is here?"
**Result:** **PASS — full round-trip with a real agent (after authorized run).**

> UPDATE 2026-05-29 (authorized bypass run, `runs/real-codex-20260529-141549/`):
> with the user's explicit OK, the run used
> `--dangerously-bypass-approvals-and-sandbox` (empty cwd; success judged by
> proto.log only). Result: **clean pass.** proto.log shows codex-2 made one
> real `list` call returning `ids:[1,2,3,4,5,6]`, and Codex reported codex-1's
> six activities correctly — read renderer.rs ×2, cargo build, grep
> handle_acp_event, edit acp.rs, read activities-design.md. No `ps` fallback,
> no source-read. This is the regression test from `activities-design.md`
> §"What success looks like" passing end-to-end with a REAL agent:
> **"Codex sees Codex"** — the direct answer to the presence report's
> *"I appear to be alone here."* Discovery + execution + cross-actor visibility
> all confirmed.

**Setup:** real `codex exec` (gpt-5.5) as `codex-2`, our `mcp_bridge.py`
registered as an MCP server via inline `-c` (host-assigned identity, no change
to the user's `~/.codex/config.toml`). Peer `codex-1` pre-seeded with 6 tool
calls. Prompt named **no** tools.

**Observed:**
- **Discovery works.** In both runs Codex immediately chose
  `terminite_activities_list`, reasoning aloud: *"I'm checking the room
  activity log, since that's the workspace-level signal for who else has acted
  here."* It picked the right tool from the description alone. Precision 2's
  core claim is validated — and consistent with the original in-terminite scout
  (`codex/terminite-presence-report.md`), which *successfully executed*
  terminite's MCP tools when hosted in a real ACP pane.
- **In-lab execution blocked.** `codex exec` gates MCP/dynamic tool calls
  behind `default_tools_approval_mode` (the `GranularApprovalConfig`, *separate*
  from `approval_policy`). Non-interactively it cancels the call ("user
  cancelled MCP tool call") before it reaches the bridge — proto.log shows
  `hello` from codex-2 but zero `list` ops. Neither `approval_policy=never` nor
  `=on-failure`, nor `-s read-only`/`workspace-write`, unblocks it. The only
  known switch is `--dangerously-bypass-approvals-and-sandbox`, which also
  disables the OS sandbox — not run (safety).

**Surprises / method findings:**
- **Run 1 was a false positive.** With a normal cwd, Codex got the *correct*
  answer by `sed`-ing the lab's own `agent_mock.py` source — not via the room.
  proto.log (zero `list` ops) caught it. The report alone would have fooled us.
  Lesson: **proto.log is ground truth; the agent's prose is not.**
- **Run 2 was an honest negative.** Empty cwd (no source to cheat from) + tool
  blocked → Codex reported *"I don't have reliable evidence of anyone else
  being present"* rather than inventing a peer. Correct failure behavior.

**Design implications:** none for the design itself — the blocker is the codex
*harness*, not the activities shape. Discoverability (the thing only a real
agent could test) holds. Execution-against-the-room by real Codex is already
demonstrated in terminite proper by the original scout; reproducing it in this
out-of-band lab needs the codex approval knob cracked or the bypass authorized.

**Notes for the next runner:** if you want the clean in-lab execution loop,
either (a) find the exact `-c` path for `default_tools_approval_mode` (it is
NOT `tools.default_tools_approval_mode` nor top-level — both rejected by
`--strict-config`), or (b) run with `--dangerously-bypass-approvals-and-sandbox`
with the user's explicit OK, empty cwd to prevent the source-read cheat, and
verify success via `list` ops in proto.log — not via Codex's prose.

---

## Open questions tracked across runs

(Added by each runner as they go. Don't lose these between sessions.)

1. **Decision/Note coverage** (E5) — do we add a tag convention (`decision`)
   for broadcast messages that are really state-markers, or leave it? Daniel's
   call. Lean: tag convention, no new kind.
2. **Push is an attention question, not a perf question** (E4) — the scripted
   lab can't measure whether a real agent misses mid-turn messages. The
   real-Codex follow-up should probe this directly before brick 4 is scoped.
3. **Pinning is the durability mechanism in a loud room** (E2) — a quiet
   actor's early work ages out under a noisy peer. `pin` tag is the escape
   hatch and works. Make sure the Rust port keeps pin-excluded-from-eviction.
4. **Forgery-resistant identity** (E1) — host-assigned actor labels (from the
   session, never from tool args) are stronger than the doc states. Keep this
   property in the Rust port; don't let `activity_emit` accept an `actor` arg.

---

## What this scripted lab did NOT test (the honest boundary)

The mocks are deterministic scripts, not LLMs. So this lab validated the
**mechanics** of the design, not its **discoverability** or its **feel**:

- **Self-discovery from prose** — whether a real agent finds and correctly
  uses the tools from the catalog descriptions alone. Untested. This is the
  whole point of the real-Codex follow-up (lab README §Option A).
- **True staleness feel** — scripted agents poll on a fixed loop; real agents
  poll between turns. The "does pull feel laggy" question (E4) needs a real
  agent in the loop.
- **The emission-choice instinct** (E5) — what a *real* Claude chooses to emit
  vs. what the script was told to emit. The gap it found (decisions) is real,
  but a real agent might surface gaps the script's author didn't think to test.

**Next step:** the real-Codex follow-up. Spawn an actual Codex (or two) against
this same mock proto+bridge, ask it "who else is here, and what have they
done?", and watch whether it discovers the tools and reports the *other* actor
— the regression test from `activities-design.md` §"What success looks like."

---

## Verdicts to date

(Concise rolling summary so a new session doesn't have to read every
entry to know where we are.)

- **Activities design overall:** **mechanically validated.** Emission,
  attribution, ordering, coexistence, eviction, and bidirectional addressing
  all pass. The shape is sound; porting to Rust is mechanical.
- **Pull-poll cadence sweet spot (E4):** **250–500ms.** Latency ≈ cadence/2,
  CPU negligible. Push (brick 4) is not urgent on performance grounds.
- **AgentMessage broadcast vs addressed (E3):** **natural for messages.** The
  channel is clean; the only strain is using it for non-messages (decisions).
- **Three `ActivityKind`s sufficient (E5):** **almost — one gap.** Decisions
  don't fit cleanly. Recommended fix: `decision` tag on a broadcast message,
  not a 4th kind. Daniel's call.
- **Discoverability + execution (precision 2):** **validated end-to-end.**
  Real Codex discovers `terminite_activities_list` from its prose with no
  hints, calls it, reads the room, and reports the *other* actor's activities
  (proto.log ground truth: one real `list` returning all 6 peer activities,
  empty cwd, no cheat). "Codex sees Codex" achieved — the direct answer to the
  presence report. The design's "self-evident at the protocol layer" claim
  holds for a real agent.
- **Ready to build in terminite Rust?** **Yes for the mechanics, and
  discoverability is confirmed.** Remaining: (1) Daniel's call on the
  decision-kind gap; (2) the three code-audit NOTEs in `activities-design.md`
  (workspace-global store, `AcpEvent::TurnEnded`, slug assignment) folded into
  the build.
