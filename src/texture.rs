//! Decoded images — Kitty graphics from a shell, stills and animation frames
//! from a data module (Preview).
//!
//! This used to hold a wgpu textured-quad pipeline that uploaded each image to
//! VRAM. Rendering is CPU-side now: an image keeps its decoded RGBA and
//! `renderer::render::blit_image` samples it straight into the softbuffer pixel
//! buffer. What's left is the image itself plus where to put it.

use crate::images::ImageData;

/// Where one image is drawn this frame.
#[derive(Copy, Clone)]
pub struct TextureInstance {
    /// x, y, w, h in physical pixels (top-left origin). `render` fits the image
    /// to its pane and never upscales, so this is usually a downscale.
    pub rect: [f32; 4],
}

/// One decoded image, held as `width * height * 4` bytes of RGBA8.
pub struct TextureImage {
    pub width: u32,
    pub height: u32,
    rgba: Vec<u8>,
}

impl TextureImage {
    /// Take ownership of a decoded image's pixels.
    pub fn new(image: &ImageData) -> Self {
        Self {
            width: image.width,
            height: image.height,
            rgba: image.rgba.clone(),
        }
    }

    /// Decoded RGBA8, row-major, `width * height * 4` bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.rgba
    }
}

/// Maximum images drawn per frame; extras are silently dropped. Generous — a
/// session with > 64 simultaneous on-screen images is unreal. Kept from the
/// wgpu path, where it sized the instance buffer up front, so that an absurd
/// image count still can't turn into an unbounded per-frame blit.
pub const MAX_INSTANCES: usize = 64;
