//! STL (binary and ASCII): triangles, nothing else. Mostly from CAD and 3D printing,
//! where Z is up and units are millimetres: the scene is turned Y up (units don't
//! matter to a viewer, which frames whatever it gets).
//!
//! STL stores every triangle's three corners separately. Shared corners are welded
//! back together, so normals can be smoothed across gentle edges and kept hard across
//! sharp ones (scene-data generates them); the file's facet normals are ignored,
//! as exporters often leave them zero or wrong.

use std::collections::HashMap;

use anyhow::{Context, bail};
use scene_data::glam::{Mat4, Vec3};
use scene_data::{CpuScene, Indices, MeshData, Node};
use anyhow::Result;
use libfileparser_core::ParseContext;

/// Above this many triangles corners aren't welded: the hash map would cost more
/// time and memory than flat shading is worth.
const WELD_LIMIT: usize = 5_000_000;

pub struct Loaded {
    pub scene: CpuScene,
    pub binary: bool,
    /// The model's name from an ASCII `solid` line, if any.
    pub name: Option<String>,
}

/// Whether `bytes` are a binary STL: an 80-byte header, a triangle count, and exactly
/// 50 bytes per triangle. Checked by size, since binary files may start with "solid".
pub fn is_binary(bytes: &[u8]) -> bool {
    let Some(count) = bytes.get(80..84) else { return false };
    let count = u32::from_le_bytes(count.try_into().unwrap()) as u64;
    84 + count * 50 == bytes.len() as u64
}

/// Whether `header` starts like an ASCII STL.
pub fn looks_ascii(header: &[u8]) -> bool {
    let text = header.trim_ascii_start();
    text.starts_with(b"solid") && header.windows(5).any(|w| w == b"facet")
}

pub fn load(bytes: &[u8], cx: &ParseContext) -> Result<Loaded> {
    let binary = is_binary(bytes);
    let (positions, name) = if binary { (read_binary(bytes), None) } else { read_ascii(bytes)? };
    if positions.is_empty() {
        bail!("no triangles");
    }
    let mesh = if positions.len() / 3 <= WELD_LIMIT { weld(positions) } else { MeshData::new(positions) };
    let (scene, mut builder) = crate::build::new_scene(cx);
    let id = builder.add_mesh(mesh).context("the mesh")?;
    // Z up (CAD, slicers) to the scene's Y up.
    let z_up = Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    builder.add_node(Node::mesh(id, None, z_up, None)).context("the mesh")?;
    builder.finish();
    Ok(Loaded { scene, binary, name })
}

fn read_binary(bytes: &[u8]) -> Vec<Vec3> {
    // `is_binary` checked the size, so every record is there.
    let mut positions = Vec::with_capacity((bytes.len() - 84) / 50 * 3);
    for tri in bytes[84..].as_chunks::<50>().0 {
        // 12 bytes of normal, three 12-byte corners, 2 bytes of attributes.
        for corner in tri[12..48].as_chunks::<12>().0 {
            let f = |i: usize| f32::from_le_bytes(corner[i..i + 4].try_into().unwrap());
            positions.push(Vec3::new(f(0), f(4), f(8)));
        }
    }
    positions
}

/// `solid name` … `facet normal … outer loop vertex x y z ×3 endloop endfacet` …
/// Only `vertex` lines matter; they are taken three at a time.
fn read_ascii(bytes: &[u8]) -> Result<(Vec<Vec3>, Option<String>)> {
    let text = String::from_utf8_lossy(bytes);
    let mut positions = Vec::new();
    let mut name = None;
    for (n, line) in text.lines().enumerate() {
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            Some("vertex") => {
                let mut v = [0f32; 3];
                for c in &mut v {
                    *c = words.next().and_then(|w| w.parse().ok()).with_context(|| format!("line {}: bad vertex", n + 1))?;
                }
                positions.push(Vec3::from(v));
            }
            Some("solid") if name.is_none() => {
                let rest = line.trim_start()["solid".len()..].trim();
                name = (!rest.is_empty()).then(|| rest.to_string());
            }
            _ => {}
        }
    }
    // A cut-off file: keep the whole triangles.
    positions.truncate(positions.len() / 3 * 3);
    Ok((positions, name))
}

/// Indexes the corners, merging ones at exactly the same position. Bit patterns are
/// compared, with -0.0 counted as 0.0; exporters write shared corners identically.
fn weld(corners: Vec<Vec3>) -> MeshData {
    let mut index: HashMap<[u32; 3], u32> = HashMap::with_capacity(corners.len() / 4);
    let mut positions = Vec::with_capacity(corners.len() / 4);
    let mut indices = Vec::with_capacity(corners.len());
    for p in corners {
        let key = (p + Vec3::ZERO).to_array().map(f32::to_bits);
        let i = *index.entry(key).or_insert_with(|| {
            positions.push(p);
            positions.len() as u32 - 1
        });
        indices.push(i);
    }
    let indices = if positions.len() <= u16::MAX as usize + 1 {
        Indices::U16(indices.into_iter().map(|i| i as u16).collect::<Vec<_>>().into())
    } else {
        Indices::U32(indices.into())
    };
    MeshData::new(positions).with_indices(indices)
}

