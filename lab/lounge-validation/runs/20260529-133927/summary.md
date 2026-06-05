# Lounge validation run — 20260529-133927

## E1 — emission round-trip: **PASS**
- codex-2 saw 10/10 of codex-1's activities; order preserved: True
- attribution: all sightings carried actor=codex-1 (self-emits excluded by design)
- emit→seen latency: median 137.2ms, p95 236.4ms (poll cadence 250ms)

## E2 — coexistence + eviction: **PASS**
- emitted: codex-1=250, B1(claude)=50; cap=120
- store after run: total=120, per-actor={'codex-1': 103, 'B1': 17}
- evictions: 180 (oldest-closed-first); both actors still present: True

## E3 — agent-to-agent addressing: **PASS**
- codex-1 received from codex-2: True; codex-2 received from codex-1: True
- to:self filter delivered only addressed messages (broadcast to:None excluded)
- codex-1 inbox: ['codex-2: are you there, codex-1?', 'codex-2: yes, I see codex-1.act-2']
- codex-2 inbox: ['codex-1: are you there, codex-2?', 'codex-1: yes, I see codex-2.act-1']

## E4 — polling latency sweep
| poll cadence | median | p95 | poller CPU (user+sys) | polls |
|---|---|---|---|---|
| 100ms | 51.4ms | 95.7ms | 0.040s | 58 |
| 250ms | 148.7ms | 247.2ms | 0.030s | 24 |
| 500ms | 295.4ms | 496.0ms | 0.030s | 12 |
| 1000ms | 541.4ms | 877.4ms | 0.020s | 6 |
| 2000ms | 1057.1ms | 1792.9ms | 0.020s | 3 |
| 5000ms | 2659.7ms | 4486.1ms | 0.020s | 2 |

_Note: latency is dominated by poll cadence (≈ cadence/2 median, ≈ cadence p95). Socket+store round-trip is sub-millisecond. This is the inherent cost of pull; brick 4 (push) would collapse it._

## E5 — claude-shape introspection
- of 9 scripted actions, emitted 5, skipped 4 (navigation/noise)
- emitted (what a peer would benefit from):
  - `tool_call` act-1: Edited guide/activities-design.md — added NOTE from E1
  - `tool_call` act-2: Edited src/acp.rs — wired activity emission
  - `tool_call` act-3: Ran cargo test — 42 passed
  - `tool_call` act-4: Ran the E1 regression — green
  - `agent_message` act-5: Decision: porting ActivityStore to Rust as-is
- skipped (stayed opaque, opt-in honored):
  - edited a local scratch note — not relevant to the room
  - ran `ls` — navigation noise
  - ran `git status` — navigation noise
  - ran `cd lab` — navigation noise
- **gap felt:** the 1 *decision* had to be modeled as an `agent_message` broadcast (`to:None`). There is no `Decision`/`Note` kind. It works, but a decision isn't really a message to anyone — flagged for the design doc.
