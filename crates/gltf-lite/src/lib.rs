//! glTF 2.0 (`.gltf` and `.glb`) into a `scene-data` scene, which RendererX draws
//! without conversion. A format library: it knows nothing of libfileparser
//! (docs/design/libfileparser.md §6).
//!
//! - **Zero-copy where possible.** Vertex and index data already in the renderer's
//!   layout (packed `FLOAT` streams, `u16`/`u32` indices) are borrowed from the file's
//!   bytes, so a memory-mapped GLB is never copied on the CPU.
//! - **Camera first.** Scene bounds come from the accessors' min/max before any mesh
//!   is read, so the host can frame the model immediately.
//! - **No image decoding.** Textures are reserved in the scene and returned as encoded
//!   [`PendingImage`]s; the host decodes them on its own threads and sets them through
//!   a [`TextureSink`](scene_data::TextureSink).
//! - **Shading as authored** (docs/design/3d-renderer.md §6): core → `pbr`,
//!   `KHR_materials_unlit` → `unlit`, `VRMC_materials_mtoon` → `mtoon`.
//! - **Show what can be shown.** A broken primitive, texture or external file is a
//!   warning; only an unreadable container or JSON fails the load.

mod accessor;
mod json;

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use scene_data::glam::{Mat4, Quat, Vec3};
use scene_data::{
    Aabb, AlphaMode, Bytes, ColorSpace, Material, MaterialId, MeshData, MeshId, Node, NodeId, ParamValue, SceneBuilder, ShadingId,
    ShadingSource, Slot, Stream, TextureId, Topology,
};

use accessor::{BufferData, Reader};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("not a valid glTF file: {0}")]
    Format(String),
    #[error("needs {0}, which isn't supported")]
    Unsupported(String),
    /// The scene refused what the file holds: over its limits, or inconsistent data.
    #[error(transparent)]
    Scene(#[from] scene_data::Error),
}

/// Loads the external files a `.gltf` refers to (`.bin` buffers, images).
pub trait Resolver {
    fn resolve(&self, uri: &str) -> Result<Bytes, ResolveError>;
}

/// Why an external file couldn't be loaded. The kind lets a host tell the user what
/// would help: [`NotPermitted`](ResolveErrorKind::NotPermitted) (grant the folder)
/// from [`NotFound`](ResolveErrorKind::NotFound) (the file wasn't copied along).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    pub kind: ResolveErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveErrorKind {
    /// It may be there, but reading it isn't allowed (a sandbox, file modes).
    NotPermitted,
    NotFound,
    /// Not looked for: outside the model's folder, a URL, too big, not a file.
    Refused,
    /// A malformed data URI, or another I/O failure.
    Other,
}

