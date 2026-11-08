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

mod objects;
mod records;

pub struct Loaded {
    pub scene: CpuScene,
    pub warnings: Vec<String>,
    /// Scene items by the path of the record (FBX) or object (OBJ) they came from.
    pub origins: crate::build::SceneOrigins,
    /// Base colour maps queued for decoding.
    pub textures: Textures,
    /// Whether the file has anything under its root (bones, cameras, curves).
    pub has_nodes: bool,
}

/// Reads `src`; other files (an OBJ's `.mtl`, textures) only through `cx.files`, each
/// recorded in `deps`. ufbx itself opens nothing: the `.mtl` data is handed to it.
pub fn load(src: &FileSource, format: ufbx::FileFormat, cx: &ParseContext, deps: &Dependencies) -> Result<Loaded> {
    let mtl = if format == ufbx::FileFormat::Obj { mtl_libraries(src, cx, deps) } else { Vec::new() };
    // ufbx sizes arrays by what the file claims: a few bytes can ask for tens of
    // gigabytes, and a failed allocation ends the process. Both its arenas stay within
    // the document's memory limit.
    let arena = || ufbx::AllocatorOpts { memory_limit: usize::try_from(cx.limits.memory).unwrap_or(usize::MAX), ..Default::default() };
    let opts = |mtl: &[u8]| ufbx::LoadOpts {
        temp_allocator: arena(),
        result_allocator: arena(),
        file_format: format,
        target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
        target_unit_meters: 1.0,
        load_external_files: false,
        ignore_missing_external_files: true,
        obj_search_mtl_by_filename: false,
        obj_mtl_data: if mtl.is_empty() { ufbx::BlobOpt::Unset } else { mtl.to_vec().into() },
        ..Default::default()
    };
    // Some exporters write OBJ as UTF-16, which ufbx doesn't read.
    let utf8 = if format == ufbx::FileFormat::Obj { from_utf16(src.bytes()) } else { None };
    let bytes = utf8.as_ref().map_or(src.bytes(), |s| s.as_bytes());
    let describe = |e: &ufbx::Error| {
        if matches!(e.type_, ufbx::ErrorType::MemoryLimit | ufbx::ErrorType::AllocationLimit) {
            let max = cx.limits.memory;
            return anyhow::Error::new(libfileparser_core::ParseError::too_large(libfileparser_core::Limit::Memory, max.saturating_add(1), max));
        }
        if e.info().is_empty() { anyhow!("{}", e.description) } else { anyhow!("{}: {}", e.description, e.info()) }
    };
    let (root, mut note) = match ufbx::load_memory(bytes, opts(&mtl)) {
        Ok(root) => (root, None),
        Err(e) if format == ufbx::FileFormat::Obj => match (!mtl.is_empty()).then(|| ufbx::load_memory(bytes, opts(&[])).ok()).flatten() {
            Some(root) => (root, Some("its material file couldn't be read".to_string())),
            None => {
                let Some(mut data) = plain_obj(bytes) else { return Err(describe(&e)) };
                let (scene, mut builder) = crate::build::new_scene(cx);
                let warnings = vec!["read without normals, UVs or materials: the file breaks OBJ's rules".to_string()];
                data.normals = None;
                let id = builder.add_mesh(data).context("the mesh")?;
                builder.add_node(Node::mesh(id, None, Mat4::IDENTITY, None)).context("the mesh")?;
                builder.finish();
                let textures = textures::spawn(builder.texture_sink(), Vec::new(), cx);
                return Ok(Loaded { scene, warnings, textures, origins: crate::build::SceneOrigins::new(), has_nodes: true });
            }
        },
        Err(e) => return Err(describe(&e)),
    };
    let (scene, mut builder) = crate::build::new_scene(cx);
    // ufbx's own notes (repaired indices…), deduplicated by its count.
    let mut warnings: Vec<String> = root
        .metadata
        .warnings
        .iter()
        .map(|w| if w.count > 1 { format!("{} (×{})", w.description, w.count) } else { w.description.to_string() })
        .collect();
    warnings.extend(note.take());
    let mut images = Images { from: src.clone(), cx: cx.clone(), deps: deps.clone(), seen: HashMap::new(), packed: HashMap::new(), jobs: Vec::new() };
    let mut origins = crate::build::SceneOrigins::new();
    convert(&root, format, &mut builder, &mut images, &mut warnings, &mut origins).context("building the scene")?;
    if format == ufbx::FileFormat::Obj && scene.read().nodes().is_empty() {
        // Only vertices: ufbx makes no mesh of them, but they're a point cloud.
        let points = vertex_lines(bytes);
        if !points.is_empty() {
            let mut data = MeshData::new(points);
            data.topology = scene_data::Topology::Points;
            let id = builder.add_mesh(data).context("the points")?;
            builder.add_node(Node::mesh(id, None, Mat4::IDENTITY, None)).context("the points")?;
        }
    }
    builder.finish();
    let textures = textures::spawn(builder.texture_sink(), images.jobs, cx);
    Ok(Loaded { scene, warnings, textures, origins, has_nodes: root.nodes.len() > 1 })
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

/// Paths that name an object the way the Records and Objects trees do, so the scene's
/// origins match their nodes: FBX `Objects/Model/Tri` (record class, then the object's
/// name), OBJ `Tri`; a name met again gets `#1`, `#2`…, in file order.
#[derive(Default)]
pub(crate) struct Keys(HashMap<String, usize>);

impl Keys {
    pub(crate) fn next(&mut self, base: String) -> String {
        let n = self.0.entry(base.clone()).or_insert(0);
        let key = if *n == 0 { base } else { format!("{base}#{n}") };
        *n += 1;
        key
    }
}

fn convert(root: &ufbx::Scene, format: ufbx::FileFormat, b: &mut SceneBuilder, images: &mut Images, warnings: &mut Vec<String>, origins: &mut crate::build::SceneOrigins) -> Result<()> {
    let obj = format == ufbx::FileFormat::Obj;
    let (mut node_keys, mut mesh_keys, mut material_keys) = (Keys::default(), Keys::default(), Keys::default());
    // Keyed in file order, every one counted, so the n-th of a name is the n-th record.
    let mesh_key: HashMap<u32, String> = root.meshes.iter().map(|m| (m.element.typed_id, mesh_keys.next(if obj { m.element.name.to_string() } else { format!("Objects/Geometry/{}", m.element.name) }))).collect();
    let material_key: HashMap<u32, String> =
        root.materials.iter().map(|m| (m.element.typed_id, material_keys.next(if obj { format!("usemtl {}", m.element.name) } else { format!("Objects/Material/{}", m.element.name) }))).collect();
    let mut meshes: HashMap<u32, Parts> = HashMap::new();
    let mut materials: HashMap<u32, Option<MaterialId>> = HashMap::new();
    let mut curves: HashMap<u32, Vec<MeshId>> = HashMap::new();
    for node in root.nodes.iter() {
        if node.is_root || node.is_geometry_transform_helper || node.is_scale_helper {
            continue;
        }
        let key = node_keys.next(if obj { node.element.name.to_string() } else { format!("Objects/Model/{}", node.element.name) });
        let Some(mesh) = node.mesh.as_ref() else {
            if node.visible && let Some(attrib) = node.attrib.as_ref() {
                let id = attrib.element_id;
                let drawn = curves
                    .entry(id)
                    .or_insert_with(|| {
                        let mut parts = Vec::new();
                        if let Some(line) = root.line_curves.iter().find(|c| c.element.element_id == id) {
                            parts.extend(line_part(line, b, warnings));
                        } else if let Some(curve) = root.nurbs_curves.iter().find(|c| c.element.element_id == id) {
                            match ufbx::tessellate_nurbs_curve(curve, ufbx::TessellateCurveOpts { span_subdivision: 16, ..Default::default() }) {
                                Ok(line) => parts.extend(line_part(&line, b, warnings)),
                                Err(e) => warnings.push(format!("curve {}: {}", curve.element.name, e.description)),
                            }
                        } else if let Some(surface) = root.nurbs_surfaces.iter().find(|c| c.element.element_id == id) {
                            match ufbx::tessellate_nurbs_surface(surface, ufbx::TessellateSurfaceOpts { span_subdivision_u: 16, span_subdivision_v: 16, ..Default::default() }) {
                                Ok(mesh) => parts.extend(mesh_parts(&mesh, b, warnings).into_iter().map(|(id, _)| id)),
                                Err(e) => warnings.push(format!("surface {}: {}", surface.element.name, e.description)),
                            }
                        }
                        parts
                    })
                    .clone();
                let world = mat4(&node.geometry_to_world);
                for id in drawn {
                    if let Err(e) = b.add_node(Node::mesh(id, None, world, None)) {
                        warnings.push(format!("node {}: {e}", node.element.name));
                    }
                }
            }
            continue;
        };
        if !node.visible {
            continue;
        }
        let parts = match meshes.get(&mesh.element.typed_id) {
            Some(parts) => parts.clone(),
            None => {
                let parts = mesh_parts(mesh, b, warnings);
                if let Some(k) = mesh_key.get(&mesh.element.typed_id) {
                    for (id, _) in &parts {
                        origins.mesh(k.clone(), *id);
                    }
                }
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
                    let id = b.add_material(out).map_err(|e| warnings.push(format!("material {}: {e}", m.element.name))).ok();
                    if let (Some(id), Some(k)) = (id, material_key.get(&m.element.typed_id)) {
                        origins.material(k.clone(), id);
                    }
                    id
                })
            });
            let added = b.add_node(Node::mesh(id, material, world, None));
            if let Ok(n) = &added {
                origins.node(key.clone(), *n);
            }
            if let Err(e) = added {
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
    // Faces of one or two corners are points and lines, in their own meshes.
    let (mut points, mut lines) = (Vec::new(), Vec::new());
    for face in mesh.faces.iter().filter(|f| f.num_indices < 3) {
        let corner = |i: u32| mesh.vertex_position[(face.index_begin + i) as usize];
        let v = |p: ufbx::Vec3| Vec3::new(p.x as f32, p.y as f32, p.z as f32);
        match face.num_indices {
            1 => points.push(v(corner(0))),
            2 => lines.extend([v(corner(0)), v(corner(1))]),
            _ => {}
        }
    }
    for (positions, topology) in [(points, scene_data::Topology::Points), (lines, scene_data::Topology::Lines)] {
        if positions.is_empty() {
            continue;
        }
        let mut data = MeshData::new(positions);
        data.topology = topology;
        match b.add_mesh(data) {
            Ok(id) => out.push((id, 0)),
            Err(e) => warnings.push(format!("mesh {name}: {e}")),
        }
    }
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

/// A line curve, each of its polylines as segments.
fn line_part(line: &ufbx::LineCurve, b: &mut SceneBuilder, warnings: &mut Vec<String>) -> Option<MeshId> {
    let points: Vec<Vec3> = line.control_points.iter().map(|p| Vec3::new(p.x as f32, p.y as f32, p.z as f32)).collect();
    let mut indices = Vec::new();
    for s in line.segments.iter() {
        let run = line.point_indices.get(s.index_begin as usize..(s.index_begin + s.num_indices) as usize)?;
        run.windows(2).for_each(|w| indices.extend_from_slice(w));
    }
    if indices.is_empty() {
        return None;
    }
    let mut data = MeshData::new(points).with_indices(Indices::U32(indices.into()));
    data.topology = scene_data::Topology::Lines;
    b.add_mesh(data).map_err(|e| warnings.push(format!("curve {}: {e}", line.element.name))).ok()
}

/// An OBJ's vertices and faces alone, for a file ufbx won't load: a number it can't read
/// is zero and a face naming a vertex that isn't there is left out. `None` with no faces.
fn plain_obj(bytes: &[u8]) -> Option<MeshData> {
    let text = String::from_utf8_lossy(bytes);
    let mut positions: Vec<Vec3> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for line in text.lines() {
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            Some("v") => {
                let mut n = words.map(|w| w.parse::<f32>().ok().filter(|v| v.is_finite()).unwrap_or(0.0));
                positions.push(Vec3::new(n.next().unwrap_or(0.0), n.next().unwrap_or(0.0), n.next().unwrap_or(0.0)));
            }
            Some("f") => {
                let corner = |w: &str| {
                    let i: i64 = w.split('/').next()?.parse().ok()?;
                    let at = if i < 0 { positions.len() as i64 + i } else { i - 1 };
                    u32::try_from(at).ok().filter(|&a| (a as usize) < positions.len())
                };
                let Some(face) = words.map(corner).collect::<Option<Vec<u32>>>() else { continue };
                for k in 1..face.len().saturating_sub(1) {
                    indices.extend([face[0], face[k], face[k + 1]]);
                }
            }
            _ => {}
        }
    }
    (!indices.is_empty()).then(|| MeshData::new(positions).with_indices(Indices::U32(indices.into())))
}

/// Every `v x y z` line of an OBJ.
fn vertex_lines(bytes: &[u8]) -> Vec<Vec3> {
    bytes
        .split(|&c| c == b'\n')
        .filter_map(|line| {
            let line = std::str::from_utf8(line).ok()?.trim().strip_prefix("v ")?;
            let mut n = line.split_ascii_whitespace().map(str::parse::<f32>);
            Some(Vec3::new(n.next()?.ok()?, n.next()?.ok()?, n.next()?.ok()?))
        })
        .collect()
}

/// UTF-16 text (by its byte-order mark) as UTF-8.
fn from_utf16(bytes: &[u8]) -> Option<String> {
    let units = |of: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|c| of(*c)).collect();
        Some(String::from_utf16_lossy(&units))
    };
    match bytes.get(..2)? {
        [0xff, 0xfe] => units(u16::from_le_bytes),
        [0xfe, 0xff] => units(u16::from_be_bytes),
        _ => None,
    }
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

