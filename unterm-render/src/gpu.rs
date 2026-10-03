//! Drawing the quads.
//!
//! The first code here that touches hardware, so it is also the first that
//! cannot be checked by reasoning alone. It renders to an off-screen texture
//! and the tests read the pixels back: a wrong colour, a glyph in the wrong
//! place or a quad that never reached the buffer is an assertion failure, not
//! something to notice later in a window.
//!
//! Backgrounds and glyphs share one pipeline. A background carries no texture
//! and is filled with its colour; a glyph is tinted by its colour with the
//! atlas supplying coverage. One buffer, one bind group, one pass.

use crate::atlas::GlyphAtlas;
use crate::quads::{FrameQuads, Quad};
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    tex_coord: [f32; 2],
    color: [f32; 4],
    has_texture: f32,
    _padding: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Uniforms {
    viewport: [f32; 2],
    _padding: [f32; 2],
}

/// Six vertices per quad: two triangles, no index buffer.
///
/// A terminal frame is a few thousand quads at most, so the memory an index
/// buffer would save is not worth the extra upload and bind.
fn push_quad(out: &mut Vec<Vertex>, quad: Quad, tex: [f32; 4], has_texture: f32) {
    let left = quad.left;
    let top = quad.top;
    let right = quad.left + quad.width;
    let bottom = quad.top + quad.height;
    let (tex_left, tex_top, tex_right, tex_bottom) = (tex[0], tex[1], tex[2], tex[3]);

    let corners = [
        ([left, top], [tex_left, tex_top]),
        ([right, top], [tex_right, tex_top]),
        ([left, bottom], [tex_left, tex_bottom]),
        ([right, top], [tex_right, tex_top]),
        ([right, bottom], [tex_right, tex_bottom]),
        ([left, bottom], [tex_left, tex_bottom]),
    ];

    for (position, tex_coord) in corners {
        out.push(Vertex {
            position,
            tex_coord,
            color: quad.color,
            has_texture,
            _padding: [0.0; 3],
        });
    }
}

/// Build the vertex buffer for a frame.
///
/// Backgrounds first, then glyphs, so text lands on top of its own cell
/// without needing a depth buffer or a second pass.
pub fn build_vertices(quads: &FrameQuads) -> Vec<u8> {
    let mut vertices = Vec::with_capacity((quads.backgrounds.len() + quads.glyphs.len()) * 6);

    // The picture first, under everything, and from its own texture.
    if let Some(image) = &quads.image {
        push_quad(
            &mut vertices,
            image.quad,
            [
                image.tex_left,
                image.tex_top,
                image.tex_right,
                image.tex_bottom,
            ],
            2.0,
        );
    }
    for quad in &quads.backgrounds {
        push_quad(&mut vertices, *quad, [0.0; 4], 0.0);
    }
    // Pictures between the backgrounds and the text, six vertices each, each
    // drawn with its own texture bound -- see `draw`.
    for image in &quads.inline_images {
        let glyph = &image.glyph;
        push_quad(
            &mut vertices,
            glyph.quad,
            [glyph.tex_left, glyph.tex_top, glyph.tex_right, glyph.tex_bottom],
            2.0,
        );
    }
    for glyph in &quads.glyphs {
        push_quad(
            &mut vertices,
            glyph.quad,
            [
                glyph.tex_left,
                glyph.tex_top,
                glyph.tex_right,
                glyph.tex_bottom,
            ],
            1.0,
        );
    }
    // Then the overlays, which is what puts a panel in front of the text it
    // covers rather than behind it.
    for quad in &quads.overlay_backgrounds {
        push_quad(&mut vertices, *quad, [0.0; 4], 0.0);
    }
    for glyph in &quads.overlay_glyphs {
        push_quad(
            &mut vertices,
            glyph.quad,
            [
                glyph.tex_left,
                glyph.tex_top,
                glyph.tex_right,
                glyph.tex_bottom,
            ],
            1.0,
        );
    }
    // And the modals last of all: a palette card over the docks, its text
    // over the card.
    for quad in &quads.modal_backgrounds {
        push_quad(&mut vertices, *quad, [0.0; 4], 0.0);
    }
    for glyph in &quads.modal_glyphs {
        push_quad(
            &mut vertices,
            glyph.quad,
            [
                glyph.tex_left,
                glyph.tex_top,
                glyph.tex_right,
                glyph.tex_bottom,
            ],
            1.0,
        );
    }

    bytemuck::cast_slice(&vertices).to_vec()
}

