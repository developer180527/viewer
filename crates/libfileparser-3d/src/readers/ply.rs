//! PLY (ASCII, binary little- and big-endian): scans and point clouds. Vertices with
//! optional normals and colours, and faces (any polygon, fanned into triangles). A
//! file without faces is a point cloud and draws as points. Gaussian-splat PLYs are
//! point clouds too: their base colour (`f_dc_*`) is shown, the splats themselves
//! aren't (yet).
//!
//! The header says how many of everything follow; those counts are checked against
//! what the file can hold before anything is allocated.

use anyhow::{Context, anyhow, bail};
use scene_data::glam::{Mat4, Vec3, Vec4};
use scene_data::{CpuScene, Indices, Material, MeshData, Node, ParamValue, Topology};
use anyhow::Result;
use std::sync::Arc;

use libfileparser_core::{Content, OwnedTree, ParseContext, Role};

pub struct Loaded {
    pub scene: CpuScene,
    pub format: &'static str,
    pub points: bool,
    pub warnings: Vec<String>,
}

/// Whether `header` is a PLY file: its first line is `ply`.
pub fn is_ply(header: &[u8]) -> bool {
    header.starts_with(b"ply\n") || header.starts_with(b"ply\r\n")
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Scalar {
    fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "char" | "int8" => Scalar::I8,
            "uchar" | "uint8" => Scalar::U8,
            "short" | "int16" => Scalar::I16,
            "ushort" | "uint16" => Scalar::U16,
            "int" | "int32" => Scalar::I32,
            "uint" | "uint32" => Scalar::U32,
            "float" | "float32" => Scalar::F32,
            "double" | "float64" => Scalar::F64,
            _ => bail!("unknown property type {name:?}"),
        })
    }

    fn size(self) -> usize {
        match self {
            Scalar::I8 | Scalar::U8 => 1,
            Scalar::I16 | Scalar::U16 => 2,
            Scalar::I32 | Scalar::U32 | Scalar::F32 => 4,
            Scalar::F64 => 8,
        }
    }

    /// The scale that maps this type's colour values to 0..1.
    fn colour_scale(self) -> f64 {
        match self {
            Scalar::U8 | Scalar::I8 => 1.0 / 255.0,
            Scalar::U16 | Scalar::I16 => 1.0 / 65535.0,
            _ => 1.0,
        }
    }
}

#[derive(Debug)]
enum Property {
    Scalar(Scalar, String),
    /// count type, item type, name
    List(Scalar, Scalar, String),
}

impl Property {
    fn name(&self) -> &str {
        match self {
            Property::Scalar(_, n) | Property::List(_, _, n) => n,
        }
    }
}

#[derive(Debug)]
struct Element {
    name: String,
    count: usize,
    props: Vec<Property>,
}

impl Element {
    /// The smallest size one record can have in the file: a bound for `count`.
    fn min_bytes(&self, ascii: bool) -> usize {
        let per = |p: &Property| match p {
            _ if ascii => 2, // a digit and a separator
            Property::Scalar(t, _) => t.size(),
            Property::List(c, _, _) => c.size(),
        };
        self.props.iter().map(per).sum::<usize>().max(1)
    }

    fn index(&self, names: &[&str]) -> Option<usize> {
        self.props.iter().position(|p| names.contains(&p.name()))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Encoding {
    Ascii,
    Little,
    Big,
}

/// Reads values from the body, as text tokens or binary in either byte order.
struct Body<'a> {
    bytes: &'a [u8],
    at: usize,
    encoding: Encoding,
}

impl Body<'_> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn value(&mut self, ty: Scalar) -> Result<f64> {
        if self.encoding == Encoding::Ascii {
            let start = self.at + self.bytes[self.at..].iter().position(|b| !b.is_ascii_whitespace()).ok_or_else(|| anyhow!("file ends early"))?;
            let end = self.bytes[start..].iter().position(|b| b.is_ascii_whitespace()).map_or(self.bytes.len(), |n| start + n);
            self.at = end;
            let word = std::str::from_utf8(&self.bytes[start..end]).ok().ok_or_else(|| anyhow!("bad number"))?;
            return word.parse().with_context(|| format!("bad number {word:?}"));
        }
        let n = ty.size();
        let raw = self.bytes.get(self.at..self.at + n).ok_or_else(|| anyhow!("file ends early"))?;
        self.at += n;
        let mut b = [0u8; 8];
        b[..n].copy_from_slice(raw);
        if self.encoding == Encoding::Big {
            b[..n].reverse();
        }
        Ok(match ty {
            Scalar::I8 => b[0] as i8 as f64,
            Scalar::U8 => b[0] as f64,
            Scalar::I16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            Scalar::U16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            Scalar::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Scalar::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Scalar::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Scalar::F64 => f64::from_le_bytes(b),
        })
    }
}