use std::sync::Arc;

use libfileparser_core::{Category, Content, Detection, Document, FacetInfo, Model, Reader, ReaderInfo, Role};

use crate::build::{SceneDocument, detect_by};

/// FBX and OBJ are both read by ufbx; only the format and the wording differ.
fn open_with_ufbx(reader: &dyn Reader, format: ufbx::FileFormat, name: &str, src: &FileSource, cx: &ParseContext) -> libfileparser_core::Result<Document> {
    cx.cancel.check()?;
    let deps = Dependencies::new();
    let loaded = load(src, format, cx, &deps).map_err(super::parse_error)?;
    let from = src.clone();
    let obj = Arc::new(objects::Shared::new(src));
    let for_facet = obj.clone();
    let facets = move |doc: Document| match format {
        ufbx::FileFormat::Obj => doc.with_lazy_facet(FacetInfo::new(Role::Structure, "Objects", Model::Tree), move |cx| Ok(for_facet.get(cx)?.content())),
        _ if records::is_binary(from.bytes()) => {
            let Ok(binary) = records::BinaryRecords::new(&from) else { return doc };
            let media = binary.media();
            let doc = doc.with_facet(Role::Structure, "Records", Content::Tree(Arc::new(binary)));
            if media.is_empty() { doc } else { doc.with_facet(Role::Package, "Embedded media", Content::Container(Arc::new(libfileparser_core::FileParts::new(&from, media)))) }
        }
        _ => doc.with_lazy_facet(FacetInfo::new(Role::Structure, "Records", Model::Tree), move |cx| {
            Ok(Content::Tree(Arc::new(records::ascii_records(from.bytes(), cx.limits.nodes, cx.limits.depth)?)))
        }),
    };
    let structure = if format == ufbx::FileFormat::Obj { "Objects" } else { "Records" };
    let doc = SceneDocument::new(reader, src, name, loaded.scene)
        .origins(loaded.origins, structure)
        .nothing_to_draw_ok(loaded.has_nodes)
        .warnings(loaded.warnings)
        .textures(loaded.textures)
        .dependencies(deps)
        .facets(facets)
        .build()?;
    Ok(if format == ufbx::FileFormat::Obj { doc.with_links(objects::ObjLinks(obj)) } else { doc })
}

pub struct FbxReader;

static FBX: ReaderInfo = ReaderInfo::new("fbx", "FBX model", Category::ThreeD).extensions(&["fbx"]);

impl Reader for FbxReader {
    fn info(&self) -> &ReaderInfo {
        &FBX
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        // ASCII FBX is text in its own language (its Source facet); binary isn't text.
        detect_by(&FBX, header.starts_with(b"Kaydara FBX Binary"), name).with_language("fbx")
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> libfileparser_core::Result<Document> {
        let format = if src.header().starts_with(b"Kaydara FBX Binary") { "FBX (binary)" } else { "FBX (ASCII)" };
        open_with_ufbx(self, ufbx::FileFormat::Fbx, format, src, cx)
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
        open_with_ufbx(self, ufbx::FileFormat::Obj, "Wavefront OBJ", src, cx)
    }
}
