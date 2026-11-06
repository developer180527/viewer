//! Any 3D file into a document (docs/design/libfileparser-3d.md): glTF/GLB/VRM, FBX,
//! OBJ, STL, PLY and USD/USDZ as a RendererX CPU scene, their textures decoded in the
//! background; `.blend` files as their saved preview image.
//!
//! The API is the shape every libfileparser category has: [`readers`], [`detect`],
//! [`open`] and [`open_with`]. Other files (buffers, textures, layers, `.mtl` files) are
//! read only through the context's files hook and recorded in the document's
//! dependencies; texture decoding runs as background jobs through its spawn hook, and
//! each texture that arrives is announced through its notify hook.

mod blend;
mod ply;
mod stl;
mod textures;
mod ufbx_scene;
mod usd;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use libfileparser_core::{
    Category, Confidence, Content, Dependencies, DependencyStatus, Detection, Document, FileSource, FormatInfo, Limit, ParseContext,
    ParseError, Reader, ReaderInfo, Result, Role, SceneDoc, Unreadable,
};
use rendererx_scene::{CpuScene, SceneBuilder, SceneLimits, SceneRead, Topology};

/// Every format this category reads, in the order ties are broken.
pub fn readers() -> &'static [&'static dyn Reader] {
    &[&GltfReader, &FbxReader, &ObjReader, &StlReader, &PlyReader, &UsdReader, &BlendReader]
}

/// The readers that fit a file, best first.
pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
    libfileparser_core::detect(readers(), header, name)
}

/// Opens `src` with the best reader that succeeds.
pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open(readers(), src, cx)
}

/// Opens `src` with the reader whose id is `id`.
pub fn open_with(id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open_with(readers(), id, src, cx)
}

// ---------------------------------------------------------------- shared

/// A scene builder that enforces the document's limits as it goes, so a file can't
/// make it allocate past them.
pub(crate) fn new_scene(cx: &ParseContext) -> (CpuScene, SceneBuilder) {
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

/// A reader's internal error as a [`ParseError`]: a scene limit becomes `TooLarge`,
/// cancellation stays cancellation, anything else is damage.
fn parse_error(e: anyhow::Error) -> ParseError {
    for cause in e.chain() {
        if let Some(rendererx_scene::Error::LimitExceeded { limit, .. }) = cause.downcast_ref::<rendererx_scene::Error>() {
            return ParseError::too_large(Limit::Items, limit.saturating_add(1), *limit);
        }
        if let Some(rendererx_gltf::Error::Scene(rendererx_scene::Error::LimitExceeded { limit, .. })) = cause.downcast_ref::<rendererx_gltf::Error>() {
            return ParseError::too_large(Limit::Items, limit.saturating_add(1), *limit);
        }
        if let Some(p) = cause.downcast_ref::<ParseError>()
            && !p.try_next_reader()
        {
            return match p {
                ParseError::TooLarge(l) => ParseError::too_large(l.limit, l.value, l.max),
                ParseError::Cancelled => ParseError::Cancelled,
                other => ParseError::corrupt(other),
            };
        }
    }
    ParseError::corrupt(format!("{e:#}"))
}

/// The document for a loaded scene: its Scene facet first, with the counts the info
/// panel shows. A scene with nothing to draw is an error, so a file that only looks
/// like a model (a compiler's `.obj`) falls through to the next reader.
fn scene_document(
    reader: &'static str,
    format: &str,
    scene: CpuScene,
    warnings: &[String],
    pending: Arc<AtomicUsize>,
    deps: Dependencies,
) -> Result<Document> {
    let read = scene.read();
    let stats = Stats::of(&read);
    if stats.placed == 0 {
        // Nothing to show because a file it needs couldn't be read: say which, so the
        // app can offer to open the folder, instead of calling the model damaged.
        for status in [DependencyStatus::Blocked, DependencyStatus::Missing] {
            if let Some(reference) = deps.with_status(status).into_iter().next() {
                let reason = if status == DependencyStatus::Blocked { Unreadable::Blocked } else { Unreadable::Missing };
                return Err(ParseError::Unreadable { reference, reason });
            }
        }
        return Err(ParseError::corrupt(format!("no geometry{}", warnings.first().map(|w| format!(" ({w})")).unwrap_or_default())));
    }
    drop(read);
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
    Ok(doc)
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
        Self {
            meshes: read.meshes().len(),
            placed: read.nodes().iter().filter(|n| n.node.mesh.is_some()).count(),
            triangles: primitives(read, Topology::Triangles),
            lines: primitives(read, Topology::Lines),
            points: primitives(read, Topology::Points),
            vertices: read.meshes().iter().map(|m| m.info.vertices as u64).sum(),
            materials: read.materials().len(),
        }
    }
}

