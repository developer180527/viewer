//! The GPU side of a scene: what has been uploaded, and the instance batches to draw.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rendererx_scene::glam::{Mat3, Vec4};
use rendererx_scene::{CpuScene, Indices, MaterialId, MeshData, MeshId, ParamValue, ReaderGuard, SceneRead, TextureId, Topology};
use wgpu::util::DeviceExt;

use crate::textures::{GpuTexture, MaterialBinding, Model, SLOTS, texture_for, upload_texture};

/// Clay colour for nodes without a material colour, linear.
const CLAY: Vec4 = Vec4::new(0.60, 0.57, 0.53, 1.0);
/// Model matrix (4 × vec4), normal matrix (3 × vec4; the first column's w is the
/// mirror sign), colour (vec4).
pub(crate) const INSTANCE_FLOATS: usize = 32;
const INSTANCE_BYTES: u64 = INSTANCE_FLOATS as u64 * 4;
const TOO_MANY_INSTANCES: &str = "too many placed meshes";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) struct GpuMesh {
    pub positions: wgpu::Buffer,
    pub normals: wgpu::Buffer,
    pub colors: Option<wgpu::Buffer>,
    pub uvs: Option<wgpu::Buffer>,
    pub indices: Option<(wgpu::Buffer, wgpu::IndexFormat)>,
    pub elements: u32,
    pub topology: Topology,
    pub bytes: u64,
    /// Scene revision of the data that was uploaded.
    pub revision: u64,
}

/// A material's two bind groups over one factors block: with its maps, and with the
/// 1×1 defaults for meshes that have no uvs to sample maps with.
pub(crate) struct MaterialGroups {
    uniforms: wgpu::Buffer,
    pub textured: wgpu::BindGroup,
    pub plain: wgpu::BindGroup,
    /// Revision of the material these were built from.
    revision: u64,
}

pub(crate) struct Batch {
    pub mesh: MeshId,
    /// Selects the bind group (factors and maps); `None` uses the defaults.
    pub material: Option<MaterialId>,
    /// The material's shading model: which pipeline draws the batch.
    pub model: Model,
    pub instances: Range<u32>,
}

/// A [`CpuScene`] attached to a renderer. Uploads happen in [`Renderer::sync`](crate::Renderer::sync),
/// a budget at a time, so a huge model never stalls a frame. Dropping it frees all its
/// GPU memory.
///
/// Several `GpuScene`s may follow one `CpuScene` (one per device or renderer); CPU data
/// is freed only once all of them have uploaded it. Attach them before the first sync:
/// one attached later finds released meshes gone ([`unavailable_meshes`](Self::unavailable_meshes)).
/// Views of one model on one renderer should share a single `GpuScene`.
pub struct GpuScene {
    pub(crate) id: u64,
    pub(crate) scene: CpuScene,
    pub(crate) reader: ReaderGuard,
    pub(crate) meshes: Vec<Option<GpuMesh>>,
    textures: Vec<Option<GpuTexture>>,
    /// Textures the device refused, by the revision that failed.
    texture_failed: HashMap<usize, u64>,
    /// Per material: its bind groups (factors, and whichever maps are on the GPU).
    pub(crate) material_groups: Vec<Option<MaterialGroups>>,
    /// Meshes that can't be uploaded (over a device limit), by the revision that failed.
    failed: HashMap<usize, u64>,
    errors: Vec<(MeshId, String)>,
    texture_errors: Vec<(TextureId, String)>,
    unavailable: usize,
    pub(crate) batches: Vec<Batch>,
    pub(crate) instances: Option<wgpu::Buffer>,
    /// Bumped whenever what a frame would draw changes.
    pub(crate) generation: u64,
    seen: u64,
    /// [`SceneRead::bounds_revision`] the cached bounds were computed at.
    bounds_seen: Option<u64>,
    pending: bool,
    bounds: Option<rendererx_scene::Aabb>,
    pub(crate) first_mesh_traced: bool,
    pub(crate) first_frame_traced: AtomicBool,
}

/// What one sync did.
pub(crate) struct Synced {
    /// Uploaded this time (for tracing).
    pub uploaded: usize,
    /// Meshes this scene is done with (uploaded or refused), whose CPU data may go.
    pub done: Vec<MeshId>,
    /// The same for textures.
    pub done_textures: Vec<TextureId>,
    pub changed: bool,
}

