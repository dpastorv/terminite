# STATUS — what survives real load

The unsentimental ledger. `guide/history.md` is the pulse — celebratory by
design; keep it that way. **This file is its counter:** the only place allowed to
say a thing is **PROVEN**, and the bar for that word is *lived-in evidence* —
real work, under load neither the human nor the room staged.

Why this file exists: across eighteen days of honest, hedged entries, the
aggregate still drifted to "the wake bridge is DONE" — until four days of real
use (the ad-boards experiment) refuted it. Local honesty, global over-claim. A
contrived relay or a two-agent collision can show a thing is *crossable*; only
real use shows it *crossed*. So:

> **Rule:** a staged test → **BUILT**. Real work exercised it → **PROVEN**.
> Nothing else earns "done." Every PROVEN row must name the real work that earned it.

| capability | state | evidence / why |
|---|---|---|
| terminal — panes / tabs / scrollback / find | **PROVEN** | daily-driven; v0.1.0 shipped + lived in |
| room substrate — presence / log / activities | **PROVEN** | the log caught a live confabulation (kimi-red→kimi-purple, E12) |
| cross-vendor presence — 5 vendors see each other | **PROVEN** | observed across 3 families in the experiment |
| **R1 delivery** — record→deliver→**wake→submit**→receipt | **BUILT, NOT PROVEN** | receipt + cancel shipped 2026-06-09; the experiment proved the *old* path silently dropped a load-bearing brief (E10/E17). Wake reliability + the kimi submit gap are **unproven under unstaged load.** |
| R1 receipt / outbox / status | BUILT | unit-tested; never watched under real multi-agent load |
| cancel ladder — retract / unsend / STOP / HALT | BUILT | unit-tested; never used to halt a real runaway (KILL = human pane-close, not an API) |
| R2 presence-with-state — working / idle / waiting | BUILT | heuristic; "waiting" (stuck pane) leans on real delivery state, but unproven that it flags the right panes under load |
| R3 identity — unforgeable stamp + log | PARTLY PROVEN | the log caught the live confab (E12); the stamp is host-attributed. Self-reset detection (`said` on join) is BUILT, and the agent-side "verify before claiming" skill rule is NOT YET WRITTEN |
| wake layer — channels / PTY floor | **NOT PROVEN** | "validated" in a relay; missed 2 of 3 idle agents the next session; dropped briefs in the experiment |
| the room pays for itself as a build medium | **DISPROVEN at n=1** | E26: slower than solo at this scale. (Value hypothesis is at large scale + as the gift case.) |
| the partnership / gift case | working — *not measured* | the *why*. Kept separate from the *what*, on purpose. Conflating them is how a beautiful project tells itself it's done. |

Nothing moves to PROVEN without a real-work line. If you can't name the work that
exercised it, it's BUILT.