impl ResolveError {
    pub fn new(kind: ResolveErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    fn io(path: &Path, e: &std::io::Error) -> Self {
        let kind = match e.kind() {
            std::io::ErrorKind::PermissionDenied => ResolveErrorKind::NotPermitted,
            std::io::ErrorKind::NotFound => ResolveErrorKind::NotFound,
            _ => ResolveErrorKind::Other,
        };
        Self::new(kind, format!("{}: {e}", path.display()))
    }
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// For GLB files and other sources with nowhere to look for external files.
pub struct NoResolver;

impl Resolver for NoResolver {
    fn resolve(&self, uri: &str) -> Result<Bytes, ResolveError> {
        Err(ResolveError::new(ResolveErrorKind::Refused, format!("can't open external file {uri:?}")))
    }
}

/// Resolves relative URIs against the model's folder. Absolute paths, other schemes,
/// `..` and symlinks that lead out of the folder are refused, so a file can't make the
/// viewer read outside its folder. Only regular files up to `max_bytes` are read (no
/// FIFOs or devices that would block or never end).
pub struct DirResolver {
    pub dir: PathBuf,
    pub max_bytes: u64,
}

impl DirResolver {
    /// The default cap matches [`SceneLimits::max_bytes`](scene_data::SceneLimits).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), max_bytes: scene_data::SceneLimits::default().max_bytes }
    }

    fn path(&self, uri: &str) -> Result<PathBuf, ResolveError> {
        let refused = |why: &str| ResolveError::new(ResolveErrorKind::Refused, format!("{uri:?}: {why}"));
        let path = self.dir.join(relative_path(uri)?);
        // Follow symlinks, then check the real file is still inside the real folder.
        let (real, root) = (path.canonicalize(), self.dir.canonicalize());
        match (real, root) {
            (Ok(real), Ok(root)) if real.starts_with(&root) => Ok(real),
            (Ok(_), Ok(_)) => Err(refused("path leaves the model's folder")),
            (Err(e), _) | (_, Err(e)) => Err(ResolveError::io(&path, &e)),
        }
    }
}

impl Resolver for DirResolver {
    fn resolve(&self, uri: &str) -> Result<Bytes, ResolveError> {
        use std::io::Read;
        let path = self.path(uri)?;
        let fail = |e: std::io::Error| ResolveError::io(&path, &e);
        let meta = std::fs::metadata(&path).map_err(fail)?;
        if !meta.is_file() {
            return Err(ResolveError::new(ResolveErrorKind::Refused, format!("{}: not a regular file", path.display())));
        }
        if meta.len() > self.max_bytes {
            let why = format!("{}: {} bytes is over the {}-byte limit", path.display(), meta.len(), self.max_bytes);
            return Err(ResolveError::new(ResolveErrorKind::Refused, why));
        }
        // `take` also caps a file that grows after the size check.
        let mut data = Vec::with_capacity(meta.len() as usize);
        std::fs::File::open(&path).and_then(|f| f.take(self.max_bytes).read_to_end(&mut data)).map_err(fail)?;
        Ok(Arc::new(data))
    }
}

/// The path a buffer or image URI names, relative to the model and percent-decoded.
/// Refused when it has a scheme, is absolute, or climbs out of the model's folder
/// (`..`), so a file can't make a host read elsewhere. For hosts writing their own
/// [`Resolver`].
pub fn relative_path(uri: &str) -> Result<String, ResolveError> {
    let refused = |why: &str| ResolveError::new(ResolveErrorKind::Refused, format!("{uri:?}: {why}"));
    if uri.contains(':') {
        return Err(refused("only relative paths are allowed"));
    }
    let rel = percent_decode(uri);
    if !Path::new(&rel).components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        return Err(refused("path leaves the model's folder"));
    }
    Ok(rel)
}

/// An image the scene is waiting for: decode `data` (PNG, JPEG, WebP, KTX2… per
/// `mime_type`) and set it on `texture`.
pub struct PendingImage {
    pub texture: TextureId,
    /// For people: the image's name in the file, else its file name, else `image N`.
    pub name: String,
    /// Inside the file (a buffer view or a `data:` URI), not a file of its own.
    pub embedded: bool,
    pub data: Stream<u8>,
    pub mime_type: Option<String>,
    pub color_space: ColorSpace,
}

pub struct Loaded {
    pub images: Vec<PendingImage>,
    /// Things that were skipped or approximated, for the host's info panel.
    pub warnings: Vec<String>,
    /// External files (buffers, images) that couldn't be read because they are
    /// missing or not permitted, by URI as the file gives it. A blocked buffer loses
    /// geometry, not only a texture.
    pub unreadable: Vec<(String, ResolveErrorKind)>,
}

/// Extensions that change what the data means and can't be skipped.
const FATAL_EXTENSIONS: &[&str] = &["KHR_draco_mesh_compression", "EXT_meshopt_compression"];
/// Extensions read here (or harmless to ignore when required).
const KNOWN_EXTENSIONS: &[&str] =
    &["KHR_materials_unlit", "KHR_mesh_quantization", "KHR_texture_basisu", "EXT_texture_webp", "EXT_texture_avif", "VRMC_materials_mtoon", "VRM"];

