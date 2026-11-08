//! What a viewer needs from a layer, as plain data: the prim tree with transforms,
//! meshes (with primvars and material subsets), material bindings and
//! `UsdPreviewSurface` materials with their textures.
//!
//! Matrices are `[f64; 16]` in column-major order for column vectors (glam, glTF
//! convention). USD writes row-major matrices for row vectors, which is the same 16
//! numbers, so `matrix4d` values pass through unchanged.

use std::sync::Arc;

use crate::layer::split_property;
use crate::{Layer, Value};

pub type Matrix = [f64; 16];

pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// Closed on purpose: USD allows only Y and Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpAxis {
    Y,
    Z,
}

/// The layer's up axis (`upAxis`, Y when unset).
pub fn up_axis(layer: &Layer) -> UpAxis {
    match layer.metadata("upAxis").and_then(Value::as_str) {
        Some("Z") => UpAxis::Z,
        _ => UpAxis::Y,
    }
}

/// Metres per scene unit (`metersPerUnit`; USD's default is centimetres).
pub fn meters_per_unit(layer: &Layer) -> f64 {
    layer.metadata("metersPerUnit").and_then(Value::as_f64).filter(|m| m.is_finite() && *m > 0.0).unwrap_or(0.01)
}

pub fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    out
}

fn translation(t: [f64; 3]) -> Matrix {
    let mut m = IDENTITY;
    m[12..15].copy_from_slice(&t);
    m
}

fn scale(s: [f64; 3]) -> Matrix {
    let mut m = IDENTITY;
    (m[0], m[5], m[10]) = (s[0], s[1], s[2]);
    m
}

/// A rotation of `degrees` about axis 0 (x), 1 (y) or 2 (z).
fn rotation(axis: usize, degrees: f64) -> Matrix {
    let (s, c) = degrees.to_radians().sin_cos();
    let mut m = IDENTITY;
    let (a, b) = [(1, 2), (2, 0), (0, 1)][axis];
    m[a * 4 + a] = c;
    m[b * 4 + b] = c;
    m[a * 4 + b] = s;
    m[b * 4 + a] = -s;
    m
}

fn quaternion([x, y, z, w]: [f64; 4]) -> Matrix {
    let n = (x * x + y * y + z * z + w * w).sqrt();
    // Zero or NaN length: no rotation.
    if n.is_nan() || n <= 0.0 {
        return IDENTITY;
    }
    let (x, y, z, w) = (x / n, y / n, z / n, w / n);
    [
        1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w), 0.0,
        2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w), 0.0,
        2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y), 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]
}

