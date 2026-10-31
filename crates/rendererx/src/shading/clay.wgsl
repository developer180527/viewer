// Clay: the shading every model gets first (docs/3d-renderer.md §7.3), and the only
// one STL/PLY files need. Soft sky/ground ambient, a key light that follows the
// camera, a faint rim; the base colour map is multiplied in.

fn shade_clay(in: VOut, front_facing: bool) -> vec3<f32> {
    // Sampled first, in uniform control flow (derivatives for mip selection).
    let albedo = in.color.rgb * textureSample(base_tex, mat_sampler, in.uv).rgb;
    let n = facing_normal(in, front_facing);
    let to_eye = normalize(u.eye.xyz - in.world);
    let sky = vec3<f32>(0.62, 0.66, 0.72);
    let ground = vec3<f32>(0.30, 0.27, 0.25);
    let ambient = mix(ground, sky, n.y * 0.5 + 0.5);
    let key = max(dot(n, normalize(u.light.xyz)), 0.0);
    let rim = pow(1.0 - max(dot(n, to_eye), 0.0), 3.0) * 0.12;
    return albedo * (ambient * 0.45 + key * 0.8) + vec3<f32>(rim);
}

@fragment
fn fs_clay(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    return vec4<f32>(linear_to_srgb(shade_clay(in, front)), 1.0);
}

@fragment
fn fs_clay_linear(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    return vec4<f32>(shade_clay(in, front), 1.0);
}
