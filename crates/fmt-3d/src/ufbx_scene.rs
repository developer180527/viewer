//! FBX and OBJ through ufbx, into a RendererX CPU scene.
//!
//! ufbx converts axes and units (Y up, metres), triangulates polygons and resolves the
//! node hierarchy. Each mesh is converted once per material part and placed by every
//! node that uses it, with the node's world transform.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use anyhow::{Context, anyhow};
use rendererx_scene::glam::{Mat4, Vec2, Vec3, Vec4};
use rendererx_scene::{
    ColorSpace, CpuScene, Indices, Material, MaterialId, MeshData, MeshId, Node, ParamValue, SceneBuilder, ShadingId, ShadingSource, Slot,
    Stream, TextureId,
};
use viewer_core::{FileSource, Result};

use crate::textures::{self, ImageJob, ImageSource};

pub struct Loaded {
    pub scene: CpuScene,
    pub warnings: Vec<String>,
    /// Base colour maps queued for decoding.
    pub textures: usize,
    pub pending: Arc<AtomicUsize>,
}

pub fn load(src: &FileSource, format: ufbx::FileFormat) -> Result<Loaded> {
    let filename = src.path().map(|p| p.to_string_lossy().into_owned());
    let opts = ufbx::LoadOpts {
        file_format: format,
        target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
        target_unit_meters: 1.0,
        // OBJ's materials live in a .mtl next to it; found through `filename`.
        load_external_files: filename.is_some(),
        ignore_missing_external_files: true,
        obj_search_mtl_by_filename: true,
        filename: filename.as_deref().map_or_else(Default::default, Into::into),
        ..Default::default()
    };
    let root = ufbx::load_memory(src.bytes(), opts).map_err(|e| anyhow!("{}", e.description))?;
    let (scene, mut builder) = CpuScene::new();
    // ufbx's own notes (a missing .mtl, repaired indices…), deduplicated by its count.
    let mut warnings: Vec<String> = root
        .metadata
        .warnings
        .iter()
        .map(|w| if w.count > 1 { format!("{} (×{})", w.description, w.count) } else { w.description.to_string() })
        .collect();
    let mut images = Images { dir: src.path().and_then(|p| p.parent()).map(PathBuf::from), seen: HashMap::new(), jobs: Vec::new() };
    convert(&root, &mut builder, &mut images, &mut warnings).context("building the scene")?;
    builder.finish();
    let textures = images.jobs.len();
    let pending = textures::spawn(builder.texture_sink(), images.jobs);
    Ok(Loaded { scene, warnings, textures, pending })
}

/// Texture images found while converting materials, each queued once.
struct Images {
    /// The model's folder; texture files must be inside it.
    dir: Option<PathBuf>,
    seen: HashMap<u32, Option<TextureId>>,
    jobs: Vec<ImageJob>,
}

impl Images {
    /// The scene texture for `tex` as a colour map: its embedded bytes, or the file it
    /// names, found next to the model.
    fn texture(&mut self, tex: &ufbx::Texture, b: &mut SceneBuilder) -> Option<TextureId> {
        if let Some(id) = self.seen.get(&tex.element.typed_id) {
            return *id;
        }
        // A layered texture: use its first file.
        let file = tex.file_textures.first().map(|t| &**t).unwrap_or(tex);
        let source = if !file.content.is_empty() {
            Some(ImageSource::Bytes(Stream::from(file.content.to_vec())))
        } else {
            self.dir.as_deref().and_then(|dir| textures::find_file(dir, &file.relative_filename, &file.absolute_filename)).map(ImageSource::File)
        };
        let id = source.and_then(|source| {
            let texture = b.reserve_texture().ok()?;
            self.jobs.push(ImageJob { texture, source, color_space: ColorSpace::Srgb });
            Some(texture)
        });
        self.seen.insert(tex.element.typed_id, id);
        id
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
                    let map = [&m.pbr.base_color, &m.fbx.diffuse_color].into_iter().find_map(|map| map.texture.as_ref().filter(|_| map.texture_enabled));
                    if let Some(id) = map.and_then(|t| images.texture(t, b)) {
                        out.textures.push((Slot::BASE_COLOR, id));
                    }
                    b.add_material(out).map_err(|e| warnings.push(format!("material {}: {e}", m.element.name))).ok()
                })
            });
            if let Err(e) = b.add_node(Node::mesh(id, material, world, None)) {
                warnings.push(format!("node {}: {e}", node.element.name));
                // Every later node would hit the same limit.
                if matches!(e, rendererx_scene::Error::LimitExceeded { .. }) {
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
/// colour (docs/3d-renderer.md §6).
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
    out
}
