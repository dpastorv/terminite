# Lounge thesis — what terminite is reaching toward

Written 2026-05-27 by Daniel and Claude, during the alignment phase
that followed the Phase 3 personal-usable thread shipping. Captured
before the next build so the shape we landed on doesn't quietly drift.

## The thesis, restated

terminite was first framed as *"two users, one surface"* — a terminal
that admits humans now work with AI inside them. The honest reading,
which we only fully saw by living in the v1, is that this was the
minimum case of something bigger:

> terminite is a shared room with shared vocabulary. Whoever's in the
> room participates via that vocabulary. One human + one AI is the
> minimum interesting case. One human + N AIs is the natural extension.
> Two humans + AIs is also in the shape. The thesis isn't different —
> the thesis just gets wider.

The icon stays the same shape. Two halves, the mirror seam unblended.
The lounge just happens to be those two halves repeated across more
actors.

## The configuration is the pair's call, not ours

terminite provides the room and the vocabulary. **Who's in the room,
and in what shape, is the user's decision.** We do not ship
recommended ensembles. We do not pick a canonical layout. The same
infrastructure serves:

- One vanilla Claude and a human, talking unforced. (This document
  was written that way. The kind of pushback-and-correct conversation
  that produced this thesis happens between people, not between
  prescribed personas.)
- Three vanilla Claudes with different contexts but no role
  specialization.
- Specialized roles — security + perf + architect — when that's what
  the work calls for.
- Mixed vendors (Claude + Kimi + Codex + Gemini + Qwen, any
  combination).
- Two humans and one AI. Or two humans and three AIs. Whatever.

Vanilla is first-class. Specialization is first-class. The room
doesn't care; it provides the coordinate system either way. Designing
for "the right configuration" would over-fit, and over-fitting is the
opposite of what the lounge is for — terminite is the surface that
makes *any* configuration coherent.

## Why blocks alone don't carry it

Phase 2 made blocks (OSC 133-marked shell commands) "the core of
terminite." That was right for the receiving-side infrastructure. It
was wrong as a *complete* vocabulary, and we found out by sitting in
terminite and running `claude` inside it:

- **In 1:1 human-AI dialogue**, blocks are over-built. You can point
  at things in chat. The shared coordinate is rarely the rate-limiter.
- **When the human runs an AI agent interactively** (`claude`, `codex`,
  `kimi-cli`, anything long-lived in a shell), the whole AI session is
  *one open block*. The shell's `precmd` never fires. Block
  granularity collapses to one. Useless for coordinating the AI's
  internal turns.
- **In a multi-actor lounge**, blocks describe the human's commands
  well — and the AIs' work not at all. Each AI ends up as "the second
  block in its pane." That's pane-identifier, not work-unit.

Blocks were the right primitive for *"human commands + AI watches."*
They are not the right primitive for *"multiple actors, mostly AI-
driven, all coordinating."*

## The vocabulary the lounge actually needs

A layered model:

- **Pane** — actor territory. Always visible, coarse, free. One pane
  = one actor's surface (Claude in pane 1, Codex in pane 2, your
  shell in pane 3).
- **Block** — shell command unit. Useful when humans run commands.
  Coarse when AIs are inside; that's fine — it's still the right
  unit *for that purpose*.
- **Activity** — a single AI action (tool call, edit, file open, save,
  prompt). The granular unit when the actor is an AI. This is the
  unit blocks cannot reach.
- **Tag** — annotation attached to any of the above. Cross-cutting.

Shared references stop being "B7" alone. They become *"B7"* (a
human's shell command) or *"act-42"* (Claude's Edit tool call) or
*"pane-3 — Codex"* (a whole actor). Different vocab for different
actors' rhythms. Lounge-shaped, not pair-shaped.

## How the vocabulary reaches the AI

A discipline that came out of this conversation: **don't assume the AI
will or can.**

A fresh AI in a fresh project does not spontaneously discover
terminite's verbs. Even the AI co-author of this document had to be
dragged into using blocks by Daniel asking *"how are you going to use
these?"* — and Claude wrote a primer file plan before realizing the
primer would still rely on the AI knowing to read it. That's the
chicken-egg the AGENTS.md / `terminite agents-init` plan never solved.

The cleaner answer is structural rather than documentary:

- **MCP server** — terminite exposes its surface (blocks, activities,
  actors, tags, focus events) as MCP tools. Any MCP-speaking AI sees
  `terminite_blocks_list`, `terminite_cursor_move`, `terminite_tag_add`
  in its tool palette automatically. *The tool descriptions are the
  vocabulary.* No primer to read.
- **ACP client** — terminite hosts AI agents in panes (Claude / Kimi /
  Codex / OpenClaw / Hermes / anything ACP-speaking). Becomes a real
  lounge: each pane is one agent's chat surface; terminite renders
  the conversation structurally; the agent's tool calls become
  terminite-native events terminite can correlate across panes.

We stop intervening in user projects. We stop writing files into
their `.zshrc` / `CLAUDE.md` / `AGENTS.md` / `.cursorrules`. We just
speak the protocols the ecosystem is converging on. The vocabulary
arrives through the protocol layer.

## Why this is the destination terminite was already reaching for