/// Most corners one face may have: past this a count is garbage, not a polygon.
const MAX_FACE_CORNERS: usize = 256;

pub fn load(bytes: &[u8], cx: &ParseContext) -> Result<Loaded> {
    let (encoding, elements, body_start) = header(bytes)?;
    let format = match encoding {
        Encoding::Ascii => "PLY (ASCII)",
        Encoding::Little | Encoding::Big => "PLY (binary)",
    };
    let mut body = Body { bytes, at: body_start, encoding };
    let mut warnings = Vec::new();
    let mut positions: Vec<Vec3> = Vec::new();
    let mut normals: Vec<Vec3> = Vec::new();
    let mut colors: Vec<Vec4> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut have_vertices = false;

    for el in &elements {
        // Counts come from the file: check them against the bytes left first.
        if el.count > body.remaining() / el.min_bytes(encoding == Encoding::Ascii) {
            bail!("{} {} elements can't fit in the file", el.count, el.name);
        }
        match el.name.as_str() {
            "vertex" if !have_vertices => {
                have_vertices = true;
                read_vertices(&mut body, el, &mut positions, &mut normals, &mut colors)?;
            }
            "face" => {
                let Some(list) = el.index(&["vertex_indices", "vertex_index"]) else {
                    skip(&mut body, el)?;
                    continue;
                };
                let mut dropped = 0usize;
                let mut corners = Vec::new();
                for _ in 0..el.count {
                    for (i, p) in el.props.iter().enumerate() {
                        match p {
                            Property::List(ct, it, _) => {
                                let n = body.value(*ct)? as usize;
                                if n > MAX_FACE_CORNERS {
                                    bail!("a face with {n} corners");
                                }
                                corners.clear();
                                for _ in 0..n {
                                    corners.push(body.value(*it)?);
                                }
                                if i != list {
                                    continue;
                                }
                                // Fan into triangles; faces naming a vertex that
                                // doesn't exist are dropped.
                                if corners.iter().any(|&c| c < 0.0 || c as usize >= positions.len()) {
                                    dropped += 1;
                                    continue;
                                }
                                if n < 3 {
                                    continue; // a point or an edge: nothing to fill
                                }
                                for k in 1..n - 1 {
                                    indices.extend([corners[0] as u32, corners[k] as u32, corners[k + 1] as u32]);
                                }
                            }
                            Property::Scalar(t, _) => {
                                body.value(*t)?;
                            }
                        }
                    }
                }
                if dropped > 0 {
                    warnings.push(format!("{dropped} face(s) name vertices that don't exist; left out"));
                }
            }
            _ => skip(&mut body, el)?,
        }
    }
    if positions.is_empty() {
        bail!("no vertices");
    }

    let points = indices.is_empty();
    let mut mesh = MeshData::new(positions);
    let n = mesh.positions.len();
    if normals.len() == n && !points {
        mesh.normals = Some(normals.into());
    }
    let coloured = colors.len() == n;
    if coloured {
        mesh.colors = Some(colors.into());
    }
    if points {
        mesh.topology = Topology::Points;
    } else {
        mesh.indices = if n <= u16::MAX as usize + 1 {
            Indices::U16(indices.into_iter().map(|i| i as u16).collect::<Vec<_>>().into())
        } else {
            Indices::U32(indices.into())
        };
    }

    let (scene, mut builder) = crate::build::new_scene(cx);
    let id = builder.add_mesh(mesh).context("the mesh")?;
    // Vertex colours are the surface's colour: a white factor shows them as they are
    // (clay's own grey would darken them).
    let material = coloured
        .then(|| builder.add_material(Material::clay().with_param("base_color", ParamValue::Vec4(Vec4::ONE))))
        .transpose()
        .context("the material")?;
    builder.add_node(Node::mesh(id, material, Mat4::IDENTITY, None)).context("the mesh")?;
    builder.finish();
    Ok(Loaded { scene, format, points, warnings })
}

