//! Draw to an off-screen texture and read the pixels back.
//!
//! The point of these is that they fail on a wrong result rather than a wrong
//! *looking* result. Every bug this layer produced before was of the second
//! kind: it compiled, it submitted, and the window showed something else.
//!
//! They skip when no adapter is available rather than failing, so a machine
//! without a GPU still runs the rest of the suite.

use unterm_render::atlas::{GlyphAtlas, GlyphKey};
use unterm_render::gpu::Renderer;
use unterm_render::quads::{FrameQuads, GlyphQuad, Quad};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

struct Offscreen {
    renderer: Renderer,
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

/// One device for the whole suite.
///
/// Each test used to open its own, so five ran at once and five were torn down
/// at once. Drivers do not enjoy that: under a full workspace run it took the
/// test binary down with an access violation, roughly one run in three, in a
/// place that looked nothing like the code that caused it. One device, opened
/// once, is also what an application does.
static GPU: std::sync::OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = std::sync::OnceLock::new();

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    GPU.get_or_init(|| {
        let instance = wgpu::Instance::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    })
    .clone()
}

fn offscreen(width: u32, height: u32) -> Option<Offscreen> {
    let (device, queue) = gpu()?;

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });

    Some(Offscreen {
        renderer: Renderer::new(device, queue, FORMAT),
        texture,
        width,
        height,
    })
}

impl Offscreen {
    /// Read the whole target back as RGBA.
    fn read_pixels(&self) -> Vec<u8> {
        // Copies out of a texture need rows padded to 256 bytes, so the buffer
        // is wider than the image and has to be un-padded on the way out.
        let unpadded = self.width * 4;
        let padded = unpadded.div_ceil(256) * 256;

        let buffer = self
            .renderer
            .device()
            .create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size: (padded * self.height) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

        let mut encoder =
            self.renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("readback encoder"),
                });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.renderer.queue().submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.renderer.device().poll(wgpu::PollType::Wait);

        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((unpadded * self.height) as usize);
        for row in 0..self.height {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        buffer.unmap();
        pixels
    }

    fn pixel(&self, pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
        let index = ((y * self.width + x) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    }

    fn view(&self) -> wgpu::TextureView {
        self.texture
            .create_view(&wgpu::TextureViewDescriptor::default())
    }
}

fn empty_atlas(renderer: &Renderer) -> wgpu::Texture {
    renderer.upload_atlas(&GlyphAtlas::new(4, 4))
}

#[test]
fn offscreen_an_empty_frame_is_the_clear_colour() {
    let Some(target) = offscreen(8, 8) else {
        return;
    };
    let atlas = empty_atlas(&target.renderer);

    target.renderer.draw(
        &target.view(),
        target.width,
        target.height,
        &FrameQuads::default(),
        &atlas,
        None,
        [0.0, 0.0, 1.0, 1.0],
    );

    let pixels = target.read_pixels();
    // A frame with nothing in it still has to clear, or the previous one stays
    // on screen.
    assert_eq!(target.pixel(&pixels, 4, 4), [0, 0, 255, 255]);
}

#[test]
fn offscreen_a_background_quad_lands_where_it_was_put() {
    let Some(target) = offscreen(16, 16) else {
        return;
    };
    let atlas = empty_atlas(&target.renderer);
    let mut quads = FrameQuads::default();
    quads.backgrounds.push(Quad {
        left: 0.0,
        top: 0.0,
        width: 8.0,
        height: 8.0,
        color: [1.0, 0.0, 0.0, 1.0],
    });

    target.renderer.draw(
        &target.view(),
        target.width,
        target.height,
        &quads,
        &atlas,
        None,
        [0.0, 0.0, 0.0, 1.0],
    );

    let pixels = target.read_pixels();
    // Inside the quad is red; outside it is still the clear colour. Getting
    // the pixel-to-clip-space mapping wrong shows up here as a quad in the
    // wrong quadrant or flipped vertically.
    assert_eq!(target.pixel(&pixels, 2, 2), [255, 0, 0, 255]);
    assert_eq!(target.pixel(&pixels, 12, 12), [0, 0, 0, 255]);
}

