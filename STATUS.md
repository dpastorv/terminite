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

## Daily-driver UX — the migration axis

A second axis, distinct from the room. Not "does the room hold N agents" but
"can Daniel close iTerm and live in terminite for every project." Opened
2026-07-07 with a three-surface audit (config, layout persistence, input) plus a
text-rendering audit — run from *inside* terminite. The diagnosis was uniform:
**the safety engineering is excellent (clamped, atomic, crash-proof); the
personalization coverage is thin.** The fixes below are the first batch off that
audit. Every one is **BUILT, NOT PROVEN** — they compile and unit-test, but the
proof is Daniel's hands and eyes in real use, and that hasn't happened yet.

| fix | state | how it gets proven |
|---|---|---|
| window remembers size + position across launches | BUILT | reopen after a resize/move lands where you left it, not 900×600 in a random spot |
| restore opens the tab you left focused (was a real bug) | BUILT | quit with a non-first tab active in a pane; reopen on *that* tab |
| font zoom (Cmd+/- / Cmd-scroll) survives restart | BUILT | zoom, quit, reopen at the zoomed size — without your config.toml being rewritten |
| Cmd+K clear-scrollback · Cmd+A select-all | BUILT | the reflexes land during real terminal work, not a staged keypress |
| text rendering — sRGB-space glyph blend (was linear → thin/gray) | BUILT | eyeball old-vs-new: text reads heavier/sharper, and **no color regressed** (bg stays near-black, selection/cursor/syntax colors true) |

Deferred by decision (2026-07-07): themes/palette (One Dark is fine for now),
full keybinding remap (E2 — needs the config format to grow past flat
`key=value`; the missing default keys landed now), named per-project workspaces
(single auto-restore is enough), copy-on-select change (current behavior is
wanted). Rendering follow-ups if still thin after the eyeball: stem-darkening,
real Bold/Italic masters (variable fonts expose only Regular), HiDPI
scale-factor tracking (only bites on non-Retina / fractional-scale displays).
