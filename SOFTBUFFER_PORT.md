# Softbuffer / CPU-render port — handoff

Branch: `spike/softbuffer` (off `main` @ `89c3117`). `main` is the shipping
wgpu build and is untouched — it's also the A/B reference for eyeballing the CPU
build. Resume with: `git checkout spike/softbuffer`, read this file, then say
**"resume step 6."**

Steps 1–5 are done, plus a 2.2× frame-time fix (`0eabb16`). What's left is
Daniel's verdict on the flash, the feel, and the colours — see "Step 6" at the
bottom. Nothing merges to `main` until he calls it.

Headline numbers, all measured: footprint **414 → 88 MB**, frame time
**18.8 → 8.67 ms**, release binary **11 → 7.9 MB**, deps **199 → 173 crates**.

Render cost against Apple Terminal lives in **`BENCHMARKS.md`** (tool:
`tools/bench-render.sh`). Apple Terminal's baseline is measured; **terminite has
not been run through it yet** — that's the open half of the comparison. Its
worst per-byte cases are cursor addressing and SGR churn, not scrolling, so
that's where the CPU renderer gets judged.

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
| Step 3 — CPU text (layer display list + cosmic-text raster) | ✅ **visual confirmed** — Daniel: "looks the same as the other one", Cmd+G card included. ⏳ flash verdict still pending a longer soak. | `96e588f` |
| Step 4 — images (bilinear blit, single-copy residency) | ✅ **visual confirmed** — Daniel: "the images are viewable" | `03c4c89` |
| Step 5 — cut wgpu (CPU is the only path) | ✅ 199→173 crates, 21→14 MB binary, wgpu/glyphon/naga out of the tree, footprint 414→88 MB | `f840b5e` |
| sRGB window colour space | ✅ frame time 18.8→8.67 ms, CPU ~25%→~13% (profiled, then measured live) | `0eabb16` |
| **The flash** | ⚠️ **UNRESOLVED — evidence contaminated.** Seen once on the CPU build, but during a window I was polluting with window launches/kills, `sample`, and fat-LTO builds. Not usable evidence. Two of my step-5 regressions found and fixed in `fefcf1b` regardless. Needs a clean-room soak — see below | `fefcf1b` |

Run it: `cargo run`. There's one render path — the `TERMINITE_CPU` flag was
removed in step 5.

## Testing protocol — read before judging the flash

