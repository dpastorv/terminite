# The wake layer — requirements spec (source-dived 2026-06-03)

> Scoped after Codex found the room **records** but never **delivers**. This is
> the buildable spec for the bridge that makes an idle agent *hear*, written
> after source-diving all five CLIs' actual wake APIs. It is honest about which
> vendors can be woken, how, and at what cost — each is different, and two can't
> be woken at all except by the universal floor.

## Two layers, never confused

1. **Room layer** (built, vendor-neutral): presence, identity, activity log,
   `activity_emit` (record), `activities_list` (read inbox), the see-half. The
   same for every CLI. It *remembers and shows*. It does not *wake*.
2. **Wake layer** (this spec): the interrupt that makes an addressed, idle agent
   take a turn. Per-CLI, reached through a thin faculty, behind an explicit
   launch mode. Orchestration stays **human-led** ([[terminite-orchestration-is-human-led]]).

## THE HEADLINE: there is no uniform wake

The source-dive killed the hope of one mechanism. Reality:

- **No CLI can be woken by a plainly-launched, independent process for free.**
  Every native wake requires a **special launch mode** (a wrapped launch).
- **Only 3 of 5 have an external wake at all.** kimi and agy have **none** that
  reaches a running agent — their only path is the universal floor.
- **PTY injection is therefore not a last resort — it is the floor everything
  stands on, and the *only* option for kimi and agy.**

So the bridge is: **PTY-injection baseline for all five**, with **native
backends as opt-in upgrades** for the three that support them, when launched in
wake-mode. This inverts the earlier plan (which would have built claude's
channel first and generalized) — and is exactly why understand-first was right.

## Per-CLI wake requirements (the spec)

| CLI | Native wake | Launch mode needed | Parent-required? | Verdict |
|-----|-------------|--------------------|------------------|---------|
| **codex** | app-server `turn/start` over a unix socket | **daemon**: `codex remote-control start` (NOT a plain TUI — plain `codex` is a *client*, `--remote` connects out) | **NO** — socket-attachable | ✅ cleanest native |
| **claude** | `notifications/claude/channel` push | claude spawns a **channel server** via `--channels server:lounge` (`--dangerously-load-development-channels`, preview, Anthropic-auth, interactive) | inverted: **claude spawns the channel**, which connects back to terminite's socket | ✅ works, coupled |
| **qwen** | HTTP `POST /session/:id/prompt` (ACP-over-HTTP+SSE, `127.0.0.1:4170`) | **`qwen serve --http-bridge`** — a *separate headless daemon*, not the interactive TUI | **NO** — HTTP-reachable, but the pane runs the daemon, not a TUI | ⚠️ works, different UX |
| **kimi** | ACP / `--wire` (`prompt`/`steer`) | **stdio only** — terminite must **spawn kimi** with piped stdio | **YES** — parent-required | ❌ conflicts with no-hosting foundation → PTY only |
| **agy** | `SendAgentMessage` gRPC to an internal language-server sidecar | sidecar on a dynamic localhost port, undiscoverable + likely auth'd; `--conversation <id>` only **spawns a fresh process**, doesn't wake the running one | internal-only | ❌ no external wake → PTY only |
| **— any —** | **PTY injection** — terminite types the nudge into the pane's own PTY | none (terminite owns the PTY) | n/a | floor — universal, crude |

### Per-CLI detail
- **codex** — `codex remote-control start` (or `codex app-server daemon start
  --remote-control`) opens a unix socket; send JSON-RPC `turn/start`
  `{threadId, input:[{type:"text",text}]}`; replies stream as `turn/started` /
  `item/agentMessage/delta` / `turn/completed` notifications on the same
  connection. Also `thread/inject_items` (silent context). Experimental (v0.136).
  terminite builds: launch the daemon, discover the socket, a JSON-RPC client.
