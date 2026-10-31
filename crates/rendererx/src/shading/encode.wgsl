// With MSAA the frame is drawn and resolved in an sRGB texture, so the hardware
// averages samples in linear light (resolving sRGB-encoded bytes darkens edges). This
// pass copies the resolved frame into the host's Rgba8Unorm target, encoding by hand
// so it works on GL/GLES too (no sRGB view formats needed).

@group(0) @binding(0) var frame: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the target.
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = x * 12.92;
    let hi = 1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, x <= vec3<f32>(0.0031308));
}

@fragment
fn fs_main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    // textureLoad of an sRGB texture returns linear values. They are premultiplied
    // (the clear colour is, and resolving averages coverage into alpha); the target is
    // straight alpha, so divide it back out. Fully transparent pixels have no colour.
    let c = textureLoad(frame, vec2<i32>(p.xy), 0);
    let straight = select(vec3<f32>(0.0), c.rgb / max(c.a, 1e-6), c.a > 0.0);
    return vec4<f32>(linear_to_srgb(straight), c.a);
}