/// General 4×4 inverse; identity when singular.
pub fn inverse(m: &Matrix) -> Matrix {
    let mut inv = [0.0; 16];
    let a = m;
    inv[0] = a[5] * a[10] * a[15] - a[5] * a[11] * a[14] - a[9] * a[6] * a[15] + a[9] * a[7] * a[14] + a[13] * a[6] * a[11] - a[13] * a[7] * a[10];
    inv[4] = -a[4] * a[10] * a[15] + a[4] * a[11] * a[14] + a[8] * a[6] * a[15] - a[8] * a[7] * a[14] - a[12] * a[6] * a[11] + a[12] * a[7] * a[10];
    inv[8] = a[4] * a[9] * a[15] - a[4] * a[11] * a[13] - a[8] * a[5] * a[15] + a[8] * a[7] * a[13] + a[12] * a[5] * a[11] - a[12] * a[7] * a[9];
    inv[12] = -a[4] * a[9] * a[14] + a[4] * a[10] * a[13] + a[8] * a[5] * a[14] - a[8] * a[6] * a[13] - a[12] * a[5] * a[10] + a[12] * a[6] * a[9];
    inv[1] = -a[1] * a[10] * a[15] + a[1] * a[11] * a[14] + a[9] * a[2] * a[15] - a[9] * a[3] * a[14] - a[13] * a[2] * a[11] + a[13] * a[3] * a[10];
    inv[5] = a[0] * a[10] * a[15] - a[0] * a[11] * a[14] - a[8] * a[2] * a[15] + a[8] * a[3] * a[14] + a[12] * a[2] * a[11] - a[12] * a[3] * a[10];
    inv[9] = -a[0] * a[9] * a[15] + a[0] * a[11] * a[13] + a[8] * a[1] * a[15] - a[8] * a[3] * a[13] - a[12] * a[1] * a[11] + a[12] * a[3] * a[9];
    inv[13] = a[0] * a[9] * a[14] - a[0] * a[10] * a[13] - a[8] * a[1] * a[14] + a[8] * a[2] * a[13] + a[12] * a[1] * a[10] - a[12] * a[2] * a[9];
    inv[2] = a[1] * a[6] * a[15] - a[1] * a[7] * a[14] - a[5] * a[2] * a[15] + a[5] * a[3] * a[14] + a[13] * a[2] * a[7] - a[13] * a[3] * a[6];
    inv[6] = -a[0] * a[6] * a[15] + a[0] * a[7] * a[14] + a[4] * a[2] * a[15] - a[4] * a[3] * a[14] - a[12] * a[2] * a[7] + a[12] * a[3] * a[6];
    inv[10] = a[0] * a[5] * a[15] - a[0] * a[7] * a[13] - a[4] * a[1] * a[15] + a[4] * a[3] * a[13] + a[12] * a[1] * a[7] - a[12] * a[3] * a[5];
    inv[14] = -a[0] * a[5] * a[14] + a[0] * a[6] * a[13] + a[4] * a[1] * a[14] - a[4] * a[2] * a[13] - a[12] * a[1] * a[6] + a[12] * a[2] * a[5];
    inv[3] = -a[1] * a[6] * a[11] + a[1] * a[7] * a[10] + a[5] * a[2] * a[11] - a[5] * a[3] * a[10] - a[9] * a[2] * a[7] + a[9] * a[3] * a[6];
    inv[7] = a[0] * a[6] * a[11] - a[0] * a[7] * a[10] - a[4] * a[2] * a[11] + a[4] * a[3] * a[10] + a[8] * a[2] * a[7] - a[8] * a[3] * a[6];
    inv[11] = -a[0] * a[5] * a[11] + a[0] * a[7] * a[9] + a[4] * a[1] * a[11] - a[4] * a[3] * a[9] - a[8] * a[1] * a[7] + a[8] * a[3] * a[5];
    inv[15] = a[0] * a[5] * a[10] - a[0] * a[6] * a[9] - a[4] * a[1] * a[10] + a[4] * a[2] * a[9] + a[8] * a[1] * a[6] - a[8] * a[2] * a[5];
    let det = a[0] * inv[0] + a[1] * inv[4] + a[2] * inv[8] + a[3] * inv[12];
    if det.abs() < 1e-300 || !det.is_finite() {
        return IDENTITY;
    }
    inv.map(|v| v / det)
}

/// A prim's local transform from its `xformOpOrder`, and whether it resets the
/// parent's transform (`!resetXformStack!`).
pub fn local_transform(layer: &Layer, prim: &str) -> (Matrix, bool) {
    let Some(order) = layer.attr(prim, "xformOpOrder").and_then(Value::as_tokens) else { return (IDENTITY, false) };
    let mut m = IDENTITY;
    let mut reset = false;
    for op in order {
        if op == "!resetXformStack!" {
            reset = true;
            m = IDENTITY;
            continue;
        }
        let (invert, name) = match op.strip_prefix("!invert!") {
            Some(n) => (true, n),
            None => (false, op.as_str()),
        };
        let Some(value) = layer.attr(prim, name) else { continue };
        let kind = name.strip_prefix("xformOp:").unwrap_or(name).split(':').next().unwrap_or("");
        let v3 = || value.as_vec3();
        let op_m = match kind {
            "translate" => v3().map(translation),
            "scale" => v3().map(scale),
            "rotateX" => value.as_f64().map(|d| rotation(0, d)),
            "rotateY" => value.as_f64().map(|d| rotation(1, d)),
            "rotateZ" => value.as_f64().map(|d| rotation(2, d)),
            // rotateXYZ: X first, then Y, then Z, so Rz · Ry · Rx for column vectors.
            k if k.len() == 9 && k.starts_with("rotate") && k[6..].bytes().all(|a| matches!(a, b'X' | b'Y' | b'Z')) => v3().map(|angles| {
                k[6..].bytes().fold(IDENTITY, |acc, axis| {
                    let i = (axis - b'X') as usize;
                    mul(&rotation(i, angles[i]), &acc)
                })
            }),
            "orient" => match value {
                Value::Quat(q) => Some(quaternion(*q)),
                _ => None,
            },
            "transform" => value.as_matrix4(),
            _ => None,
        };
        if let Some(op_m) = op_m {
            let op_m = if invert { inverse(&op_m) } else { op_m };
            m = mul(&m, &op_m);
        }
    }
    (m, reset)
}