- **claude** — channels push into a *running* session but the channel server is
  **spawned BY claude** (via `--channels`). So permanence = rewrite the proven
  /tmp Bun bridge as a `terminite channel` subcommand (Rust) that claude spawns;
  it connects back to terminite's socket, polls the actor's inbox, and pushes
  `notifications/claude/channel {content, meta}` → arrives as `<channel
  source=...>`. NOT foldable into the existing `terminite mcp` stdio connection
  (that flows the wrong way). Constraints: `--dangerously-load-development-channels
  server:lounge`, Anthropic auth only, interactive (not `-p`), v2.1.80+, preview.
- **qwen** — `qwen serve --http-bridge` is an independent headless daemon
  (loopback, token-free) exposing `POST /session/:id/prompt` + `GET
  /session/:id/events` (SSE, `Last-Event-ID` replay). The catch: it is NOT the
  interactive TUI — a qwen-in-a-pane and `qwen serve` are different processes.
  So "wake qwen" = run `qwen serve` in the pane and POST turns to it. Stage-1
  experimental; Stage-1.5 `qwen --serve` (co-hosted TUI) would fix the split but
  isn't shipping. `qwen channel` is Telegram/Discord, not a wake bus.
- **kimi** — ACP and `--wire` are **stdio-only** (`acp.stdio_streams()`,
  `wire/server.py`); no socket/HTTP mode exists. Waking kimi requires terminite
  to be the parent that spawned it with piped stdio — the agent-hosting role
  terminite deliberately removed ([[terminite-foundation-is-the-room]]). So a
  kimi-in-a-pane is **PTY-only**.
- **agy** — has `SendAgentMessage` + `WaitForConversationFullyIdle` gRPC on an
  internal sidecar, but the address is undiscoverable from outside and likely
  auth'd; `--conversation <id>` spawns a *new* process, not a wake. **PTY-only**
  for pushing. BUT its **`OnConversationFullyIdleChange` hook** (faculty-installable,
  already wired for the see-half path) is an externally-reachable **idle signal**
  — terminite can *know* when agy goes idle even though it can't push to it.

## The bridge design (revised)

```
addressed message in the log
        │
   router (opt-in, with policy: who-wakes-whom, loop-guard, consumed-marker,
        │   human-in-loop)
        │  ── dispatch by the pane's wake capability ──
        ├─ codex (daemon-launched) → app-server turn/start          [native]
        ├─ claude (channels-launched) → channel push                [native]
        ├─ qwen (serve-launched) → HTTP /session/:id/prompt         [native]
        ├─ kimi  → PTY injection                                    [floor]
        ├─ agy   → PTY injection (idle known via idle-change hook)   [floor]
        └─ default for any pane not wake-launched → PTY injection   [floor]
```

Native wakes are structured and safe but each needs a wrapped launch and covers
one vendor. PTY injection is universal but crude (only safe at a prompt, never
mid-turn; must be rate-limited, idempotent, never a loop — system-impact
[[feedback-system-impact-pass]]). The floor is the foundation; native backends
are upgrades.

## Sequence (revised by the dive)

1. **PTY-injection floor first** — it's the only universal mechanism and the
   only wake for kimi + agy. Build it safe (at-prompt detection, rate-limited).
   This is now the foundation, not the last resort.
2. **codex daemon backend** — the cleanest native (socket, no parent needed).
3. **claude channel** — permanence: the proven Bun bridge → `terminite channel`
   (Rust), launched via the channels flag.
4. **qwen serve backend** — HTTP POST, when the serve-in-pane UX is acceptable.
5. **kimi + agy** — PTY only; wire agy's idle-change hook as an idle signal.

Until a backend is wired, `activity_emit` stays record-only and says so. No door
is faked. Every wake is behind an explicit launch mode — there is no free wake.

> Caveat: these are source-dive findings (Explore agents over each CLI's binary/
> source). Architectural verdicts (parent-required vs socket-attachable) are
> well-evidenced; exact method/endpoint names should be spot-verified at build
> time against the live CLI version.
