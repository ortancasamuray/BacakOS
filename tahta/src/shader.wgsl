struct Uniforms {
    // Framebuffer size in physical pixels, used to map top-left pixel
    // coordinates into GPU NDC space ([-1, 1], Y-down -> Y-up).
    screen_size: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let ndc_x = (in.position.x / uniforms.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.position.y / uniforms.screen_size.y) * 2.0;
    out.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}

// --- Textured quad (PDF page background) ------------------------------

struct TexVertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct TexVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(1) @binding(0)
var page_texture: texture_2d<f32>;
@group(1) @binding(1)
var page_sampler: sampler;

@vertex
fn vs_tex(in: TexVertexInput) -> TexVertexOutput {
    var out: TexVertexOutput;
    let ndc_x = (in.position.x / uniforms.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.position.y / uniforms.screen_size.y) * 2.0;
    out.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_tex(in: TexVertexOutput) -> @location(0) vec4<f32> {
    return textureSample(page_texture, page_sampler, in.uv);
}

// --- Magnifier lens (real pixel zoom of the "content" texture) --------
//
// Vertex stage reuses `vs_tex` (same quad-in-pixel-space -> clip-space
// transform); this only adds a fragment stage that, for each pixel of a
// quad covering the lens's bounding box, maps back to a source pixel in
// `content_texture` — `(frag_pos - center) / zoom + center` — discarding
// outside the circle or outside the texture. See `magnifier.rs` for why
// this replaced an earlier per-stroke vector-zoom approach.

struct MagnifierUniforms {
    center: vec2<f32>,
    radius: f32,
    zoom: f32,
};

@group(1) @binding(0)
var content_texture: texture_2d<f32>;
@group(1) @binding(1)
var content_sampler: sampler;
@group(2) @binding(0)
var<uniform> magnifier: MagnifierUniforms;

// --- Real-font text (text box, `font_atlas.rs`) ------------------------
//
// A single-channel coverage atlas (`R8Unorm`, bound at the same group(1)
// shape as `page_texture`/`content_texture` above) sampled as alpha and
// tinted per-vertex — unlike the other textured quads, glyph quads vary
// in size/position per character, so this needs its own vertex color
// instead of one uniform tint.

struct GlyphVertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct GlyphVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_glyph(in: GlyphVertexInput) -> GlyphVertexOutput {
    var out: GlyphVertexOutput;
    let ndc_x = (in.position.x / uniforms.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.position.y / uniforms.screen_size.y) * 2.0;
    out.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_glyph(in: GlyphVertexOutput) -> @location(0) vec4<f32> {
    let coverage = textureSample(page_texture, page_sampler, in.uv).r;
    return vec4<f32>(in.color.rgb, in.color.a * coverage);
}

@fragment
fn fs_magnifier(in: TexVertexOutput) -> @location(0) vec4<f32> {
    let frag_xy = in.clip_position.xy;
    let d = frag_xy - magnifier.center;
    if (length(d) > magnifier.radius) {
        discard;
    }
    let source_xy = d / magnifier.zoom + magnifier.center;
    let uv = source_xy / uniforms.screen_size;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        discard;
    }
    return textureSample(content_texture, content_sampler, uv);
}