/// Parses the header: the encoding, the elements in file order, and where the body
/// starts.
fn header(bytes: &[u8]) -> Result<(Encoding, Vec<Element>, usize)> {
    if !is_ply(bytes) {
        bail!("not a PLY file");
    }
    let end = bytes.windows(10).position(|w| w == b"end_header").ok_or_else(|| anyhow!("no end_header"))?;
    let body_start = end + 10 + bytes[end + 10..].iter().position(|&b| b == b'\n').map_or(0, |n| n + 1);
    let text = std::str::from_utf8(&bytes[..end]).context("header isn't text")?;
    let mut encoding = None;
    let mut elements: Vec<Element> = Vec::new();
    for line in text.lines().skip(1) {
        let w: Vec<&str> = line.split_ascii_whitespace().collect();
        match w.as_slice() {
            ["format", f, _version] => {
                encoding = Some(match *f {
                    "ascii" => Encoding::Ascii,
                    "binary_little_endian" => Encoding::Little,
                    "binary_big_endian" => Encoding::Big,
                    _ => bail!("unknown PLY format {f:?}"),
                });
            }
            ["element", name, count] => {
                let count = count.parse().with_context(|| format!("bad count for {name}"))?;
                elements.push(Element { name: name.to_string(), count, props: Vec::new() });
            }
            ["property", "list", ct, it, name] => {
                let el = elements.last_mut().ok_or_else(|| anyhow!("property before any element"))?;
                el.props.push(Property::List(Scalar::parse(ct)?, Scalar::parse(it)?, name.to_string()));
            }
            ["property", ty, name] => {
                let el = elements.last_mut().ok_or_else(|| anyhow!("property before any element"))?;
                el.props.push(Property::Scalar(Scalar::parse(ty)?, name.to_string()));
            }
            _ => {} // comment, obj_info, blank
        }
    }
    Ok((encoding.ok_or_else(|| anyhow!("no format line"))?, elements, body_start))
}

/// The Header facet: the format, comments and each element with its count and
/// properties, as the file writes them.
pub fn header_tree(bytes: &[u8]) -> Option<OwnedTree> {
    let end = bytes.windows(10).position(|w| w == b"end_header")?;
    let text = String::from_utf8_lossy(&bytes[..end]);
    let mut tree = OwnedTree::new();
    let mut element = None;
    for line in text.lines().skip(1) {
        let (key, rest) = line.trim().split_once(char::is_whitespace).unwrap_or((line.trim(), ""));
        let rest = rest.trim();
        match key {
            "format" | "comment" | "obj_info" => {
                tree.add(None, key, rest);
            }
            "element" => {
                let (name, count) = rest.rsplit_once(char::is_whitespace).unwrap_or((rest, ""));
                let items = if count == "1" { "1 item".to_string() } else { format!("{count} items") };
                element = Some(tree.add(None, name, items));
            }
            "property" => {
                let (ty, name) = rest.rsplit_once(char::is_whitespace).unwrap_or(("", rest));
                tree.add(element, name, ty);
            }
            _ => {}
        }
    }
    Some(tree)
}

