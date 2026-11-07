//! FBX and OBJ through ufbx, into a CPU scene (`scene-data`).
//!
//! ufbx converts axes and units (Y up, metres), triangulates polygons and resolves the
//! node hierarchy. Each mesh is converted once per material part and placed by every
//! node that uses it, with the node's world transform.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use libfileparser_core::{Dependencies, DependencyStatus, FileSource, ParseContext};
use scene_data::glam::{Mat4, Vec2, Vec3, Vec4};
use scene_data::{
    ColorSpace, CpuScene, Indices, Material, MaterialId, MeshData, MeshId, Node, ParamValue, SceneBuilder, ShadingId, ShadingSource, Slot,
    Stream, TextureId,
};

use crate::textures::{self, ImageJob, ImageSource, Textures};

pub struct Loaded {
    pub scene: CpuScene,
    pub warnings: Vec<String>,
    /// Base colour maps queued for decoding.
    pub textures: Textures,
}

/// Reads `src`; other files (an OBJ's `.mtl`, textures) only through `cx.files`, each
/// recorded in `deps`. ufbx itself opens nothing: the `.mtl` data is handed to it.
pub fn load(src: &FileSource, format: ufbx::FileFormat, cx: &ParseContext, deps: &Dependencies) -> Result<Loaded> {
    let mtl = if format == ufbx::FileFormat::Obj { mtl_libraries(src, cx, deps) } else { Vec::new() };
    let opts = ufbx::LoadOpts {
        file_format: format,
        target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
        target_unit_meters: 1.0,
        load_external_files: false,
        ignore_missing_external_files: true,
        obj_search_mtl_by_filename: false,
        obj_mtl_data: if mtl.is_empty() { ufbx::BlobOpt::Unset } else { mtl.as_slice().into() },
        ..Default::default()
    };
    let root = ufbx::load_memory(src.bytes(), opts).map_err(|e| anyhow!("{}", e.description))?;
    let (scene, mut builder) = crate::build::new_scene(cx);
    // ufbx's own notes (repaired indices…), deduplicated by its count.
    let mut warnings: Vec<String> = root
        .metadata
        .warnings
        .iter()
        .map(|w| if w.count > 1 { format!("{} (×{})", w.description, w.count) } else { w.description.to_string() })
        .collect();
    let mut images = Images { from: src.clone(), cx: cx.clone(), deps: deps.clone(), seen: HashMap::new(), packed: HashMap::new(), jobs: Vec::new() };
    convert(&root, &mut builder, &mut images, &mut warnings).context("building the scene")?;
    builder.finish();
    let textures = textures::spawn(builder.texture_sink(), images.jobs, cx);
    Ok(Loaded { scene, warnings, textures })
}

/// The `.mtl` files an OBJ names (`mtllib`), read through the files hook and joined:
/// materials and their maps live in them. Names are space-separated, though some
/// exporters write one name with spaces in it: the whole rest of the line is tried
/// first, and if neither it nor any word of it is there, it's reported whole.
fn mtl_libraries(src: &FileSource, cx: &ParseContext, deps: &Dependencies) -> Vec<u8> {
    let mut data = Vec::new();
    for line in src.bytes().split(|&b| b == b'\n') {
        // A byte test first: this walks every line of what can be a very large file.
        if !line.trim_ascii_start().starts_with(b"mtllib") {
            continue;
        }
        let line = String::from_utf8_lossy(line);
        let Some(names) = line.trim_start().strip_prefix("mtllib") else { continue };
        if !names.starts_with([' ', '\t']) {
            continue;
        }
        let whole = names.trim().replace('\\', "/");
        if whole.is_empty() {
            continue;
        }
        let parts: Vec<&str> = whole.split_whitespace().collect();
        let mut add = |file: FileSource| {
            data.extend_from_slice(file.bytes());
            data.push(b'\n');
        };
        if parts.len() == 1 {
            if let Some(file) = deps.resolve(cx, src, &whole) {
                add(file);
            }
            deps.set_kind(&whole, MATERIAL_LIBRARY);
            continue;
        }
        match cx.files.resolve(src, &whole) {
            Ok(file) => {
                deps.record(&whole, DependencyStatus::Read);
                add(file);
            }
            // Refused: the words would be refused too (it's the folder that is out of
            // reach), so the whole name is what to report.
            Err(why @ libfileparser_core::Unreadable::Blocked) => deps.record(&whole, why.into()),
            Err(why) => {
                let found: Vec<_> = parts.iter().map(|p| (*p, cx.files.resolve(src, p))).collect();
                if found.iter().all(|(_, r)| r.is_err()) {
                    deps.record(&whole, why.into());
                } else {
                    for (part, result) in found {
                        match result {
                            Ok(file) => {
                                deps.record(part, DependencyStatus::Read);
                                add(file);
                            }
                            Err(why) => deps.record(part, why.into()),
                        }
                    }
                }
            }
        }
        for name in std::iter::once(whole.as_str()).chain(parts.iter().copied()) {
            deps.set_kind(name, MATERIAL_LIBRARY);
        }
    }
    data
}

