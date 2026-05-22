# Friction Log

terminite's real roadmap. Not a list of ideas — a record of friction *felt* by
the human-AI pair while building terminite from inside a terminal. Features are
promoted from here, one at a time. See [development.md](development.md).

Each entry: what happened, why it hurt, who it hurt (the human, the AI, or
both), and where it points in the architecture.

---

## Seeded at kickoff — the AI's perspective

These first entries are written by the AI partner at project kickoff
(2026-05-19): friction it genuinely experiences working in today's terminal
while building terminite. They are terminite's reason to exist, stated as
friction.

### Output has no boundaries

- **What:** When a command runs, its result returns as one undifferentiated
  stream of bytes. Nothing marks where the command was, where its output begins
  and ends, or where the next prompt starts.
- **Why it hurt:** The AI must reconstruct all of that with fragile heuristics,
  and gets it wrong when output is unusual. The human reads it at a glance; the
  AI is guessing at structure that was never recorded.
- **Who:** The AI.
- **Points at:** the Model — command and output as structured blocks.

### Nothing says when work is done

- **What:** A long-running command emits no event that means "finished, exit
  code 0."
- **Why it hurt:** Both users end up watching and waiting. The AI polls or
  guesses; the human babysits a screen.
- **Who:** Both.
- **Points at:** the Model emitting lifecycle events — started, finished, exit
  code.

### Output is all-or-nothing

- **What:** A command can dump megabytes. There is no way to ask for "the last
  50 lines" or "a summary" — only the whole stream, or none of it.
- **Why it hurt:** It floods the AI's limited context with noise and crowds out
  what matters.
- **Who:** The AI.
- **Points at:** the module protocol exposing structured, sliceable access to
  the Model.

### The pair cannot point at the same thing

- **What:** The human says "that error above." The AI has only a
  coordinate-free byte stream — the two do not share *names* for what is on
  screen.
- **Why it hurt:** Every reference has to be re-grounded from scratch. The pair
  looks at one screen but not at one set of objects.
- **Who:** Both.
- **Points at:** a shared Model — the same blocks, turns, and commands, named
  the same way for both users. "Two users, one surface," made literal.

---

## Live log

### 2026-05-21 — The OS won't tell us where the shell is

- **What:** Tab titles should show the shell's working directory
  (`zsh · ~/dev/terminite`). The obvious macOS way is
  `proc_pidinfo(PROC_PIDVNODEPATHINFO)` — ask the kernel for another
  process's cwd. It returns `EPERM` on every call. macOS's TCC hardening
  blocks an unsigned binary (anything from `cargo run`) from reading
  another process's vnode path info — even a direct child, even
  same-user. A long debugging session went into slave-fd vs master-fd,
  struct sizes, and finally `errno` before the number (1 = EPERM) made it
  unambiguous.
- **Why it hurt:** A small feature with a deep hole behind it. The "just
  ask the OS" instinct is wrong on modern macOS — the OS now treats
  process introspection as a privilege. And the failure was *silent*:
  `proc_pidinfo` returns 0 with no error surfaced unless you go read
  `errno` yourself. The cost wasn't the feature; it was the time spent
  assuming our code was wrong while the platform had quietly closed the
  door.
- **Who:** Both. The human watched thousands of identical error lines
  scroll past; the AI chased its own tail through fd plumbing before
  checking the one number that mattered.
- **Points at:** The terminal can't introspect the shell from the
  outside — it has to be *told*. OSC 7 is exactly that channel: the
  shell announces its cwd in-band, the terminal listens. The catch:
  `vte` 0.15 (and therefore `alacritty_terminal` 0.26) doesn't dispatch
  OSC 7 — the case isn't in the parser. The fix is a vendored fork of
  both: teach `vte` to call a new `Handler::set_current_directory`, teach
  `alacritty_terminal` to emit a `CwdChanged` event, route it through the
  `Notifier`. In-band beats out-of-band. The same lesson applies to
  anything we'd be tempted to "ask the OS" for — prefer what the shell
  tells us over what we reach in and grab.

### 2026-05-20 — Spawn-per-render melted the laptop

