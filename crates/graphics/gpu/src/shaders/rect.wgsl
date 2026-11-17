// Mag rectangle pipeline (WGSL). Used by the hardware backend; the software
// device reads the display list directly, so both paths have identical input.
struct VertexIn {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct Uniforms {
    // x, y, width, height of the viewport in device pixels.
    viewport: vec4<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    var out: VertexOut;
    let ndc_x = (input.position.x / uniforms.viewport.z) * 2.0 - 1.0;
    let ndc_y = 1.0 - (input.position.y / uniforms.viewport.w) * 2.0;
    out.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = input.uv;
    out.color = input.color;
    return out;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    return input.color;
}
