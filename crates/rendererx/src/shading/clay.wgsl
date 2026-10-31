// Clay: the shading every model gets first (docs/3d-renderer.md §7.3), and the only
// one STL/PLY files need. Soft sky/ground ambient, a key light that follows the
// camera, a faint rim. `fs_main` writes sRGB-encoded colour into an Rgba8Unorm target
// by hand, so it works on GL/GLES too (no sRGB view formats needed); `fs_linear`
// writes linear colour into an sRGB MSAA target (see encode.wgsl).

struct ViewUniforms {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    // Towards the key light, world space.
    light: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: ViewUniforms;

// The material's base colour map (a 1×1 white texture when it has none).
@group(1) @binding(0) var base_tex: texture_2d<f32>;
@group(1) @binding(1) var base_sampler: sampler;

struct Instance {
    @location(4) m0: vec4<f32>,
    @location(5) m1: vec4<f32>,
    @location(6) m2: vec4<f32>,
    @location(7) m3: vec4<f32>,
    // Normal matrix (inverse transpose of the model's 3×3), one column each. n0.w is
    // -1 when the model matrix mirrors (negative determinant), else 1.
    @location(8) n0: vec4<f32>,
    @location(9) n1: vec4<f32>,
    @location(10) n2: vec4<f32>,
    // Material base colour, linear, straight alpha.
    @location(11) color: vec4<f32>,
}

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world: vec3<f32>,
    // Constant per instance, so plain interpolation is exact (and works everywhere).
    @location(3) mirror: f32,
    @location(4) uv: vec2<f32>,
}

fn shade_vertex(pos: vec3<f32>, normal: vec3<f32>, color: vec4<f32>, uv: vec2<f32>, inst: Instance) -> VOut {
    let model = mat4x4<f32>(inst.m0, inst.m1, inst.m2, inst.m3);
    let normal_m = mat3x3<f32>(inst.n0.xyz, inst.n1.xyz, inst.n2.xyz);
    let world = model * vec4<f32>(pos, 1.0);
    var out: VOut;
    out.clip = u.view_proj * world;
    out.world = world.xyz;
    out.normal = normal_m * normal;
    out.color = inst.color * color;
    out.mirror = inst.n0.w;
    out.uv = uv;
    return out;
}

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, inst: Instance) -> VOut {
    return shade_vertex(pos, normal, vec4<f32>(1.0), vec2<f32>(0.0), inst);
}

@vertex
fn vs_color(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) color: vec4<f32>, inst: Instance) -> VOut {
    return shade_vertex(pos, normal, color, vec2<f32>(0.0), inst);
}

@vertex
fn vs_uv(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(3) uv: vec2<f32>, inst: Instance) -> VOut {
    return shade_vertex(pos, normal, vec4<f32>(1.0), uv, inst);
}

@vertex
fn vs_color_uv(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) uv: vec2<f32>,
    inst: Instance,
) -> VOut {
    return shade_vertex(pos, normal, color, uv, inst);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = x * 12.92;
    let hi = 1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, x <= vec3<f32>(0.0031308));
}

fn shade(in: VOut, front_facing: bool) -> vec3<f32> {
    // Sampled first, in uniform control flow (derivatives for mip selection).
    let albedo = in.color.rgb * textureSample(base_tex, base_sampler, in.uv).rgb;
    // A mirrored transform reverses winding on screen, so the outside shows as "back".
    let front = front_facing != (in.mirror < 0.0);
    let to_eye = normalize(u.eye.xyz - in.world);
    // Lines and points have no normal: light them as if facing the camera.
    var n = to_eye;
    if dot(in.normal, in.normal) > 1e-12 {
        n = normalize(in.normal);
        // Clay is double-sided, so models with flipped winding still read correctly.
        if !front {
            n = -n;
        }
    }
    let sky = vec3<f32>(0.62, 0.66, 0.72);
    let ground = vec3<f32>(0.30, 0.27, 0.25);
    let ambient = mix(ground, sky, n.y * 0.5 + 0.5);
    let key = max(dot(n, normalize(u.light.xyz)), 0.0);
    let rim = pow(1.0 - max(dot(n, to_eye), 0.0), 3.0) * 0.12;
    return albedo * (ambient * 0.45 + key * 0.8) + vec3<f32>(rim);
}

@fragment
fn fs_main(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    return vec4<f32>(linear_to_srgb(shade(in, front)), 1.0);
}

@fragment
fn fs_linear(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    return vec4<f32>(shade(in, front), 1.0);
}
