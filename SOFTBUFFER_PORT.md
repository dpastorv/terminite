# Softbuffer / CPU-render port — handoff

Branch: `spike/softbuffer` (off `main` @ `89c3117`). `main` is the shipping
wgpu build and is untouched — it's also the A/B reference for eyeballing the CPU
build. Resume with: `git checkout spike/softbuffer`, read this file, then say
**"resume step 6."**

Steps 1–5 are done. What's left is the flash verdict and the per-frame
framebuffer allocation — both under "Step 6" at the bottom.

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
| Step 5 — cut wgpu (CPU is the only path) | ✅ 199→173 crates, 21→14 MB binary, wgpu/glyphon/naga out of the tree | (this commit) |

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

### Why RSS did *not* drop — the real answer

Measured, idle, one shell pane: **172 MB, flat over 30s.** Versus 174–177 MB with
wgpu still resident, and 105 MB for the pure wgpu path. So cutting wgpu freed
almost nothing, and the CPU path costs ~67 MB more than the GPU one. The earlier
guess in this doc — "it's softbuffer stacked on top of wgpu, step 5 fixes it" —
**was wrong**. Here's the actual mechanism, from softbuffer 0.4.8's
`src/backends/cg.rs`:

```rust
fn buffer_mut(&mut self) -> Result<BufferImpl<'_, D, W>, SoftBufferError> {
    Ok(BufferImpl { buffer: util::PixelBuffer(vec![0; self.width * self.height]), imp: self })
}
```

- `buffer_mut()` allocates a **brand-new zeroed buffer every frame** — ~20 MB at
  full-screen Retina.
- `present()` then **gives that allocation away**: `Box::into_raw` hands it to a
  `CGDataProvider`, which frees it from a release callback whenever Core Graphics
  is finished with it.

So each frame mints a fresh multi-MB mapping, faults it in as `fill()` +
`blit_*` touch every page, and surrenders it to CG for an indeterminate time.
`vec![0; n]` at that size is `alloc_zeroed` → fresh zero pages, so the "zeroing"
is virtually free, but faulting 20 MB of pages in per frame is not. Steady-state
churn with several buffers in flight is what the 172 MB is; it's flat because
it's churn, not a leak.

This is a softbuffer API limitation, not something terminite can fix from the
outside — the allocation is internal to `buffer_mut()`. Options for step 6 below.
Note the spike still measured 275 fps / 3.6 ms per frame *with* this behaviour,
so it's an energy and footprint question, not a "does it work" question.

## Remaining steps
- **Step 6 — prove & merge:** run CPU-terminite vs the wgpu build on `main`. Bar
  (Daniel's words): **"similar, not pixel-perfect, is fine."** Must look right,
  feel as smooth, and — the whole point — **never flash**. Then merge to `main`.

  Two open items to settle first:

  1. **The flash verdict.** Not seen yet as of step 4, but not yet confirmed
     either. Now finally testable without a confound: until step 5, wgpu's
     CAMetalLayer was still attached to the window even in CPU mode.
  2. **The per-frame framebuffer allocation** (see above). Worth fixing before
     merge if the energy number matters — it's a 20 MB map + page-fault per
     frame. Three ways out, cheapest first:
     - Upstream it: softbuffer could reuse a buffer instead of `vec![0; …]` per
       call. File the issue; the CG backend's `present()` giving the allocation
       to `CGDataProvider` is what forces a fresh one, so this needs a real
       design change there (keep a pool, or copy into the provider).
     - Hand-roll the macOS present path: our own `CALayer` + `CGImage` over a
       persistent double buffer. `objc2` is already a dependency. Costs us
       softbuffer's cross-platform story on macOS specifically, which is the one
       platform we ship today.
     - Accept it and measure. The spike hit 275 fps with this behaviour; if
       Activity Monitor's Energy Impact is fine, this is a footnote, not a bug.
       **Measure before choosing** — don't optimize on the strength of a
       code read.

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