/// How many vertices `build_vertices` produced.
pub fn vertex_count(quads: &FrameQuads) -> u32 {
    ((usize::from(quads.image.is_some())
        + quads.backgrounds.len()
        + quads.inline_images.len()
        + quads.glyphs.len()
        + quads.overlay_backgrounds.len()
        + quads.overlay_glyphs.len()
        + quads.modal_backgrounds.len()
        + quads.modal_glyphs.len())
        * 6) as u32
}

/// Everything needed to draw, once a device exists.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    blank_image: wgpu::Texture,
    format: wgpu::TextureFormat,
    /// Pictures programs put on the screen, by name. Uploaded once each; the
    /// name is the content, so a cached texture is never stale.
    inline_textures: std::cell::RefCell<std::collections::HashMap<String, wgpu::Texture>>,
}

impl Renderer {
    /// Build a renderer on an existing device.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("unterm-render"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("unterm-render bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // The background picture. Always bound, because a bind
                // group has to match its layout whether or not anything
                // is drawn from it -- a single transparent pixel stands in
                // when there is no picture.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("unterm-render pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("unterm-render pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 16,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                        wgpu::VertexAttribute {
                            offset: 32,
                            shader_location: 3,
                            format: wgpu::VertexFormat::Float32,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Straight alpha: a glyph's coverage decides how much of
                    // the foreground reaches the cell it sits on.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("unterm-render sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blank_image = upload_image(&device, &queue, 1, 1, &[0, 0, 0, 0]);

        Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            sampler,
            blank_image,
            format,
            inline_textures: Default::default(),
        }
    }

    /// Whether a picture's texture is already here.
    pub fn has_inline_image(&self, image: &str) -> bool {
        self.inline_textures.borrow().contains_key(image)
    }

    /// Upload a picture under its name.
    pub fn upload_inline_image(&self, image: &str, width: u32, height: u32, rgba: &[u8]) {
        let texture = upload_image(&self.device, &self.queue, width, height, rgba);
        self.inline_textures
            .borrow_mut()
            .insert(image.to_string(), texture);
    }

    /// Drop the textures of pictures no longer on any screen.
    pub fn retain_inline_images(&self, keep: impl Fn(&str) -> bool) {
        self.inline_textures.borrow_mut().retain(|name, _| keep(name));
    }

    pub fn inline_image_count(&self) -> usize {
        self.inline_textures.borrow().len()
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Upload the atlas as a single-channel coverage texture.
    pub fn upload_atlas(&self, atlas: &GlyphAtlas) -> wgpu::Texture {
        let size = wgpu::Extent3d {
            width: atlas.width() as u32,
            height: atlas.height() as u32,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("unterm-render atlas"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            atlas.coverage(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width() as u32),
                rows_per_image: Some(atlas.height() as u32),
            },
            size,
        );

        texture
    }

    /// Draw a frame into `target`.
    /// Upload a picture as an ordinary colour texture.
    ///
    /// Separate from the atlas, which stores coverage in one channel: a
    /// photograph in a coverage texture is a photograph of its own alpha.
    pub fn upload_image(&self, width: u32, height: u32, rgba: &[u8]) -> wgpu::Texture {
        upload_image(&self.device, &self.queue, width, height, rgba)
    }

    pub fn draw(
        &self,
        target: &wgpu::TextureView,
        width: u32,
        height: u32,
        quads: &FrameQuads,
        atlas_texture: &wgpu::Texture,
        image_texture: Option<&wgpu::Texture>,
        clear: [f32; 4],
    ) {
        let vertices = build_vertices(quads);
        let count = vertex_count(quads);

        let uniforms = Uniforms {
            viewport: [width as f32, height as f32],
            _padding: [0.0; 2],
        };
        let uniform_buffer = self.create_buffer(
            bytemuck::bytes_of(&uniforms),
            wgpu::BufferUsages::UNIFORM,
            "uniforms",
        );

        // An empty frame still has to clear, or the previous one stays on
        // screen.
        let vertex_buffer = self.create_buffer(
            if vertices.is_empty() {
                &[0u8; std::mem::size_of::<Vertex>()]
            } else {
                &vertices
            },
            wgpu::BufferUsages::VERTEX,
            "vertices",
        );

        let view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        // One transparent pixel when there is no picture: the binding has to
        // exist either way.
        let image_view = match image_texture {
            Some(texture) => texture.create_view(&wgpu::TextureViewDescriptor::default()),
            None => self
                .blank_image
                .create_view(&wgpu::TextureViewDescriptor::default()),
        };

        let bind_group = self.bind_group(&uniform_buffer, &view, &image_view);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("unterm-render encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("unterm-render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear[0] as f64,
                            g: clear[1] as f64,
                            b: clear[2] as f64,
                            a: clear[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            if count > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                if quads.inline_images.is_empty() {
                    pass.draw(0..count, 0..1);
                } else {
                    // Everything below the pictures, then each picture with
                    // its own texture in the picture slot, then the rest.
                    let below = ((usize::from(quads.image.is_some()) + quads.backgrounds.len())
                        * 6) as u32;
                    pass.draw(0..below, 0..1);
                    let textures = self.inline_textures.borrow();
                    let mut groups = Vec::with_capacity(quads.inline_images.len());
                    for image in &quads.inline_images {
                        let group = textures.get(&image.image).map(|texture| {
                            let image_view =
                                texture.create_view(&wgpu::TextureViewDescriptor::default());
                            self.bind_group(&uniform_buffer, &view, &image_view)
                        });
                        groups.push(group);
                    }
                    for (index, group) in groups.iter().enumerate() {
                        // Not uploaded yet: skipped this frame, drawn the next.
                        if let Some(group) = group {
                            let first = below + index as u32 * 6;
                            pass.set_bind_group(0, group, &[]);
                            pass.draw(first..first + 6, 0..1);
                        }
                    }
                    let above = below + quads.inline_images.len() as u32 * 6;
                    pass.set_bind_group(0, &bind_group, &[]);
                    if above < count {
                        pass.draw(above..count, 0..1);
                    }
                }
            }
        }

        self.queue.submit(Some(encoder.finish()));
    }

    fn bind_group(
        &self,
        uniform_buffer: &wgpu::Buffer,
        atlas_view: &wgpu::TextureView,
        image_view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unterm-render bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(image_view),
                },
            ],
        })
    }

    fn create_buffer(
        &self,
        contents: &[u8],
        usage: wgpu::BufferUsages,
        label: &str,
    ) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
    }
}

