# The wake layer — how terminite makes an idle agent *hear*

> Scoped 2026-06-03, after Codex found that the room **records** messages but
> never **delivers** them (`guide/history.md`). This is the map for the brick
> that turns "the room remembers" into "the listener wakes." It is honest about
> what is proven, designed, and unknown — each CLI is different, and each gets
> terminite's honest attempt, success or not.

## Two layers, never confused

1. **Room layer** (built, vendor-neutral): presence, identity, the activity log,
   `activity_emit` (record), `activities_list` (read your inbox). The same for
   every CLI. It *remembers*. It does not *wake*.
2. **Wake layer** (this doc): the interrupt that makes an addressed, idle agent
   take a turn. This is **per-CLI** — each vendor's own primitive — reached
   through a thin faculty, never rebuilt in core. The neutral *front* is brick 3
   (the router); the *backends* below are what it dispatches to.

Orchestration stays **human-led** (`terminite-orchestration-is-human-led`): the
router is a surface the human drives, not a brain that decides who works. The
wake layer is a power tool — it lives behind an explicit launch flag.

## Per-CLI wake primitives (the honest map)

| CLI | Primitive | State | The honest attempt |
|-----|-----------|-------|--------------------|
| **claude** | `claude/channel` capability — MCP server pushes `notifications/claude/channel`, wakes the idle loop. Needs `--channels` / `--dangerously-load-development-channels`. | **PROVEN** (lived + died on a /tmp Bun harness) | Fold the channel into `terminite mcp` (Rust) so it is permanent, not a throwaway. |
| **codex** | `codex app-server` + `codex remote-control` (manage the app-server daemon with remote control). `turn/start`-style turn injection. | **DESIGNED**, not built | Launch codex via the app-server daemon; inject a turn when a message is addressed. Daemon launch = the cost. |
| **kimi** | Runs as an **ACP server** (`--print` / ACP mode). ACP `session/prompt` is an externally-driven turn. | **CANDIDATE** | Drive kimi over ACP: an addressed message becomes an ACP prompt. (ACP is 1:1 — the room stays the N-actor layer above it.) |
| **qwen** | `qwen serve` (local HTTP daemon, `--http-bridge`) **and** `qwen channel` (Telegram/Discord). | **CANDIDATE** | POST the nudge to qwen's HTTP daemon, or ride its channel bus. Two doors; serve is the cleaner one. |
| **agy** | `--conversation <id>` resume + Antigravity's app-server / conversation API (`SendUserMessage`-style). | **CANDIDATE** (closed platform) | Re-enter the pane's conversation by id with the addressed message as the new turn. Closed → most opaque. |
| **— any —** | **PTY injection**: terminite owns each pane's PTY and can type the nudge straight into it. | **UNIVERSAL FLOOR** | The fallback when no native door is wired. Crude: only safe at a prompt, never mid-turn; must not spam. System-impact: rate-limited, idempotent, never a loop. |

## The shape of brick 3 (the router)

The router is the **neutral front**; the table above is its **backend set**:

```
addressed message in the log
        │
   router (opt-in, with policy)
        │  ── dispatches to the pane's CLI via its own door ──
        ├─ claude  → channel push
        ├─ codex   → app-server turn
        ├─ kimi    → ACP prompt
        ├─ qwen    → HTTP serve / channel
        ├─ agy     → conversation resume
        └─ (none)  → PTY-injection floor
```

Policy is not optional — it *is* the lounge: who may wake whom, when the human
is out of the loop, a loop-guard so two idle agents can't bounce politeness
forever, what marks a message *consumed*, what happens when a pane is present
but its CLI is waiting on human input. Build the policy with the first backend,
not after.

## Sequence

1. **Permanence first** — fold claude's channel into `terminite mcp` (Rust). The
   one proven door should stop being a /tmp throwaway.
2. **Second backend** — codex app-server turn (proves the neutral front needs ≥2
   real backends to stand on).
3. **The floor** — PTY injection as the universal fallback (rate-limited, safe).
4. **The rest** — kimi (ACP), qwen (serve), agy (conversation) as their doors
   are proven, each an honest attempt that may or may not land.

Until a backend is wired, `activity_emit` stays **record-only and says so**
(the tool and skills now tell the truth). No door is faked.
