//! USD (`.usda`, `.usdc`, `.usd`, `.usdz`) through `usd-lite`, into a RendererX scene.
//!
//! One layer, uncomposed: references, payloads and variants aren't followed (a
//! warning says so). Meshes are triangulated per material (GeomSubsets give per-face
//! materials), UsdPreviewSurface becomes `pbr`, and the stage is converted to Y up and
//! metres like every other format here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use anyhow::anyhow;
use rendererx_scene::glam::{DMat4, Mat4, Vec2, Vec3, Vec4};
use rendererx_scene::{
    AlphaMode, ColorSpace, CpuScene, Indices, Material, MaterialId, MeshData, Node, ParamValue, SceneBuilder, ShadingId, ShadingSource, Slot, Stream,
    TextureId,
};
use usd_lite::geom::{self, Channel, Input, TextureInput};
use usd_lite::{Format, Layer, Limits, Value};
use viewer_core::{FileSource, Result};

use crate::textures::{self, ImageJob, ImageSource};

/// A texture channel as a map source: (asset, channel index).
type ChannelSource = (String, usize);

pub struct Loaded {
    pub scene: CpuScene,
    pub warnings: Vec<String>,
    pub textures: usize,
    pub pending: Arc<AtomicUsize>,
    pub format: &'static str,
}

/// Where texture files come from: inside the package, or next to the layer file.
enum Files<'a> {
    Package { package: usd_lite::Package<'a>, src: &'a FileSource },
    Dir(Option<PathBuf>),
}

impl Files<'_> {
    fn source(&self, asset: &str) -> Option<ImageSource> {
        if asset.contains("<UDIM>") {
            return None;
        }
        match self {
            Files::Package { package, src } => {
                // Exporters often record the path on their machine: fall back to the
                // file name, at the package root or in `textures/`.
                let name = asset.rsplit(['/', '\\']).next().unwrap_or(asset);
                let bytes = [asset.to_string(), name.to_string(), format!("textures/{name}")].iter().find_map(|a| package.file(a))?;
                // A slice of the mapped file: shared, not copied.
                let start = bytes.as_ptr() as usize - src.bytes().as_ptr() as usize;
                Stream::from_bytes(src.shared_bytes(), start, bytes.len()).ok().map(ImageSource::Bytes)
            }
            Files::Dir(dir) => {
                let dir = dir.as_deref()?;
                let relative = if Path::new(asset).is_absolute() { "" } else { asset.trim_start_matches("./") };
                textures::find_file(dir, relative, asset).map(ImageSource::File)
            }
        }
    }
}

pub fn load(src: &FileSource) -> Result<Loaded> {
    let limits = Limits::default();
    let bytes = src.bytes();
    let format = usd_lite::detect(bytes).ok_or_else(|| anyhow!("not a USD file"))?;
    let (layer, files, label) = match format {
        Format::Usdz => {
            let package = usd_lite::Package::open(bytes, &limits)?;
            let layer = package.layer.clone();
            (layer, Files::Package { package, src }, "USDZ package")
        }
        Format::Usdc => (usd_lite::read_layer(bytes, &limits)?, Files::Dir(src.path().and_then(Path::parent).map(PathBuf::from)), "USD (binary)"),
        Format::Usda => (usd_lite::read_layer(bytes, &limits)?, Files::Dir(src.path().and_then(Path::parent).map(PathBuf::from)), "USD (text)"),
    };

    let mut warnings = Vec::new();
    let arcs = layer.composition_arcs();
    if !arcs.is_empty() {
        warnings.push(format!("uses {}, which aren't followed yet: parts may be missing", arcs.join(", ")));
    }
    let (scene, mut builder) = CpuScene::new();
    let mut c = Converter { layer: &layer, files: &files, b: &mut builder, materials: HashMap::new(), images: HashMap::new(), packed: HashMap::new(), jobs: Vec::new(), warnings: &mut warnings, missing_textures: 0 };
    c.convert();
    if c.missing_textures > 0 {
        let n = c.missing_textures;
        c.warnings.push(format!("{n} texture file(s) not found next to the model"));
    }
    let jobs = std::mem::take(&mut c.jobs);
    builder.finish();
    let textures = jobs.len();
    let pending = textures::spawn(builder.texture_sink(), jobs);
    Ok(Loaded { scene, warnings, textures, pending, format: label })
}