/// Parses `file` into `builder` and finishes its geometry. `file` is kept alive by the
/// scene for as long as borrowed streams point into it.
pub fn load(file: Bytes, resolver: &dyn Resolver, builder: &mut SceneBuilder) -> Result<Loaded, Error> {
    let (json, bin) = split_container(&file)?;
    let root: json::Root = serde_json::from_slice(&(*file).as_ref()[json]).map_err(|e| Error::Format(e.to_string()))?;
    if !root.asset.version.starts_with('2') {
        return Err(Error::Unsupported(format!("glTF version {:?}", root.asset.version)));
    }
    let mut warnings = Vec::new();
    for ext in &root.extensions_required {
        if FATAL_EXTENSIONS.contains(&ext.as_str()) {
            return Err(Error::Unsupported(ext.clone()));
        }
        if !KNOWN_EXTENSIONS.contains(&ext.as_str()) {
            warnings.push(format!("ignoring required extension {ext}; the model may look different"));
        }
    }

    let mut unreadable = Vec::new();
    let buffers: Vec<Option<BufferData>> = root
        .buffers
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let loaded = match (&b.uri, i, &bin) {
                (None, 0, Some(range)) => Ok(BufferData { bytes: file.clone(), start: range.start, len: range.len() }),
                (None, _, _) => Err(ResolveError::new(ResolveErrorKind::Other, "has no data")),
                (Some(uri), _, _) => load_uri(uri, resolver).map(BufferData::whole).inspect_err(|e| note(&mut unreadable, uri, e)),
            };
            loaded.map_err(|e| warnings.push(format!("buffer {i}: {e}"))).ok()
        })
        .collect();

    let mut loader = Loader {
        reader: Reader { root: &root, buffers: &buffers },
        resolver,
        builder,
        meshes: vec![None; root.meshes.len()],
        materials: vec![None; root.materials.len()],
        textures: Vec::new(),
        images: Vec::new(),
        warnings,
        unreadable,
        over_limit: None,
    };
    let placed = loader.placements();
    loader.set_bounds(&placed);
    loader.add_nodes(&placed);
    if let Some(e) = loader.over_limit.take() {
        return Err(Error::Scene(e));
    }
    loader.builder.finish();
    Ok(Loaded { images: loader.images, warnings: loader.warnings, unreadable: loader.unreadable })
}

/// JSON range, and the BIN chunk's range for GLB.
fn split_container(file: &Bytes) -> Result<(std::ops::Range<usize>, Option<std::ops::Range<usize>>), Error> {
    let bytes = (**file).as_ref();
    if !bytes.starts_with(b"glTF") {
        return Ok((0..bytes.len(), None));
    }
    let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let bad = |what: &str| Error::Format(format!("GLB {what}"));
    if u32_at(4) != Some(2) {
        return Err(Error::Unsupported("GLB container version other than 2".into()));
    }
    let total = u32_at(8).ok_or_else(|| bad("header is truncated"))?.min(bytes.len());
    let (mut json, mut bin) = (None, None);
    let mut at = 12;
    while at + 8 <= total {
        let (len, kind) = (u32_at(at).unwrap(), u32_at(at + 4).unwrap());
        let start = at + 8;
        let end = start.checked_add(len).filter(|&e| e <= total).ok_or_else(|| bad("chunk runs past the file"))?;
        match kind {
            0x4E4F_534A if json.is_none() => json = Some(start..end),
            0x004E_4942 if bin.is_none() => bin = Some(start..end),
            _ => {}
        }
        at = end.next_multiple_of(4);
    }
    Ok((json.ok_or_else(|| bad("has no JSON chunk"))?, bin))
}

fn load_uri(uri: &str, resolver: &dyn Resolver) -> Result<Bytes, ResolveError> {
    let Some(rest) = uri.strip_prefix("data:") else { return resolver.resolve(uri) };
    let other = |m: &str| ResolveError::new(ResolveErrorKind::Other, m);
    let (meta, payload) = rest.split_once(',').ok_or_else(|| other("malformed data URI"))?;
    if !meta.ends_with(";base64") {
        return Err(other("only base64 data URIs are supported"));
    }
    let data = base64::engine::general_purpose::STANDARD.decode(payload.trim()).map_err(|e| other(&e.to_string()))?;
    Ok(Arc::new(data))
}

