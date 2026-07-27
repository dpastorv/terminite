# Softbuffer / CPU-render port — handoff

Branch: `spike/softbuffer` (off `main` @ `89c3117`). `main` is the shipping
wgpu build and is untouched. Resume with: `git checkout spike/softbuffer`, read
this file, then say **"resume step 3."**

## Why we're doing this

The intermittent macOS "desktop shows through for a frame" flash is a
**wgpu / CAMetalLayer async-present** limitation. We ruled everything else out
(see "Research verdict" below). The only clean fix wgpu can't give us
(`presentsWithTransaction`) is a deadlock trap. Meanwhile Daniel's priorities
are **small footprint, low energy, cross-platform, keep the "room" + the vibe.**

Moving off wgpu to **CPU rendering (softbuffer + cosmic-text)** hits all of it:
- leaner (drops the whole wgpu + glyphon GPU dep stack — big binary/compile win)
- lower idle energy (no GPU; draw on change)
- **kills the flash** (softbuffer blits synchronously — no CAMetalLayer, no async present)
- stays cross-platform (softbuffer = mac/linux/windows)
- the room + all app logic are untouched (only the pixel layer changes)

## What the spike proved (already de-risked)

`examples/softbuffer_spike.rs` — run `cargo run --release --example softbuffer_spike`.
Full-screen Retina, scrolling every frame (worst case), via cosmic-text +
SwashCache + softbuffer blit:
- **~275 fps, avg 3.6ms/frame, max ~4.5ms** at 1600×1200 @2x. Huge headroom.
- **Zero flash** even under that hammering.
Conclusion: CPU rendering is fast enough with margin. The port is worth it.

## Status — done & validated

| Step | State | Commit |
|---|---|---|
| Spike (perf + flash proof) | ✅ | `c04bbe9` |
| Step 1 — softbuffer bg present behind `TERMINITE_CPU=1` | ✅ coexistence confirmed (softbuffer owns the window despite wgpu) | `5873ab3` |
| Step 2 — CPU-rasterize rect layers (bg, cursor, selection, dividers, tab strips, modal/menu bg) | ✅ chrome skeleton renders positionally correct | `00bef89` |