struct Converter<'a, 'f> {
    layer: &'a Layer,
    files: &'a Files<'f>,
    b: &'a mut SceneBuilder,
    materials: HashMap<String, Option<MaterialId>>,
    /// By (asset, sRGB).
    images: HashMap<(String, bool), Option<TextureId>>,
    /// Packed roughness + metalness by their sources.
    packed: HashMap<(Option<ChannelSource>, Option<ChannelSource>), Option<TextureId>>,
    jobs: Vec<ImageJob>,
    warnings: &'a mut Vec<String>,
    missing_textures: usize,
}

/// Prim types whose subtrees hold no geometry to draw.
const NOT_GEOMETRY: &[&str] = &["Material", "Shader", "NodeGraph", "GeomSubset"];

impl Converter<'_, '_> {
    fn convert(&mut self) {
        // Y up, metres.
        let up = match geom::up_axis(self.layer) {
            geom::UpAxis::Z => DMat4::from_rotation_x(-std::f64::consts::FRAC_PI_2),
            geom::UpAxis::Y => DMat4::IDENTITY,
        };
        let root = DMat4::from_scale(rendererx_scene::glam::DVec3::splat(geom::meters_per_unit(self.layer))) * up;
        let mut stack: Vec<(String, DMat4)> = self.layer.root_prims().into_iter().rev().map(|p| (p, root)).collect();
        let mut visited = 0usize;
        while let Some((prim, parent)) = stack.pop() {
            visited += 1;
            if visited > 1_000_000 {
                self.warnings.push("more than a million prims; the rest are skipped".into());
                return;
            }
            let ty = self.layer.type_name(&prim).unwrap_or("");
            let class = matches!(self.layer.field(&prim, "specifier"), Some(Value::Specifier(usd_lite::Specifier::Class)));
            if class || NOT_GEOMETRY.contains(&ty) || !geom::is_drawn(self.layer, &prim) || self.layer.field(&prim, "active").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            let (local, reset) = geom::local_transform(self.layer, &prim);
            let local = DMat4::from_cols_array(&local);
            let world = if reset { root * local } else { parent * local };
            if ty == "Mesh" {
                self.mesh(&prim, world);
            }
            for child in self.layer.children(&prim).into_iter().rev() {
                stack.push((child, world));
            }
        }
    }

    fn mesh(&mut self, prim: &str, world: DMat4) {
        let Some(mesh) = geom::mesh(self.layer, prim) else {
            self.warnings.push(format!("{prim}: not a readable mesh"));
            return;
        };
        let faces = mesh.face_vertex_counts.len();
        // Material per face: subsets first, then the mesh's (inherited) binding.
        let mut face_material: Vec<Option<&str>> = vec![None; faces];
        let binding = geom::bound_material(self.layer, prim, true);
        for f in face_material.iter_mut() {
            *f = binding.as_deref();
        }
        for subset in &mesh.subsets {
            for &f in &subset.faces {
                if let Some(slot) = usize::try_from(f).ok().and_then(|f| face_material.get_mut(f)) {
                    *slot = subset.material.as_deref();
                }
            }
        }
        // Face start offsets in the corner list.
        let mut starts = Vec::with_capacity(faces);
        let mut at = 0usize;
        for &n in &mesh.face_vertex_counts {
            starts.push(at);
            at = at.saturating_add(n.max(0) as usize);
        }
        if at > mesh.face_vertex_indices.len() {
            self.warnings.push(format!("{prim}: face counts and indices disagree"));
            return;
        }
        let mut groups: Vec<Option<&str>> = face_material.clone();
        groups.sort();
        groups.dedup();
        let transform = Mat4::from_cols_array(&world.to_cols_array().map(|v| v as f32));
        let transform = if transform.is_finite() { transform } else { Mat4::IDENTITY };
        for group in groups {
            let Some(data) = self.part(&mesh, &starts, |f| face_material[f] == group) else { continue };
            let material = match group {
                Some(path) => self.material(path),
                // Unbound: the display colour, if any, over clay.
                None => mesh.display_color.as_ref().filter(|c| c.interpolation == geom::Interpolation::Constant).and_then(|c| c.values.first()).and_then(|&c| {
                    let m = Material::clay().with_param("base_color", ParamValue::Vec4(Vec3::from(c).extend(1.0)));
                    self.b.add_material(m).ok()
                }),
            };
            match self.b.add_mesh(data) {
                Ok(id) => {
                    if let Err(e) = self.b.add_node(Node::mesh(id, material, transform, None)) {
                        self.warnings.push(format!("{prim}: {e}"));
                    }
                }
                Err(e) => self.warnings.push(format!("{prim}: {e}")),
            }
        }
    }