**The flash evidence collected on 2026-07-27 is contaminated and should not be
used.** While Daniel was watching for an intermittent one-frame artifact, the
agent was, on the same machine: `rm -rf`-ing and replacing the running `.app`
twice; running `sample` (which **suspends the target's threads**) against the
live process; launching and killing two *maximized* spike windows rendering at
105–586 fps; launching and killing two more terminite windows for a footprint
comparison; and running `cargo build --release` with fat LTO across all cores.
Every one of those is a way to make the window server drop a frame.

**The asymmetry matters.** Contamination causes false *positives*, not false
negatives:
- The 17-minute clean stretch **is** meaningful — it survived a hostile
  environment.
- The single flash sighting **is not** — "terminite dropped a frame" is
  indistinguishable from "an agent killed a 586 fps maximized window."

**A clean-room soak, then:**
1. Agent runs **nothing** — no builds, no app launches, no `sample`, no installs.
   Not "light" activity; none.
2. Nothing else heavy on the machine. Check first: `nesessionmanager`,
   `mDNSResponder` and `com.jamf.protect.security-extension` were each seen at
   80–90% CPU, and Battle.net Helper at ~33%, independent of terminite.
3. Daniel uses terminite normally for a stretch longer than the one that already
   passed clean.
4. If it flashes, the one detail that decides everything: **dark rectangle or the
   desktop?** `c0057da` established that layer opacity never stopped the blink,
   only changed what it shows. Dark ⇒ the see-through mechanism is confirmed and
   a dropped frame is still the real event. Desktop ⇒ neither `fefcf1b` fix was
   it. Then check `grep surface_size ~/.terminite/log/terminite.log` for whether
   a resize was involved.
5. Free control: if a **non-terminite** window ever flashes, it's environmental.

## Installed for live testing (2026-07-27)

Daniel is dogfooding the CPU build as his actual terminal; the flash verdict
comes from real use.

| | build | size |
|---|---|---|
| `/Applications/Terminite.app` | CPU (`0eabb16`) | 7.9 MB |
| `/Applications/Terminite-wgpu.app` | shipping wgpu (`89c3117` = `main`) | 11 MB |

The wgpu copy is both the **A/B reference** and the **escape hatch**. Revert with:

```sh
rm -rf /Applications/Terminite.app && cp -R /Applications/Terminite-wgpu.app /Applications/Terminite.app
```

Rebuild + reinstall after a change: `./tools/build-app.sh` (bundle, icon, ad-hoc
sign, install, PATH — all idempotent). Replacing the bundle does **not** affect
an already-running instance; it keeps the old binary mapped until it exits, so a
full quit + relaunch is required to actually test a new build.

## How it's wired

- **Deps:** `softbuffer = "0.4"` (owns the window's pixel buffer, blits
  synchronously) + `cosmic-text = "0.18"` (shapes and rasterizes glyphs). No
  wgpu, no glyphon.
- **`src/renderer/render.rs`** — `render()` builds ONE ordered display list and
  hands it to `present_cpu`:
  - `CpuLayer::{Rects, Text, Images}` — the frame's 8 layers in z-order. Rects
    and text interleave, so nothing can present until the frame is fully
    described. That constraint is why `render()` collects everything before
    consuming any of it.
  - `present_cpu(sb, font_system, swash_cache, cache_bytes, size, bg, layers)` —
    a free function, not a method: the `TextArea`s borrow `self.root` /
    `self.glyph_cache` / the overlay state, so the raster can't also take
    `&mut self` for the font system. Callers pass disjoint fields.
  - `blit_rect` / `blit_text_area` / `blit_image` → all composite through
    `blend_px` into a **0RGB u32** buffer (softbuffer's macOS format), straight
    alpha, sRGB throughout — no linearization anywhere.
- **`src/renderer/mod.rs`** — local `TextArea` / `TextBounds` (were glyphon's),
  `SurfaceSize`, and the two cache ceilings (`SWASH_CACHE_MAX_BYTES`,
  `GLYPH_CACHE_CAP`).
- **`src/rect.rs` / `src/texture.rs`** — reduced to the data types
  (`RectInstance`, `TextureInstance`, `TextureImage`); both GPU pipelines deleted.

## STEP 3 — text — HOW IT ACTUALLY LANDED

The plan below called for a per-command `enum DrawCmd { Rect | Text | Image }`.
That turned out to be more machinery than the problem needs: rects and text
don't interleave arbitrarily, they interleave at **exactly 8 fixed layers**, and
`render()` already collected each one into its own `Vec`. So the display list is
a list of *layers*:

```rust
enum CpuLayer<'a> { Rects(&'a [RectInstance]), Text(&'a [TextArea<'a>]) }
```

Same z-order guarantee, no per-frame per-command Vec churn. Step 4 added the
`Image` variant.

What changed in `render()`:
- The five scattered `modal_text_renderer.prepare(...)` calls became collection
  into one `overlay_areas` Vec. Each block **assigns** (or `clear()`s first)
  rather than appending — glyphon's `prepare` *replaces* a renderer's contents,
  so under wgpu only the last block to run was ever drawn. Assigning reproduces
  that exactly, which is what keeps the refactor behaviour-neutral for the GPU
  path. Same for `overlay_rects`, which the Cmd+G card replaces wholesale.
- All `prepare` calls moved to *after* the CPU branch point, so both backends
  consume the same fully-built layer set. This also folded in the two late rect
  groups step 2 skipped (phase-2 block-label highlights, the Cmd+G card).
- `root`, `content_areas`, `tab_areas` hoisted out of the phase-2 block so they
  outlive it.
- `render_cpu` (method) → `present_cpu` + `blit_text_area` + `blend_px`
  (module-level free functions). It has to be a free function: the `TextArea`s
  borrow `self.root` / `self.glyph_cache` / the overlay state, so the raster
  can't also take `&mut self` for the font system. Callers pass disjoint fields.

`blit_text_area` deliberately does **not** use `Buffer::draw` (what the spike
used). `draw` hardcodes its glyph origin to `(0, run.line_y)`, so a caller can
only offset it by whole pixels afterwards — that loses the sub-pixel x bucket in
the glyph cache key and drifts up to a pixel from where the GPU path puts the
same glyph. Instead it walks `layout_runs()` and reproduces glyphon's placement
math exactly (see `glyphon::text_render`):

```
x = physical(left, top).x + image.placement.left
y = round(line_y × scale) + physical(left, top).y − image.placement.top
```

`swash_cache` supplies the two `placement` terms and rasterizes each glyph once
per cache key — the CPU analogue of glyphon's atlas. glyphon's `is_run_visible`
run culling is replicated too, so a 1 MB Editor body or a full scrollback only
touches runs that can land in `bounds`. (Step 4 replaced the original
`SwashCache::with_pixels` call with a hand-rolled walk, so cache growth can be
accounted for — `with_pixels` hides hit-vs-miss.)

Verified: `cargo build` clean, `cargo test` 99/99, `cargo clippy --all-targets`
warning set **byte-identical** to `e240a7f` (no new lints), both binaries run
without panicking. cosmic-text promoted dev-dep → real dep; `cargo tree` confirms
one copy (0.18.2) shared with glyphon.

### Known gaps / follow-ups

- **Colour glyphs (emoji)** blend straight-alpha. If swash hands back
  premultiplied RGBA the edges will read slightly dark. Untested — the spike
  didn't cover emoji.
- Text composites in **sRGB** (softbuffer's format), whereas the wgpu rect shader
  linearizes. Expect text to read very slightly bolder or thinner than the GPU
  build. Within the "similar, not pixel-perfect" bar.
- **Per-frame `TextArea` churn.** A shell pane materializes one `TextArea` per
  visible cell each frame (~12k at full-screen Retina, ~700 KB allocated and
  freed). Inherited from the wgpu path — glyphon's `prepare` needed the Vec — but
  nothing needs it now: the rasterizer could walk `cell_glyphs` directly. Small
  next to the framebuffer allocation below, but free to fix. Allocator traffic,
  not a leak.

## STEP 4 — images ✅

Decoded images now blit into the CPU buffer (`blit_image`), slotted into the
display list between `above` and `tab_bar` — where `texture_renderer.render`
sits in the wgpu pass. Bilinear-sampled to match the texture pipeline's
`FilterMode::Linear`: nearest-neighbour visibly aliases a downscaled photo, and
downscaling is the common case since `render` fits images to the pane and never
upscales. Same `MAX_INSTANCES` cap as the GPU instance buffer, so both backends
drop the same extras.

`TextureImage` grew a CPU representation, and the two are **mutually
exclusive** — `upload(.., for_cpu)` either uploads to the GPU *or* keeps the
decoded RGBA, never both. So an image still costs `w × h × 4` exactly once on
either path, rather than doubling while the backends coexist. `bind_group()`
returns `Option` accordingly. Step 5 drops the GPU half.

## Footprint / leak audit (Daniel asked, 2026-07-27)

> **Superseded — read this first.** The original version of this section
> compared the two backends by **`ps` RSS** and concluded "CPU mode is ~72 MB
> heavier, not leaner." **That was wrong, and the metric was the reason.** On
> macOS, `ps` RSS does not charge a process for its GPU/driver allocations, but
> *does* count mapped font and binary pages. For this comparison it inverts the
> ranking outright. Use `footprint -p <pid>` (phys_footprint). Same finding
> arrived at independently while benchmarking terminite against Apple Terminal.

Measured with `footprint`, both as fresh socket-less instances, one shell, 20s up:

| | phys_footprint | peak | (`ps` RSS) |
|---|---|---|---|
| wgpu `89c3117` | **414 MB** | 422 MB | 108 MB |
| CPU `0eabb16` | **88 MB** | 107 MB | 156 MB |

**The CPU build is 4.7× smaller — 326 MB less.** RSS said the opposite (108 vs
156) because wgpu's Metal driver and GPU-side allocations don't land in RSS,
while terminite's ~33 MB of mapped files do. For scale, Apple Terminal with one
shell measures 288 MB phys_footprint (peak 392 MB), so the CPU build is ~3×
leaner than the system terminal too.

Leak check: RSS held flat across 60s (~120 frames) and phys_footprint peak sits
close to steady state, so the per-frame path releases what it takes.

What's bounded, and by what:
- **`swash_cache.image_cache`** — `SWASH_CACHE_MAX_BYTES` (16 MB), blunt clear +
  one frame of re-rasterization. This was a real unbounded-growth bug introduced
  by step 3: `SwashCache` exposes **no eviction of its own**, and under wgpu that
  didn't matter (it fed glyphon's atlas, which has `trim()`) — on the CPU path it
  *is* the glyph cache. Keys include font + size + sub-pixel x bucket, so
  dragging the Cmd+G sliders mints fresh entries indefinitely. Bounded by bytes
  rather than entry count because entry sizes differ ~100× (ASCII mask ≈ a few
  hundred bytes, colour emoji ≈ tens of KB). Every new key is also charged
  `SWASH_ENTRY_OVERHEAD`, so keys that cache to *no* bitmap (a space, a glyph
  swash declines) still count — otherwise a churn of empty entries would grow the
  map forever with the counter stuck at zero.
- **`glyph_cache`** — `GLYPH_CACHE_CAP`, pre-existing.
- **Images** — one copy, on one side, per above. No eviction of the images
  themselves, but that's bounded per tab and pre-existing.
- **RSS kill switch** — `check_rss_kill_switch` still runs first thing in
  `render()` on both paths.

Crash safety: `blend_px` / `blit_rect` / `blit_image` are unit-tested against
degenerate input — off-buffer, zero-size, NaN origin/extent, absurd 1e9 rects,
and a source shorter than `w × h × 4` (truncated decode). All clip or refuse;
none draw, none panic. Glyph and image writes are proven in-bounds by
construction (the clip is resolved into source-relative row/column ranges before
the inner loop). `overflow-checks` is off in this profile, so a wrapped
coordinate wouldn't trap on the arithmetic — but Rust's slice bounds checks are
always on, so the worst case is a clean panic, never memory corruption. Nothing
here can take the kernel down; there's no unsafe code and no driver call on the
CPU path.

## STEP 3 — original plan (kept for reference)

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

## STEP 5 — cut wgpu ✅

CPU is the only path; `TERMINITE_CPU` is gone. `cargo tree` confirms **wgpu,
glyphon and naga are out of the dependency tree**.

| | before | after |
|---|---|---|
| unique crates | 199 | **173** (−26) |
| debug binary | 21 MB | **14 MB** (−33%) |
| tests | 104 | 101 (−3, the surface-retry tests went with the code) |

Deleted: `RectRenderer` + its WGSL, `TextureRenderer` + its WGSL, the three
glyphon `TextRenderer`s, `TextAtlas`, `Viewport`, `Cache`, the
instance/adapter/device/queue/surface, `SurfaceConfiguration`, the wgpu
uncaptured-error (VRAM OOM) handler, `rgb_to_clear`, and `bytemuck` + `pollster`
as dependencies (`Renderer::new` is no longer `async` — nothing to await).

Also deleted, and worth noting: the **entire surface-failure retry machinery** —
`surface_retry_delay`, `schedule_surface_retry`, the backoff deadline, the
timeout/outdated/lost/suboptimal match. Those existed because acquiring a GPU
drawable can fail (the 2026-06-03 alt-tab freeze). A CPU frame is a memory write
and a blit; there is no acquisition to fail. Same for
`set_window_layer_opaque` — that was a *flash mitigation* for wgpu's
CAMetalLayer, so it went with the thing it was mitigating. `objc2` stays, but
only for `disable_press_and_hold` in `main.rs` (unrelated Foundation call).

glyphon's `TextArea` / `TextBounds` are now local definitions in
`renderer/mod.rs` — field-for-field, minus `custom_glyphs`, which terminite never
used. `Color`, `Buffer`, `FontSystem`, `Attrs`, `Metrics`, `Shaping`, `Style`,
`Weight`, `SwashCache` were always cosmic-text's types (glyphon re-exported
them), so those imports just changed crate. `surface_config` → a local
`SurfaceSize { width, height }`, updated by `resize`.

### Footprint: 414 MB → 88 MB

Measured with `footprint -p` (**not** `ps` RSS — see the audit section above for
why RSS inverts this comparison), both as fresh socket-less instances, one
shell, 20s up:

| | phys_footprint | peak |
|---|---|---|
| wgpu `89c3117` | 414 MB | 422 MB |
| CPU `0eabb16` | **88 MB** | 107 MB |

**4.7× smaller, 326 MB less.** Most of what went away is the Metal driver and
its GPU-side allocations, which `ps` never charged to the process — which is
exactly why an RSS-based reading of this same pair said the CPU build was
*heavier*. It isn't, and it never was.

### The per-frame framebuffer allocation (still true, still minor)

softbuffer 0.4.8's macOS backend allocates a fresh buffer every frame
(`src/backends/cg.rs`):

```rust
fn buffer_mut(&mut self) -> ... {
    Ok(BufferImpl { buffer: PixelBuffer(vec![0; self.width * self.height]), .. })
}
```

`present()` then gives that allocation away — `Box::into_raw` into a
`CGDataProvider` that frees it from a release callback. So each frame maps
~20 MB at Retina, faults it in as the blitters touch it, and surrenders it to
Core Graphics.

This is real, but it is **not** what was costing us. Two rounds of guessing from
source reads (this, and per-cell `TextArea` churn before it) both pointed at the
wrong thing; profiling found the actual cost in one pass — a per-frame CPU
colour-space conversion, fixed in `0eabb16` for a 2.2× frame-time win. Leaving
this documented as a known property, not an action item. **Profile first.**

## Remaining steps

- **Step 6 — prove & merge:** run the CPU build against the wgpu build on
  `main`. Bar (Daniel's words): **"similar, not pixel-perfect, is fine."** Must
  look right, feel as smooth, and — the whole point — **never flash**. Then merge
  to `main`.

  **Settled since the port started:**
  - Footprint: **414 MB → 88 MB** phys_footprint (4.7× smaller; ~3× leaner than
    Apple Terminal's 288 MB too). Binary 21 → 14 MB debug, 11 → 7.9 MB release.
    Deps 199 → 173 crates.
  - Frame time: **18.8 ms → 8.67 ms** avg live (4 panes, agent spinners), p99
    27.1 → 12.2 ms, CPU ~25% → ~10–17% of a core. The win came from declaring
    the window sRGB (`0eabb16`).
  - Visual parity at steps 3 and 4, confirmed by Daniel.

  - **The flash: REPRODUCED on the CPU build.** Daniel saw it ~20 minutes in,
    after initially reporting it clean. He could not attribute it: *"i cant
    confirm if it was something that you did or not."* **This invalidates the
    port's founding premise** — there is no wgpu, no Metal and no CAMetalLayer in
    this binary (verified: zero such symbols), so "the flash is wgpu async
    present" cannot be the whole story. See the reopened Research verdict.

  **Still open — Daniel's eyes, not code questions:**
  1. **Feel** — scroll, splits, tab switching, against the wgpu build.
  2. **Colour** — the sRGB window change in `0eabb16` landed after his last
     look. Should read *more* accurate, not less; one line to revert if not.

  Then: merge to `main`.

- **Known, deliberately not acted on:** softbuffer's per-frame framebuffer
  allocation (documented above) and the per-cell `TextArea` churn (~13.8k
  `TextArea`s per frame across 4 panes, inherited from glyphon's `prepare`; the
  rasterizer could walk `cell_glyphs` directly). Both real, both minor at
  8.67 ms/frame. **Profile before touching either** — source-reading picked the
  wrong culprit twice on this port.

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

## Research verdict — REOPENED 2026-07-27

**The flash reproduced on a build with no wgpu, no Metal and no CAMetalLayer.**
Everything below was written when wgpu was the only suspect; treat it as history,
not as settled. What still stands: `presentsWithTransaction` isn't reachable
through wgpu, and it deadlocked Zed. What does *not* stand: that removing wgpu
removes the flash.

Two regressions of my own were found while investigating, both introduced in
step 5 and both now fixed. Neither is confirmed as *the* cause:

1. **Layer opacity was deleted.** `c0057da` set `opaque = true` on the window's
   layer; step 5 removed it as "a wgpu mitigation," which was wrong — layer
   opacity is a window property under any renderer. Worse, softbuffer's
   `Surface::new` adds its *own* sublayer (`CALayer::new()`, which defaults to
   `opaque = false`), so a see-through sublayer sat over a root layer we'd
   stopped forcing opaque. Restored, now covering root **and** sublayers.
   Note what `c0057da` said: opacity never stopped the blink, it only made the
   blink show dark backing instead of the desktop. So if the flash recurs and is
   now **dark rather than desktop**, that confirms the see-through mechanism and
   the underlying dropped frame is still there.
2. **Buffer sized from stale `surface_size`.** Step 5 presented at
   `surface_size` rather than the live `inner_size()`. Before a Resized event is
   processed those disagree, and a buffer smaller than the window leaves an
   uncovered strip — a literal hole to the desktop. Now painted at the live
   window size (background fill always covers), with a `logging::warn` when the
   two disagree, so a correlation would be visible in the log.

Prior investigation to build on, not repeat: `c0057da` recorded *"Confirmed not
terminite logic — resize/surface/occlusion never fired at flash time"* under
wgpu, and `fd98a73` found it *only happens while the screen is actively
changing*.

### Original verdict (pre-CPU-port, kept as history)

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