const MATERIAL_LIBRARY: &str = "material library";

/// Texture images found while converting materials, each queued once.
struct Images {
    /// The model: texture files are looked for beside it, through `cx.files`.
    from: FileSource,
    cx: ParseContext,
    deps: Dependencies,
    /// By (texture, sRGB): one image used as colour and as data is two textures.
    seen: HashMap<(u32, bool), Option<TextureId>>,
    /// Packed roughness + metalness, by the two source textures.
    packed: HashMap<(Option<u32>, Option<u32>), Option<TextureId>>,
    jobs: Vec<ImageJob>,
}

impl Images {
    /// Where a texture's image is: its embedded bytes, or the file it names, found
    /// next to the model.
    fn source(&self, tex: &ufbx::Texture) -> Option<ImageSource> {
        // A layered texture: use its first file.
        let file = tex.file_textures.first().map(|t| &**t).unwrap_or(tex);
        if !file.content.is_empty() {
            return Some(ImageSource::Bytes(Stream::from(file.content.to_vec())));
        }
        textures::find(&self.cx, &self.deps, &self.from, &file.relative_filename, &file.absolute_filename, &[""]).map(ImageSource::File)
    }

    fn queue(&mut self, job: impl FnOnce(TextureId) -> ImageJob, b: &mut SceneBuilder) -> Option<TextureId> {
        let texture = b.reserve_texture().ok()?;
        self.jobs.push(job(texture));
        Some(texture)
    }

    fn texture(&mut self, tex: &ufbx::Texture, color_space: ColorSpace, b: &mut SceneBuilder) -> Option<TextureId> {
        let key = (tex.element.typed_id, color_space == ColorSpace::Srgb);
        if let Some(id) = self.seen.get(&key) {
            return *id;
        }
        let file = tex.file_textures.first().map(|t| &**t).unwrap_or(tex);
        let name = textures::report_name(&file.relative_filename, &file.absolute_filename);
        let name = if name.is_empty() { tex.element.name.to_string() } else { name };
        let id = self.source(tex).and_then(|source| self.queue(|t| ImageJob::new(t, source, color_space).named(name), b));
        self.seen.insert(key, id);
        id
    }

    fn metal_rough(&mut self, roughness: Option<&ufbx::Texture>, metalness: Option<&ufbx::Texture>, b: &mut SceneBuilder) -> Option<TextureId> {
        let key = (roughness.map(|t| t.element.typed_id), metalness.map(|t| t.element.typed_id));
        if let Some(id) = self.packed.get(&key) {
            return *id;
        }
        let (r, m) = (roughness.and_then(|t| self.source(t)), metalness.and_then(|t| self.source(t)));
        let id = if r.is_none() && m.is_none() {
            None
        } else {
            // FBX maps are greyscale: any channel will do.
            let pick = |s: Option<ImageSource>| s.map(|s| (Box::new(s), 0));
            let source = ImageSource::MetalRough { roughness: pick(r), metalness: pick(m) };
            self.queue(|t| ImageJob::new(t, source, ColorSpace::Linear), b)
        };
        self.packed.insert(key, id);
        id
    }
}