#[test]
fn offscreen_the_top_left_of_a_quad_is_the_top_left_of_the_image() {
    let Some(target) = offscreen(16, 16) else {
        return;
    };
    let atlas = empty_atlas(&target.renderer);
    let mut quads = FrameQuads::default();
    quads.backgrounds.push(Quad {
        left: 0.0,
        top: 0.0,
        width: 4.0,
        height: 4.0,
        color: [0.0, 1.0, 0.0, 1.0],
    });

    target.renderer.draw(
        &target.view(),
        target.width,
        target.height,
        &quads,
        &atlas,
        None,
        [0.0, 0.0, 0.0, 1.0],
    );

    let pixels = target.read_pixels();
    // Pixels count down the screen; clip space counts up. A sign error here
    // draws the whole terminal upside down.
    assert_eq!(target.pixel(&pixels, 1, 1), [0, 255, 0, 255]);
    assert_eq!(target.pixel(&pixels, 1, 14), [0, 0, 0, 255]);
}

#[test]
fn offscreen_a_glyph_is_tinted_by_its_colour_and_shaped_by_the_atlas() {
    let Some(target) = offscreen(16, 16) else {
        return;
    };

    // An atlas whose only glyph is fully opaque, so coverage is unambiguous.
    let mut atlas = GlyphAtlas::new(8, 8);
    let slot = atlas.insert(
        GlyphKey {
            stack: 0,
            face: 0,
            glyph_index: 1,
            pixel_size: 16,
        },
        &unterm_engine::next_core::font_raster::RasterizedGlyph {
            coverage: vec![255; 4 * 4],
            width: 4,
            height: 4,
            bearing_x: 0,
            bearing_y: 0,
            advance_x: 4,
        },
    );
    let atlas_texture = target.renderer.upload_atlas(&atlas);

    let mut quads = FrameQuads::default();
    quads.glyphs.push(GlyphQuad {
        quad: Quad {
            left: 0.0,
            top: 0.0,
            width: 4.0,
            height: 4.0,
            color: [1.0, 1.0, 0.0, 1.0],
        },
        tex_left: slot.x as f32 / atlas.width() as f32,
        tex_top: slot.y as f32 / atlas.height() as f32,
        tex_right: (slot.x + slot.width) as f32 / atlas.width() as f32,
        tex_bottom: (slot.y + slot.height) as f32 / atlas.height() as f32,
    });

    target.renderer.draw(
        &target.view(),
        target.width,
        target.height,
        &quads,
        &atlas_texture,
        None,
        [0.0, 0.0, 0.0, 1.0],
    );

    let pixels = target.read_pixels();
    let drawn = target.pixel(&pixels, 1, 1);
    // The atlas carries coverage, not colour: the glyph comes out in the
    // colour the cell asked for. Sampling the wrong channel would give black
    // or white here instead of yellow.
    assert!(
        drawn[0] > 200 && drawn[1] > 200,
        "expected yellow, got {drawn:?}"
    );
    assert!(drawn[2] < 60, "expected no blue, got {drawn:?}");
}

#[test]
fn offscreen_a_glyph_draws_over_its_own_background() {
    let Some(target) = offscreen(16, 16) else {
        return;
    };

    let mut atlas = GlyphAtlas::new(8, 8);
    let slot = atlas.insert(
        GlyphKey {
            stack: 0,
            face: 0,
            glyph_index: 1,
            pixel_size: 16,
        },
        &unterm_engine::next_core::font_raster::RasterizedGlyph {
            coverage: vec![255; 4 * 4],
            width: 4,
            height: 4,
            bearing_x: 0,
            bearing_y: 0,
            advance_x: 4,
        },
    );
    let atlas_texture = target.renderer.upload_atlas(&atlas);

    let mut quads = FrameQuads::default();
    quads.backgrounds.push(Quad {
        left: 0.0,
        top: 0.0,
        width: 4.0,
        height: 4.0,
        color: [1.0, 0.0, 0.0, 1.0],
    });
    quads.glyphs.push(GlyphQuad {
        quad: Quad {
            left: 0.0,
            top: 0.0,
            width: 4.0,
            height: 4.0,
            color: [0.0, 0.0, 1.0, 1.0],
        },
        tex_left: slot.x as f32 / atlas.width() as f32,
        tex_top: slot.y as f32 / atlas.height() as f32,
        tex_right: (slot.x + slot.width) as f32 / atlas.width() as f32,
        tex_bottom: (slot.y + slot.height) as f32 / atlas.height() as f32,
    });

    target.renderer.draw(
        &target.view(),
        target.width,
        target.height,
        &quads,
        &atlas_texture,
        None,
        [0.0, 0.0, 0.0, 1.0],
    );

    let pixels = target.read_pixels();
    let drawn = target.pixel(&pixels, 1, 1);
    // Ordering is what puts text on top of its cell without a depth buffer.
    assert!(drawn[2] > 200, "glyph should be on top, got {drawn:?}");
}