fn read_vertices(body: &mut Body<'_>, el: &Element, positions: &mut Vec<Vec3>, normals: &mut Vec<Vec3>, colors: &mut Vec<Vec4>) -> Result<()> {
    let find = |names: &[&str]| el.index(names);
    let pos = [find(&["x"]), find(&["y"]), find(&["z"])];
    let [Some(px), Some(py), Some(pz)] = pos else { bail!("vertices without x, y, z") };
    let nrm = [find(&["nx"]), find(&["ny"]), find(&["nz"])];
    let rgb = [find(&["red", "r", "diffuse_red"]), find(&["green", "g", "diffuse_green"]), find(&["blue", "b", "diffuse_blue"])];
    let alpha = find(&["alpha", "a", "diffuse_alpha"]);
    // Gaussian splats: the zeroth spherical-harmonic band is the base colour.
    let dc = [find(&["f_dc_0"]), find(&["f_dc_1"]), find(&["f_dc_2"])];
    let has_normals = nrm.iter().all(Option::is_some);
    let colour = if rgb.iter().all(Option::is_some) {
        Some(false)
    } else if dc.iter().all(Option::is_some) {
        Some(true)
    } else {
        None
    };
    let scale = |i: usize| match &el.props[i] {
        Property::Scalar(t, _) => t.colour_scale(),
        Property::List(..) => 1.0,
    };

    positions.reserve(el.count);
    if has_normals {
        normals.reserve(el.count);
    }
    if colour.is_some() {
        colors.reserve(el.count);
    }
    let mut v = vec![0f64; el.props.len()];
    for _ in 0..el.count {
        for (i, p) in el.props.iter().enumerate() {
            v[i] = match p {
                Property::Scalar(t, _) => body.value(*t)?,
                Property::List(ct, it, _) => {
                    let n = body.value(*ct)? as usize;
                    if n > MAX_FACE_CORNERS {
                        bail!("a vertex list of {n} items");
                    }
                    for _ in 0..n {
                        body.value(*it)?;
                    }
                    0.0
                }
            };
        }
        positions.push(Vec3::new(v[px] as f32, v[py] as f32, v[pz] as f32));
        if has_normals {
            let n = nrm.map(|i| v[i.unwrap()] as f32);
            normals.push(Vec3::from(n));
        }
        match colour {
            Some(false) => {
                let c = rgb.map(|i| srgb_to_linear((v[i.unwrap()] * scale(i.unwrap())) as f32));
                let a = alpha.map_or(1.0, |i| (v[i] * scale(i)) as f32);
                colors.push(Vec4::new(c[0], c[1], c[2], a.clamp(0.0, 1.0)));
            }
            Some(true) => {
                // SH band 0: colour = 0.5 + C0 · f_dc, in display (sRGB) terms.
                const C0: f64 = 0.282_094_79;
                let c = dc.map(|i| srgb_to_linear((0.5 + C0 * v[i.unwrap()]) as f32));
                colors.push(Vec4::new(c[0], c[1], c[2], 1.0));
            }
            None => {}
        }
    }
    Ok(())
}

