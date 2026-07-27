//! The filled-rectangle primitive: cell backgrounds, the cursor, selection
//! highlights, underline/strikethrough decorations, tab-bar chrome, overlay
//! cards — everything in a frame that isn't a glyph or an image.
//!
//! This used to also hold a wgpu instanced-quad pipeline. Rendering is CPU-side
//! now (`renderer::render::blit_rect` alpha-blends these into the softbuffer
//! pixel buffer), so all that's left is the vocabulary the frame is described
//! in.

#[derive(Copy, Clone)]
pub struct RectInstance {
    /// x, y, w, h in physical pixels (top-left origin).
    pub rect: [f32; 4],
    /// rgba in [0, 1], alpha-blended onto whatever is below. sRGB — the CPU
    /// blitter composites straight, with no linearization step.
    pub color: [f32; 4],
}
