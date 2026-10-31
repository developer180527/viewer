// `pbr`: glTF metallic-roughness (docs/3d-renderer.md §8.3). GGX specular with
// height-correlated Smith visibility and Schlick Fresnel from the key light, plus a
// procedural sky for ambient light and reflections (split-sum with an analytic
// environment BRDF), then Khronos PBR Neutral tone mapping.
//
// `unlit`: KHR_materials_unlit, the colour as authored, no lighting.
//
// Normal maps use a tangent frame built from screen-space derivatives (as the Khronos
// sample viewer does without tangents), so files without tangents still get them.

const PI: f32 = 3.14159265;
const KEY_LIGHT: f32 = 2.6;
const SKY_LIGHT: f32 = 0.9;

struct Surface {
    albedo: vec3<f32>,
    alpha: f32,
    metallic: f32,
    roughness: f32,
    occlusion: f32,
    emissive: vec3<f32>,
    normal: vec3<f32>,
}

/// The sky everything is lit by: a soft zenith, a bright horizon, a darker ground.
fn sky(dir: vec3<f32>) -> vec3<f32> {
    let zenith = vec3<f32>(0.55, 0.62, 0.75);
    let horizon = vec3<f32>(0.95, 0.93, 0.88);
    let ground = vec3<f32>(0.22, 0.20, 0.18);
    let up = clamp(dir.y, -1.0, 1.0);
    let above = mix(horizon, zenith, pow(max(up, 0.0), 0.6));
    return select(mix(horizon * 0.6, ground, min(-up * 4.0, 1.0)), above, up >= 0.0);
}

/// Diffuse light from the whole sky on a surface facing `n` (a cosine-weighted
/// average, approximated by blending the sky's up, side and down colours).
fn sky_irradiance(n: vec3<f32>) -> vec3<f32> {
    let up = sky(vec3<f32>(0.0, 1.0, 0.0));
    let side = sky(normalize(vec3<f32>(1.0, 0.05, 0.0)));
    let down = sky(vec3<f32>(0.0, -1.0, 0.0));
    let y = n.y;
    return select(mix(side, down, -y), mix(side, up, y), y >= 0.0) * 0.8 + side * 0.2;
}

fn surface(in: VOut, front_facing: bool) -> Surface {
    // Every map is sampled up front, in uniform control flow.
    let base = textureSample(base_tex, mat_sampler, in.uv);
    let mr = textureSample(metal_rough_tex, mat_sampler, in.uv);
    let nt = textureSample(normal_tex, mat_sampler, in.uv).xyz * 2.0 - 1.0;
    let occ = textureSample(occlusion_tex, mat_sampler, in.uv).r;
    let emis = textureSample(emissive_tex, mat_sampler, in.uv).rgb;
    let dp_dx = dpdx(in.world);
    let dp_dy = dpdy(in.world);
    let duv_dx = dpdx(in.uv);
    let duv_dy = dpdy(in.uv);

    var s: Surface;
    s.albedo = in.color.rgb * base.rgb;
    s.alpha = in.color.a * base.a;
    s.metallic = clamp(mat.params.x * mr.b, 0.0, 1.0);
    s.roughness = clamp(mat.params.y * mr.g, 0.045, 1.0);
    s.occlusion = mix(1.0, occ, mat.params.z);
    s.emissive = mat.emissive.rgb * emis;

    let ng = facing_normal(in, front_facing);
    // Tangent along +u from the uv derivatives; skipped where uvs don't vary (no uvs,
    // or a degenerate mapping), which leaves the geometric normal.
    let det = duv_dx.x * duv_dy.y - duv_dy.x * duv_dx.y;
    let safe = select(det, 1.0, abs(det) < 1e-12);
    let t_raw = (duv_dy.y * dp_dx - duv_dx.y * dp_dy) / safe;
    // World direction of increasing v. Comparing against it (rather than the sign of
    // `det`) works whichever way the backend's screen y runs, and for mirrored uvs.
    let v_dir = (duv_dx.x * dp_dy - duv_dy.x * dp_dx) / safe;
    let t_ortho = t_raw - ng * dot(ng, t_raw);
    if abs(det) < 1e-12 || dot(t_ortho, t_ortho) < 1e-12 {
        s.normal = ng;
    } else {
        let t = normalize(t_ortho);
        // glTF normal maps: green points towards decreasing v (uvs run top-down).
        let b0 = cross(ng, t);
        let b = select(b0, -b0, dot(b0, v_dir) > 0.0);
        let scaled = vec3<f32>(nt.xy * mat.emissive.a, nt.z);
        s.normal = normalize(mat3x3<f32>(t, b, ng) * scaled);
    }
    return s;
}