- **What:** Phase 1 Bundle 1 wired cursor blink, drag-edge auto-scroll, and
  the visual bell flash by scheduling each next wake with a fresh
  `std::thread::spawn(|| { sleep(d); send(Wakeup); })` *inside* `render()`.
  Each OS thread on macOS carries an ~8 MB stack by default. With a busy
  shell driving renders at 60+ Hz, terminite leaked thread stacks at
  hundreds of MB per second — a second debugging session caught it at
  ~76 GB RSS and the kernel watchdog panicked the Mac before the OOM
  killer could act.
- **Why it hurt:** Not just a leak. A *hostile-host* leak. terminite didn't
  crash itself; it took the whole machine down with it. The partnership
  stopped because the human's laptop was unusable.
- **Who:** Both. The human lost their session and ate a reboot; the AI
  shipped the bug and is now writing this entry.
- **Points at:** The native scheduler is right there. `ControlFlow::WaitUntil
  (deadline)` is one line, zero threads. The lesson generalizes: any
  "schedule next thing in N ms" inside a hot loop is a thread leak in
  disguise — let the event loop's own clock do it. Plus defense-in-depth:
  an RSS kill-switch (4 GB default, `TERMINITE_RSS_LIMIT_GB` to override,
  `0` to disable) so the *next* runaway is bounded long before the watchdog
  is.

### 2026-05-20 — Mouse-wheel scroll did nothing on a trackpad

- **What:** Initial scrollback implementation cast `f32` scroll lines (from
  winit's `MouseScrollDelta`) directly to `i32`. macOS trackpads deliver
  `PixelDelta` events with ~1–20 pixels per gesture frame, which divided by
  line height = 0.05–1.0 lines. Cast to `i32` = 0. Scroll silently did nothing.
- **Why it hurt:** A terminal that ignores trackpad scroll feels broken at the
  most fundamental level — you can't read history. The human notices
  immediately; the partnership looks like it shipped half a feature.
- **Who:** The human (the AI sees nothing here directly), but the absence
  shows up as "did the partner ship a terminal that can't scroll?"
- **Points at:** Any signed-`i32` boundary on continuous input needs an
  accumulator. The same principle will apply to keyboard auto-repeat tuning,
  click-and-hold gestures, anywhere a float gets truncated. Truncation is
  silent and feels like nothing.

### 2026-05-20 — terminite was burning 50% CPU on no work

- **What:** Idle terminite (no shell output, no input) consumed ~50% of one
  CPU core. The render loop self-triggered — every `RedrawRequested` ended
  with `window.request_redraw()`, so terminite ran at the monitor's refresh
  rate whether anything had changed or not.
- **Why it hurt:** A terminal that wakes the CPU constantly is the opposite
  of *quiet*. Heat, fan noise, battery drain — and measurably worse than
  Terminal.app doing nothing on the same machine. It contradicts the
  loveliness bar at the level of the laptop's chassis.
- **Who:** Both. The human feels the warm machine; the AI sees the cost as
  shared (the partnership runs on the human's battery).
- **Points at:** Render loops must be **event-driven by default**. Polling is
  friction-log fodder. Any future thread that signals "redraw" should fire on
  a real event, not on every frame. The same principle will apply to the
  multiplexer's input/output loops when they arrive.

### 2026-05-19 — The cursor was geometrically correct and visually wrong

- **What:** After measuring the cell advance, the cursor sat exactly where the
  next character would land — geometrically perfect. To the eye it sat *flush
  against* the previous character and a touch low.
- **Why it hurt:** Geometric correctness is not visual correctness. *Almost*
  right is somehow worse than off-by-a-cell — it reads as uncanny.
- **Who:** Both — the human sees it; the AI registers the pull and trusts the
  reading.
- **Points at:** Cell math is the frame; type-design intuition is the finish.
  Future polish (bell, scroll, selection, hover) will need the same discipline:
  ship the math, sit with it, nudge it where the eye expects.

### 2026-05-19 — The conversation is trapped in a transcript

- **What:** Logging this very session meant writing a Python script to dig the
  conversation out of a Claude Code `.jsonl` file — a format built for the
  harness, not for people.
- **Why it hurt:** The human-AI session is the most valuable artifact of this
  way of working, and it was the *least* reachable thing in the room. It took
  tooling, out of band, just to read back what was said.
- **Who:** Both.
- **Points at:** terminite making the session itself first-class — a
  conversation it can show, export, and carry, with no one scripting their way
  into a log file.
