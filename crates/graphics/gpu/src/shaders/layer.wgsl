// Mag multi-layer compositor (WGSL).
//
// One full-screen draw composites every layer, back to front. Each layer owns a
// slice of `layer_textures`; its inverse transform maps a device pixel back to
// layer-local pixels, and `blend_layer` combines the premultiplied sample with
// the accumulated result. The equations mirror `BlendMode::blend` in layer.rs.

struct Globals {
    // x = surface width, y = surface height, z = layer count, w = unused.
    viewport: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

// 80 bytes per layer, matching the CPU packing in native.rs.
struct LayerParams {
    inv_transform: mat4x4<f32>, // device pixels -> layer-local pixels
    src_size: vec2<f32>,        // width and height of the layer content
    opacity: f32,
    blend: u32,
};

@group(0) @binding(1) var<storage, read> layers: array<LayerParams>;

@group(1) @binding(0) var layer_textures: texture_2d_array<f32>;
@group(1) @binding(1) var layer_sampler: sampler;

struct VertexIn {
    @location(0) position: vec2<f32>, // unit quad corner, 0..1
    @location(1) uv: vec2<f32>,
};

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    let pixel = input.position * globals.viewport.xy;
    let ndc_x = (pixel.x / globals.viewport.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (pixel.y / globals.viewport.y) * 2.0;
    var out: VertexOut;
    out.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = input.uv;
    return out;
}

fn straight(channel: f32, alpha: f32) -> f32 {
    if (alpha > 0.000001) {
        return clamp(channel / alpha, 0.0, 1.0);
    }
    return 0.0;
}

fn overlay_channel(backdrop: f32, source: f32) -> f32 {
    // select(false_value, true_value, condition)
    return select(
        2.0 * backdrop * source,
        1.0 - 2.0 * (1.0 - backdrop) * (1.0 - source),
        backdrop > 0.5
    );
}

fn blend_layer(backdrop: vec4<f32>, source: vec4<f32>, mode: u32) -> vec4<f32> {
    if (mode == 4u) {
        return source; // replace
    }
    let alpha_s = clamp(source.a, 0.0, 1.0);
    let alpha_b = clamp(backdrop.a, 0.0, 1.0);
    let alpha_o = clamp(alpha_s + alpha_b - alpha_s * alpha_b, 0.0, 1.0);

    let cs = vec3<f32>(
        straight(source.r, alpha_s),
        straight(source.g, alpha_s),
        straight(source.b, alpha_s)
    );
    let cb = vec3<f32>(
        straight(backdrop.r, alpha_b),
        straight(backdrop.g, alpha_b),
        straight(backdrop.b, alpha_b)
    );

    var blended: vec3<f32>;
    switch (mode) {
        case 1u: {
            blended = cb * cs; // multiply
        }
        case 2u: {
            blended = cb + cs - cb * cs; // screen
        }
        case 3u: {
            blended = vec3<f32>(
                overlay_channel(cb.r, cs.r),
                overlay_channel(cb.g, cs.g),
                overlay_channel(cb.b, cs.b)
            ); // overlay
        }
        default: {
            blended = cs; // normal / source-over
        }
    }

    let weight_blended = alpha_s * alpha_b;
    let weight_source = alpha_s * (1.0 - alpha_b);
    let weight_backdrop = (1.0 - alpha_s) * alpha_b;
    let co = blended * weight_blended
        + cs * weight_source
        + cb * weight_backdrop;
    return vec4<f32>(clamp(co, vec3<f32>(0.0), vec3<f32>(1.0)), alpha_o);
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let surface = input.uv * globals.viewport.xy;
    let count = u32(globals.viewport.z);
    var accum = vec4<f32>(0.0, 0.0, 0.0, 0.0);

    for (var i: u32 = 0u; i < count; i = i + 1u) {
        let layer = layers[i];
        let local = (layer.inv_transform * vec4<f32>(surface, 0.0, 1.0)).xy;
        if (local.x < 0.0 || local.y < 0.0
            || local.x >= layer.src_size.x || local.y >= layer.src_size.y) {
            continue;
        }
        let uv = local / globals.viewport.xy;
        let texel = textureSampleLevel(layer_textures, layer_sampler, uv, i, 0.0);
        let source = vec4<f32>(texel.rgb * texel.a, texel.a) * layer.opacity;
        accum = blend_layer(accum, source, layer.blend);
    }

    // The swap chain is opaque: premultiplied colour over a black backdrop.
    return vec4<f32>(clamp(accum.rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