fn d_ggx(n_h: f32, a: f32) -> f32 {
    let a2 = a * a;
    let f = n_h * n_h * (a2 - 1.0) + 1.0;
    return a2 / (PI * f * f);
}

fn v_smith(n_v: f32, n_l: f32, a: f32) -> f32 {
    let a2 = a * a;
    let gv = n_l * sqrt(n_v * n_v * (1.0 - a2) + a2);
    let gl = n_v * sqrt(n_l * n_l * (1.0 - a2) + a2);
    return 0.5 / max(gv + gl, 1e-5);
}

fn f_schlick(f0: vec3<f32>, v_h: f32) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - v_h, 5.0);
}

/// The specular part of the environment's split-sum integral, fitted (Karis 2014).
fn env_brdf(f0: vec3<f32>, roughness: f32, n_v: f32) -> vec3<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * n_v)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return f0 * ab.x + ab.y;
}

/// Khronos PBR Neutral: keeps base colours as authored, compresses highlights.
fn tonemap(c: vec3<f32>) -> vec3<f32> {
    let start = 0.8 - 0.04;
    let x = min(c.r, min(c.g, c.b));
    let offset = select(0.04, x - 6.25 * x * x, x < 0.08);
    let col = c - offset;
    let peak = max(col.r, max(col.g, col.b));
    if peak < start {
        return col;
    }
    let d = 1.0 - start;
    let new_peak = 1.0 - d * d / (peak + d - start);
    let scaled = col * (new_peak / peak);
    let g = 1.0 - 1.0 / (0.15 * (peak - new_peak) + 1.0);
    return mix(scaled, vec3<f32>(new_peak), g);
}

fn shade_pbr(in: VOut, front_facing: bool) -> vec4<f32> {
    let s = surface(in, front_facing);
    if mat.params.w >= 0.0 && s.alpha < mat.params.w {
        discard;
    }
    let n = s.normal;
    let v = normalize(u.eye.xyz - in.world);
    let n_v = max(dot(n, v), 1e-4);
    let f0 = mix(vec3<f32>(0.04), s.albedo, s.metallic);
    let diffuse = s.albedo * (1.0 - s.metallic);
    let a = s.roughness * s.roughness;

    // Key light.
    let l = normalize(u.light.xyz);
    let h = normalize(l + v);
    let n_l = max(dot(n, l), 0.0);
    let f = f_schlick(f0, max(dot(v, h), 0.0));
    let spec = d_ggx(max(dot(n, h), 0.0), a) * v_smith(n_v, n_l, a) * f;
    let direct = ((vec3<f32>(1.0) - f) * diffuse / PI + spec) * n_l * KEY_LIGHT;

    // Sky: diffuse from the irradiance, specular from the reflected direction, blurred
    // towards the irradiance as the surface gets rougher.
    let r = reflect(-v, n);
    let reflected = mix(sky(r), sky_irradiance(r), s.roughness * s.roughness);
    let ambient = (diffuse * sky_irradiance(n) + reflected * env_brdf(f0, s.roughness, n_v)) * SKY_LIGHT * s.occlusion;

    return vec4<f32>(tonemap(direct + ambient + s.emissive), 1.0);
}

fn shade_unlit(in: VOut) -> vec4<f32> {
    let base = textureSample(base_tex, mat_sampler, in.uv);
    let c = in.color * base;
    if mat.params.w >= 0.0 && c.a < mat.params.w {
        discard;
    }
    return vec4<f32>(c.rgb, 1.0);
}

@fragment
fn fs_pbr(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let c = shade_pbr(in, front);
    return vec4<f32>(linear_to_srgb(c.rgb), c.a);
}

@fragment
fn fs_pbr_linear(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    return shade_pbr(in, front);
}

@fragment
fn fs_unlit(in: VOut) -> @location(0) vec4<f32> {
    let c = shade_unlit(in);
    return vec4<f32>(linear_to_srgb(c.rgb), c.a);
}

@fragment
fn fs_unlit_linear(in: VOut) -> @location(0) vec4<f32> {
    return shade_unlit(in);
}