fn primitives(read: &SceneRead<'_>, topology: Topology) -> u64 {
    read.meshes().iter().filter(|m| m.info.topology == topology).map(|m| m.info.primitives).sum()
}

fn texture_note(maps: usize) -> String {
    match maps {
        0 => "none".into(),
        n => format!("{n} map(s), up to {} px", textures::MAX_SIDE),
    }
}

fn detect_by(info: &ReaderInfo, magic: bool, name: &str) -> Detection {
    if magic {
        Detection::new(Confidence::Magic)
    } else if info.matches_extension(name) {
        Detection::new(Confidence::Extension)
    } else {
        Detection::NONE
    }
}

// ---------------------------------------------------------------- glTF

pub struct GltfReader;

static GLTF: ReaderInfo = ReaderInfo::new("gltf", "glTF model", Category::ThreeD)
    .extensions(&["gltf", "glb", "vrm"])
    .mime_types(&["model/gltf+json", "model/gltf-binary"]);

/// glTF's buffers and images, read through the files hook and recorded under their
/// file name. Paths that climb out of the model's folder are refused.
struct GltfFiles<'a> {
    from: &'a FileSource,
    cx: &'a ParseContext,
    deps: &'a Dependencies,
}

impl rendererx_gltf::Resolver for GltfFiles<'_> {
    fn resolve(&self, uri: &str) -> std::result::Result<rendererx_scene::Bytes, rendererx_gltf::ResolveError> {
        use rendererx_gltf::{ResolveError, ResolveErrorKind};
        let path = rendererx_gltf::relative_path(uri)?;
        let name = textures::report_name(&path, "");
        match self.deps.resolve_any(self.cx, self.from, &name, &[path.as_str()]) {
            Some(file) if file.len() > self.cx.limits.allocation => {
                Err(ResolveError::new(ResolveErrorKind::Refused, format!("{uri:?}: {} bytes is over the limit", file.len())))
            }
            Some(file) => Ok(file.shared_bytes()),
            None => {
                let blocked = self.deps.with_status(libfileparser_core::DependencyStatus::Blocked).contains(&name);
                let kind = if blocked { ResolveErrorKind::NotPermitted } else { ResolveErrorKind::NotFound };
                Err(ResolveError::new(kind, format!("{uri:?} couldn't be read")))
            }
        }
    }
}

impl Reader for GltfReader {
    fn info(&self) -> &ReaderInfo {
        &GLTF
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        detect_by(&GLTF, header.starts_with(b"glTF"), name).with_language("json")
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let deps = Dependencies::new();
        let (scene, mut builder) = new_scene(cx);
        let files = GltfFiles { from: src, cx, deps: &deps };
        let loaded = rendererx_gltf::load(src.shared_bytes(), &files, &mut builder).map_err(|e| parse_error(e.into()))?;
        let format = if src.header().starts_with(b"glTF") { "glTF binary (GLB)" } else { "glTF 2.0" };
        let jobs: Vec<_> = loaded
            .images
            .into_iter()
            .map(|i| textures::ImageJob { texture: i.texture, source: textures::ImageSource::Bytes(i.data), color_space: i.color_space })
            .collect();
        let decoding = jobs.len();
        let pending = textures::spawn(builder.texture_sink(), jobs, cx);
        Ok(scene_document("gltf", format, scene, &loaded.warnings, pending, deps)?.with_meta("Textures", texture_note(decoding)))
    }
}

// ---------------------------------------------------------------- USD

pub struct UsdReader;

static USD: ReaderInfo =
    ReaderInfo::new("usd", "USD scene", Category::ThreeD).extensions(&["usd", "usda", "usdc", "usdz"]).mime_types(&["model/vnd.usdz+zip"]);

impl Reader for UsdReader {
    fn info(&self) -> &ReaderInfo {
        &USD
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        let ext = |e: &str| name.rsplit_once('.').is_some_and(|(_, x)| x.eq_ignore_ascii_case(e));
        let d = match usd_lite::detect(header) {
            // A zip is only USD when it says so, and then it beats the zip reader.
            Some(usd_lite::Format::Usdz) if ext("usdz") => Detection::new(Confidence::Certain),
            Some(usd_lite::Format::Usdz) => Detection::NONE,
            Some(_) => Detection::new(Confidence::Magic),
            None => detect_by(&USD, false, name),
        };
        if usd_lite::detect(header) == Some(usd_lite::Format::Usda) || (d.is_match() && ext("usda")) { d.with_language("usd") } else { d }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let deps = Dependencies::new();
        let loaded = usd::load(src, cx, &deps).map_err(parse_error)?;
        let note = texture_note(loaded.textures);
        let doc = scene_document("usd", loaded.format, loaded.scene, &loaded.warnings, loaded.pending, deps)?.with_meta("Textures", note);
        Ok(if loaded.layers > 0 { doc.with_meta("Layers composed", loaded.layers + 1) } else { doc })
    }
}

