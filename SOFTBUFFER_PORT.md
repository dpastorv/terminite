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
| Step 3 — CPU text (layer display list + cosmic-text raster) | ✅ **visual confirmed** — Daniel: "looks the same as the other one", Cmd+G card included. ⏳ flash verdict still pending a longer soak. | `96e588f` |
| Step 4 — images (bilinear blit, single-copy residency) | ✅ code + footprint/leak audit; ⏳ needs an eyeball on a Preview / Kitty pane | (this commit) |

Run the WIP: `TERMINITE_CPU=1 cargo run` → full chrome + **readable text**
(content grid, tab labels, overlays); images are still step 4. Plain
`cargo run` = normal wgpu.

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

## STEP 3 — text — HOW IT ACTUALLY LANDED

The plan below called for a per-command `enum DrawCmd { Rect | Text | Image }`.
That turned out to be more machinery than the problem needs: rects and text
don't interleave arbitrarily, they interleave at **exactly 8 fixed layers**, and
`render()` already collected each one into its own `Vec`. So the display list is
a list of *layers*:

```rust
enum CpuLayer<'a> { Rects(&'a [RectInstance]), Text(&'a [TextArea<'a>]) }
```

Same z-order guarantee, no per-frame per-command Vec churn. Step 4 adds an
`Image` variant at the marked slot.

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

`SwashCache::with_pixels` supplies the two `placement` terms and rasterizes each
glyph once per cache key — the CPU analogue of glyphon's atlas. glyphon's
`is_run_visible` run culling is replicated too, so a 1 MB Editor body or a full
scrollback only touches runs that can land in `bounds`.

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
  freed). That's inherited from the wgpu path — glyphon's `prepare` needed it —
  but the CPU path could rasterize straight from `cell_glyphs` and skip the Vec
  entirely. Worth doing once glyphon is gone (step 5/6); it's allocator traffic,
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

Measured, `./target/debug/terminite`, one shell pane idle (cursor blink drives
~2 frames/sec):

| | RSS | drift |
|---|---|---|
| wgpu | 105 MB | 0 MB over 15s |
| CPU (`TERMINITE_CPU=1`) | 174–177 MB | 0 MB over **60s** (~120 frames), trending slightly *down* |

**CPU mode is currently ~72 MB heavier, not leaner** — and that's the expected
shape of the transition, not a regression to chase. `TERMINITE_CPU=1` stands up
softbuffer *alongside* a fully-constructed wgpu (device, queue, surface, three
glyphon renderers, the atlas), because the `Renderer` fields aren't optional
yet. The delta is softbuffer's Retina pixel buffer plus the glyph cache, stacked
on top of a wgpu stack that's still resident. **Step 5 is where the footprint
win lands**, by deleting the wgpu side — that's what makes the number move.

Flat RSS across 60s is the useful signal here: the per-frame path allocates
nothing that it doesn't release.

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

## Remaining steps

- **Step 5 — cut wgpu:** remove wgpu + glyphon deps, delete the GPU present path,
  make CPU the only path (drop the flag). The footprint/compile-time win lands
  here — see the audit above for why CPU mode is *heavier* until this lands.
  Mechanical parts: `TextureImage::gpu` goes away; the three glyphon renderers,
  the atlas, `viewport`, `device`/`queue`/`surface` go away; glyphon's `TextArea`
  / `TextBounds` get local definitions (a field-for-field copy — `Color` is
  already cosmic-text's type, re-exported); `blit_text_area` stops needing
  glyphon at all. The display list means `render()` itself barely changes.
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