fn upload_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width: width.max(1),
        height: height.max(1),
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unterm-render image"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
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
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.width * 4),
            rows_per_image: Some(size.height),
        },
        size,
    );
    texture
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quads::{FrameQuads, Quad};

    #[test]
    fn a_quad_becomes_two_triangles() {
        let mut quads = FrameQuads::default();
        quads.backgrounds.push(Quad {
            left: 0.0,
            top: 0.0,
            width: 10.0,
            height: 10.0,
            color: [1.0, 0.0, 0.0, 1.0],
        });

        assert_eq!(vertex_count(&quads), 6);
        assert_eq!(
            build_vertices(&quads).len(),
            6 * std::mem::size_of::<Vertex>()
        );
    }

    #[test]
    fn backgrounds_come_before_glyphs() {
        let mut quads = FrameQuads::default();
        quads.backgrounds.push(Quad {
            left: 0.0,
            top: 0.0,
            width: 1.0,
            height: 1.0,
            color: [1.0, 0.0, 0.0, 1.0],
        });
        quads.glyphs.push(crate::quads::GlyphQuad {
            quad: Quad {
                left: 0.0,
                top: 0.0,
                width: 1.0,
                height: 1.0,
                color: [0.0, 1.0, 0.0, 1.0],
            },
            tex_left: 0.0,
            tex_top: 0.0,
            tex_right: 1.0,
            tex_bottom: 1.0,
        });

        let bytes = build_vertices(&quads);
        let vertices: &[Vertex] = bytemuck::cast_slice(&bytes);

        // Text has to land on top of its own cell, and ordering is how that
        // happens without a depth buffer or a second pass.
        assert_eq!(vertices[0].has_texture, 0.0);
        assert_eq!(vertices[6].has_texture, 1.0);
    }

    #[test]
    fn an_empty_frame_produces_no_vertices() {
        let quads = FrameQuads::default();

        assert_eq!(vertex_count(&quads), 0);
        assert!(build_vertices(&quads).is_empty());
    }
}
