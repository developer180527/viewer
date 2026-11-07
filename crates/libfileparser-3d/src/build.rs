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
//!     build::SceneDocument::new(self, src, "OFF model", scene).build()
//! }
//! ```

use std::ops::Range;
use std::sync::Arc;

use std::collections::HashMap;

use libfileparser_core::{
    Category, Confidence, Content, Count, Dependencies, DependencyStatus, Detection, Document, FacetInfo, FormatInfo, Limit, Links, Model,
    FileSource, ParseContext, ParseError, Place, Reader, ReaderInfo, Result, Role, SceneDoc, SceneItem, TableInfo, TableSource, Unreadable,
};
use scene_data::{AlphaMode, ColorSpace, CpuScene, ParamValue, SceneBuilder, SceneLimits, SceneRead, ShadingSource, Topology};

pub use crate::textures::{ImageJob, ImageSource, MAX_SIDE as MAX_TEXTURE_SIDE, Textures, find as find_texture, spawn as decode_textures};

/// A scene builder that enforces the context's limits as it goes, so a file can't
/// make it allocate past them.
pub fn new_scene(cx: &ParseContext) -> (CpuScene, SceneBuilder) {
    let l = &cx.limits;
    let d = SceneLimits::default();
    let small = |v: u32| v.min(l.items.min(u32::MAX as u64) as u32);
    let mut s = SceneLimits::default();
    s.max_triangles = d.max_triangles.min(l.items);
    s.max_drawn_triangles = d.max_drawn_triangles.min(l.items.saturating_mul(16));
    s.max_vertices = d.max_vertices.min(l.items);
    s.max_bytes = d.max_bytes.min(l.memory);
    s.max_node_depth = d.max_node_depth.min(l.depth.min(u32::MAX as u64) as u32);
    s.max_nodes = small(d.max_nodes);
    s.max_meshes = small(d.max_meshes);
    s.max_materials = small(d.max_materials);
    s.max_textures = small(d.max_textures);
    CpuScene::with_limits(s)
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
/// the format's own facets ([`Self::facets`]), a Dependencies facet when it read or
/// looked for other files, Materials and Textures facets when it has them, then the
/// file's Source (when it is text, in the language the reader detects) and Raw; the
/// counts an app shows (meshes, triangles, lines, points, vertices, materials), the
/// reader's warnings, and the links between them all.
///
/// A scene with nothing to draw is an error, so a file that only looks like a model (a
/// compiler's `.obj`) falls through to the next reader; when that's because a file it
/// needs couldn't be read, the error names that file instead.
pub struct SceneDocument {
    reader: &'static str,
    src: FileSource,
    language: Option<&'static str>,
    format: String,
    scene: CpuScene,
    warnings: Vec<String>,
    textures: Option<Textures>,
    deps: Dependencies,
    facets: Option<Box<dyn FnOnce(Document) -> Document>>,
    origins: Option<(SceneOrigins, &'static str)>,
}

impl SceneDocument {
    /// `reader` read `src`; `format` names what was read ("glTF binary (GLB)").
    pub fn new(reader: &dyn Reader, src: &FileSource, format: impl Into<String>, scene: CpuScene) -> Self {
        let language = reader.detect(src.header(), src.name()).language;
        Self {
            reader: reader.info().id,
            src: src.clone(),
            language,
            format: format.into(),
            scene,
            warnings: Vec::new(),
            textures: None,
            deps: Dependencies::new(),
            facets: None,
            origins: None,
        }
    }

    /// Things the reader skipped or guessed; the first 20 are kept.
    pub fn warnings(mut self, warnings: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.warnings.extend(warnings.into_iter().map(Into::into));
        self
    }

    /// The textures being decoded, as [`decode_textures`] returned them.
    pub fn textures(mut self, textures: Textures) -> Self {
        self.textures = Some(textures);
        self
    }

    /// Every other file the reader asked for, and what became of it.
    pub fn dependencies(mut self, deps: Dependencies) -> Self {
        self.deps = deps;
        self
    }

    /// The format's own facets (its structure, say), added right after the Scene.
    pub fn facets(mut self, add: impl FnOnce(Document) -> Document + 'static) -> Self {
        self.facets = Some(Box::new(add));
        self
    }

    /// Where the scene's items came from, keyed by their path in the facet `facet`
    /// (one of the format's own, a tree): links that facet's nodes and the Scene.
    pub fn origins(mut self, origins: SceneOrigins, facet: &'static str) -> Self {
        self.origins = Some((origins, facet));
        self
    }

    pub fn build(self) -> Result<Document> {
        let Self { reader, src, language, format, scene, warnings, textures, deps, facets, origins } = self;
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
        let queued = textures.as_ref().map(Textures::len);
        let textures = textures.unwrap_or_default();
        let mut doc = Document::new(FormatInfo::new(reader, format, Category::ThreeD))
            .with_facet(Role::Primary, "Scene", Content::Scene(SceneDoc::new(scene.clone(), textures.counter())))
            .with_dependencies(deps.clone());
        if let Some(add) = facets {
            doc = add(doc);
        }
        if !deps.is_empty() {
            doc = doc.with_dependencies_facet();
        }
        doc = doc.with_links(SceneLinks { scene: scene.clone(), textures: textures.clone() });
        if let Some((origins, facet)) = origins {
            doc = doc.with_links(OriginLinks { origins, facet });
        }
        let materials = MaterialTable { scene: scene.clone(), textures: textures.clone() };
        if materials.scene.read().materials().iter().any(|m| m.material.source != ShadingSource::Default) {
            let table = Arc::new(materials);
            doc = doc.with_lazy_facet(FacetInfo::new(Role::Inspector, "Materials", Model::Table), move |_| Ok(Content::Table(table.clone())));
        }
        let table = TextureTable { scene, textures, deps };
        if !table.all().is_empty() {
            let table = Arc::new(table);
            doc = doc.with_lazy_facet(FacetInfo::new(Role::Inspector, "Textures", Model::Table), move |_| Ok(Content::Table(table.clone())));
        }
        doc = doc
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
        Ok(doc.with_file_facets(&src, language))
    }
}

/// Whether a material is one of the file's own, and so a row of Materials.
fn shown(m: &scene_data::MaterialEntry) -> bool {
    m.material.source != ShadingSource::Default
}

/// Where a scene's items came from in the file, keyed by the path of what describes
/// them in the format's structure facet: a USD prim path, a glTF JSON pointer
/// (`/nodes/3`). One key may give several items (a prim drawn as one node per
/// material), and an item has one key.
#[derive(Default, Clone)]
pub struct SceneOrigins {
    by_key: HashMap<String, Vec<SceneItem>>,
    by_item: HashMap<SceneItem, String>,
}

impl SceneOrigins {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, key: impl Into<String>, item: SceneItem) {
        let key = key.into();
        if self.by_item.contains_key(&item) {
            return;
        }
        self.by_item.insert(item, key.clone());
        self.by_key.entry(key).or_default().push(item);
    }

    pub fn node(&mut self, key: impl Into<String>, id: scene_data::NodeId) {
        self.add(key, SceneItem::Node(id.index() as u32));
    }

    pub fn mesh(&mut self, key: impl Into<String>, id: scene_data::MeshId) {
        self.add(key, SceneItem::Mesh(id.index() as u32));
    }

    pub fn material(&mut self, key: impl Into<String>, id: scene_data::MaterialId) {
        self.add(key, SceneItem::Material(id.index() as u32));
    }

    /// The items `key` gave.
    pub fn items(&self, key: &str) -> &[SceneItem] {
        self.by_key.get(key).map_or(&[], Vec::as_slice)
    }

    /// The key `item` came from.
    pub fn key(&self, item: SceneItem) -> Option<&str> {
        self.by_item.get(&item).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.by_item.is_empty()
    }
}

/// Links between the Scene and the format's structure facet, through [`SceneOrigins`].
struct OriginLinks {
    origins: SceneOrigins,
    facet: &'static str,
}

impl Links for OriginLinks {
    fn links(&self, doc: &Document, from: &Place, cx: &ParseContext) -> Result<Vec<Place>> {
        let (Some(scene), Some(tree_facet)) = (doc.facet_index("Scene"), doc.facet_index(self.facet)) else { return Ok(Vec::new()) };
        match from {
            Place::Scene { facet, item } if *facet == scene => {
                let Some(key) = self.origins.key(*item) else { return Ok(Vec::new()) };
                let Content::Tree(tree) = doc.facet(tree_facet, cx)? else { return Ok(Vec::new()) };
                Ok(tree.find(key)?.map(|node| Place::Node { facet: tree_facet, node }).into_iter().collect())
            }
            Place::Node { facet, node } if *facet == tree_facet => {
                let Content::Tree(tree) = doc.facet(tree_facet, cx)? else { return Ok(Vec::new()) };
                let Some(key) = tree.path(*node)? else { return Ok(Vec::new()) };
                Ok(self.origins.items(&key).iter().map(|&item| Place::Scene { facet: scene, item }).collect())
            }
            _ => Ok(Vec::new()),
        }
    }
}

/// The links every 3D document has: a row of Materials and its material in the Scene
/// (and the Textures rows of its maps), a row of Textures and its texture, and a scene
/// node and its material's row.
struct SceneLinks {
    scene: CpuScene,
    textures: Textures,
}

impl SceneLinks {
    /// Material indices, in Materials row order.
    fn material_rows(&self) -> Vec<u32> {
        self.scene.read().materials().iter().enumerate().filter(|(_, m)| shown(m)).map(|(i, _)| i as u32).collect()
    }

    /// Texture indices, in Textures row order (the queued ones come first).
    fn texture_rows(&self) -> Vec<u32> {
        self.textures.rows().into_iter().map(|(id, ..)| id.index() as u32).collect()
    }

    /// The Materials row of material `m`, and the Textures rows of its maps.
    fn of_material(&self, doc: &Document, m: u32) -> Vec<Place> {
        let mut out = Vec::new();
        if let Some(facet) = doc.facet_index("Materials")
            && let Some(row) = self.material_rows().iter().position(|&i| i == m)
        {
            out.push(Place::Row { facet, row: row as u64 });
        }
        if let Some(facet) = doc.facet_index("Textures") {
            let rows = self.texture_rows();
            let read = self.scene.read();
            for (_, t) in read.materials().get(m as usize).map(|e| e.material.textures.as_slice()).unwrap_or_default() {
                if let Some(row) = rows.iter().position(|&r| r == t.index() as u32) {
                    out.push(Place::Row { facet, row: row as u64 });
                }
            }
        }
        out
    }
}

impl Links for SceneLinks {
    fn links(&self, doc: &Document, from: &Place, _: &ParseContext) -> Result<Vec<Place>> {
        let scene = doc.facet_index("Scene");
        let materials = doc.facet_index("Materials");
        let textures = doc.facet_index("Textures");
        let in_scene = |item| scene.map(|facet| Place::Scene { facet, item });
        Ok(match from {
            Place::Row { facet, row } if Some(*facet) == materials => {
                let Some(&m) = self.material_rows().get(*row as usize) else { return Ok(Vec::new()) };
                let mut out: Vec<Place> = in_scene(SceneItem::Material(m)).into_iter().collect();
                out.extend(self.of_material(doc, m).into_iter().filter(|p| p != from));
                out
            }
            Place::Row { facet, row } if Some(*facet) == textures => {
                let Some(&t) = self.texture_rows().get(*row as usize) else { return Ok(Vec::new()) };
                let mut out: Vec<Place> = in_scene(SceneItem::Texture(t)).into_iter().collect();
                // The materials that use it.
                let read = self.scene.read();
                let users: Vec<u32> = read.materials().iter().enumerate().filter(|(_, m)| m.material.textures.iter().any(|(_, id)| id.index() as u32 == t)).map(|(i, _)| i as u32).collect();
                drop(read);
                if let Some(facet) = materials {
                    let rows = self.material_rows();
                    out.extend(users.iter().filter_map(|u| rows.iter().position(|r| r == u)).map(|row| Place::Row { facet, row: row as u64 }));
                }
                out
            }
            Place::Scene { facet, item } if Some(*facet) == scene => match *item {
                SceneItem::Material(m) => self.of_material(doc, m),
                SceneItem::Texture(t) => match (textures, self.texture_rows().iter().position(|&r| r == t)) {
                    (Some(facet), Some(row)) => vec![Place::Row { facet, row: row as u64 }],
                    _ => Vec::new(),
                },
                SceneItem::Node(n) => {
                    let material = self.scene.read().nodes().get(n as usize).and_then(|e| e.node.material).map(|m| m.index() as u32);
                    match (material, materials) {
                        (Some(m), Some(facet)) => self.material_rows().iter().position(|&i| i == m).map(|row| Place::Row { facet, row: row as u64 }).into_iter().collect(),
                        _ => Vec::new(),
                    }
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        })
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

/// The Materials facet: one row per material the file has, with its shading model,
/// its maps (by texture name, as the Textures facet lists them) and its main factors.
struct MaterialTable {
    scene: CpuScene,
    textures: Textures,
}

impl MaterialTable {
    const COLUMNS: [&str; 7] = ["Material", "Shading", "Maps", "Base colour", "Metallic", "Roughness", "Alpha"];

    fn all(&self) -> Vec<Vec<String>> {
        let names: std::collections::HashMap<_, _> = self.textures.rows().into_iter().map(|(id, name, ..)| (id, name)).collect();
        let read = self.scene.read();
        read.materials()
            .iter()
            .enumerate()
            .filter(|(_, m)| shown(m))
            .map(|(i, m)| {
                let m = &m.material;
                let name = if m.name.is_empty() { format!("material {i}") } else { m.name.clone() };
                let shading = match m.source {
                    ShadingSource::Inferred => format!("{} (inferred)", m.shading.as_str()),
                    _ => m.shading.as_str().to_string(),
                };
                let maps: Vec<String> = m
                    .textures
                    .iter()
                    .map(|(slot, id)| format!("{}: {}", slot.as_str(), names.get(id).cloned().unwrap_or_else(|| format!("texture {}", id.index()))))
                    .collect();
                let param = |name: &str| m.params.get(name).map(param_text).unwrap_or_default();
                let alpha = match m.alpha {
                    AlphaMode::Opaque => "opaque".to_string(),
                    AlphaMode::Mask(cutoff) => format!("mask at {}", number(cutoff)),
                    AlphaMode::Blend => "blend".to_string(),
                };
                let alpha = if m.double_sided { format!("{alpha}, double-sided") } else { alpha };
                vec![name, shading, maps.join(", "), param("base_color"), param("metallic"), param("roughness"), alpha]
            })
            .collect()
    }
}

impl TableSource for MaterialTable {
    fn info(&self) -> TableInfo {
        TableInfo::new(Self::COLUMNS.map(String::from).to_vec(), Count::Exact(self.all().len() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<String>>> {
        Ok(slice(self.all(), range))
    }
}

fn number(v: f32) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn param_text(v: ParamValue) -> String {
    let list = |xs: &[f32]| xs.iter().map(|&x| number(x)).collect::<Vec<_>>().join(", ");
    match v {
        ParamValue::Float(x) => number(x),
        ParamValue::Int(i) => i.to_string(),
        ParamValue::Vec2(x) => list(&x.to_array()),
        ParamValue::Vec3(x) => list(&x.to_array()),
        ParamValue::Vec4(x) => list(&x.to_array()),
        _ => String::new(),
    }
}

fn slice(all: Vec<Vec<String>>, range: Range<u64>) -> Vec<Vec<String>> {
    let end = (range.end.min(all.len() as u64)) as usize;
    let start = (range.start as usize).min(end);
    all[start..end].to_vec()
}

/// The Textures facet: one row per texture the scene uses, as it is now (decoding,
/// decoded at its size, failed), then each texture file it names but couldn't read.
struct TextureTable {
    scene: CpuScene,
    textures: Textures,
    deps: Dependencies,
}

impl TextureTable {
    const COLUMNS: [&str; 5] = ["Texture", "From", "Size", "Colour space", "Status"];

    fn all(&self) -> Vec<Vec<String>> {
        let read = self.scene.read();
        let mut rows: Vec<Vec<String>> = self
            .textures
            .rows()
            .into_iter()
            .map(|(id, name, origin, space, state)| {
                let size = read.texture(id).and_then(|t| t.info).map(|i| format!("{}×{}", i.width, i.height)).unwrap_or_default();
                let space = if space == ColorSpace::Srgb { "sRGB" } else { "linear" };
                vec![name, origin, size, space.into(), state.into()]
            })
            .collect();
        for d in self.deps.list() {
            if d.kind == "texture" && d.status != DependencyStatus::Read {
                rows.push(vec![d.reference, "file".into(), String::new(), String::new(), d.status.label().into()]);
            }
        }
        rows
    }
}

impl TableSource for TextureTable {
    fn info(&self) -> TableInfo {
        TableInfo::new(Self::COLUMNS.map(String::from).to_vec(), Count::Exact(self.all().len() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<String>>> {
        Ok(slice(self.all(), range))
    }
}