// ---------------------------------------------------------------- FBX / OBJ

pub struct FbxReader;

static FBX: ReaderInfo = ReaderInfo::new("fbx", "FBX model", Category::ThreeD).extensions(&["fbx"]);

impl Reader for FbxReader {
    fn info(&self) -> &ReaderInfo {
        &FBX
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        detect_by(&FBX, header.starts_with(b"Kaydara FBX Binary"), name)
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let deps = Dependencies::new();
        let loaded = ufbx_scene::load(src, ufbx::FileFormat::Fbx, cx, &deps).map_err(parse_error)?;
        let format = if src.header().starts_with(b"Kaydara FBX Binary") { "FBX (binary)" } else { "FBX (ASCII)" };
        let note = texture_note(loaded.textures);
        Ok(scene_document("fbx", format, loaded.scene, &loaded.warnings, loaded.pending, deps)?.with_meta("Textures", note))
    }
}

pub struct ObjReader;

static OBJ: ReaderInfo = ReaderInfo::new("obj", "OBJ model", Category::ThreeD).extensions(&["obj"]).mime_types(&["model/obj"]);

impl Reader for ObjReader {
    fn info(&self) -> &ReaderInfo {
        &OBJ
    }

    fn detect(&self, _: &[u8], name: &str) -> Detection {
        // No magic: `.obj` is also a compiler object file, which fails here and falls
        // through to the next reader.
        detect_by(&OBJ, false, name).with_language("obj")
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let deps = Dependencies::new();
        let loaded = ufbx_scene::load(src, ufbx::FileFormat::Obj, cx, &deps).map_err(parse_error)?;
        let note = texture_note(loaded.textures);
        Ok(scene_document("obj", "Wavefront OBJ", loaded.scene, &loaded.warnings, loaded.pending, deps)?.with_meta("Textures", note))
    }
}

// ---------------------------------------------------------------- STL / PLY

pub struct StlReader;

static STL: ReaderInfo = ReaderInfo::new("stl", "STL model", Category::ThreeD).extensions(&["stl"]).mime_types(&["model/stl"]);

