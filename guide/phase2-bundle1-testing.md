# Phase 2 Bundle 1 — Testing handoff

Written by one session (running inside terminite) for the next one
(also running inside terminite). The point: stress-test Bundle 1 in
the wild before we move on to Bundle 2.

## What Bundle 1 is

Commit `365e066`. Block Model + visible IDs:

- `src/blocks.rs` — `Block`, `BlockStore`, lenient A/B/C/D state machine,
  per-tab cap `MAX_BLOCKS_PER_TAB = 1000`, oldest-first eviction.
- `vendor/.../event.rs` — `Event::ShellIntegration { kind, exit, line }`
  now carries the cursor's absolute line at fire time.
- `src/term.rs` — forwards as `UserEvent::ShellIntegration` to the main
  thread.
- `src/main.rs` — dispatches into the right tab via
  `renderer.handle_shell_integration`.
- `src/renderer.rs` — in the phase-2 immutable walk, each pane pushes a
  pre-shaped `Bn` label into the chrome text renderer for every visible
  block, clipped to the left-gutter strip.

OSC 133 marks: `A` prompt-start, `B` prompt-end (command start), `C`
output-start, `D[;exit]` output-end.

## What's already proved

- **Unit tests** — `cargo test blocks::` → 6/6 pass. Covers full
  lifecycle, A-without-D lossy graduation, C-without-A opening,
  D-without-open no-op, eviction at cap, anchor fallback chain.
- **End-to-end wire** — Daniel ran `/tmp/blocks_demo.sh` in a terminite
  pane, `B1` appeared in the left gutter on the output row. Proves
  PTY → vte → `Event::ShellIntegration` → main thread → `BlockStore`
  → renderer.

## What's still untested in the wild

These are the variants the next session should help drive. The user
runs each in a real terminite pane and reports what they see — see the
**Constraint** below for why the agent can't drive the pane directly.

1. **Scroll behavior.** Run the demo, then scroll up (mouse wheel or
   keyboard scroll). The `B1` label should track the *content row*, not
   the screen position. Coordinates are stored as
   `viewport_line - display_offset_at_fire`, recovered as
   `abs + current_display_offset`. If the label drifts off the row,
   the anchor convention is wrong.

2. **Multi-block stacking.** Run the demo three times in a row. Expect
   `B1`, `B2`, `B3` each anchored to their own output row, all visible
   simultaneously (no overwriting, no collisions). If the labels stack
   on the same row, the per-block anchor line isn't unique enough; if
   only the latest shows, the renderer is overwriting.

3. **Lossy graduation in the wild.** Run a sequence that emits `A`
   then another `A` without a `D` in between (simulating a shell whose
   prompt redrew without finishing the previous command). The
   already-open block should graduate to closed, a fresh one should
   open. The first block's gutter label should still render even
   though its `D` never arrived. (Anchor falls back to
   command-end-line or prompt-line — see `Block::anchor_line`.)

4. **C without A.** Emit only `C` then `D`. A block should still open
   anchored at output-start, `prompt_line: None`, and its label should
   still render.

5. **Real shell integration.** Wire up a zsh `precmd`/`preexec` (or
   bash equivalent) so the marks fire automatically on every prompt.
   Run a few commands. Verify `Bn` labels appear without any manual
   `printf`. This is the load-bearing real-world test — the demo
   script proves the wire, this proves the integration shape we
   actually want users to use.

6. **Eviction at cap, live.** Synthesize >1000 blocks (loop the demo
   in a script) and confirm the oldest labels disappear, not the
   newest. Also confirm memory doesn't grow unbounded — `ps` on the
   terminite pid before and after should be roughly flat.

## The demo script

Committed at `tools/blocks_demo.sh` so it survives a fresh session:

```bash
#!/usr/bin/env bash
esc=$'\e'; bel=$'\a'
printf '%s]133;A%s' "$esc" "$bel"
printf 'demo$ '
printf '%s]133;B%s' "$esc" "$bel"
printf 'echo hello world\n'
printf '%s]133;C%s' "$esc" "$bel"
echo "hello world"
printf '%s]133;D;0%s' "$esc" "$bel"
echo
echo "[demo done — look for 'B1' in the left gutter]"
```

## The Constraint (read this before trying to drive tests yourself)

If you're a Claude session running inside terminite, your `Bash` tool
calls run in a **different pty** from the visible terminite pane. Your
stdout is captured by the Claude CLI and re-rendered as text in the
conversation view — escape sequences you `printf` will *not* reach
terminite's vte parser.

Practical consequence: you can run `cargo test` and read code, but you
can't drive the gutter end-to-end yourself. The user runs the demo in
a real pane and tells you what they saw. Plan handoffs accordingly:
write the script, tell them what to look for, ask the right
post-condition question.

## How to verify the running binary has Bundle 1

```bash
ps aux | grep target/.*/terminite | grep -v grep
ls -la target/debug/terminite target/release/terminite
git log --oneline -5
```

Compare the binary mtime to commit `365e066`'s timestamp. If the
binary is older, the user needs to `cargo build` and relaunch before
any live test is meaningful. (Daniel runs the debug build; release
was last rebuilt 2026-05-20.)

## Next bundle

If all six variants above pass, Bundle 2 (module protocol) is the
unblocking move per `phase2-plan.md`. Bundle 2 reads from the
`BlockStore` surface this bundle established — it's how a co-pilot
process queries "what was the output of B7" without re-parsing
scrollback.