/// A rounded panel reaches the framebuffer with its corners actually missing.
///
/// The geometry is checked in the module's own tests; this is the other half:
/// that the rows it produces survive the vertex builder and the pass, and land
/// as pixels rather than as a rectangle with a seam in it.
#[test]
fn offscreen_a_rounded_panel_has_its_corners_taken_off() {
    let Some(target) = offscreen(40, 24) else {
        return;
    };
    let atlas = empty_atlas(&target.renderer);
    let mut quads = FrameQuads::default();
    quads.backgrounds.extend(unterm_render::rounded::panel(
        0.0,
        0.0,
        40.0,
        24.0,
        6.0,
        [1.0, 0.0, 0.0, 1.0],
    ));

    target.renderer.draw(
        &target.view(),
        target.width,
        target.height,
        &quads,
        &atlas,
        None,
        [0.0, 0.0, 0.0, 1.0],
    );

    let pixels = target.read_pixels();
    const RED: [u8; 4] = [255, 0, 0, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 255];

    // The four corners are not drawn.
    for (x, y) in [(0, 0), (39, 0), (0, 23), (39, 23)] {
        assert_eq!(
            target.pixel(&pixels, x, y),
            CLEAR,
            "the corner at {x},{y} is square"
        );
    }
    // The middle is solid, with no gap between the rows the corners are made
    // of and the rectangle between them.
    for y in 0..24 {
        for x in 8..32 {
            assert_eq!(target.pixel(&pixels, x, y), RED, "a hole at {x},{y}");
        }
    }
    // And each edge is reached between its corners.
    assert_eq!(target.pixel(&pixels, 20, 0), RED, "no top edge");
    assert_eq!(target.pixel(&pixels, 20, 23), RED, "no bottom edge");
    assert_eq!(target.pixel(&pixels, 0, 12), RED, "no left edge");
    assert_eq!(target.pixel(&pixels, 39, 12), RED, "no right edge");
}

/// A background picture reaches the framebuffer, and text lands on top of it.
///
/// The two claims are separate: the picture comes from its own texture in full
/// colour, and everything else still draws over it. A picture that covered the
/// terminal would be a wallpaper rather than a background.
#[test]
fn offscreen_a_background_picture_is_drawn_under_everything() {
    let Some(target) = offscreen(8, 8) else {
        return;
    };
    let atlas = empty_atlas(&target.renderer);
    // Four pixels of solid blue, so any part of the quad samples the same
    // colour and the test is about where it lands rather than about filtering.
    let blue: Vec<u8> = (0..4).flat_map(|_| [0u8, 0, 255, 255]).collect();
    let picture = target.renderer.upload_image(2, 2, &blue);

    let mut quads = FrameQuads::default();
    quads.image = Some(unterm_render::quads::GlyphQuad {
        quad: Quad {
            left: 0.0,
            top: 0.0,
            width: 8.0,
            height: 8.0,
            color: [1.0, 1.0, 1.0, 1.0],
        },
        tex_left: 0.0,
        tex_top: 0.0,
        tex_right: 1.0,
        tex_bottom: 1.0,
    });
    // And something opaque over part of it.
    quads.backgrounds.push(Quad {
        left: 0.0,
        top: 0.0,
        width: 4.0,
        height: 8.0,
        color: [1.0, 0.0, 0.0, 1.0],
    });

    target.renderer.draw(
        &target.view(),
        target.width,
        target.height,
        &quads,
        &atlas,
        Some(&picture),
        [0.0, 0.0, 0.0, 1.0],
    );

    let pixels = target.read_pixels();
    let right = target.pixel(&pixels, 6, 4);
    assert!(
        right[2] > 200 && right[0] < 60,
        "the picture is not there: {right:?}"
    );
    let left = target.pixel(&pixels, 1, 4);
    assert!(
        left[0] > 200 && left[2] < 60,
        "the picture drew over what is in front of it: {left:?}"
    );
}