impl Reader for StlReader {
    fn info(&self) -> &ReaderInfo {
        &STL
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        // Binary STL has no magic number; ASCII starts with `solid` (as do some binary
        // headers, so the size check in `stl::load` decides).
        if STL.matches_extension(name) {
            Detection::new(Confidence::Extension)
        } else if stl::looks_ascii(header) {
            Detection::new(Confidence::Content)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let loaded = stl::load(src.bytes(), cx).map_err(parse_error)?;
        let format = if loaded.binary { "STL (binary)" } else { "STL (ASCII)" };
        let doc = scene_document("stl", format, loaded.scene, &[], Arc::default(), Dependencies::new())?;
        Ok(match loaded.name {
            Some(name) => doc.with_meta("Name", name),
            None => doc,
        })
    }
}

pub struct PlyReader;

static PLY: ReaderInfo = ReaderInfo::new("ply", "PLY model", Category::ThreeD).extensions(&["ply"]);

impl Reader for PlyReader {
    fn info(&self) -> &ReaderInfo {
        &PLY
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        detect_by(&PLY, ply::is_ply(header), name)
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let loaded = ply::load(src.bytes(), cx).map_err(parse_error)?;
        let doc = scene_document("ply", loaded.format, loaded.scene, &loaded.warnings, Arc::default(), Dependencies::new())?;
        Ok(if loaded.points { doc.with_meta("Kind", "point cloud") } else { doc })
    }
}

// ---------------------------------------------------------------- .blend

pub struct BlendReader;

static BLEND: ReaderInfo = ReaderInfo::new("blend", "Blender preview", Category::ThreeD).extensions(&["blend"]);

impl Reader for BlendReader {
    fn info(&self) -> &ReaderInfo {
        &BLEND
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        detect_by(&BLEND, header.starts_with(b"BLENDER"), name)
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let (raster, version) = blend::preview(src.bytes(), &cx.limits).map_err(parse_error)?;
        Ok(Document::new(FormatInfo::new("blend", "Blender file", Category::ThreeD))
            .with_facet(Role::Primary, "Preview", Content::Raster(raster))
            .with_meta("Saved by", format!("Blender {version}"))
            .with_meta("Shown", "the preview image saved in the file; its 3D data isn't read yet"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE_OBJ: &str = "\
o cube
v -1 -1 -1
v  1 -1 -1
v  1  1 -1
v -1  1 -1
v -1 -1  1
v  1 -1  1
v  1  1  1
v -1  1  1
usemtl missing
f 1 4 3 2
f 5 6 7 8
f 1 2 6 5
f 2 3 7 6
f 3 4 8 7
f 4 1 5 8
";

    fn scene(doc: &Document) -> CpuScene {
        let Ok(Content::Scene(s)) = doc.facet(0, &ParseContext::new()) else { panic!("expected a scene") };
        s.scene().clone()
    }

    #[test]
    fn obj_quads_are_triangulated_and_unlit_materials_stay_clay() {
        let src = FileSource::from_bytes("cube.obj", CUBE_OBJ.as_bytes().to_vec());
        let doc = ObjReader.open(&src, &ParseContext::new()).unwrap();
        let scene = scene(&doc);
        let read = scene.read();
        assert_eq!(read.meshes().iter().map(|m| m.info.primitives).sum::<u64>(), 12);
        let b = read.bounds().unwrap();
        assert!((b.size() - rendererx_scene::glam::Vec3::splat(2.0)).length() < 1e-4);
        // `usemtl missing` names a material that no .mtl defines: no colour, so clay.
        assert!(read.materials().iter().all(|m| m.material.params.get("base_color").is_none()));
    }

    #[test]
    fn usd_meshes_split_by_material_in_y_up_metres() {
        let src = FileSource::from_bytes("scene.usda", include_bytes!("../../usd-lite/tests/fixtures/scene.usda").to_vec());
        assert_eq!(UsdReader.detect(src.header(), src.name()).confidence, Confidence::Magic);
        let doc = UsdReader.open(&src, &ParseContext::new()).unwrap();
        let scene = scene(&doc);
        let read = scene.read();
        // The quad (Red) and the triangle (its GeomSubset, Textured) are separate meshes;
        // the hidden prim adds nothing.
        assert_eq!(read.meshes().len(), 2);
        assert_eq!(read.materials().len(), 2);
        let names: Vec<_> = read.materials().iter().map(|m| m.material.name.as_str()).collect();
        assert!(names.contains(&"Red") && names.contains(&"Textured"), "{names:?}");
        // /World is at (10, 0, 0) cm, Z up: 0.1 m along X in Y-up metres. Points span
        // 0..1 (×2 scale, rotated 90° about the up axis), so everything stays within
        // ~0.1 m of it, and the up axis is now Y.
        let b = read.bounds().unwrap();
        assert!((b.center().x - 0.08).abs() < 0.05, "{b:?}");
        assert!(b.size().y.abs() < 1e-3, "flat quad: no height after Z-up → Y-up, {b:?}");
        // Its reference (../other.usda) doesn't exist: recorded, and the rest still loads.
        assert!(doc.dependencies().with_status(libfileparser_core::DependencyStatus::Missing).iter().any(|d| d.ends_with("other.usda")), "{:?}", doc.dependencies());
    }

    #[test]
    fn files_with_no_geometry_fall_through() {
        // A compiler object file named .obj: ufbx reads it as an empty OBJ, which must
        // fail so the front door falls back to the next reader (hex).
        let coff = FileSource::from_bytes("main.obj", vec![0x4c, 0x01, 0x03, 0x00, 0, 0, 0, 0, 0xff, 0xfe, 0x00]);
        assert!(ObjReader.open(&coff, &ParseContext::new()).err().expect("an empty OBJ fails").try_next_reader());
    }

    #[test]
    fn a_usdz_outranks_a_plain_zip_and_text_formats_name_their_language() {
        let usdz = b"PK\x03\x04";
        assert_eq!(UsdReader.detect(usdz, "a.usdz").confidence, Confidence::Certain);
        assert!(!UsdReader.detect(usdz, "a.zip").is_match(), "a plain zip isn't USD");
        assert_eq!(UsdReader.detect(b"#usda 1.0\n", "a.usda").language, Some("usd"));
        assert_eq!(GltfReader.detect(b"{", "a.gltf").language, Some("json"));
    }
}
