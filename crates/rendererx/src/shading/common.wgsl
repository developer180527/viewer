// What every shading model shares: the view and material bindings, the per-instance
// data and the vertex stage. Each fragment stage (clay.wgsl, pbr.wgsl) is appended
// to this file into one module; a material's shading model picks its entry points.
//
// Fragment stages come in pairs: `fs_<model>` writes sRGB-encoded colour into the
// Rgba8Unorm target by hand (works on GL/GLES, no sRGB view formats needed);
// `fs_<model>_linear` writes linear colour into the sRGB MSAA target (encode.wgsl).

struct ViewUniforms {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    // Towards the key light, world space.
    light: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: ViewUniforms;

// One bind group per material, shared by every shading model (each uses what it
// needs). Missing maps are 1×1 defaults: white, or a flat normal.
struct MaterialUniforms {
    // rgb: emissive factor; a: normal map scale.
    emissive: vec4<f32>,
    // metallic, roughness, occlusion strength, alpha cutoff (negative: opaque).
    params: vec4<f32>,
}

@group(1) @binding(0) var<uniform> mat: MaterialUniforms;
@group(1) @binding(1) var base_tex: texture_2d<f32>;
// glTF packing: roughness in G, metalness in B.
@group(1) @binding(2) var metal_rough_tex: texture_2d<f32>;
@group(1) @binding(3) var normal_tex: texture_2d<f32>;
@group(1) @binding(4) var occlusion_tex: texture_2d<f32>;
@group(1) @binding(5) var emissive_tex: texture_2d<f32>;
@group(1) @binding(6) var mat_sampler: sampler;

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


/// The shading normal: the interpolated one, facing the viewer on double-sided
/// surfaces. Lines and points have none: they face the camera.
fn facing_normal(in: VOut, front_facing: bool) -> vec3<f32> {
    // A mirrored transform reverses winding on screen, so the outside shows as "back".
    let front = front_facing != (in.mirror < 0.0);
    if dot(in.normal, in.normal) <= 1e-12 {
        return normalize(u.eye.xyz - in.world);
    }
    let n = normalize(in.normal);
    return select(-n, n, front);
}