/// And an opacity below one lets the clear colour through, which is what makes
/// text readable on top of a photograph.
#[test]
fn offscreen_a_background_picture_is_dimmed_by_its_opacity() {
    let Some(target) = offscreen(4, 4) else {
        return;
    };
    let atlas = empty_atlas(&target.renderer);
    let white: Vec<u8> = (0..4).flat_map(|_| [255u8, 255, 255, 255]).collect();
    let picture = target.renderer.upload_image(2, 2, &white);

    let mut quads = FrameQuads::default();
    quads.image = Some(unterm_render::quads::GlyphQuad {
        quad: Quad {
            left: 0.0,
            top: 0.0,
            width: 4.0,
            height: 4.0,
            color: [1.0, 1.0, 1.0, 0.25],
        },
        tex_left: 0.0,
        tex_top: 0.0,
        tex_right: 1.0,
        tex_bottom: 1.0,
    });

    target.renderer.draw(
        &target.view(),
        target.width,
        target.height,
        &quads,
        &atlas,
        Some(&picture),
        [0.0, 0.0, 0.0, 1.0],
    );

    let pixels = target.read_pixels();
    let shown = target.pixel(&pixels, 2, 2);
    assert!(
        shown[0] > 10 && shown[0] < 245,
        "a quarter-strength white picture came out at {shown:?}"
    );
}

fn inline(image: &str, left: f32, width: f32, color: [f32; 4]) -> unterm_render::quads::InlineImageQuad {
    unterm_render::quads::InlineImageQuad {
        glyph: unterm_render::quads::GlyphQuad {
            quad: Quad {
                left,
                top: 0.0,
                width,
                height: 8.0,
                color,
            },
            tex_left: 0.0,
            tex_top: 0.0,
            tex_right: 1.0,
            tex_bottom: 1.0,
        },
        image: image.to_string(),
        pane: 1,
    }
}

/// Pictures a program put on the screen: each from its own texture, over
/// the cell backgrounds, and one whose texture has not arrived yet is
/// simply not drawn rather than drawn from another picture's.
#[test]
fn offscreen_inline_pictures_draw_from_their_own_textures_over_backgrounds() {
    let Some(target) = offscreen(12, 8) else {
        return;
    };
    let atlas = empty_atlas(&target.renderer);
    let red: Vec<u8> = (0..4).flat_map(|_| [255u8, 0, 0, 255]).collect();
    let green: Vec<u8> = (0..4).flat_map(|_| [0u8, 255, 0, 255]).collect();
    target.renderer.upload_inline_image("red", 2, 2, &red);
    target.renderer.upload_inline_image("green", 2, 2, &green);
    assert!(target.renderer.has_inline_image("red"));

    let mut quads = FrameQuads::default();
    // A blue cell background under the first picture.
    quads.backgrounds.push(Quad {
        left: 0.0,
        top: 0.0,
        width: 4.0,
        height: 8.0,
        color: [0.0, 0.0, 1.0, 1.0],
    });
    quads.inline_images.push(inline("red", 0.0, 4.0, [1.0; 4]));
    quads.inline_images.push(inline("green", 4.0, 4.0, [1.0; 4]));
    quads.inline_images.push(inline("not-uploaded", 8.0, 4.0, [1.0; 4]));

    target.renderer.draw(
        &target.view(),
        target.width,
        target.height,
        &quads,
        &atlas,
        None,
        [0.0, 0.0, 0.0, 1.0],
    );

    let pixels = target.read_pixels();
    let left = target.pixel(&pixels, 2, 4);
    assert!(left[0] > 200 && left[2] < 60, "red picture over the blue cell: {left:?}");
    let middle = target.pixel(&pixels, 6, 4);
    assert!(middle[1] > 200 && middle[0] < 60, "green picture from its own texture: {middle:?}");
    let right = target.pixel(&pixels, 10, 4);
    assert!(right[0] < 30 && right[1] < 30, "a missing texture draws nothing: {right:?}");

    // Dropping what is not shown keeps only what was asked for.
    target.renderer.retain_inline_images(|name| name == "red");
    assert!(target.renderer.has_inline_image("red"));
    assert!(!target.renderer.has_inline_image("green"));
}

/// An inactive pane dims its pictures with it: the quad's colour tints.
#[test]
fn offscreen_an_inline_picture_is_tinted_by_its_quad() {
    let Some(target) = offscreen(4, 8) else {
        return;
    };
    let atlas = empty_atlas(&target.renderer);
    let white: Vec<u8> = (0..4).flat_map(|_| [255u8, 255, 255, 255]).collect();
    target.renderer.upload_inline_image("white", 2, 2, &white);

    let mut quads = FrameQuads::default();
    quads.inline_images.push(inline("white", 0.0, 4.0, [0.5, 0.5, 0.5, 1.0]));
    target.renderer.draw(
        &target.view(),
        target.width,
        target.height,
        &quads,
        &atlas,
        None,
        [0.0, 0.0, 0.0, 1.0],
    );

    let pixels = target.read_pixels();
    let shown = target.pixel(&pixels, 2, 4);
    assert!(
        shown[0] > 40 && shown[0] < 230,
        "half-tinted white came out at {shown:?}"
    );
}