Run the WIP: `TERMINITE_CPU=1 cargo run` → dark bg + tab strips + cursor +
colors, **no text yet** (that's step 3). Plain `cargo run` = normal wgpu.

## How it's wired so far

- **Flag:** `TERMINITE_CPU=1` env var. `src/renderer/mod.rs` `Renderer::new` — if
  set, builds `sb_surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>`
  (wgpu still fully constructed; we cut it out in step 5).
- **`src/renderer/render.rs`:**
  - `render()` — after `overlay_rects` is built (search `let overlay_rects`),
    branches: `if self.sb_surface.is_some() { self.render_cpu(...); return; }`
    before the wgpu `rects_below.prepare`.
  - `render_cpu(&mut self, below, above, tab_bar, overlay)` — resizes the
    softbuffer surface, fills bg, blits rect layers in wgpu draw order, presents.
  - `blit_rect(buf, stride, height, &RectInstance)` — module-level; alpha-blends
    sRGB rgba(0..1) into a **0RGB u32** buffer (softbuffer's macOS format). No
    linearization (softbuffer wants sRGB straight; the wgpu shader linearizes,
    we don't).
- **Deps (`Cargo.toml`, branch only):** `softbuffer = "0.4"` (real dep);
  `cosmic-text = "0.18"` (dev-dep for the spike; promote to real dep in step 3);
  both pinned to the versions glyphon already pulls (cosmic-text 0.18.2) so no
  duplicate copies.

## STEP 3 — text (the crux, do this next)

**The problem:** text and rects interleave in z-order. wgpu draw order is:
`rects_below → content text → rects_above → images → tab-bar rects → tab text
→ modal/overlay rects → overlay text`. You cannot rasterize all rects, present,
then add text — text lands *between* rect layers in one buffer.

**The fix — display-list refactor.** Restructure `render()` (+ `render_pane.rs`)
to build ONE ordered list of draw commands instead of prepping wgpu incrementally:

```
enum DrawCmd { Rect(RectInstance), Text(TextRun), Image(...) }
```
…in draw order. Then a backend consumes it: the wgpu path builds its
rect/text/texture buffers from the list (keeps working), and `render_cpu`
rasterizes it top-to-bottom into the softbuffer buffer. This also cleans up the
wgpu path and makes steps 4–5 trivial.

**CPU text raster is already solved** (the spike). For each text run/area:
`buffer.draw(&mut font_system, &mut swash_cache, default_color, |x,y,w,h,color| …)`
offset by the area's (left, top), clipped to its bounds, alpha-blended into the
pixel buffer (reuse `blit_rect`'s blend). cosmic-text respects per-glyph colors
(rich text) with `default_color` as fallback.

**Where terminite builds text today (reuse this positioning, don't reinvent):**
- Content grid: `src/renderer/render_pane.rs` — `tab.content_buffer` /
  `tab.text_buffer` (cosmic `Buffer`s), iterated via `layout_runs()`; scroll is
  done by adjusting `TextArea.top` per line. This is the fiddly part — replicate
  the exact top/bounds/color it computes.
- Tab labels + `×` close glyph: tab-bar text areas in `render.rs` (search
  `tab_text_renderer`).
- Overlays (modal, menu, palette, Cmd+G sliders card, find, claims): built at
  several points in `render.rs`, prepped via `modal_text_renderer` /
  `text_renderer`. Search `TextArea {` and `.prepare(`.
- Three glyphon renderers to replace: `text_renderer` (content),
  `tab_text_renderer` (tabs), `modal_text_renderer` (overlays).

**Also fold in the late-built rects** step 2 skipped: phase-2 tab-bar highlights
(block labels) and the display-settings (Cmd+G) card rects — they're built in
the interleaved present section after the current branch point. The display-list
refactor naturally captures them.

Payoffs at step 3: first **readable** CPU frame (the real look/feel eyeball) and
the **real flash verdict** (text + activity finally exercise async present).

## Remaining steps after 3

- **Step 4 — images:** blit decoded images (Preview module, Kitty graphics) into
  the buffer. Currently `src/texture.rs` (wgpu `TextureRenderer`). CPU = copy
  RGBA → 0RGB into the target rect.
- **Step 5 — cut wgpu:** remove wgpu + glyphon deps, delete the GPU present path,
  make CPU the only path (drop the flag). The footprint/compile-time win lands here.
- **Step 6 — prove & merge:** run CPU-terminite vs the wgpu build. Bar (Daniel's
  words): **"similar, not pixel-perfect, is fine."** Must look right, feel as
  smooth, and — the whole point — **never flash**. Then merge to `main`.

## Gotchas / notes

- **Pixel format:** softbuffer macOS = 0RGB (`(r<<16)|(g<<8)|b`, top byte 0),
  sRGB, no premultiply. `blit_rect` does straight-alpha blend.
- **Occlusion:** `render()` early-returns when `self.occluded` — keep that before
  the CPU branch (already is).
- **Retina:** render at physical px (window.inner_size()); metrics already scale
  by `scale_factor`. cosmic-text font size should be physical (logical × scale).
- **Two instances:** running `TERMINITE_CPU=1 cargo run` alongside the installed
  app is fine (proto socket just fails to bind → no module surface). Quit extras.
- **Don't reintroduce:** the flash is NOT sync-output (terminite already supports
  DEC 2026 via vendored alacritty/vte + event_loop.rs sync gating) and NOT layer
  opacity (`c0057da` set the CAMetalLayer opaque; didn't help). It's async present.

## Research verdict (so nobody re-litigates)

- Flash root cause = wgpu async CAMetalLayer present. Fix = `presentsWithTransaction`
  + `waitUntilScheduled` (synchronous present). **wgpu doesn't expose it**, and the
  synchronous-main-thread approach **hard-deadlocked Zed** (native Metal experts) —
  zed-industries/zed#53390. Not worth pursuing in-engine.
- Cheap wgpu knobs are dead ends: `PresentMode::Fifo` == what AutoVsync already
  resolves to on macOS (no-op); `desired_maximum_frame_latency` is buffering, not
  sync.
- Even Ghostty (native Metal, does presentsWithTransaction) has a Claude-Code
  flicker discussion (ghostty#8285) — switching engines is no guarantee either.
- CPU rendering sidesteps the whole class: no CAMetalLayer, synchronous blit.