/// A prim that is drawn: not invisible, not a guide or proxy.
pub fn is_drawn(layer: &Layer, prim: &str) -> bool {
    let token = |name| layer.attr(prim, name).and_then(Value::as_str);
    token("visibility") != Some("invisible") && !matches!(token("purpose"), Some("guide" | "proxy"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Interpolation {
    /// One value for the whole mesh.
    Constant,
    /// One per face.
    Uniform,
    /// One per point (`varying` is the same for meshes).
    Vertex,
    /// One per face corner.
    FaceVarying,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Primvar<T> {
    pub values: Vec<T>,
    /// For indexed primvars: an index into `values` per element.
    pub indices: Option<Vec<i32>>,
    pub interpolation: Interpolation,
}

impl<T: Copy> Primvar<T> {
    /// The value at a face corner: `face` index, the corner's point index, and the
    /// corner's position in the face-vertex list.
    pub fn at(&self, face: usize, point: usize, corner: usize) -> Option<T> {
        let i = match self.interpolation {
            Interpolation::Constant => 0,
            Interpolation::Uniform => face,
            Interpolation::Vertex => point,
            Interpolation::FaceVarying => corner,
        };
        let i = match &self.indices {
            Some(idx) => usize::try_from(*idx.get(i)?).ok()?,
            None => i,
        };
        self.values.get(i).copied()
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Subset {
    pub name: String,
    /// Face indices.
    pub faces: Vec<i64>,
    pub material: Option<String>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Mesh {
    pub points: Vec<[f32; 3]>,
    pub face_vertex_counts: Vec<i64>,
    pub face_vertex_indices: Vec<i64>,
    pub normals: Option<Primvar<[f32; 3]>>,
    /// The texture coordinates (`primvars:st`, or the first 2D texCoord primvar).
    pub uvs: Option<Primvar<[f32; 2]>>,
    pub display_color: Option<Primvar<[f32; 3]>>,
    /// `orientation = "leftHanded"`: faces wind clockwise.
    pub left_handed: bool,
    pub double_sided: bool,
    /// Per-face material assignments (`GeomSubset`s in the `materialBind` family).
    pub subsets: Vec<Subset>,
}

fn interpolation(layer: &Layer, prim: &str, name: &str, default: Interpolation) -> Interpolation {
    match layer.attr_meta(prim, name, "interpolation").and_then(Value::as_str) {
        Some("constant") => Interpolation::Constant,
        Some("uniform") => Interpolation::Uniform,
        Some("vertex" | "varying") => Interpolation::Vertex,
        Some("faceVarying") => Interpolation::FaceVarying,
        _ => default,
    }
}

fn primvar<T: Clone>(layer: &Layer, prim: &str, name: &str, default: Interpolation, get: impl Fn(&Value) -> Option<&[T]>) -> Option<Primvar<T>> {
    let values = get(layer.attr(prim, name)?)?.to_vec();
    let indices = layer.attr(prim, &format!("{name}:indices")).and_then(|v| match v {
        Value::IntArray(i) => Some(i.clone()),
        _ => None,
    });
    Some(Primvar { values, indices: indices.map(|i: Arc<[i32]>| i.to_vec()), interpolation: interpolation(layer, prim, name, default) })
}

/// Reads a `Mesh` prim, or tessellates an implicit shape ([`is_implicit_shape`]).
/// `None` when it's neither, or has no points or faces.
pub fn mesh(layer: &Layer, prim: &str) -> Option<Mesh> {
    let ty = layer.type_name(prim)?;
    if is_implicit_shape(ty) {
        return shape::tessellate(layer, prim, ty);
    }
    if ty != "Mesh" {
        return None;
    }
    let points = layer.attr(prim, "points")?.as_vec3_array()?.to_vec();
    let face_vertex_counts = layer.attr(prim, "faceVertexCounts")?.as_ints()?;
    let face_vertex_indices = layer.attr(prim, "faceVertexIndices")?.as_ints()?;
    let normals = primvar(layer, prim, "primvars:normals", Interpolation::Vertex, Value::as_vec3_array)
        .or_else(|| primvar(layer, prim, "normals", Interpolation::Vertex, Value::as_vec3_array));
    let uv_name = if layer.has_property(prim, "primvars:st") {
        Some("primvars:st".to_string())
    } else {
        // The first 2D texture-coordinate primvar.
        let props = layer.field(prim, "properties").and_then(Value::as_tokens).unwrap_or_default();
        props.iter().find(|p| {
            p.starts_with("primvars:")
                && !p.ends_with(":indices")
                && layer.attr_meta(prim, p, "typeName").and_then(Value::as_str).is_some_and(|t| t.starts_with("texCoord2") || t.starts_with("float2"))
        }).cloned()
    };
    let uvs = uv_name.and_then(|n| primvar(layer, prim, &n, Interpolation::Vertex, Value::as_vec2_array));
    let display_color = primvar(layer, prim, "primvars:displayColor", Interpolation::Constant, Value::as_vec3_array);
    let subsets = layer
        .children(prim)
        .into_iter()
        .filter(|c| layer.type_name(c) == Some("GeomSubset"))
        .filter(|c| layer.attr(c, "familyName").and_then(Value::as_str).is_none_or(|f| f == "materialBind"))
        .filter(|c| layer.attr(c, "elementType").and_then(Value::as_str).is_none_or(|e| e == "face"))
        .filter_map(|c| {
            let faces = layer.attr(&c, "indices")?.as_ints()?;
            let name = c.rsplit('/').next().unwrap_or("").to_string();
            Some(Subset { name, faces, material: bound_material(layer, &c, false) })
        })
        .collect();
    Some(Mesh {
        points,
        face_vertex_counts,
        face_vertex_indices,
        normals,
        uvs,
        display_color,
        left_handed: layer.attr(prim, "orientation").and_then(Value::as_str) == Some("leftHanded"),
        double_sided: layer.attr(prim, "doubleSided").and_then(Value::as_bool).unwrap_or(false),
        subsets,
    })
}

/// The material bound to a prim (`material:binding`), looking up its ancestors when
/// `inherit` (bindings apply to whole subtrees).
pub fn bound_material(layer: &Layer, prim: &str, inherit: bool) -> Option<String> {
    let mut at = prim;
    loop {
        if let Some(t) = layer.targets(at, "material:binding").first() {
            return Some(t.clone());
        }
        if !inherit {
            return None;
        }
        at = match at.rfind('/') {
            Some(0) | None => return None,
            Some(i) => &at[..i],
        };
    }
}

/// Closed on purpose: the outputs a `UsdUVTexture` has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Rgb,
    Rgba,
    R,
    G,
    B,
    A,
}

/// A `UsdUVTexture` feeding a material input.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TextureInput {
    /// The asset path as authored (often absolute, from the exporting machine).
    pub file: String,
    pub channel: Channel,
    /// `sourceColorSpace`: `Some(true)` sRGB, `Some(false)` raw, `None` auto.
    pub srgb: Option<bool>,
    pub scale: [f64; 4],
    pub bias: [f64; 4],
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Input<T> {
    Value(T),
    Texture(TextureInput),
}

/// A `UsdPreviewSurface`, with the spec's defaults for unauthored inputs.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PreviewSurface {
    pub diffuse_color: Input<[f64; 3]>,
    pub emissive_color: Input<[f64; 3]>,
    pub metallic: Input<f64>,
    pub roughness: Input<f64>,
    pub opacity: Input<f64>,
    pub opacity_threshold: f64,
    pub normal: Option<TextureInput>,
    pub occlusion: Option<TextureInput>,
    /// `useSpecularWorkflow = 1`: metallic doesn't apply.
    pub specular_workflow: bool,
}

/// Follows a connection, through node-graph outputs, to the shader output it ends
/// at: (shader prim, output name).
fn source<'a>(layer: &'a Layer, prim: &str, input: &str) -> Option<(&'a str, &'a str)> {
    let mut target = layer.connections(prim, input).first()?.as_str();
    for _ in 0..8 {
        let (p, out) = split_property(target)?;
        if layer.field(p, "typeName").and_then(Value::as_str) == Some("Shader") || layer.attr(p, "info:id").is_some() {
            return Some((p, out));
        }
        // A node graph: its output connects onward.
        target = layer.connections(p, out).first()?.as_str();
    }
    None
}

fn texture(layer: &Layer, prim: &str, input: &str) -> Option<TextureInput> {
    let (shader, output) = source(layer, prim, input)?;
    if layer.attr(shader, "info:id").and_then(Value::as_str) != Some("UsdUVTexture") {
        return None;
    }
    let file = layer.attr(shader, "inputs:file").and_then(Value::as_str)?.to_string();
    let channel = match output.strip_prefix("outputs:") {
        Some("r") => Channel::R,
        Some("g") => Channel::G,
        Some("b") => Channel::B,
        Some("a") => Channel::A,
        Some("rgba") => Channel::Rgba,
        _ => Channel::Rgb,
    };
    let srgb = match layer.attr(shader, "inputs:sourceColorSpace").and_then(Value::as_str) {
        Some("sRGB") => Some(true),
        Some("raw") => Some(false),
        _ => None,
    };
    let v4 = |name, d: [f64; 4]| layer.attr(shader, name).and_then(Value::as_vec4).unwrap_or(d);
    Some(TextureInput { file, channel, srgb, scale: v4("inputs:scale", [1.0; 4]), bias: v4("inputs:bias", [0.0; 4]) })
}

fn input3(layer: &Layer, shader: &str, name: &str, default: [f64; 3]) -> Input<[f64; 3]> {
    match texture(layer, shader, name) {
        Some(t) => Input::Texture(t),
        None => Input::Value(layer.attr(shader, name).and_then(Value::as_vec3).unwrap_or(default)),
    }
}

fn input1(layer: &Layer, shader: &str, name: &str, default: f64) -> Input<f64> {
    match texture(layer, shader, name) {
        Some(t) => Input::Texture(t),
        None => Input::Value(layer.attr(shader, name).and_then(Value::as_f64).unwrap_or(default)),
    }
}

/// The `UsdPreviewSurface` a `Material` prim's `outputs:surface` leads to.
pub fn preview_surface(layer: &Layer, material: &str) -> Option<PreviewSurface> {
    let shader = match source(layer, material, "outputs:surface") {
        Some((s, _)) => s.to_string(),
        // No connection: a preview surface among the material's children.
        None => layer.children(material).into_iter().find(|c| layer.attr(c, "info:id").and_then(Value::as_str) == Some("UsdPreviewSurface"))?,
    };
    if layer.attr(&shader, "info:id").and_then(Value::as_str) != Some("UsdPreviewSurface") {
        return None;
    }
    let s = shader.as_str();
    Some(PreviewSurface {
        diffuse_color: input3(layer, s, "inputs:diffuseColor", [0.18; 3]),
        emissive_color: input3(layer, s, "inputs:emissiveColor", [0.0; 3]),
        metallic: input1(layer, s, "inputs:metallic", 0.0),
        roughness: input1(layer, s, "inputs:roughness", 0.5),
        opacity: input1(layer, s, "inputs:opacity", 1.0),
        opacity_threshold: layer.attr(s, "inputs:opacityThreshold").and_then(Value::as_f64).unwrap_or(0.0),
        normal: texture(layer, s, "inputs:normal"),
        occlusion: texture(layer, s, "inputs:occlusion"),
        specular_workflow: layer.attr(s, "inputs:useSpecularWorkflow").and_then(Value::as_f64).is_some_and(|v| v != 0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &Matrix, p: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
    }

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn rotations_follow_the_right_hand_rule() {
        assert!(close(apply(&rotation(2, 90.0), [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
        assert!(close(apply(&rotation(0, 90.0), [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));
        assert!(close(apply(&rotation(1, 90.0), [0.0, 0.0, 1.0]), [1.0, 0.0, 0.0]));
        // A quaternion of 90° about Z matches.
        let h = std::f64::consts::FRAC_1_SQRT_2;
        assert!(close(apply(&quaternion([0.0, 0.0, h, h]), [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
    }

    #[test]
    fn inverse_undoes_a_transform() {
        let m = mul(&mul(&translation([1.0, 2.0, 3.0]), &rotation(1, 30.0)), &scale([2.0, 3.0, 4.0]));
        let back = apply(&inverse(&m), apply(&m, [0.3, -0.7, 5.0]));
        assert!(close(back, [0.3, -0.7, 5.0]));
    }
}

/// Whether a prim type is one of UsdGeom's shapes defined by a few numbers, which
/// [`mesh`] turns into polygons: `Sphere`, `Cube`, `Cylinder`, `Cone`, `Capsule`, `Plane`.
pub fn is_implicit_shape(ty: &str) -> bool {
    matches!(ty, "Sphere" | "Cube" | "Cylinder" | "Cone" | "Capsule" | "Plane")
}

mod shape {
    //! UsdGeom's implicit shapes as polygons, with the schema's fallback sizes.

    use std::f64::consts::{FRAC_PI_2, TAU};

    use super::{Interpolation, Layer, Mesh, Primvar};

    /// Around the axis.
    const SEGMENTS: usize = 32;
    /// From pole to pole of a sphere.
    const RINGS: usize = 16;

    /// (radius, height), and the normal there as (radial, axial).
    type Point = ((f64, f64), (f64, f64));

    #[derive(Default)]
    struct Polys {
        points: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
        counts: Vec<i64>,
    }

    impl Polys {
        /// One polygon with its own corners, counter-clockwise from outside.
        fn face(&mut self, corners: &[([f64; 3], [f64; 3])]) {
            let f = |v: [f64; 3]| v.map(|c| c as f32);
            self.counts.push(corners.len() as i64);
            for &(p, n) in corners {
                self.points.push(f(p));
                self.normals.push(f(n));
            }
        }

        /// A profile (bottom to top) swept around Z.
        fn revolve(&mut self, profile: &[(Point, Point)]) {
            let at = |((r, z), (nr, nz)): Point, j: usize| {
                let (s, c) = (TAU * (j % SEGMENTS) as f64 / SEGMENTS as f64).sin_cos();
                ([r * c, r * s, z], [nr * c, nr * s, nz])
            };
            for &(low, high) in profile {
                for j in 0..SEGMENTS {
                    let (a, b, c, d) = (at(low, j), at(low, j + 1), at(high, j + 1), at(high, j));
                    // A profile touching the axis ends in a triangle.
                    match (low.0.0 == 0.0, high.0.0 == 0.0) {
                        (true, _) => self.face(&[a, c, d]),
                        (_, true) => self.face(&[a, b, c]),
                        _ => self.face(&[a, b, c, d]),
                    }
                }
            }
        }
    }

    /// A quarter or half of a circle of `radius` about height `z`: `from` to `to` radians
    /// of latitude, as profile segments.
    fn arc(radius: f64, z: f64, from: f64, to: f64, steps: usize) -> Vec<(Point, Point)> {
        let at = |i: usize| {
            let (s, c) = (from + (to - from) * i as f64 / steps as f64).sin_cos();
            // Exactly on the axis at the poles, where the profile ends in a triangle.
            let c = if c.abs() < 1e-12 { 0.0 } else { c };
            ((radius * c, z + radius * s), (c, s))
        };
        (0..steps).map(|i| (at(i), at(i + 1))).collect()
    }

    fn cap(radius: f64, z: f64, up: bool) -> (Point, Point) {
        let n = (0.0, if up { 1.0 } else { -1.0 });
        let (centre, rim) = (((0.0, z), n), ((radius, z), n));
        if up { (rim, centre) } else { (centre, rim) }
    }

    fn cube(p: &mut Polys, h: f64) {
        for axis in 0..3 {
            for sign in [1.0, -1.0] {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                let corner = |a: f64, b: f64| {
                    let (mut c, mut n) = ([0.0; 3], [0.0; 3]);
                    c[axis] = sign * h;
                    n[axis] = sign;
                    c[u] = a * h;
                    c[v] = b * h;
                    (c, n)
                };
                let quad = [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)];
                if sign > 0.0 {
                    p.face(&quad);
                } else {
                    p.face(&[quad[3], quad[2], quad[1], quad[0]]);
                }
            }
        }
    }

    pub(super) fn tessellate(layer: &Layer, prim: &str, ty: &str) -> Option<Mesh> {
        // A size that's negative, zero or not a number has no shape to draw.
        let size = |name: &str, fallback: f64| {
            let v = layer.attr(prim, name).and_then(|v| v.as_f64()).unwrap_or(fallback);
            (v.is_finite() && v > 0.0 && v < f32::MAX as f64).then_some(v)
        };
        let axis = layer.attr(prim, "axis").and_then(|v| v.as_str()).unwrap_or("Z").to_string();
        let mut p = Polys::default();
        match ty {
            "Sphere" => p.revolve(&arc(size("radius", 1.0)?, 0.0, -FRAC_PI_2, FRAC_PI_2, RINGS)),
            "Cube" => cube(&mut p, size("size", 2.0)? / 2.0),
            "Cylinder" => {
                let (r, h) = (size("radius", 1.0)?, size("height", 2.0)? / 2.0);
                p.revolve(&[cap(r, -h, false), (((r, -h), (1.0, 0.0)), ((r, h), (1.0, 0.0))), cap(r, h, true)]);
            }
            "Cone" => {
                let (r, h) = (size("radius", 1.0)?, size("height", 2.0)? / 2.0);
                let len = (4.0 * h * h + r * r).sqrt();
                let n = (2.0 * h / len, r / len);
                p.revolve(&[cap(r, -h, false), (((r, -h), n), ((0.0, h), n))]);
            }
            "Capsule" => {
                let (r, h) = (size("radius", 0.5)?, size("height", 1.0)? / 2.0);
                let mut profile = arc(r, -h, -FRAC_PI_2, 0.0, RINGS / 2);
                profile.push((((r, -h), (1.0, 0.0)), ((r, h), (1.0, 0.0))));
                profile.extend(arc(r, h, 0.0, FRAC_PI_2, RINGS / 2));
                p.revolve(&profile);
            }
            "Plane" => {
                let (mut w, mut l) = (size("width", 2.0)? / 2.0, size("length", 2.0)? / 2.0);
                // The width runs along X for axes Y and Z; the rotation below moves it.
                if axis == "Y" {
                    std::mem::swap(&mut w, &mut l);
                }
                let n = [0.0, 0.0, 1.0];
                p.face(&[([-w, -l, 0.0], n), ([w, -l, 0.0], n), ([w, l, 0.0], n), ([-w, l, 0.0], n)]);
            }
            _ => return None,
        }
        // Built around Z; a cyclic swap keeps the winding.
        let turn = |[x, y, z]: [f32; 3]| match (ty, axis.as_str()) {
            ("Sphere" | "Cube", _) => [x, y, z],
            (_, "X") => [z, x, y],
            (_, "Y") => [y, z, x],
            _ => [x, y, z],
        };
        let points: Vec<[f32; 3]> = p.points.into_iter().map(turn).collect();
        let normals: Vec<[f32; 3]> = p.normals.into_iter().map(turn).collect();
        let display_color = layer.attr(prim, "primvars:displayColor").and_then(|v| v.as_vec3_array()).map(|c| c.to_vec()).map(|values| Primvar {
            values,
            indices: None,
            interpolation: Interpolation::Constant,
        });
        Some(Mesh {
            face_vertex_indices: (0..points.len() as i64).collect(),
            face_vertex_counts: p.counts,
            normals: Some(Primvar { values: normals, indices: None, interpolation: Interpolation::Vertex }),
            points,
            uvs: None,
            display_color,
            left_handed: layer.attr(prim, "orientation").and_then(|v| v.as_str()) == Some("leftHanded"),
            // A plane has no back to cull.
            double_sided: ty == "Plane" || layer.attr(prim, "doubleSided").and_then(|v| v.as_bool()).unwrap_or(false),
            subsets: Vec::new(),
        })
    }
}
