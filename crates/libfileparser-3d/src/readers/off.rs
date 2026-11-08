//! OFF (Object File Format): `OFF`, `COFF` (vertex colours), `NOFF` (normals), `STOFF`
//! and their combinations, in text. A file with no faces is a point cloud.

use libfileparser_core::{Category, Confidence, Detection, Document, FileSource, Limit, ParseContext, ParseError, Reader, ReaderInfo, Result};
use scene_data::glam::{Mat4, Vec3, Vec4};
use scene_data::{Indices, MeshData, Node, Topology};

use crate::build::{self, SceneDocument};

static OFF: ReaderInfo = ReaderInfo::new("off", "OFF model", Category::ThreeD).extensions(&["off", "coff", "noff"]);

pub struct OffReader;

/// Which optional columns a vertex has, from a header word like `CNOFF`.
fn keyword(word: &str) -> Option<(bool, bool, bool)> {
    let flags = word.strip_suffix("OFF")?;
    flags.chars().all(|c| matches!(c, 'S' | 'T' | 'C' | 'N')).then(|| (flags.contains("ST"), flags.contains('C'), flags.contains('N')))
}

fn first_word(header: &[u8]) -> Option<&str> {
    let end = header.iter().position(u8::is_ascii_whitespace).unwrap_or(header.len());
    std::str::from_utf8(&header[..end]).ok()
}

/// A colour's channels: floats from 0 to 1, or integers to 255.
fn colour(c: &[f32]) -> Option<Vec4> {
    let scale = if c.iter().all(|v| *v == v.trunc()) && c.iter().any(|v| *v > 1.0) { 255.0 } else { 1.0 };
    match *c {
        [r, g, b] => Some(Vec4::new(r / scale, g / scale, b / scale, 1.0)),
        [r, g, b, a] => Some(Vec4::new(r / scale, g / scale, b / scale, a / scale)),
        _ => None,
    }
}

impl Reader for OffReader {
    fn info(&self) -> &ReaderInfo {
        &OFF
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if first_word(header).and_then(keyword).is_some() {
            Detection::new(Confidence::Magic).with_language("off")
        } else if OFF.matches_extension(name) {
            Detection::new(Confidence::Extension).with_language("off")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let text = std::str::from_utf8(src.bytes()).map_err(|_| ParseError::NotThisFormat)?;
        // Blank lines and comments carry nothing; a vertex is one line, whatever it holds.
        let mut lines = text.lines().map(|l| l.split('#').next().unwrap_or("").trim()).filter(|l| !l.is_empty());
        let bad = |what: &str| ParseError::corrupt(format!("OFF: {what}"));
        let numbers = |l: &str| l.split_ascii_whitespace().map(str::parse::<f32>).collect::<std::result::Result<Vec<_>, _>>().ok();

        // The keyword is optional, and may share its line with the counts.
        let first = lines.next().ok_or(ParseError::NotThisFormat)?;
        let mut words = first.split_ascii_whitespace();
        let word = words.next().unwrap_or("");
        let (flags, rest) = match keyword(word) {
            Some(f) => (f, words.collect::<Vec<_>>().join(" ")),
            None if word.ends_with("OFF") => return Err(ParseError::NotThisFormat),
            None => ((false, false, false), first.to_string()),
        };
        if rest.contains("BINARY") {
            return Err(ParseError::unsupported("Binary OFF files aren't supported"));
        }
        let (texture, coloured, normals) = flags;
        let counts = if rest.is_empty() { lines.next().ok_or_else(|| bad("no counts"))?.to_string() } else { rest };
        let counts = numbers(&counts).filter(|c| c.len() >= 2).ok_or(ParseError::NotThisFormat)?;
        let (vertices, faces) = (counts[0] as u64, counts[1] as u64);
        cx.limits.check(Limit::Geometry, vertices.saturating_add(faces))?;

        let (mut positions, mut colours) = (Vec::new(), Vec::new());
        for _ in 0..vertices {
            let v = numbers(lines.next().ok_or_else(|| bad("the file ends among its vertices"))?).filter(|v| v.len() >= 3).ok_or_else(|| bad("a vertex without three numbers"))?;
            positions.push(Vec3::new(v[0], v[1], v[2]));
            let extra = 3 + if normals { 3 } else { 0 };
            if coloured && let Some(c) = v.get(extra..v.len() - if texture { 2 } else { 0 }).and_then(colour) {
                colours.push(c);
            }
        }
        let mut indices = Vec::new();
        let mut segments = Vec::new();
        for _ in 0..faces {
            // Some files stop short of their count; what's there is still a model.
            let Some(line) = lines.next() else { break };
            let Some(f) = numbers(line) else { continue };
            let Some(&n) = f.first() else { continue };
            let Some(corners) = (n as usize).checked_add(1).and_then(|end| f.get(1..end)) else { continue };
            let corners: Vec<u32> = corners.iter().map(|&c| c as u32).collect();
            if corners.iter().any(|&c| c as u64 >= vertices) {
                continue;
            }
            match corners.len() {
                0 | 1 => {}
                2 => segments.extend(&corners),
                _ => (1..corners.len() - 1).for_each(|i| indices.extend([corners[0], corners[i], corners[i + 1]])),
            }
        }
        let topology = if !indices.is_empty() {
            Topology::Triangles
        } else if !segments.is_empty() {
            indices = segments;
            Topology::Lines
        } else {
            Topology::Points
        };
        let mut mesh = MeshData::new(positions);
        if colours.len() as u64 == vertices {
            mesh.colors = Some(colours.into());
        }
        mesh.topology = topology;
        if topology != Topology::Points {
            mesh = mesh.with_indices(Indices::U32(indices.into()));
        }
        let (scene, mut builder) = build::new_scene(cx);
        if vertices > 0 {
            let id = builder.add_mesh(mesh).map_err(|e| build::scene_error(&e))?;
            builder.add_node(Node::mesh(id, None, Mat4::IDENTITY, None)).map_err(|e| build::scene_error(&e))?;
        }
        builder.finish();
        SceneDocument::new(self, src, "OFF model", scene).build()
    }
}
