//! A small wgpu pipeline for textured rectangles. Used to draw decoded
//! images (Kitty graphics) over the cell grid. Sibling of `rect.rs`; the
//! difference is a second bind group per draw that carries the image's
//! texture + sampler. One quad per image, alpha-blended onto the surface.
//!
//! The pipeline is instance-based: one instance buffer holds the pixel
//! rects of every image in the frame, and `render` walks the per-image
//! bind groups, issuing a draw per image at its instance index.

use bytemuck::{Pod, Zeroable};

use crate::images::ImageData;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct TextureInstance {
    /// x, y, w, h in physical pixels (top-left origin).
    pub rect: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Uniforms {
    resolution: [f32; 2],
    _pad: [f32; 2],
}

const TEXTURE_WGSL: &str = r#"
struct Uniforms {
    resolution: vec2<f32>,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var img: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @location(0) rect: vec4<f32>,
) -> VertexOutput {
    let corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[vid];
    let px = rect.x + c.x * rect.z;
    let py = rect.y + c.y * rect.w;
    let ndc_x = px / uniforms.resolution.x * 2.0 - 1.0;
    let ndc_y = 1.0 - py / uniforms.resolution.y * 2.0;
    var out: VertexOutput;
    out.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = c;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(img, samp, in.uv);
}
"#;

/// One uploaded image — owns the GPU texture, its view, and a bind group
/// that pairs the view with the shared sampler. Dropping it releases all
/// three; wgpu reference-counts behind the scenes.
pub struct TextureImage {
    pub width: u32,
    pub height: u32,
    #[allow(dead_code)]
    texture: wgpu::Texture,
    #[allow(dead_code)]
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

impl TextureImage {
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }
}

/// Maximum images drawn per frame. The instance buffer is sized to this
/// up front; extra images in the same frame would be silently dropped.
/// Generous: a session with > 64 simultaneous on-screen images is unreal.
const MAX_INSTANCES: usize = 64;

pub struct TextureRenderer {
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    instance_buffer: wgpu::Buffer,
    count: u32,
}

impl TextureRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terminite texture shader"),
            source: wgpu::ShaderSource::Wgsl(TEXTURE_WGSL.into()),
        });

        // Group 0: per-frame uniforms (resolution).
        let uniform_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terminite texture uniforms bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terminite texture uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terminite texture uniforms bg"),
            layout: &uniform_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        // Group 1: per-image texture + sampler.
        let texture_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terminite texture image bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("terminite texture sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terminite texture pl"),
            bind_group_layouts: &[Some(&uniform_bgl), Some(&texture_bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terminite texture pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TextureInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[wgpu::VertexAttribute {
                        offset: 0,
                        shader_location: 0,
                        format: wgpu::VertexFormat::Float32x4,
                    }],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terminite texture instances"),
            size: (MAX_INSTANCES * std::mem::size_of::<TextureInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            uniform_buffer,
            uniform_bind_group,
            texture_bind_group_layout: texture_bgl,
            sampler,
            instance_buffer,
            count: 0,
        }
    }

    /// Upload a decoded image to the GPU and return a handle that owns its
    /// texture + bind group. Drop the handle to release the GPU memory.
    pub fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, image: &ImageData) -> TextureImage {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("terminite image texture"),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Non-sRGB to match the (now Unorm) surface: the decoded PNG
            // bytes are already sRGB-encoded, and we want them sampled raw
            // and passed straight through to the raw target unchanged.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &image.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 4),
                rows_per_image: Some(image.height),
            },
            wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terminite image bg"),
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        TextureImage {
            width: image.width,
            height: image.height,
            texture,
            view,
            bind_group,
        }
    }

    /// Stage the instance buffer for this frame's images and the resolution.
    /// `instances[i]` is drawn with the `i`-th bind group passed to `render`.
    /// Instances past `MAX_INSTANCES` are silently dropped.
    pub fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        instances: &[TextureInstance],
        resolution: [f32; 2],
    ) {
        let count = instances.len().min(MAX_INSTANCES);
        self.count = count as u32;
        if count > 0 {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&instances[..count]),
            );
        }
        let uniforms = Uniforms { resolution, _pad: [0.0, 0.0] };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    /// Draw `bind_groups[i]` at instance `i` of the prepared buffer. Caller
    /// must pass exactly as many bind groups as instances were prepared.
    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>, bind_groups: &[wgpu::BindGroup]) {
        if self.count == 0 || bind_groups.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        let n = (self.count as usize).min(bind_groups.len());
        for (i, bg) in bind_groups.iter().take(n).enumerate() {
            pass.set_bind_group(1, bg, &[]);
            let idx = i as u32;
            pass.draw(0..4, idx..idx + 1);
        }
    }
}