impl GpuScene {
    pub(crate) fn new(scene: &CpuScene) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            scene: scene.clone(),
            reader: scene.add_reader(),
            meshes: Vec::new(),
            textures: Vec::new(),
            texture_failed: HashMap::new(),
            material_groups: Vec::new(),
            failed: HashMap::new(),
            errors: Vec::new(),
            texture_errors: Vec::new(),
            unavailable: 0,
            batches: Vec::new(),
            instances: None,
            generation: 0,
            seen: 0,
            bounds_seen: None,
            pending: true,
            bounds: None,
            first_mesh_traced: false,
            first_frame_traced: AtomicBool::new(false),
        }
    }

    pub fn cpu_scene(&self) -> &CpuScene {
        &self.scene
    }

    /// Something in the CPU scene hasn't been uploaded yet.
    pub fn needs_sync(&self) -> bool {
        self.pending || self.scene.revision() != self.seen
    }

    /// The scene's bounds as of the last sync, for framing the camera.
    pub fn bounds(&self) -> Option<rendererx_scene::Aabb> {
        self.bounds
    }

    /// GPU bytes held: vertex, index and instance buffers, and textures.
    pub fn gpu_bytes(&self) -> u64 {
        let meshes: u64 = self.meshes.iter().flatten().map(|m| m.bytes).sum();
        let textures: u64 = self.textures.iter().flatten().map(|t| t.bytes).sum();
        meshes + textures + self.instances.as_ref().map_or(0, |b| b.size())
    }

    /// Meshes (and instances) the device refused, e.g. a buffer over its
    /// `max_buffer_size`. They are skipped; the rest of the model still draws.
    pub fn upload_errors(&self) -> &[(MeshId, String)] {
        &self.errors
    }

    /// Textures the device refused (an unsupported compressed format, over its size
    /// limit). Those materials draw untextured.
    pub fn texture_errors(&self) -> &[(TextureId, String)] {
        &self.texture_errors
    }

    /// Meshes whose CPU data was already freed before this scene uploaded them (it was
    /// attached too late). Zero in normal use.
    pub fn unavailable_meshes(&self) -> usize {
        self.unavailable
    }

    /// Uploads what changed since the last call, up to `budget` bytes (at least one
    /// mesh, so progress is always made).
    pub(crate) fn sync(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, materials: &MaterialBinding, budget: u64) -> Synced {
        let mut out = Synced { uploaded: 0, done: Vec::new(), done_textures: Vec::new(), changed: false };
        if !self.needs_sync() {
            return out;
        }
        let scene = self.scene.clone();
        let read = scene.read();
        let revision = read.revision();
        let limits = device.limits();
        let mut spent = 0u64;
        let mut fresh = Vec::new();
        self.pending = false;
        self.unavailable = 0;
        if self.meshes.len() < read.meshes().len() {
            self.meshes.resize_with(read.meshes().len(), || None);
        }
        for (i, entry) in read.meshes().iter().enumerate() {
            let stale = self.meshes[i].as_ref().is_none_or(|m| m.revision < entry.changed)
                && self.failed.get(&i).is_none_or(|&rev| rev < entry.changed);
            if !stale {
                continue;
            }
            let Some(data) = entry.data.as_ref() else {
                self.unavailable += self.meshes[i].is_none() as usize;
                continue;
            };
            if spent > 0 && spent + entry.info.bytes > budget {
                self.pending = true;
                break;
            }
            spent += entry.info.bytes;
            let id = MeshId(i as u32);
            match upload_mesh(device, &limits, data, entry.info.elements, entry.changed) {
                Ok(mesh) => {
                    self.meshes[i] = mesh;
                    out.uploaded += 1;
                    fresh.push(id);
                }
                Err(e) => {
                    self.failed.insert(i, entry.changed);
                    self.errors.push((id, e));
                }
            }
            out.done.push(id);
        }

        // Textures share the budget, after geometry: the model shows first, then its
        // surfaces arrive.
        if self.textures.len() < read.textures().len() {
            self.textures.resize_with(read.textures().len(), || None);
        }
        let mut arrived: Vec<TextureId> = Vec::new();
        for (i, entry) in read.textures().iter().enumerate() {
            let stale = self.textures[i].as_ref().is_none_or(|t| t.revision < entry.changed)
                && self.texture_failed.get(&i).is_none_or(|&rev| rev < entry.changed);
            let (Some(data), true, false) = (entry.data.as_ref(), stale, self.pending) else { continue };
            let bytes = data.byte_len();
            if spent > 0 && spent + bytes > budget {
                self.pending = true;
                break;
            }
            spent += bytes;
            let id = TextureId(i as u32);
            match upload_texture(device, queue, &limits, data, entry.changed) {
                Ok(t) => {
                    self.textures[i] = Some(t);
                    arrived.push(id);
                }
                Err(e) => {
                    self.texture_failed.insert(i, entry.changed);
                    self.texture_errors.push((id, e));
                }
            }
            out.done_textures.push(id);
        }
        // Only the groups an arrival or edit affects are rebuilt: with every material
        // rebuilt per arriving texture, a model's load time grew with materials ×
        // textures.
        let mut textures_shown = false;
        self.material_groups.resize_with(read.materials().len(), || None);
        for (i, m) in read.materials().iter().enumerate() {
            let edited = self.material_groups[i].as_ref().is_none_or(|g| g.revision < m.changed);
            let uses_arrival = !arrived.is_empty() && m.material.textures.iter().any(|(_, t)| arrived.contains(t));
            if !edited && !uses_arrival {
                continue;
            }
            textures_shown |= uses_arrival;
            let maps = std::array::from_fn(|i| {
                let id = texture_for(&m.material, &SLOTS[i])?;
                self.textures.get(id.index())?.as_ref().map(|t| &t.view)
            });
            match &mut self.material_groups[i] {
                Some(g) if !edited => g.textured = materials.group(device, &g.uniforms, maps),
                slot => {
                    let uniforms = materials.uniforms(device, Some(&m.material));
                    let textured = materials.group(device, &uniforms, maps);
                    let plain = materials.group(device, &uniforms, [None; 5]);
                    *slot = Some(MaterialGroups { uniforms, textured, plain, revision: m.changed });
                }
            }
        }

        let nodes_changed = read.nodes().iter().any(|n| n.changed > self.seen) || read.materials().iter().any(|m| m.changed > self.seen);
        // A new mesh only changes the frame if some node places it.
        let placed_fresh = !fresh.is_empty() && read.nodes().iter().any(|n| n.node.mesh.is_some_and(|m| fresh.contains(&m)));
        // Instances depend on nodes, materials and which meshes are on the GPU, not on
        // textures: an arriving texture only swaps a bind group.
        if nodes_changed || placed_fresh {
            self.rebuild_instances(device, queue, &limits, &read);
        }
        out.changed = nodes_changed || placed_fresh || textures_shown;
        if out.changed {
            self.generation += 1;
        }
        // Bounds walk every node; only when what they depend on changed.
        if self.bounds_seen != Some(read.bounds_revision()) {
            self.bounds = read.bounds();
            self.bounds_seen = Some(read.bounds_revision());
        }
        self.seen = revision;
        out
    }

    /// One instance per placed node whose mesh is on the GPU, grouped by mesh so each
    /// mesh is one instanced draw. The buffer is reused while it is big enough, so a
    /// model streaming in doesn't allocate a new one every sync.
    fn rebuild_instances(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, limits: &wgpu::Limits, read: &SceneRead<'_>) {
        let world = read.world_transforms();
        let mut placed: Vec<(MeshId, Option<MaterialId>, usize)> = read
            .nodes()
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.node.mesh.map(|m| (m, n.node.material, i)))
            .filter(|(m, _, _)| self.meshes.get(m.index()).is_some_and(Option::is_some))
            .collect();
        // One draw per (mesh, material): instances of a batch share the texture.
        placed.sort_by_key(|&(m, mat, _)| (m, mat));
        let max = (limits.max_buffer_size / INSTANCE_BYTES) as usize;
        // Replace, not append: this runs on every rebuild while a model streams in.
        self.errors.retain(|(_, e)| !e.starts_with(TOO_MANY_INSTANCES));
        if placed.len() > max {
            self.errors.push((placed[max].0, format!("{TOO_MANY_INSTANCES}: {} placed, this device draws at most {max}", placed.len())));
            placed.truncate(max);
        }

        let mut data: Vec<[f32; INSTANCE_FLOATS]> = Vec::with_capacity(placed.len());
        self.batches.clear();
        for (mesh, material, node) in placed {
            let m = world[node];
            let m3 = Mat3::from_mat4(m);
            let det = m3.determinant();
            let normal = if det.abs() > 1e-20 { m3.inverse().transpose() } else { Mat3::IDENTITY };
            let color = read.nodes()[node].node.material.and_then(|id| read.material(id)).map_or(CLAY, |e| base_color(&e.material));
            let mut inst = [0f32; INSTANCE_FLOATS];
            inst[..16].copy_from_slice(&m.to_cols_array());
            for (c, col) in [normal.x_axis, normal.y_axis, normal.z_axis].iter().enumerate() {
                inst[16 + c * 4..19 + c * 4].copy_from_slice(&col.to_array());
            }
            // A mirrored transform flips triangle winding on screen; the shader uses
            // this to keep "front" meaning the outside (glTF spec §3.7.4).
            inst[19] = if det < 0.0 { -1.0 } else { 1.0 };
            inst[28..].copy_from_slice(&color.to_array());

            let index = data.len() as u32;
            match self.batches.last_mut() {
                Some(b) if b.mesh == mesh && b.material == material => b.instances.end = index + 1,
                _ => {
                    let model = Model::of(material.and_then(|m| read.material(m)).map(|e| &e.material));
                    self.batches.push(Batch { mesh, material, model, instances: index..index + 1 })
                }
            }
            data.push(inst);
        }
        if data.is_empty() {
            self.instances = None;
            return;
        }
        let bytes: &[u8] = bytemuck::cast_slice(&data);
        match &self.instances {
            Some(buffer) if buffer.size() >= bytes.len() as u64 => queue.write_buffer(buffer, 0, bytes),
            _ => {
                let size = (bytes.len() as u64).next_power_of_two().min(limits.max_buffer_size);
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("rendererx instances"),
                    size,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                queue.write_buffer(&buffer, 0, bytes);
                self.instances = Some(buffer);
            }
        }
    }
}