/// Keeps the URIs that failed because they're missing or not permitted.
fn note(unreadable: &mut Vec<(String, ResolveErrorKind)>, uri: &str, e: &ResolveError) {
    if matches!(e.kind, ResolveErrorKind::NotPermitted | ResolveErrorKind::NotFound) && !unreadable.iter().any(|(u, _)| u == uri) {
        unreadable.push((uri.to_string(), e.kind));
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One appearance of a glTF node in the scene tree, in parent-first order.
struct Placed {
    node: usize,
    parent: Option<usize>,
    local: Mat4,
    world: Mat4,
}

type Primitives = Vec<(MeshId, Option<MaterialId>)>;
/// Turns strip / loop / fan indices into lists.
type Convert = fn(&[u32]) -> Vec<u32>;

struct Loader<'a> {
    unreadable: Vec<(String, ResolveErrorKind)>,
    reader: Reader<'a>,
    resolver: &'a dyn Resolver,
    builder: &'a mut SceneBuilder,
    /// Per glTF mesh: its primitives, loaded on first use and shared by every node
    /// that places it.
    meshes: Vec<Option<Primitives>>,
    materials: Vec<Option<Option<MaterialId>>>,
    textures: Vec<((usize, ColorSpace), Option<TextureId>)>,
    images: Vec<PendingImage>,
    warnings: Vec<String>,
    /// The first scene limit a mesh went past: the whole load fails with it, rather
    /// than showing a model with parts silently missing.
    over_limit: Option<scene_data::Error>,
}

impl<'a> Loader<'a> {
    fn root(&self) -> &'a json::Root {
        self.reader.root
    }

    /// The node tree of the default scene, flattened parent-first. Iterative, and each
    /// node is placed at most once, so cycles and deep nesting can't hang or overflow.
    fn placements(&mut self) -> Vec<Placed> {
        let root = self.root();
        let roots: Vec<usize> = match root.scenes.get(root.scene.unwrap_or(0)) {
            Some(scene) => scene.nodes.clone(),
            None => {
                let mut child = vec![false; root.nodes.len()];
                for &c in root.nodes.iter().flat_map(|n| &n.children) {
                    if let Some(slot) = child.get_mut(c) {
                        *slot = true;
                    }
                }
                (0..root.nodes.len()).filter(|&i| !child[i]).collect()
            }
        };
        let mut visited = vec![false; root.nodes.len()];
        let mut placed = Vec::new();
        let mut stack: Vec<(usize, Option<usize>)> = roots.into_iter().rev().map(|n| (n, None)).collect();
        while let Some((node, parent)) = stack.pop() {
            let Some(n) = root.nodes.get(node) else { continue };
            if std::mem::replace(&mut visited[node], true) {
                continue;
            }
            let local = node_transform(n);
            let world = parent.map_or(Mat4::IDENTITY, |p: usize| placed.get(p).map_or(Mat4::IDENTITY, |p: &Placed| p.world)) * local;
            let index = placed.len();
            placed.push(Placed { node, parent, local, world });
            stack.extend(n.children.iter().rev().map(|&c| (c, Some(index))));
        }
        placed
    }

    fn set_bounds(&mut self, placed: &[Placed]) {
        let mut bounds = Aabb::EMPTY;
        for p in placed {
            let Some(mesh) = self.root().nodes[p.node].mesh.and_then(|m| self.root().meshes.get(m)) else { continue };
            for prim in &mesh.primitives {
                if let Some((lo, hi)) = prim.attributes.get("POSITION").and_then(|&a| self.reader.min_max(a)) {
                    bounds = bounds.union(Aabb::new(lo, hi).transformed(&p.world));
                }
            }
        }
        let _ = self.builder.set_bounds(bounds);
    }

    fn add_nodes(&mut self, placed: &[Placed]) {
        let mut ids: Vec<Option<NodeId>> = Vec::with_capacity(placed.len());
        for p in placed {
            let parent = match p.parent {
                Some(i) => match ids[i] {
                    Some(id) => Some(id),
                    // The parent was rejected (a limit); its subtree goes with it.
                    None => {
                        ids.push(None);
                        continue;
                    }
                },
                None => None,
            };
            let prims = match self.root().nodes[p.node].mesh {
                Some(m) => self.mesh(m),
                None => Vec::new(),
            };
            // The first primitive goes on the node itself; the rest are children.
            let node = match prims.first() {
                Some(&(mesh, material)) => Node::mesh(mesh, material, p.local, parent),
                None => Node::group(p.local, parent),
            };
            let id = self.builder.add_node(node).map_err(|e| self.warnings.push(format!("node {}: {e}", p.node))).ok();
            if let Some(id) = id {
                for &(mesh, material) in prims.iter().skip(1) {
                    if let Err(e) = self.builder.add_node(Node::mesh(mesh, material, Mat4::IDENTITY, Some(id))) {
                        self.warnings.push(format!("node {}: {e}", p.node));
                    }
                }
            }
            ids.push(id);
        }
    }

    fn mesh(&mut self, index: usize) -> Primitives {
        if let Some(Some(prims)) = self.meshes.get(index) {
            return prims.clone();
        }
        let Some(mesh) = self.root().meshes.get(index) else {
            self.warnings.push(format!("mesh {index} doesn't exist"));
            return Vec::new();
        };
        let mut prims = Vec::new();
        for (i, prim) in mesh.primitives.iter().enumerate() {
            match self.primitive(prim) {
                Ok(p) => prims.push(p),
                Err(Error::Scene(e @ scene_data::Error::LimitExceeded { .. })) => {
                    self.over_limit.get_or_insert(e);
                }
                Err(e) => self.warnings.push(format!("mesh {index} primitive {i}: {e}")),
            }
        }
        self.meshes[index] = Some(prims.clone());
        prims
    }

    fn primitive(&mut self, prim: &json::Primitive) -> Result<(MeshId, Option<MaterialId>), Error> {
        if let Some(ext) = FATAL_EXTENSIONS.iter().find(|e| prim.extensions.contains_key(**e)) {
            return Err(Error::Unsupported(ext.to_string()));
        }
        let attr = |name: &str| prim.attributes.get(name).copied();
        let position = attr("POSITION").ok_or_else(|| Error::Format("no POSITION".into()))?;
        let mut mesh = MeshData::new(self.reader.vec3(position)?);
        mesh.normals = attr("NORMAL").map(|a| self.reader.vec3(a)).transpose()?;
        mesh.uvs = attr("TEXCOORD_0").map(|a| self.reader.vec2(a)).transpose()?;
        mesh.tangents = attr("TANGENT").map(|a| self.reader.vec4(a, 1.0)).transpose()?;
        mesh.colors = attr("COLOR_0").map(|a| self.reader.vec4(a, 1.0)).transpose()?;
        if let Some(i) = prim.indices {
            mesh.indices = self.reader.indices(i)?;
        }
        let (topology, convert): (Topology, Option<Convert>) = match prim.mode.unwrap_or(4) {
            0 => (Topology::Points, None),
            1 => (Topology::Lines, None),
            2 => (Topology::Lines, Some(line_loop)),
            3 => (Topology::Lines, Some(line_strip)),
            4 => (Topology::Triangles, None),
            5 => (Topology::Triangles, Some(triangle_strip)),
            6 => (Topology::Triangles, Some(triangle_fan)),
            m => return Err(Error::Format(format!("primitive mode {m}"))),
        };
        mesh.topology = topology;
        if let Some(convert) = convert {
            let list: Vec<u32> = match &mesh.indices {
                scene_data::Indices::None => (0..mesh.positions.len() as u32).collect(),
                scene_data::Indices::U16(s) => s.as_slice().iter().map(|&i| i as u32).collect(),
                scene_data::Indices::U32(s) => s.as_slice().to_vec(),
            };
            mesh.indices = scene_data::Indices::U32(convert(&list).into());
        }
        let id = self.builder.add_mesh(mesh)?;
        let material = prim.material.and_then(|m| self.material(m));
        Ok((id, material))
    }

    fn material(&mut self, index: usize) -> Option<MaterialId> {
        if let Some(Some(id)) = self.materials.get(index) {
            return *id;
        }
        let m = self.root().materials.get(index)?;
        let shading = if m.extensions.contains_key("KHR_materials_unlit") {
            ShadingId::UNLIT
        } else if m.extensions.contains_key("VRMC_materials_mtoon") || is_vrm0_mtoon(self.root(), m) {
            ShadingId::MTOON
        } else {
            ShadingId::PBR
        };
        let mut out = Material::new(shading, ShadingSource::FromFile);
        out.name = m.name.clone().unwrap_or_default();
        let pbr = m.pbr_metallic_roughness.as_ref();
        let base = pbr.and_then(|p| p.base_color_factor).unwrap_or([1.0; 4]);
        out.params.set("base_color", ParamValue::Vec4(base.into()));
        out.params.set("metallic", ParamValue::Float(pbr.and_then(|p| p.metallic_factor).unwrap_or(1.0)));
        out.params.set("roughness", ParamValue::Float(pbr.and_then(|p| p.roughness_factor).unwrap_or(1.0)));
        out.params.set("emissive", ParamValue::Vec3(m.emissive_factor.unwrap_or([0.0; 3]).into()));
        if let Some(scale) = m.normal_texture.as_ref().and_then(|t| t.scale) {
            out.params.set("normal_scale", ParamValue::Float(scale));
        }
        if let Some(strength) = m.occlusion_texture.as_ref().and_then(|t| t.strength) {
            out.params.set("occlusion_strength", ParamValue::Float(strength));
        }
        let slots = [
            (Slot::BASE_COLOR, pbr.and_then(|p| p.base_color_texture.as_ref()), ColorSpace::Srgb),
            (Slot::METALLIC_ROUGHNESS, pbr.and_then(|p| p.metallic_roughness_texture.as_ref()), ColorSpace::Linear),
            (Slot::NORMAL, m.normal_texture.as_ref(), ColorSpace::Linear),
            (Slot::OCCLUSION, m.occlusion_texture.as_ref(), ColorSpace::Linear),
            (Slot::EMISSIVE, m.emissive_texture.as_ref(), ColorSpace::Srgb),
        ];
        for (slot, info, space) in slots {
            let Some(info) = info else { continue };
            if info.tex_coord != 0 {
                self.warnings.push(format!("material {index}: {} uses TEXCOORD_{}; only TEXCOORD_0 is read", slot.as_str(), info.tex_coord));
            }
            if let Some(tex) = self.texture(info.index, space) {
                out.textures.push((slot, tex));
            }
        }
        out.alpha = match m.alpha_mode.as_deref() {
            Some("MASK") => AlphaMode::Mask(m.alpha_cutoff.unwrap_or(0.5)),
            Some("BLEND") => AlphaMode::Blend,
            _ => AlphaMode::Opaque,
        };
        out.double_sided = m.double_sided;
        let id = self.builder.add_material(out).map_err(|e| self.warnings.push(format!("material {index}: {e}"))).ok();
        self.materials[index] = Some(id);
        id
    }

    /// Reserves a scene texture for a glTF texture's image in a colour space, once per
    /// pair, and queues its encoded bytes for the host to decode.
    fn texture(&mut self, index: usize, space: ColorSpace) -> Option<TextureId> {
        let tex = self.root().textures.get(index)?;
        // Prefer the core image (PNG/JPEG, always decodable); fall back to an
        // extension's (KTX2, WebP, AVIF).
        let image = tex.source.or_else(|| {
            ["KHR_texture_basisu", "EXT_texture_webp", "EXT_texture_avif"]
                .iter()
                .find_map(|e| tex.extensions.get(*e)?.get("source")?.as_u64())
                .map(|s| s as usize)
        })?;
        if let Some((_, id)) = self.textures.iter().find(|(key, _)| *key == (image, space)) {
            return *id;
        }
        let id = self.image(image, space).map_err(|e| self.warnings.push(format!("image {image}: {e}"))).ok();
        self.textures.push(((image, space), id));
        id
    }

    fn image(&mut self, index: usize, color_space: ColorSpace) -> Result<TextureId, String> {
        let img = self.root().images.get(index).ok_or("doesn't exist")?;
        let data = match (&img.buffer_view, &img.uri) {
            (Some(view), _) => self.reader.view_bytes(*view).map_err(|e| e.to_string())?,
            (None, Some(uri)) => {
                let bytes = load_uri(uri, self.resolver).map_err(|e| {
                    note(&mut self.unreadable, uri, &e);
                    e.to_string()
                })?;
                let len = (*bytes).as_ref().len();
                Stream::from_bytes(bytes, 0, len).map_err(|e| e.to_string())?
            }
            (None, None) => return Err("has no data".into()),
        };
        let texture = self.builder.reserve_texture().map_err(|e| e.to_string())?;
        let embedded = img.uri.as_deref().is_none_or(|u| u.starts_with("data:"));
        let file_name = img.uri.as_deref().filter(|_| !embedded).map(|u| u.rsplit('/').next().unwrap_or(u).to_string());
        let name = img.name.clone().filter(|n| !n.is_empty()).or(file_name).unwrap_or_else(|| format!("image {index}"));
        self.images.push(PendingImage { texture, name, embedded, data, mime_type: img.mime_type.clone(), color_space });
        Ok(texture)
    }
}