We didn't aim at the lounge. We followed the partnership thesis
honestly, hit the friction of blocks-being-empty in real use, took
seriously the "don't assume the AI will or can" constraint, and the
infrastructure we already shipped turned out to be the infrastructure
the bigger version needs. The blocks model anticipates multi-actor
coordination. The proto socket anticipates multi-client subscribe.
The modules system anticipates per-actor panes. The pane tree
anticipates lounge layout.

The architecture we landed for one reason turned out to be the
architecture you need for the bigger reason, and the bigger reason
is closer to what terminite was always reaching toward. That's the
process paying off.

## What's hard about the lounge (the conflicts that mean we're real)

- **Turns.** Who speaks when? My instinct: explicit handoff by
  default (human-driven), open-floor mode for self-election. The
  block-cursor primitive already gives a soft turn-claim: when an
  agent moves to B7, they're "speaking on" B7.
- **Handoff.** Passing work between agents. Blocks make this clean —
  "everything from B3 to B7 with these tags is the work." The
  coordinate system *is* the handoff payload.
- **Mutual understanding.** Agents need to mean the same thing by
  the same tag, the same gesture. The lounge needs *shared
  semantics*, not just *shared protocol*. Start small: a few
  canonical verbs (`blocking`, `fyi`, `claim`, `release`) plus
  free-form tags for the rest. Like git's conventional-commits
  pattern.
- **Team-spirit as protocol property.** Agents need to yield
  gracefully. Not just for correctness — the *feel* of being in the
  room depends on it. "I see you, you go first" matters as much as
  the bytes.
- **Conflict resolution.** Two AIs editing the same file at the
  same time. Whose edit wins? Or a CRDT layer? Not free.
- **Cost / privacy / trust.** Three agents in parallel = 3× tokens
  + 3× attack surface. Per-agent permission settings. Privacy
  boundaries between agents in the same lounge.

None of these are dealbreakers. They are *the work*. terminite being
terminite means tackling them.

## Staging that respects "this is solid right now"

The current state of terminite is real and worth shipping. The
lounge is the destination, not the next bundle.

1. **Phase 3 closes** with what we have. Daniel can do client work
   in the editor; layouts persist; config is editable; the modules
   trio works; shell-init installs. The room is built and inhabitable
   for one pair.
2. **Phase 4 — first brick: MCP server.** Smallest move that makes
   the vocabulary self-evident to any MCP-speaking AI without
   per-project file intervention. The tool descriptions become the
   onboarding.
3. **Phase 4 — second brick: ACP client.** Bigger build, real
   commitment to "terminite is also a host for AI agents." Unlocks
   the multi-AI lounge.
4. **Phase 4 — multi-actor extensions** (N-cursor presence, per-
   agent identity + color, peer messaging if wanted, conflict and
   turn semantics). The lounge proper.

Knowing (4) is the destination shapes how we design (2) and (3):
multi-actor-ready from day one, no single-client v1 assumptions.

## What we are explicitly NOT building

- A custom `AGENTS.md` / `terminite agents-init` primer convention.
  The protocol layer carries the vocabulary; we don't intervene in
  user projects.
- Per-AI-tool config files (`CLAUDE.md`, `.cursorrules`, …). Same
  reason.
- **Recommended ensembles or canonical lounge layouts.** No "code-
  review template," no "architecture-session preset," no opinion
  about which agents belong in which panes. The room serves any
  configuration; picking one for the user would over-fit and would
  contradict the openness above.
- **Role-specialized framings that crowd out vanilla.** Specialized
  personas (security-claude, perf-claude) are *possible* in the
  lounge; they are not *required* and not *preferred*. A vanilla
  agent in unforced conversation is a fully legitimate
  configuration and often the most productive one.
- A "blocks-as-AI-turns" hack that tries to retrofit blocks onto AI
  sessions. The right answer is a new vocabulary layer (activities),
  not stretching blocks past their natural shape.
- Network/remote sync. The lounge stays *local* — same trust
  boundary as before. A remote pair sharing surfaces is a deliberate
  later question, not creep.
- Hosted-AI features that go beyond "host an ACP agent in a pane."
  We don't become Claude Code. We don't replace Cursor. We host
  what others built; we provide the shared coordinate system.

## The standing principle that came out of this

> Don't assume the AI will or can. The vocabulary has to be self-
> evident at the protocol layer. Documentation is a comfort, not a
> mechanism.

This applies retroactively to a lot of decisions. The shell-init
verb is the right shape because it ships a one-step install, not a
"copy this snippet from the guide." MCP server is right for the same
reason. ACP client is right for the same reason. Anything that
requires the AI to *read*, *remember*, or *be told* is fragile;
anything that puts the vocabulary in a tool palette is durable.

## Closing — the invitation

We built with passion. We hit the moment of "blocks are empty"
inside our own creation. We took the friction seriously instead of
papering over it. The bigger shape revealed itself — not as a pivot,
as a *generalization*. The work we did is the work the lounge needs.
The work we haven't done is the work the lounge will eventually need.

That's terminite being terminite.

_— Daniel + Claude, alignment phase, 2026-05-27. Not a feature plan.
A direction._