/// The material's `base_color` parameter when it has one. Without one: the clay
/// colour for clay materials, and white for the rest (glTF's default factor), so a
/// map or an unlit colour isn't darkened to clay grey.
fn base_color(material: &rendererx_scene::Material) -> Vec4 {
    match material.params.get("base_color") {
        Some(ParamValue::Vec4(c)) => c,
        Some(ParamValue::Vec3(c)) => c.extend(1.0),
        _ if material.shading == rendererx_scene::ShadingId::CLAY => CLAY,
        _ => Vec4::ONE,
    }
}

/// `Ok(None)` for a mesh with nothing to draw; `Err` when a buffer would exceed what
/// the device allows (wgpu would otherwise raise a validation error, which panics by
/// default).
fn upload_mesh(device: &wgpu::Device, limits: &wgpu::Limits, data: &MeshData, elements: usize, revision: u64) -> Result<Option<GpuMesh>, String> {
    if elements == 0 {
        return Ok(None);
    }
    let elements = u32::try_from(elements).map_err(|_| format!("{elements} elements don't fit one draw call"))?;
    let positions = data.positions.as_slice();
    // Triangle normals are generated when the mesh enters the scene; lines and points
    // get zero normals, which the shader lights as facing the camera.
    let zeros;
    let normals: &[_] = match &data.normals {
        Some(n) => n.as_slice(),
        None => {
            zeros = vec![rendererx_scene::glam::Vec3::ZERO; positions.len()];
            &zeros
        }
    };
    let colors = data.colors.as_ref().map(|c| c.as_slice());
    let uvs = data.uvs.as_ref().map(|c| c.as_slice());
    let (index_bytes, format): (&[u8], _) = match &data.indices {
        Indices::None => (&[], None),
        Indices::U16(s) => (bytemuck::cast_slice(s.as_slice()), Some(wgpu::IndexFormat::Uint16)),
        Indices::U32(s) => (bytemuck::cast_slice(s.as_slice()), Some(wgpu::IndexFormat::Uint32)),
    };
    let sizes = [
        ("positions", size_of_val(positions)),
        ("normals", size_of_val(normals)),
        ("colors", colors.map_or(0, size_of_val)),
        ("uvs", uvs.map_or(0, size_of_val)),
        ("indices", index_bytes.len()),
    ];
    if let Some((what, size)) = sizes.iter().find(|(_, size)| *size as u64 > limits.max_buffer_size) {
        return Err(format!("{what} need {size} bytes; this device's buffers hold at most {}", limits.max_buffer_size));
    }

    let buffer = |label: &str, contents: &[u8], usage: wgpu::BufferUsages| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents, usage })
    };
    let vertex = wgpu::BufferUsages::VERTEX;
    let mesh = GpuMesh {
        positions: buffer("rendererx positions", bytemuck::cast_slice(positions), vertex),
        normals: buffer("rendererx normals", bytemuck::cast_slice(normals), vertex),
        colors: colors.map(|c| buffer("rendererx colors", bytemuck::cast_slice(c), vertex)),
        uvs: uvs.map(|c| buffer("rendererx uvs", bytemuck::cast_slice(c), vertex)),
        indices: format.map(|f| (buffer("rendererx indices", index_bytes, wgpu::BufferUsages::INDEX), f)),
        elements,
        topology: data.topology,
        bytes: 0,
        revision,
    };
    let bytes = mesh.positions.size()
        + mesh.normals.size()
        + mesh.colors.as_ref().map_or(0, |b| b.size())
        + mesh.uvs.as_ref().map_or(0, |b| b.size())
        + mesh.indices.as_ref().map_or(0, |(b, _)| b.size());
    Ok(Some(GpuMesh { bytes, ..mesh }))
}