// ---------------------------------------------------------------- the reader

use libfileparser_core::{Category, Confidence, Detection, Document, FileSource, Reader, ReaderInfo};

use crate::build::SceneDocument;

pub struct StlReader;

static STL: ReaderInfo = ReaderInfo::new("stl", "STL model", Category::ThreeD).extensions(&["stl"]).mime_types(&["model/stl"]);

impl Reader for StlReader {
    fn info(&self) -> &ReaderInfo {
        &STL
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        // Binary STL has no magic number; ASCII starts with `solid` (as do some binary
        // headers, so the size check in `load` decides).
        let d = if STL.matches_extension(name) {
            Detection::new(Confidence::Extension)
        } else if looks_ascii(header) {
            Detection::new(Confidence::Content)
        } else {
            Detection::NONE
        };
        d.with_language("stl")
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> libfileparser_core::Result<Document> {
        cx.cancel.check()?;
        let loaded = load(src.bytes(), cx).map_err(super::parse_error)?;
        let format = if loaded.binary { "STL (binary)" } else { "STL (ASCII)" };
        let doc = SceneDocument::new(self, src, format, loaded.scene).build()?;
        Ok(match loaded.name {
            Some(name) => doc.with_meta("Name", name),
            None => doc,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the default context, as the tests below were written.
    fn load(bytes: &[u8]) -> Result<Loaded> {
        super::load(bytes, &ParseContext::new())
    }

    /// A unit cube's 12 triangles as corner lists.
    pub(crate) fn cube() -> Vec<[Vec3; 3]> {
        let c = |i: u32| Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32);
        let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        quads.iter().flat_map(|q| [[c(q[0]), c(q[1]), c(q[2])], [c(q[0]), c(q[2]), c(q[3])]]).collect()
    }

    pub(crate) fn binary_stl(tris: &[[Vec3; 3]]) -> Vec<u8> {
        // A header that starts with "solid", as some exporters write: still binary.
        let mut out = b"solid but actually binary".to_vec();
        out.resize(80, 0);
        out.extend((tris.len() as u32).to_le_bytes());
        for t in tris {
            out.extend([0u8; 12]);
            for v in t {
                v.to_array().iter().for_each(|f| out.extend(f.to_le_bytes()));
            }
            out.extend([0u8; 2]);
        }
        out
    }

    fn info(l: &Loaded) -> (usize, u64) {
        let read = l.scene.read();
        let m = &read.meshes()[0].info;
        (m.vertices, m.primitives)
    }

    #[test]
    fn binary_and_ascii_cubes_weld_to_eight_corners() {
        let bin = binary_stl(&cube());
        assert!(is_binary(&bin));
        let l = load(&bin).unwrap();
        assert!(l.binary);
        // 8 corners, but hard 90° edges split them for flat sides: 24 after normals.
        assert_eq!(info(&l), (24, 12));

        let mut ascii = String::from("solid cube\n");
        for t in cube() {
            ascii += "facet normal 0 0 0\nouter loop\n";
            for v in t {
                ascii += &format!("vertex {} {} {}\n", v.x, v.y, v.z);
            }
            ascii += "endloop\nendfacet\n";
        }
        ascii += "endsolid cube\n";
        assert!(!is_binary(ascii.as_bytes()) && looks_ascii(ascii.as_bytes()));
        let l = load(ascii.as_bytes()).unwrap();
        assert_eq!((l.binary, l.name.as_deref()), (false, Some("cube")));
        assert_eq!(info(&l), (24, 12));
    }

    #[test]
    fn z_up_models_stand_up() {
        // A tall thin box along Z becomes tall along Y.
        let tris: Vec<[Vec3; 3]> = cube().iter().map(|t| t.map(|v| v * Vec3::new(1.0, 1.0, 10.0))).collect();
        let l = load(&binary_stl(&tris)).unwrap();
        let b = l.scene.read().bounds().unwrap();
        assert!((b.size().y - 10.0).abs() < 1e-4, "{b:?}");
    }

    #[test]
    fn broken_files_fail_or_keep_whole_triangles() {
        assert!(load(b"solid empty\nendsolid\n").is_err());
        assert!(load(b"solid x\nfacet\nouter loop\nvertex 1 nope 3\n").is_err());
        // Cut off mid-triangle: the whole ones stay.
        let l = load(b"solid x\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nvertex 5 5 5\n").unwrap();
        assert_eq!(info(&l).1, 1);
        // Binary with a count that doesn't match the size is not read as binary.
        let mut bin = binary_stl(&cube());
        bin[80] = 200;
        assert!(!is_binary(&bin));
    }
}
