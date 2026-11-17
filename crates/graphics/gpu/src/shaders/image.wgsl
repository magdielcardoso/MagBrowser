// Mag image pipeline (WGSL): samples an RGBA texture with nearest filtering.
struct VertexIn {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct Uniforms {
    viewport: vec4<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var image_texture: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    var out: VertexOut;
    let ndc_x = (input.position.x / uniforms.viewport.z) * 2.0 - 1.0;
    let ndc_y = 1.0 - (input.position.y / uniforms.viewport.w) * 2.0;
    out.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = input.uv;
    return out;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(image_texture, image_sampler, input.uv);
}