/// VRM 0.x keeps MToon outside the material, in the root `VRM` extension's
/// `materialProperties`, matched by material name.
fn is_vrm0_mtoon(root: &json::Root, m: &json::Material) -> bool {
    let Some(name) = m.name.as_deref() else { return false };
    let props = root.extensions.get("VRM").and_then(|v| v.get("materialProperties")).and_then(|v| v.as_array());
    props.is_some_and(|props| {
        props.iter().any(|p| {
            p.get("name").and_then(|n| n.as_str()) == Some(name) && p.get("shader").and_then(|s| s.as_str()) == Some("VRM/MToon")
        })
    })
}

fn node_transform(n: &json::Node) -> Mat4 {
    let m = match n.matrix {
        Some(m) => Mat4::from_cols_array(&m),
        None => {
            let t = n.translation.map_or(Vec3::ZERO, Vec3::from);
            let r = n.rotation.map_or(Quat::IDENTITY, |q| Quat::from_array(q).normalize());
            let s = n.scale.map_or(Vec3::ONE, Vec3::from);
            Mat4::from_scale_rotation_translation(s, r, t)
        }
    };
    if m.is_finite() { m } else { Mat4::IDENTITY }
}

fn line_strip(v: &[u32]) -> Vec<u32> {
    v.windows(2).flat_map(|w| [w[0], w[1]]).collect()
}

fn line_loop(v: &[u32]) -> Vec<u32> {
    let mut out = line_strip(v);
    if let (Some(&first), Some(&last)) = (v.first(), v.last())
        && v.len() > 2
    {
        out.extend([last, first]);
    }
    out
}

/// Every other triangle is flipped to keep the winding consistent (glTF spec §3.7.2.1).
fn triangle_strip(v: &[u32]) -> Vec<u32> {
    v.windows(3).enumerate().flat_map(|(i, w)| if i % 2 == 0 { [w[0], w[1], w[2]] } else { [w[1], w[0], w[2]] }).collect()
}

fn triangle_fan(v: &[u32]) -> Vec<u32> {
    match v.split_first() {
        Some((&c, rest)) => rest.windows(2).flat_map(|w| [c, w[0], w[1]]).collect(),
        None => Vec::new(),
    }
}

/// The folder a `.gltf`'s relative URIs resolve against.
pub fn resolver_for(path: &Path) -> DirResolver {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    // `Path::parent` of a bare file name is "", which canonicalize refuses.
    DirResolver::new(if dir.as_os_str().is_empty() { PathBuf::from(".") } else { dir })
}
