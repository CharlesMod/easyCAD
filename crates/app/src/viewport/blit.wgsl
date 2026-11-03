// Blit of the offscreen viewport image into egui's render pass.

fn to_lin(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// ---------------------------------------------------------------- blit to egui

struct BlitOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var blit_tex: texture_2d<f32>;
@group(0) @binding(1) var blit_samp: sampler;

@vertex
fn vs_blit(@builtin(vertex_index) vi: u32) -> BlitOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: BlitOut;
    o.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs_blit(i: BlitOut) -> @location(0) vec4<f32> {
    return textureSample(blit_tex, blit_samp, i.uv);
}

// Same, for an sRGB render target (the hardware re-encodes on write).
@fragment
fn fs_blit_srgb(i: BlitOut) -> @location(0) vec4<f32> {
    let c = textureSample(blit_tex, blit_samp, i.uv);
    return vec4<f32>(to_lin(c.rgb), c.a);
}