    /// The triangles of the faces `include` selects, with shared corners merged.
    fn part(&self, mesh: &geom::Mesh, starts: &[usize], include: impl Fn(usize) -> bool) -> Option<MeshData> {
        let mut corners: HashMap<[u32; 10], u32> = HashMap::new();
        let (mut positions, mut normals, mut uvs, mut colors) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut indices: Vec<u32> = Vec::new();
        let vertex_colors = mesh.display_color.as_ref().filter(|c| c.interpolation != geom::Interpolation::Constant);
        let mut corner_index = |face: usize, corner: usize| -> Option<u32> {
            let point = usize::try_from(*mesh.face_vertex_indices.get(corner)?).ok()?;
            let p = *mesh.points.get(point)?;
            let n = mesh.normals.as_ref().and_then(|v| v.at(face, point, corner));
            // USD's st has v = 0 at the bottom; RendererX (like glTF) at the top.
            let t = mesh.uvs.as_ref().and_then(|v| v.at(face, point, corner)).map(|[u, v]| [u, 1.0 - v]);
            let c = vertex_colors.and_then(|v| v.at(face, point, corner));
            let bits = |v: f32| v.to_bits();
            let mut key = [point as u32, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            if let Some(n) = n {
                key[1..4].copy_from_slice(&n.map(bits));
            }
            if let Some(t) = t {
                key[4..6].copy_from_slice(&t.map(bits));
            }
            if let Some(c) = c {
                key[6..9].copy_from_slice(&c.map(bits));
            }
            let next = positions.len() as u32;
            Some(*corners.entry(key).or_insert_with(|| {
                positions.push(Vec3::from(p));
                if mesh.normals.is_some() {
                    normals.push(Vec3::from(n.unwrap_or([0.0, 0.0, 1.0])));
                }
                if mesh.uvs.is_some() {
                    uvs.push(Vec2::from(t.unwrap_or([0.0, 0.0])));
                }
                if vertex_colors.is_some() {
                    colors.push(Vec3::from(c.unwrap_or([1.0; 3])).extend(1.0));
                }
                next
            }))
        };
        for (face, &start) in starts.iter().enumerate() {
            if !include(face) {
                continue;
            }
            let n = mesh.face_vertex_counts[face].max(0) as usize;
            if n < 3 {
                continue;
            }
            // Fan triangulation; left-handed faces are flipped to counter-clockwise.
            for k in 1..n - 1 {
                let tri = if mesh.left_handed { [0, k + 1, k] } else { [0, k, k + 1] };
                let ids: Option<Vec<u32>> = tri.iter().map(|&c| corner_index(face, start + c)).collect();
                if let Some(ids) = ids {
                    indices.extend(ids);
                }
            }
        }
        if indices.is_empty() {
            return None;
        }
        let mut data = MeshData::new(positions).with_indices(Indices::U32(indices.into()));
        data.normals = (!normals.is_empty()).then(|| normals.into());
        data.uvs = (!uvs.is_empty()).then(|| uvs.into());
        data.colors = (!colors.is_empty()).then(|| colors.into());
        Some(data)
    }

    fn material(&mut self, path: &str) -> Option<MaterialId> {
        if let Some(id) = self.materials.get(path) {
            return *id;
        }
        let id = match geom::preview_surface(self.layer, path) {
            Some(surface) => {
                let m = self.preview_surface(path, &surface);
                self.b.add_material(m).map_err(|e| self.warnings.push(format!("{path}: {e}"))).ok()
            }
            None => {
                self.warnings.push(format!("{path}: no UsdPreviewSurface; drawn in clay"));
                None
            }
        };
        self.materials.insert(path.to_string(), id);
        id
    }

    fn preview_surface(&mut self, path: &str, s: &geom::PreviewSurface) -> Material {
        let mut m = Material::new(ShadingId::PBR, ShadingSource::FromFile);
        m.name = path.rsplit('/').next().unwrap_or(path).to_string();
        let v3 = |v: [f64; 3]| Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32);
        let alpha = match &s.opacity {
            Input::Value(o) => *o as f32,
            Input::Texture(_) => 1.0,
        };
        match &s.diffuse_color {
            Input::Value(c) => m.params.set("base_color", ParamValue::Vec4(v3(*c).extend(alpha))),
            Input::Texture(t) => {
                m.params.set("base_color", ParamValue::Vec4(Vec4::new(1.0, 1.0, 1.0, alpha)));
                if let Some(id) = self.texture(t, t.srgb != Some(false)) {
                    m.textures.push((Slot::BASE_COLOR, id));
                }
            }
        }
        match &s.emissive_color {
            Input::Value(c) => m.params.set("emissive", ParamValue::Vec3(v3(*c))),
            Input::Texture(t) => {
                m.params.set("emissive", ParamValue::Vec3(Vec3::ONE));
                if let Some(id) = self.texture(t, t.srgb != Some(false)) {
                    m.textures.push((Slot::EMISSIVE, id));
                }
            }
        }
        // Scalars, or a channel of a texture (packed with the other into one map).
        let mut map_source = |input: &Input<f64>, param: &str| -> Option<(String, usize)> {
            match input {
                Input::Value(v) => {
                    m.params.set(param.to_string(), ParamValue::Float(*v as f32));
                    None
                }
                Input::Texture(t) => {
                    m.params.set(param.to_string(), ParamValue::Float(1.0));
                    Some((t.file.clone(), channel_index(t.channel)))
                }
            }
        };
        let rough = map_source(&s.roughness, "roughness");
        let metal = if s.specular_workflow {
            m.params.set("metallic", ParamValue::Float(0.0));
            None
        } else {
            map_source(&s.metallic, "metallic")
        };
        if let Some(id) = self.metal_rough(rough, metal) {
            m.textures.push((Slot::METALLIC_ROUGHNESS, id));
        }
        if let Some(t) = &s.normal
            && let Some(id) = self.texture(t, false)
        {
            m.textures.push((Slot::NORMAL, id));
        }
        if let Some(t) = &s.occlusion
            && let Some(id) = self.texture(t, false)
        {
            m.textures.push((Slot::OCCLUSION, id));
        }
        if s.opacity_threshold > 0.0 {
            m.alpha = AlphaMode::Mask(s.opacity_threshold as f32);
        } else if alpha < 1.0 {
            m.alpha = AlphaMode::Blend;
        }
        m.double_sided = true;
        m
    }