/// The first of `maps` with an enabled texture.
fn map_texture<'a>(maps: &[&'a ufbx::MaterialMap]) -> Option<&'a ufbx::Texture> {
    maps.iter().find_map(|m| m.texture.as_deref().filter(|_| m.texture_enabled))
}

/// Queues the material's maps and points `out` at them. A map's factor becomes 1
/// where the map carries the values (ufbx often reports 0 beside a texture).
fn attach_maps(m: &ufbx::Material, out: &mut Material, images: &mut Images, b: &mut SceneBuilder) {
    let mut add = |slot: Slot, tex: Option<&ufbx::Texture>, space: ColorSpace, b: &mut SceneBuilder| {
        tex.and_then(|t| images.texture(t, space, b)).map(|id| out.textures.push((slot, id))).is_some()
    };
    add(Slot::BASE_COLOR, map_texture(&[&m.pbr.base_color, &m.fbx.diffuse_color]), ColorSpace::Srgb, b);
    add(Slot::NORMAL, map_texture(&[&m.pbr.normal_map, &m.fbx.normal_map]), ColorSpace::Linear, b);
    add(Slot::OCCLUSION, map_texture(&[&m.pbr.ambient_occlusion]), ColorSpace::Linear, b);
    if add(Slot::EMISSIVE, map_texture(&[&m.pbr.emission_color, &m.fbx.emission_color]), ColorSpace::Srgb, b) {
        let strength = if m.pbr.emission_factor.has_value { m.pbr.emission_factor.value_vec4.x as f32 } else { 1.0 };
        let lit = matches!(out.params.get("emissive"), Some(ParamValue::Vec3(c)) if c.max_element() > 0.0);
        if !lit {
            out.params.set("emissive", ParamValue::Vec3(Vec3::splat(strength)));
        }
    }
    let (rough, metal) = (map_texture(&[&m.pbr.roughness]), map_texture(&[&m.pbr.metalness]));
    if let Some(id) = images.metal_rough(rough, metal, b) {
        out.textures.push((Slot::METALLIC_ROUGHNESS, id));
        if rough.is_some() {
            out.params.set("roughness", ParamValue::Float(1.0));
        }
        if metal.is_some() {
            out.params.set("metallic", ParamValue::Float(1.0));
        }
    }
}

/// One uploaded mesh per (ufbx mesh, material part): the scene mesh and the part's
/// material slot.
type Parts = Vec<(MeshId, usize)>;

