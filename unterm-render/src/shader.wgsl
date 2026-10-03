// Textured quads for terminal text.
//
// Three draws share this shader. Backgrounds carry `has_texture = 0` and are
// filled with their colour; glyphs carry 1 and are tinted by it, with the
// atlas supplying only coverage in the alpha channel; a background picture
// carries 2 and comes from its own texture in full colour, with the quad's
// alpha as its opacity. Keeping all three in one pipeline means one buffer,
// one bind group and one pass per frame.

struct Uniforms {
    // Pixels to clip space, so positions can be authored in pixels.
    viewport: vec2<f32>,
    _padding: vec2<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var atlas_texture: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;
@group(0) @binding(3) var image_texture: texture_2d<f32>;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) tex_coord: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) has_texture: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) has_texture: f32,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    // Pixels have y growing downward; clip space has it growing up.
    let x = (input.position.x / uniforms.viewport.x) * 2.0 - 1.0;
    let y = 1.0 - (input.position.y / uniforms.viewport.y) * 2.0;
    out.clip_position = vec4<f32>(x, y, 0.0, 1.0);
    out.tex_coord = input.tex_coord;
    out.color = input.color;
    out.has_texture = input.has_texture;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if (input.has_texture < 0.5) {
        return input.color;
    }
    if (input.has_texture > 1.5) {
        // A picture, in its own colours. The quad's alpha is how much of it
        // shows through -- a background at full strength is a background you
        // cannot read text on.
        // The quad's colour tints it: white for a picture shown as it is,
        // grey for one in an inactive pane.
        let texel = textureSample(image_texture, atlas_sampler, input.tex_coord);
        return vec4<f32>(texel.rgb * input.color.rgb, texel.a * input.color.a);
    }
    // The atlas stores coverage, not colour: the glyph takes the cell's
    // foreground and the texture decides only how much of it lands.
    let coverage = textureSample(atlas_texture, atlas_sampler, input.tex_coord).r;
    return vec4<f32>(input.color.rgb, input.color.a * coverage);
}