    fn texture(&mut self, t: &TextureInput, srgb: bool) -> Option<TextureId> {
        let key = (t.file.clone(), srgb);
        if let Some(id) = self.images.get(&key) {
            return *id;
        }
        let id = match self.files.source(&t.file) {
            Some(source) => self.b.reserve_texture().ok().inspect(|&texture| {
                self.jobs.push(ImageJob { texture, source, color_space: if srgb { ColorSpace::Srgb } else { ColorSpace::Linear } });
            }),
            None => {
                self.missing_textures += 1;
                None
            }
        };
        self.images.insert(key, id);
        id
    }

    fn metal_rough(&mut self, rough: Option<(String, usize)>, metal: Option<(String, usize)>) -> Option<TextureId> {
        if rough.is_none() && metal.is_none() {
            return None;
        }
        let key = (rough.clone(), metal.clone());
        if let Some(id) = self.packed.get(&key) {
            return *id;
        }
        let find = |s: &Option<(String, usize)>| s.as_ref().and_then(|(file, c)| self.files.source(file).map(|src| (Box::new(src), *c)));
        let (r, m) = (find(&rough), find(&metal));
        let id = if r.is_none() && m.is_none() {
            self.missing_textures += 1;
            None
        } else {
            self.b.reserve_texture().ok().inspect(|&texture| {
                self.jobs.push(ImageJob { texture, source: ImageSource::MetalRough { roughness: r, metalness: m }, color_space: ColorSpace::Linear });
            })
        };
        self.packed.insert(key, id);
        id
    }
}

fn channel_index(c: Channel) -> usize {
    match c {
        Channel::G => 1,
        Channel::B => 2,
        Channel::A => 3,
        _ => 0,
    }
}
