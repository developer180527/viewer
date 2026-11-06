//! The pieces every 3D reader is made of, public so a reader written outside this
//! crate is as short as one inside it (docs/design/libfileparser-3d.md §4): a scene
//! that keeps to the context's limits, the document every scene becomes, background
//! texture decoding, and texture lookup through the files hook.
//!
//! A reader of a new format parses its file, fills the scene from [`new_scene`],
//! queues its images with [`decode_textures`] and returns a [`SceneDocument`]:
//!
//! ```ignore
//! fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
//!     let (scene, mut builder) = build::new_scene(cx);
//!     let mesh = builder.add_mesh(MeshData::new(positions)).map_err(|e| build::scene_error(&e))?;
//!     builder.add_node(Node::mesh(mesh, None, Mat4::IDENTITY, None)).map_err(|e| build::scene_error(&e))?;
//!     builder.finish();
//!     build::SceneDocument::new("off", "OFF model", scene).build()
//! }
//! ```

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use libfileparser_core::{
    Category, Confidence, Content, Dependencies, DependencyStatus, Detection, Document, FormatInfo, Limit, ParseContext, ParseError, ReaderInfo,
    Result, Role, SceneDoc, Unreadable,
};
use scene_data::{CpuScene, SceneBuilder, SceneLimits, SceneRead, Topology};

pub use crate::textures::{ImageJob, ImageSource, MAX_SIDE as MAX_TEXTURE_SIDE, find as find_texture, spawn as decode_textures};

/// A scene builder that enforces the context's limits as it goes, so a file can't
/// make it allocate past them.
pub fn new_scene(cx: &ParseContext) -> (CpuScene, SceneBuilder) {
    let l = &cx.limits;
    let d = SceneLimits::default();
    let small = |v: u32| v.min(l.items.min(u32::MAX as u64) as u32);
    CpuScene::with_limits(SceneLimits {
        max_triangles: d.max_triangles.min(l.items),
        max_drawn_triangles: d.max_drawn_triangles.min(l.items.saturating_mul(16)),
        max_vertices: d.max_vertices.min(l.items),
        max_bytes: d.max_bytes.min(l.memory),
        max_node_depth: d.max_node_depth.min(l.depth.min(u32::MAX as u64) as u32),
        max_nodes: small(d.max_nodes),
        max_meshes: small(d.max_meshes),
        max_materials: small(d.max_materials),
        max_textures: small(d.max_textures),
        ..d
    })
}

/// A scene builder's error as a [`ParseError`]: a limit is `TooLarge` (the file may
/// be fine, the app chose to stop there), anything else is damage.
pub fn scene_error(e: &scene_data::Error) -> ParseError {
    match e {
        scene_data::Error::LimitExceeded { limit, .. } => ParseError::too_large(Limit::Items, limit.saturating_add(1), *limit),
        other => ParseError::corrupt(other),
    }
}

/// Detection by a signature the reader checked, else by extension.
pub fn detect_by(info: &ReaderInfo, magic: bool, name: &str) -> Detection {
    if magic {
        Detection::new(Confidence::Magic)
    } else if info.matches_extension(name) {
        Detection::new(Confidence::Extension)
    } else {
        Detection::NONE
    }
}

/// The document a loaded scene becomes, the same for every format: its Scene facet,
/// the counts an app shows (meshes, triangles, lines, points, vertices, materials),
/// the reader's warnings, its textures and the files it depended on.
///
/// A scene with nothing to draw is an error, so a file that only looks like a model (a
/// compiler's `.obj`) falls through to the next reader; when that's because a file it
/// needs couldn't be read, the error names that file instead.
pub struct SceneDocument {
    reader: &'static str,
    format: String,
    scene: CpuScene,
    warnings: Vec<String>,
    textures: Option<(Arc<AtomicUsize>, usize)>,
    deps: Dependencies,
}

impl SceneDocument {
    /// `reader` is the reader's id; `format` names what was read ("glTF binary (GLB)").
    pub fn new(reader: &'static str, format: impl Into<String>, scene: CpuScene) -> Self {
        Self { reader, format: format.into(), scene, warnings: Vec::new(), textures: None, deps: Dependencies::new() }
    }

    /// Things the reader skipped or guessed; the first 20 are kept.
    pub fn warnings(mut self, warnings: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.warnings.extend(warnings.into_iter().map(Into::into));
        self
    }

    /// The textures being decoded: the count [`decode_textures`] returned and how many
    /// were queued.
    pub fn textures(mut self, pending: Arc<AtomicUsize>, queued: usize) -> Self {
        self.textures = Some((pending, queued));
        self
    }

    /// Every other file the reader asked for, and what became of it.
    pub fn dependencies(mut self, deps: Dependencies) -> Self {
        self.deps = deps;
        self
    }

    pub fn build(self) -> Result<Document> {
        let Self { reader, format, scene, warnings, textures, deps } = self;
        let read = scene.read();
        let stats = Stats::of(&read);
        if stats.placed == 0 {
            for status in [DependencyStatus::Blocked, DependencyStatus::Missing] {
                if let Some(reference) = deps.with_status(status).into_iter().next() {
                    let reason = if status == DependencyStatus::Blocked { Unreadable::Blocked } else { Unreadable::Missing };
                    return Err(ParseError::Unreadable { reference, reason });
                }
            }
            return Err(ParseError::corrupt(format!("no geometry{}", warnings.first().map(|w| format!(" ({w})")).unwrap_or_default())));
        }
        drop(read);
        let (pending, queued) = match textures {
            Some((pending, queued)) => (pending, Some(queued)),
            None => (Arc::default(), None),
        };
        let mut doc = Document::new(FormatInfo::new(reader, format, Category::ThreeD))
            .with_facet(Role::Primary, "Scene", Content::Scene(SceneDoc::new(scene, pending)))
            .with_dependencies(deps)
            .with_meta("Meshes", stats.meshes)
            .with_meta("Placed meshes", stats.placed);
        // Points and lines are counted as what they are, not as triangles.
        for (key, n) in [("Triangles", stats.triangles), ("Lines", stats.lines), ("Points", stats.points)] {
            if n > 0 {
                doc = doc.with_meta(key, n);
            }
        }
        doc = doc.with_meta("Vertices", stats.vertices).with_meta("Materials", stats.materials);
        for w in warnings.iter().take(20) {
            doc = doc.with_meta("Warning", w);
        }
        if warnings.len() > 20 {
            doc = doc.with_meta("Warning", format!("…and {} more", warnings.len() - 20));
        }
        if let Some(queued) = queued {
            let note = match queued {
                0 => "none".into(),
                n => format!("{n} map(s), up to {MAX_TEXTURE_SIDE} px"),
            };
            doc = doc.with_meta("Textures", note);
        }
        Ok(doc)
    }
}

struct Stats {
    meshes: usize,
    placed: usize,
    triangles: u64,
    lines: u64,
    points: u64,
    vertices: u64,
    materials: usize,
}

impl Stats {
    fn of(read: &SceneRead<'_>) -> Self {
        let primitives = |topology| read.meshes().iter().filter(|m| m.info.topology == topology).map(|m| m.info.primitives).sum();
        Self {
            meshes: read.meshes().len(),
            placed: read.nodes().iter().filter(|n| n.node.mesh.is_some()).count(),
            triangles: primitives(Topology::Triangles),
            lines: primitives(Topology::Lines),
            points: primitives(Topology::Points),
            vertices: read.meshes().iter().map(|m| m.info.vertices as u64).sum(),
            materials: read.materials().len(),
        }
    }
}