/// Reads past an element this loader doesn't use.
fn skip(body: &mut Body<'_>, el: &Element) -> Result<()> {
    for _ in 0..el.count {
        for p in &el.props {
            match p {
                Property::Scalar(t, _) => {
                    body.value(*t)?;
                }
                Property::List(ct, it, _) => {
                    let n = body.value(*ct)? as usize;
                    if n > body.remaining() {
                        bail!("a list longer than the file");
                    }
                    for _ in 0..n {
                        body.value(*it)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn srgb_to_linear(c: f32) -> f32 {
    let c = if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 };
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

// ---------------------------------------------------------------- the reader

use libfileparser_core::{Category, Detection, Document, FileSource, Reader, ReaderInfo};

use crate::build::{SceneDocument, detect_by};

pub struct PlyReader;

static PLY: ReaderInfo = ReaderInfo::new("ply", "PLY model", Category::ThreeD).extensions(&["ply"]);

impl Reader for PlyReader {
    fn info(&self) -> &ReaderInfo {
        &PLY
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        detect_by(&PLY, is_ply(header), name).with_language("ply")
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> libfileparser_core::Result<Document> {
        cx.cancel.check()?;
        let loaded = load(src.bytes(), cx).map_err(super::parse_error)?;
        let header = header_tree(src.bytes());
        let doc = SceneDocument::new(self, src, loaded.format, loaded.scene)
            .warnings(loaded.warnings)
            .facets(move |doc| match header {
                Some(tree) => doc.with_facet(Role::Structure, "Header", Content::Tree(Arc::new(tree))),
                None => doc,
            })
            .build()?;
        Ok(if loaded.points { doc.with_meta("Kind", "point cloud") } else { doc })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the default context, as the tests below were written.
    fn load(bytes: &[u8]) -> Result<Loaded> {
        super::load(bytes, &ParseContext::new())
    }

    const QUAD: &str = "ply\nformat ascii 1.0\ncomment a red square\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\n\
property uchar red\nproperty uchar green\nproperty uchar blue\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n\
0 0 0 255 0 0\n1 0 0 255 0 0\n1 1 0 255 0 0\n0 1 0 255 0 0\n4 0 1 2 3\n";

    fn binary(big: bool) -> Vec<u8> {
        let order = if big { "binary_big_endian" } else { "binary_little_endian" };
        let mut out = format!(
            "ply\nformat {order} 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\n\
element face 1\nproperty uchar intensity\nproperty list uchar uint vertex_indices\nend_header\n"
        )
        .into_bytes();
        let f = |v: f32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
        let u = |v: u32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
        for p in [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 3.0, 0.0]] {
            p.iter().for_each(|&c| out.extend(f(c)));
        }
        out.push(7); // the face's intensity, before its list: must be read past
        out.push(3);
        [0, 1, 2].iter().for_each(|&i| out.extend(u(i)));
        out
    }

    #[test]
    fn ascii_quads_fan_into_triangles_with_colours() {
        let l = load(QUAD.as_bytes()).unwrap();
        assert!(!l.points);
        let read = l.scene.read();
        assert_eq!(read.meshes()[0].info.primitives, 2);
        let data = read.meshes()[0].data.as_ref().unwrap();
        assert_eq!(data.colors.as_ref().unwrap().as_slice()[0], Vec4::new(1.0, 0.0, 0.0, 1.0));
        // White factor: the vertex colours show as they are.
        assert_eq!(read.materials()[0].material.params.get("base_color"), Some(ParamValue::Vec4(Vec4::ONE)));
    }

    #[test]
    fn binary_in_both_byte_orders() {
        for big in [false, true] {
            let l = load(&binary(big)).unwrap();
            assert_eq!(l.format, "PLY (binary)");
            let b = l.scene.read().bounds().unwrap();
            assert_eq!(b.size(), Vec3::new(2.0, 3.0, 0.0), "big endian: {big}");
        }
    }

    #[test]
    fn files_without_faces_are_point_clouds_and_splats_get_their_colour() {
        let splat = "ply\nformat ascii 1.0\nelement vertex 2\nproperty float x\nproperty float y\nproperty float z\n\
property float f_dc_0\nproperty float f_dc_1\nproperty float f_dc_2\nproperty float opacity\nend_header\n\
0 0 0 1.7725 -1.7725 -1.7725 3\n1 1 1 0 0 0 3\n";
        let l = load(splat.as_bytes()).unwrap();
        assert!(l.points);
        let read = l.scene.read();
        assert_eq!(read.meshes()[0].info.topology, Topology::Points);
        let c = read.meshes()[0].data.as_ref().unwrap().colors.as_ref().unwrap().as_slice()[0];
        // 0.5 + C0 · 1.7725 = 1.0 (red), and 0.5 - 0.5 = 0 (green, blue).
        assert!((c.x - 1.0).abs() < 1e-3 && c.y < 1e-3 && c.z < 1e-3, "{c:?}");
    }

    #[test]
    fn hostile_counts_and_indices_fail_cleanly() {
        // A billion vertices claimed in a tiny file: refused before allocating.
        let huge = QUAD.replace("element vertex 4", "element vertex 1000000000");
        assert!(load(huge.as_bytes()).err().unwrap().to_string().contains("can't fit"));
        // A face naming a vertex that doesn't exist: left out with a warning.
        let bad = QUAD.replace("4 0 1 2 3", "3 0 1 9");
        let l = load(bad.as_bytes());
        assert!(l.is_err() || !l.unwrap().warnings.is_empty());
        // Truncated binary.
        let mut b = binary(false);
        b.truncate(b.len() - 3);
        assert!(load(&b).is_err());
        // Not PLY, no format, bad types.
        assert!(load(b"plyx").is_err());
        assert!(load(b"ply\nelement vertex 1\nproperty float x\nend_header\n0\n").is_err());
        assert!(load(b"ply\nformat ascii 1.0\nelement vertex 1\nproperty quad x\nend_header\n0\n").is_err());
    }
}
