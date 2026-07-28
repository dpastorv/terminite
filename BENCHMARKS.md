# BENCHMARKS — terminal render cost, measured

Companion to `tools/bench-render.sh`. Everything here is a real measurement with
its conditions attached; anything unmeasured says so. Read the caveats before
quoting a number — two of the obvious readings of this table are wrong.

## How to reproduce

```sh
./tools/bench-render.sh --out ~/bench-<app>.txt   # full run
./tools/bench-render.sh --quick                   # 1/5 payloads, smoke test
```

Same window size, same font and size, one app at a time, nothing else busy. The
script refuses to run without a tty, under tmux/screen, and warns over ssh.

Five payloads, pre-generated to a temp dir so generation never lands in a timing:

| workload | stresses |
|---|---|
| `ascii` | 120k plain lines — glyph-cache hits, scroll, blit |
| `wide` | CJK + emoji + ZWJ + combining mark — wide cells, grapheme handling |
| `sgr` | distinct 24-bit colour per cell — defeats run-merging, per-cell attrs |
| `cursor` | 250k scattered cursor-addressed writes, no scrolling — damage tracking |
| `repaint` | 800 full alt-screen frames — full-frame blits, no scrollback growth |

### An agent cannot run this from its own shell

Claude's Bash tool has no controlling tty (`/dev/tty` → "device not configured"),
and its output is pipe-captured, so it never reaches a renderer. To get a real
measurement, drive the app into a **fresh** window:

```sh
osascript -e 'tell application "Terminal" to do script "bash /path/to/runner.sh"'
# then: tell window 1 to set number of columns / number of rows; poll for a done-file
```

Do **not** write to Claude Code's own pty (`/dev/ttys00N`) instead. It wrecks the
live TUI, and the DSR replies land in Claude Code's input stream.

## Apple Terminal — baseline

Measured 2026-07-27, Terminal 100x50 cells, Liquid Retina XDR (3024x1964),
macOS Darwin 25.5.0. Full run, 38.2 MB total payload.

| workload | MB | wall s | MB/s | consume rate | app CPU s | WS CPU s | CPU s/MB |
|---|---|---|---|---|---|---|---|
| ascii | 11.44 | 0.19 | 58.99 | 619k lines/s | 0.24 | 0.14 | 0.021 |
| wide | 6.60 | 0.15 | 42.58 | 258k lines/s | 0.19 | 0.21 | 0.029 |
| sgr | 14.31 | 0.46 | 30.98 | 17.3k lines/s | 0.61 | 0.26 | 0.043 |
| cursor | 2.08 | 0.13 | 15.91 | 1.91M ops/s | 0.15 | 0.19 | **0.072** |
| repaint | 3.78 | 0.10 | 38.55 | 8163 frames/s | 0.11 | 0.12 | 0.029 |

**1.30 s app CPU + 0.92 s WindowServer to consume 38.2 MB.**

Cursor addressing is Terminal's worst per-byte case — 3.4x its `ascii` cost —
with SGR churn second. Those two are where a CPU renderer is won or lost; raw
scrolling throughput is the least interesting column here.

## Terminite — NOT MEASURED

No terminite run exists yet. `do script` is Terminal-specific, so the matching
pass needs a way to launch a terminite window at 100x50 running a command. Until
that row is filled in, **there is no render comparison** — the memory-footprint
comparison below is a separate, independent measurement.

## Idle footprint (separate measurement, same day)

Both apps live, before any benchmark:

| | Apple Terminal (1 shell) | Terminite CPU build (5 shells) |
|---|---|---|
| phys_footprint | 288 MB (peak 392 MB) | **72 MB** (peak 112 MB) |
| dirty | 35 MB | 39 MB |
| `ps` RSS | 125 MB | 183 MB |
| threads / mach ports | 12 / 415 | 11 / 260 |

Use `footprint -p <pid>`, **not** `ps` RSS. RSS counts shared framework and
mapped pages and ranks these two backwards — terminite maps ~33 MB of files that
inflate its RSS but are not charged to it.

## Caveats — read before quoting these numbers

**`consume_rate` is not fps.** It is how fast the terminal *ate* the stream:
parse-bound, not paint-bound. Terminals coalesce painting, so 8163 "frames/s" on
a 120 Hz display means most logical frames were never drawn. An early version of
this table said 8989 frames/s and implied it was a frame rate; it was not.

**Compare `app CPU s`, not wall time.** A terminal that parses on one thread and
paints on another answers the DSR sync early, which flatters its wall-clock
number. Coalescing can hide latency but not work, so CPU is the honest column.

**CPU is sampled after a 0.5 s settle** (outside the timed region), because the
DSR reply arrives when parsing finishes while paints are still queued. This
matters: adding the settle roughly doubled measured WindowServer CPU (`wide`
0.05 → 0.21 s, `cursor` 0.04 → 0.19 s). Numbers taken without it undercount
paint and misattribute it to the next workload.

**The footprint before/after inside a bench report is not the idle footprint.**
The run leaves ~170k lines in scrollback by design. The 2026-07-27 report read
398 → 462 MB, but that was contaminated by an earlier bench window left open;
288 MB is the clean idle figure. Close bench windows before measuring footprint.

**Terminal's CPU counter is process-wide.** It includes every window the app
hosts, including a live Claude Code TUI in another tab.
