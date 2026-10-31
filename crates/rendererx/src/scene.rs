//! The GPU side of a scene: what has been uploaded, and the instance batches to draw.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rendererx_scene::glam::{Mat3, Vec4};
use rendererx_scene::{CpuScene, Indices, MeshData, MeshId, ParamValue, ReaderGuard, SceneRead, Topology};
use wgpu::util::DeviceExt;

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
    pub indices: Option<(wgpu::Buffer, wgpu::IndexFormat)>,
    pub elements: u32,
    pub topology: Topology,
    pub bytes: u64,
    /// Scene revision of the data that was uploaded.
    pub revision: u64,
}

pub(crate) struct Batch {
    pub mesh: MeshId,
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
    _reader: ReaderGuard,
    pub(crate) meshes: Vec<Option<GpuMesh>>,
    /// Meshes that can't be uploaded (over a device limit), by the revision that failed.
    failed: HashMap<usize, u64>,
    errors: Vec<(MeshId, String)>,
    unavailable: usize,
    pub(crate) batches: Vec<Batch>,
    pub(crate) instances: Option<wgpu::Buffer>,
    /// Bumped whenever what a frame would draw changes.
    pub(crate) generation: u64,
    seen: u64,
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
    pub changed: bool,
}

impl GpuScene {
    pub(crate) fn new(scene: &CpuScene) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            scene: scene.clone(),
            _reader: scene.add_reader(),
            meshes: Vec::new(),
            failed: HashMap::new(),
            errors: Vec::new(),
            unavailable: 0,
            batches: Vec::new(),
            instances: None,
            generation: 0,
            seen: 0,
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

    /// GPU bytes held: vertex, index and instance buffers.
    pub fn gpu_bytes(&self) -> u64 {
        let meshes: u64 = self.meshes.iter().flatten().map(|m| m.bytes).sum();
        meshes + self.instances.as_ref().map_or(0, |b| b.size())
    }

    /// Meshes (and instances) the device refused, e.g. a buffer over its
    /// `max_buffer_size`. They are skipped; the rest of the model still draws.
    pub fn upload_errors(&self) -> &[(MeshId, String)] {
        &self.errors
    }

    /// Meshes whose CPU data was already freed before this scene uploaded them (it was
    /// attached too late). Zero in normal use.
    pub fn unavailable_meshes(&self) -> usize {
        self.unavailable
    }

    /// Uploads what changed since the last call, up to `budget` bytes (at least one
    /// mesh, so progress is always made).
    pub(crate) fn sync(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, budget: u64) -> Synced {
        let mut out = Synced { uploaded: 0, done: Vec::new(), changed: false };
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

        let nodes_changed = read.nodes().iter().any(|n| n.changed > self.seen) || read.materials().iter().any(|m| m.changed > self.seen);
        // A new mesh only changes the frame if some node places it.
        let placed_fresh = !fresh.is_empty() && read.nodes().iter().any(|n| n.node.mesh.is_some_and(|m| fresh.contains(&m)));
        out.changed = nodes_changed || placed_fresh;
        if out.changed {
            self.rebuild_instances(device, queue, &limits, &read);
            self.generation += 1;
        }
        self.bounds = read.bounds();
        self.seen = revision;
        out
    }

    /// One instance per placed node whose mesh is on the GPU, grouped by mesh so each
    /// mesh is one instanced draw. The buffer is reused while it is big enough, so a
    /// model streaming in doesn't allocate a new one every sync.
    fn rebuild_instances(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, limits: &wgpu::Limits, read: &SceneRead<'_>) {
        let world = read.world_transforms();
        let mut placed: Vec<(MeshId, usize)> = read
            .nodes()
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.node.mesh.map(|m| (m, i)))
            .filter(|(m, _)| self.meshes.get(m.index()).is_some_and(Option::is_some))
            .collect();
        placed.sort_by_key(|(m, _)| *m);
        let max = (limits.max_buffer_size / INSTANCE_BYTES) as usize;
        // Replace, not append: this runs on every rebuild while a model streams in.
        self.errors.retain(|(_, e)| !e.starts_with(TOO_MANY_INSTANCES));
        if placed.len() > max {
            self.errors.push((placed[max].0, format!("{TOO_MANY_INSTANCES}: {} placed, this device draws at most {max}", placed.len())));
            placed.truncate(max);
        }

        let mut data: Vec<[f32; INSTANCE_FLOATS]> = Vec::with_capacity(placed.len());
        self.batches.clear();
        for (mesh, node) in placed {
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
                Some(b) if b.mesh == mesh => b.instances.end = index + 1,
                _ => self.batches.push(Batch { mesh, instances: index..index + 1 }),
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

/// The material's `base_color` parameter when it has one, else clay.
fn base_color(material: &rendererx_scene::Material) -> Vec4 {
    match material.params.get("base_color") {
        Some(ParamValue::Vec4(c)) => c,
        Some(ParamValue::Vec3(c)) => c.extend(1.0),
        _ => CLAY,
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
    let (index_bytes, format): (&[u8], _) = match &data.indices {
        Indices::None => (&[], None),
        Indices::U16(s) => (bytemuck::cast_slice(s.as_slice()), Some(wgpu::IndexFormat::Uint16)),
        Indices::U32(s) => (bytemuck::cast_slice(s.as_slice()), Some(wgpu::IndexFormat::Uint32)),
    };
    let sizes = [
        ("positions", size_of_val(positions)),
        ("normals", size_of_val(normals)),
        ("colors", colors.map_or(0, size_of_val)),
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
        indices: format.map(|f| (buffer("rendererx indices", index_bytes, wgpu::BufferUsages::INDEX), f)),
        elements,
        topology: data.topology,
        bytes: 0,
        revision,
    };
    let bytes = mesh.positions.size()
        + mesh.normals.size()
        + mesh.colors.as_ref().map_or(0, |b| b.size())
        + mesh.indices.as_ref().map_or(0, |(b, _)| b.size());
    Ok(Some(GpuMesh { bytes, ..mesh }))
}
