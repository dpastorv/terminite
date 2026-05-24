# Phase 2 Plan — Be the pair's terminal

The point of the project. Everything before this was the floor; this is
the part terminite exists *for*. Written so we can align on the shape
before code, and so the design survives context resets.

> Phase 1 leaves us with: OSC 7 + OSC 133 dispatched through a vendored
> fork (so command-lifecycle marks already arrive at the Notifier and
> sit *parked*), a clamped renderer, a config file, and Kitty images on
> screen. Phase 2 builds on top of an OSC 133 channel that already
> flows.

## The spine — the design call

The roadmap originally framed this as "the Model." We're narrowing it.

**Blocks are primary; scrollback is a projection of them.** Today
terminite is a terminal with blocks bolted on. From here, it is a
**block store that renders as a terminal**. That sounds aesthetic; it
isn't. It changes what's *primitive*: blocks carry command, output,
exit, timestamps, tags, references. Search/slice/export/point all
operate against blocks. Bytes are downstream — the terminal grid is the
*current view* of the block list, not the source of truth.

The four kickoff friction-log entries — *boundaries / finishing /
slicing / pointing* — are one problem: the human and the AI don't share
**coordinates**. Make them share, and the rest follows. Phase 2's first
job is to make coordinates real.

## What's in (and what's out)

### Added to the original sketch

- **Tags on blocks.** Either user can attach a small set of
  human-readable tags. *"That flaky test we hunted Tuesday"* becomes a
  real handle. Tiny mechanism, large for the partnership.
- **AI presence as a second cursor.** Weighted more heavily than the
  initial sketch. When the AI is reading B7, the human sees it. Not a
  notification — a presence. The cheapest version of "two users, one
  surface" made literal.

### Cut from the original sketch

- **Module SDK.** Premature. Define the protocol, ship a stable spec;
  an SDK is Phase 3 distribution work, not Phase 2 differentiation work.
- **Per-block AI assignment / multi-AI dispatch.** Over-engineered. One
  AI partner, in cleanly, first.
- **"Conversation as artifact" as a separate item.** It's not separate;
  the conversation *is* the block list. Exporting blocks **is** exporting
  the conversation.

### Reframed

The spine is **shared coordinates**, not "the Model" abstractly. Once
both users name the same things the same way, presence / reference /
augmentation become possible. Coordinates first; everything else builds
on them.

---

## Bundle 1 — the block Model + visible IDs

The smallest honest thing that proves the idea is real.

1. **Ingest OSC 133.** The marks already reach the Notifier; route them
   into a per-tab block store:
   - `A` (prompt-start) — close any open block, prepare to open the
     next one when the user runs something.
   - `B` (prompt-end / command-input-start) — mark where the command
     text ends and output begins.
   - `C` (output-start) — confirm the output range opens here.
   - `D` (finished) — close the block with the exit code.
2. **Per-tab block store.** A `Vec<Block>` on each `Tab`, capped at
   ~1000 (well past anything fitting in scrollback; oldest evict). Each
   block carries:
   - `id`: per-tab monotonic — `B7` is "block 7 of this tab" (cross-tab
     references can come later if we need them).
   - `command_range` / `output_range`: stored in absolute alacritty
     `Line` coordinates, the same coordinate system selections already
     use — so blocks scroll with content for free.
   - `exit_code`, `started_at`, `finished_at`.
3. **Visible IDs.** A small `B7` label at the left edge of each pane's
   content, aligned with the block's output start row. Existing tab
   font, quarter-strength colour — not shouty. The pair can now
   *literally* point at B7.
4. **Internal API.** `tab.blocks()`, `block.head(n)`, `block.tail(n)`,
   `block.text()`. Not yet exposed outside the renderer — that's the
   module-protocol bundle. The surface exists so the next bundle can
   plug into it without a refactor.

That is the whole bundle. After it lands, the pair shares coordinates.
Everything after — tags, AI cursor, the module protocol — is incremental
on top.

### System-impact pass (per the standing gate)

- **Block-count cap** per tab at ~1000 — old blocks evict (their grid
  lines almost certainly already scrolled off scrollback anyway).
- **Per-block size** is fixed-shape (~100 B + command text). The command
  text is bounded by alacritty's prompt buffer in practice.
- **Per-frame visible IDs** are bounded by visible blocks on screen —
  dozens at most.
- **No new threads, no new processes, no new fds.** The OSC 133 path
  is already proven through to the Notifier; this bundle is data +
  render, both on the main thread.
- Worst-case memory addition: 1000 × ~200 B = 200 KB per tab. Trivial.

---

## After Bundle 1 (rough order)

These are sketched, not bound — Bundle 1 lands first, and we'll see
what the felt friction points us at next.

- **Bundle 2 — Module protocol.** A small JSON-over-stdio protocol so
  out-of-process partners (the AI, eventually a notes module) can
  enumerate blocks, ask for slices, post bytes back. Read-only by
  default; write intent declared.
- **Bundle 3 — Tags + AI cursor.** Attach tags to blocks; render an
  AI-side cursor / highlight when the AI is "looking at" a block.
- **Bundle 4 — Block-aware selection + copy.** Cmd-clicking inside a
  block selects the whole block; copy yields the command + output, not
  raw bytes.
- **Bundle 5 — Conversation export.** The block list as a portable
  artifact (markdown or JSON). The session, *as the session*, sharable.
- **Bundle 7a — Minimal debug surface.** Lands *before* Bundle 6, so
  the framework work has eyes open. Structured logging to
  `~/.terminite/log/terminite.log` (level-tagged, size-rotated); a
  `terminite stats` proto verb returning internal state (frame
  times, block-store sizes per tab, subscriber queue depth, memory
  snapshot); a panic handler that writes a crash dump (stack +
  state) to `~/.terminite/log/crashes/` rather than vanishing.
  Bounded throughout — log rotation cap, crash-dump-keep cap.
- **Bundle 6 — The extension surface.** Module manifest format,
  registration, lifecycle, per-pane channel. The shell type
  restructured as a built-in module so the framework hosts
  terminite's own code first. After this, terminite is no longer a
  fixed product; it's a host. See
  [phase3-plan.md](phase3-plan.md) for the rationale.
- **Bundle 7b — Debug as a module.** Closing Phase 2. Builds on
  Bundle 6: a rich debug pane *module* that visualises stats in
  real time, tails the log, inspects the block store, surfaces
  per-tab grid state. Proves the framework can host its own
  observability. After this, Phase 2 is done — terminite is a
  debuggable host ready for the load test.

---

## Standing principles, carried into Phase 2

- **System-impact pass before every commit.** No exceptions; three
  machine crashes earned this. See `feedback-system-impact-pass`.
- **Ship the smallest honest thing.** Each bundle has to survive
  contact with the pair doing actual work.
- **Two users, one surface.** Every design call should make sense
  for both human and AI reading it.
- **Blocks-primary.** When in doubt, the block is the unit. Adding a
  feature? Ask what it does *to a block*.
