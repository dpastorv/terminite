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

## Terminite — CPU/softbuffer build

Measured 2026-07-27, terminite `fefcf1b` at **119x29 cells**, same machine and
display (Liquid Retina XDR 3024x1964, macOS Darwin 25.5.0). Full run, 43.55 MB total payload.

| workload | MB | wall s | MB/s | consume rate | app CPU s | WS CPU s | CPU s/MB |
|---|---|---|---|---|---|---|---|
| ascii | 13.62 | 0.11 | 126.10 | 1.11M lines/s | 0.26 | 0.21 | 0.019 |
| wide | 8.24 | 0.08 | 104.30 | 506k lines/s | 0.19 | 0.18 | 0.023 |
| sgr | 17.03 | 0.13 | 127.06 | 59.7k lines/s | 0.28 | 0.17 | 0.016 |
| cursor | 2.09 | 0.03 | 69.79 | 8.33M ops/s | 0.12 | 0.10 | **0.057** |
| repaint | 2.57 | 0.04 | 65.79 | 20.5k frames/s | 0.10 | 0.13 | 0.039 |

**0.95 s app CPU + 0.79 s WindowServer to consume 43.55 MB.**
Footprint 92 MB → 131 MB across the run (peak 151 MB).

## Head to head — normalised

⚠️ **The two runs used different geometries** (Terminal 100x50 = 5000 cells,
terminite 119x29 = 3451 cells) because the harness sizes payloads from the host
pane. The `MB/s` and `CPU s/MB` columns are therefore **not comparable between
the two tables** — terminite's lines are 19% wider, and its repaint frames have
31% fewer cells. Normalising to *cells of work per CPU second* removes that:

| workload | Terminal cells/CPU-s | terminite cells/CPU-s | winner |
|---|---|---|---|
| ascii | 50.0M | 54.9M | terminite 1.10x |
| wide | 21.1M | 25.1M | terminite 1.19x |
| sgr | 1.31M | 3.40M | **terminite 2.59x** |
| cursor | 1.67M ops | 2.08M ops | terminite 1.25x |
| repaint | 35.6M | 26.7M | **Terminal 1.34x** |

**Combined: Terminal 2.22 s CPU (app + WS) vs terminite 1.74 s — terminite uses
1.28x less CPU while consuming 14% more bytes.**

terminite wins four of five, and wins biggest exactly where Terminal was weakest:
**SGR churn, 2.59x**. Cursor addressing — Terminal's worst per-byte case — also
goes to terminite, 1.25x.

### The one loss is the interesting result

**Full-frame repaint: Terminal is 1.34x more efficient per cell.** That is not a
damage-tracking artifact — in `repaint` every cell changes each frame, so dirty
tracking buys Terminal nothing there. It is a straight comparison of full-frame
draw cost, and terminite's is higher.

Which sharpens what to look at next. terminite has **no damage tracking at all**:
`render()` clears the whole buffer and redraws every layer, every frame, whether
one cell changed or all of them. It still wins `cursor` (1.25x) *despite* that,
because its parse path is much faster — but that means a cursor blink or a
spinner tick currently costs a full-window repaint. Damage tracking would not
help the `repaint` row, but it is the obvious lever for the sparse-update case
that dominates real use, and it bears directly on idle energy (~13% of a core
with spinners on screen).

### Reproducing this fairly

For a clean comparison both apps need the same cell grid. Neither run above did:

```sh
# terminite: size a single pane to 100x50 first, then
cd ~/src/terminite && ./tools/bench-render.sh --out ~/bench-terminite.txt
```

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