fn convert(root: &ufbx::Scene, b: &mut SceneBuilder, images: &mut Images, warnings: &mut Vec<String>) -> Result<()> {
    let mut meshes: HashMap<u32, Parts> = HashMap::new();
    let mut materials: HashMap<u32, Option<MaterialId>> = HashMap::new();
    for node in root.nodes.iter() {
        let Some(mesh) = node.mesh.as_ref() else { continue };
        if !node.visible {
            continue;
        }
        let parts = match meshes.get(&mesh.element.typed_id) {
            Some(parts) => parts.clone(),
            None => {
                let parts = mesh_parts(mesh, b, warnings);
                meshes.insert(mesh.element.typed_id, parts.clone());
                parts
            }
        };
        let world = mat4(&node.geometry_to_world);
        for (id, slot) in parts {
            // Nodes can override the mesh's materials (one mesh, several looks).
            let material = node.materials.get(slot).or_else(|| mesh.materials.get(slot)).and_then(|m| {
                *materials.entry(m.element.typed_id).or_insert_with(|| {
                    let mut out = material(m);
                    attach_maps(m, &mut out, images, b);
                    b.add_material(out).map_err(|e| warnings.push(format!("material {}: {e}", m.element.name))).ok()
                })
            });
            if let Err(e) = b.add_node(Node::mesh(id, material, world, None)) {
                warnings.push(format!("node {}: {e}", node.element.name));
                // Every later node would hit the same limit.
                if matches!(e, scene_data::Error::LimitExceeded { .. }) {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

fn mat4(m: &ufbx::Matrix) -> Mat4 {
    let c = |x: f64, y: f64, z: f64, w: f32| Vec4::new(x as f32, y as f32, z as f32, w);
    let m = Mat4::from_cols(c(m.m00, m.m10, m.m20, 0.0), c(m.m01, m.m11, m.m21, 0.0), c(m.m02, m.m12, m.m22, 0.0), c(m.m03, m.m13, m.m23, 1.0));
    if m.is_finite() { m } else { Mat4::IDENTITY }
}

/// Triangulates each material part of `mesh` into an indexed scene mesh. Corners that
/// share position, normal, uv and colour share a vertex.
fn mesh_parts(mesh: &ufbx::Mesh, b: &mut SceneBuilder, warnings: &mut Vec<String>) -> Parts {
    let name = &mesh.element.name;
    if !mesh.vertex_position.exists {
        warnings.push(format!("mesh {name}: no positions"));
        return Vec::new();
    }
    let all: Vec<u32> = (0..mesh.faces.len() as u32).collect();
    let parts: Vec<(usize, &[u32])> = if mesh.material_parts.is_empty() {
        vec![(0, &all)]
    } else {
        mesh.material_parts.iter().map(|p| (p.index as usize, p.face_indices.as_ref())).collect()
    };
    let mut tri = vec![0u32; mesh.max_face_triangles * 3];
    let mut out = Vec::new();
    for (slot, faces) in parts {
        // Keyed by position index and the other attributes' values: FBX stores normals
        // (and often uvs) per corner, so their indices are unique even where values match.
        let mut corners: HashMap<[u32; 10], u32> = HashMap::new();
        let (mut positions, mut normals, mut uvs, mut colors) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut indices: Vec<u32> = Vec::new();
        for &f in faces {
            let face = mesh.faces[f as usize];
            if face.num_indices < 3 {
                continue; // points and lines
            }
            let n = mesh.triangulate_face(&mut tri, face) as usize;
            for &i in &tri[..n * 3] {
                let i = i as usize;
                let bits = |x: f64| (x as f32).to_bits();
                let mut key = [mesh.vertex_position.indices[i], 0, 0, 0, 0, 0, 0, 0, 0, 0];
                if mesh.vertex_normal.exists {
                    let n = mesh.vertex_normal[i];
                    key[1..4].copy_from_slice(&[bits(n.x), bits(n.y), bits(n.z)]);
                }
                if mesh.vertex_uv.exists {
                    let t = mesh.vertex_uv[i];
                    key[4..6].copy_from_slice(&[bits(t.x), bits(t.y)]);
                }
                if mesh.vertex_color.exists {
                    let c = mesh.vertex_color[i];
                    key[6..10].copy_from_slice(&[bits(c.x), bits(c.y), bits(c.z), bits(c.w)]);
                }
                let next = positions.len() as u32;
                let index = *corners.entry(key).or_insert_with(|| {
                    let p = mesh.vertex_position[i];
                    positions.push(Vec3::new(p.x as f32, p.y as f32, p.z as f32));
                    if mesh.vertex_normal.exists {
                        let n = mesh.vertex_normal[i];
                        normals.push(Vec3::new(n.x as f32, n.y as f32, n.z as f32));
                    }
                    if mesh.vertex_uv.exists {
                        let t = mesh.vertex_uv[i];
                        // FBX/OBJ put v = 0 at the bottom; glTF (and RendererX) at the top.
                        uvs.push(Vec2::new(t.x as f32, 1.0 - t.y as f32));
                    }
                    if mesh.vertex_color.exists {
                        let c = mesh.vertex_color[i];
                        colors.push(Vec4::new(c.x as f32, c.y as f32, c.z as f32, c.w as f32));
                    }
                    next
                });
                indices.push(index);
            }
        }
        if indices.is_empty() {
            continue;
        }
        let mut data = MeshData::new(positions).with_indices(Indices::U32(indices.into()));
        data.normals = (!normals.is_empty()).then(|| normals.into());
        data.uvs = (!uvs.is_empty()).then(|| uvs.into());
        data.colors = (!colors.is_empty()).then(|| colors.into());
        match b.add_mesh(data) {
            Ok(id) => out.push((id, slot)),
            Err(e) => warnings.push(format!("mesh {name}: {e}")),
        }
    }
    out
}

/// The material's look as authored, as far as clay can show it: shading model and base
/// colour (docs/design/3d-renderer.md §6).
fn material(m: &ufbx::Material) -> Material {
    use ufbx::ShaderType as S;
    let shading = match m.shader_type {
        S::FbxLambert => ShadingId::LAMBERT,
        S::FbxPhong | S::BlenderPhong | S::WavefrontMtl | S::Unknown => ShadingId::BLINN_PHONG,
        _ => ShadingId::PBR,
    };
    let mut out = Material::new(shading, ShadingSource::FromFile);
    out.name = m.element.name.to_string();
    // ufbx fills the PBR maps for every material, translating classic FBX ones. A
    // material with no colour at all (an OBJ whose .mtl is missing) keeps clay's.
    let base = &m.pbr.base_color;
    let diffuse = &m.fbx.diffuse_color;
    let colour = if base.has_value { Some(base) } else if diffuse.has_value { Some(diffuse) } else { None };
    if let Some(map) = colour {
        let factor = if m.pbr.base_factor.has_value { m.pbr.base_factor.value_vec4.x as f32 } else { 1.0 };
        let c = map.value_vec4;
        let alpha = if map.value_components >= 4 { c.w as f32 } else { 1.0 };
        let color = Vec4::new(c.x as f32 * factor, c.y as f32 * factor, c.z as f32 * factor, alpha);
        if color.is_finite() {
            out.params.set("base_color", ParamValue::Vec4(color.clamp(Vec4::ZERO, Vec4::ONE)));
        }
    }
    if m.pbr.roughness.has_value {
        out.params.set("roughness", ParamValue::Float(m.pbr.roughness.value_vec4.x as f32));
    }
    if m.pbr.metalness.has_value {
        out.params.set("metallic", ParamValue::Float(m.pbr.metalness.value_vec4.x as f32));
    }
    if m.pbr.emission_color.has_value {
        let c = m.pbr.emission_color.value_vec4;
        let f = if m.pbr.emission_factor.has_value { m.pbr.emission_factor.value_vec4.x } else { 1.0 };
        let e = Vec3::new((c.x * f) as f32, (c.y * f) as f32, (c.z * f) as f32);
        if e.is_finite() {
            out.params.set("emissive", ParamValue::Vec3(e.max(Vec3::ZERO)));
        }
    }
    out
}

// ---------------------------------------------------------------- the readers

use libfileparser_core::{Category, Detection, Document, Reader, ReaderInfo};

use crate::build::{SceneDocument, detect_by};

/// FBX and OBJ are both read by ufbx; only the format and the wording differ.
fn open_with_ufbx(reader: &'static str, format: ufbx::FileFormat, name: &str, src: &FileSource, cx: &ParseContext) -> libfileparser_core::Result<Document> {
    cx.cancel.check()?;
    let deps = Dependencies::new();
    let loaded = load(src, format, cx, &deps).map_err(super::parse_error)?;
    SceneDocument::new(reader, name, loaded.scene).warnings(loaded.warnings).textures(loaded.textures).dependencies(deps).build()
}

pub struct FbxReader;

static FBX: ReaderInfo = ReaderInfo::new("fbx", "FBX model", Category::ThreeD).extensions(&["fbx"]);

impl Reader for FbxReader {
    fn info(&self) -> &ReaderInfo {
        &FBX
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        detect_by(&FBX, header.starts_with(b"Kaydara FBX Binary"), name)
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> libfileparser_core::Result<Document> {
        let format = if src.header().starts_with(b"Kaydara FBX Binary") { "FBX (binary)" } else { "FBX (ASCII)" };
        open_with_ufbx("fbx", ufbx::FileFormat::Fbx, format, src, cx)
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

    fn open(&self, src: &FileSource, cx: &ParseContext) -> libfileparser_core::Result<Document> {
        open_with_ufbx("obj", ufbx::FileFormat::Obj, "Wavefront OBJ", src, cx)
    }
}
