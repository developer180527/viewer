//! The GPU side of a scene: what has been uploaded, and the instance batches to draw.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rendererx_scene::glam::{Mat3, Vec3, Vec4};
use rendererx_scene::{Aabb, CpuScene, Indices, MeshData, MeshId, ParamValue, SceneRead, Topology};
use wgpu::util::DeviceExt;

/// Clay colour for nodes without a material colour, linear.
const CLAY: Vec4 = Vec4::new(0.60, 0.57, 0.53, 1.0);
/// Model matrix (4 × vec4), normal matrix (3 × vec4), colour (vec4).
pub(crate) const INSTANCE_FLOATS: usize = 32;

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
pub struct GpuScene {
    pub(crate) id: u64,
    pub(crate) scene: CpuScene,
    pub(crate) meshes: Vec<Option<GpuMesh>>,
    pub(crate) batches: Vec<Batch>,
    pub(crate) instances: Option<wgpu::Buffer>,
    /// Bumped whenever what a frame would draw changes.
    pub(crate) generation: u64,
    seen: u64,
    pending: bool,
    bounds: Option<Aabb>,
    pub(crate) first_mesh_traced: bool,
    pub(crate) first_frame_traced: AtomicBool,
}

impl GpuScene {
    pub(crate) fn new(scene: &CpuScene) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            scene: scene.clone(),
            meshes: Vec::new(),
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
    pub fn bounds(&self) -> Option<Aabb> {
        self.bounds
    }

    /// GPU bytes held: vertex, index and instance buffers.
    pub fn gpu_bytes(&self) -> u64 {
        let meshes: u64 = self.meshes.iter().flatten().map(|m| m.bytes).sum();
        meshes + self.instances.as_ref().map_or(0, |b| b.size())
    }

    /// Uploads what changed since the last call, up to `budget` bytes (at least one
    /// mesh, so progress is always made). Returns the meshes it uploaded, so the caller
    /// can release their CPU data, and whether anything visible changed.
    pub(crate) fn sync(&mut self, device: &wgpu::Device, budget: u64) -> (Vec<MeshId>, bool) {
        if !self.needs_sync() {
            return (Vec::new(), false);
        }
        let scene = self.scene.clone();
        let read = scene.read();
        let revision = read.revision();
        let mut uploaded = Vec::new();
        let mut spent = 0u64;
        self.pending = false;
        if self.meshes.len() < read.meshes().len() {
            self.meshes.resize_with(read.meshes().len(), || None);
        }
        for (i, entry) in read.meshes().iter().enumerate() {
            let stale = self.meshes[i].as_ref().is_none_or(|m| m.revision < entry.changed);
            let Some(data) = entry.data.as_ref().filter(|_| stale) else { continue };
            if spent > 0 && spent + entry.info.bytes > budget {
                self.pending = true;
                break;
            }
            spent += entry.info.bytes;
            self.meshes[i] = upload_mesh(device, data, entry.info.elements as u32, entry.changed);
            uploaded.push(MeshId(i as u32));
        }

        let nodes_changed = read.nodes().iter().any(|n| n.changed > self.seen) || read.materials().iter().any(|m| m.changed > self.seen);
        let changed = !uploaded.is_empty() || nodes_changed;
        if changed {
            self.rebuild_instances(device, &read);
            self.generation += 1;
        }
        self.bounds = read.bounds();
        self.seen = revision;
        (uploaded, changed)
    }

    /// One instance per placed node whose mesh is on the GPU, grouped by mesh so each
    /// mesh is one instanced draw.
    fn rebuild_instances(&mut self, device: &wgpu::Device, read: &SceneRead<'_>) {
        let world = read.world_transforms();
        let mut placed: Vec<(MeshId, usize)> = read
            .nodes()
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.node.mesh.map(|m| (m, i)))
            .filter(|(m, _)| self.meshes.get(m.index()).is_some_and(Option::is_some))
            .collect();
        placed.sort_by_key(|(m, _)| *m);

        let mut data: Vec<[f32; INSTANCE_FLOATS]> = Vec::with_capacity(placed.len());
        self.batches.clear();
        for (mesh, node) in placed {
            let m = world[node];
            let normal = {
                let m3 = Mat3::from_mat4(m);
                if m3.determinant().abs() > 1e-20 { m3.inverse().transpose() } else { Mat3::IDENTITY }
            };
            let color = read.nodes()[node].node.material.and_then(|id| read.material(id)).map_or(CLAY, |e| base_color(&e.material));
            let mut inst = [0f32; INSTANCE_FLOATS];
            inst[..16].copy_from_slice(&m.to_cols_array());
            for (c, col) in [normal.x_axis, normal.y_axis, normal.z_axis].iter().enumerate() {
                inst[16 + c * 4..19 + c * 4].copy_from_slice(&col.to_array());
            }
            inst[28..].copy_from_slice(&color.to_array());

            let index = data.len() as u32;
            match self.batches.last_mut() {
                Some(b) if b.mesh == mesh => b.instances.end = index + 1,
                _ => self.batches.push(Batch { mesh, instances: index..index + 1 }),
            }
            data.push(inst);
        }
        self.instances = (!data.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("rendererx instances"),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
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

fn upload_mesh(device: &wgpu::Device, data: &MeshData, elements: u32, revision: u64) -> Option<GpuMesh> {
    if elements == 0 {
        return None;
    }
    let buffer = |label: &str, contents: &[u8], usage: wgpu::BufferUsages| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents, usage })
    };
    // Generated streams live only until the buffers below are created.
    let generated = data.normals.is_none().then(|| generate_normals(data));
    let (positions, normals, colors, indices) = match (&generated, &data.normals) {
        (Some(g), _) => (&g.positions[..], &g.normals[..], g.colors.as_deref(), g.indices.as_deref().map_or(Ix::None, Ix::U32)),
        (None, Some(n)) => (
            data.positions.as_slice(),
            n.as_slice(),
            data.colors.as_ref().map(|c| c.as_slice()),
            match &data.indices {
                Indices::None => Ix::None,
                Indices::U16(s) => Ix::U16(s.as_slice()),
                Indices::U32(s) => Ix::U32(s.as_slice()),
            },
        ),
        (None, None) => unreachable!("generated when normals are missing"),
    };
    let vertex = wgpu::BufferUsages::VERTEX;
    let index = wgpu::BufferUsages::INDEX;
    let indices = match indices {
        Ix::None => None,
        Ix::U16(s) => Some((buffer("rendererx indices", bytemuck::cast_slice(s), index), wgpu::IndexFormat::Uint16)),
        Ix::U32(s) => Some((buffer("rendererx indices", bytemuck::cast_slice(s), index), wgpu::IndexFormat::Uint32)),
    };
    let mesh = GpuMesh {
        positions: buffer("rendererx positions", bytemuck::cast_slice(positions), vertex),
        normals: buffer("rendererx normals", bytemuck::cast_slice(normals), vertex),
        colors: colors.map(|c| buffer("rendererx colors", bytemuck::cast_slice(c), vertex)),
        indices,
        elements,
        topology: data.topology,
        bytes: 0,
        revision,
    };
    let bytes = mesh.positions.size()
        + mesh.normals.size()
        + mesh.colors.as_ref().map_or(0, |b| b.size())
        + mesh.indices.as_ref().map_or(0, |(b, _)| b.size());
    Some(GpuMesh { bytes, ..mesh })
}

enum Ix<'a> {
    None,
    U16(&'a [u16]),
    U32(&'a [u32]),
}

struct Generated {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    colors: Option<Vec<Vec4>>,
    indices: Option<Vec<u32>>,
}

/// Faces meeting at more than this angle keep a hard edge (Blender's auto smooth).
const HARD_EDGE_COS: f32 = 0.5; // cos 60°

/// Normals for a mesh that has none. Triangles are smoothed across gentle edges and
/// kept hard across sharp ones, so a scan looks smooth and a CAD part keeps its
/// edges; vertices on a hard edge are split. Non-indexed meshes come out flat (no
/// vertex is shared). Lines and points get zero normals, which the shader lights as
/// facing the camera.
fn generate_normals(data: &MeshData) -> Generated {
    let positions = data.positions.as_slice();
    let colors = data.colors.as_ref().map(|c| c.as_slice());
    let indices: Option<Vec<u32>> = match &data.indices {
        Indices::None => None,
        Indices::U16(s) => Some(s.as_slice().iter().map(|&i| i as u32).collect()),
        Indices::U32(s) => Some(s.as_slice().to_vec()),
    };
    let unchanged = |normals| Generated { positions: positions.to_vec(), normals, colors: colors.map(<[Vec4]>::to_vec), indices: indices.clone() };
    if data.topology != Topology::Triangles {
        return unchanged(vec![Vec3::ZERO; positions.len()]);
    }
    let face = |a: u32, b: u32, c: u32| {
        let (pa, pb, pc) = (positions[a as usize], positions[b as usize], positions[c as usize]);
        let n = (pb - pa).cross(pc - pa);
        if n.is_finite() { n } else { Vec3::ZERO }
    };
    // Indices were range-checked by rendererx-scene.
    let Some(indices) = indices.as_deref() else {
        let mut normals = vec![Vec3::ZERO; positions.len()];
        for t in 0..positions.len() / 3 {
            let n = face(t as u32 * 3, t as u32 * 3 + 1, t as u32 * 3 + 2).normalize_or_zero();
            normals[t * 3..t * 3 + 3].fill(n);
        }
        return unchanged(normals);
    };

    let tris = indices.len() / 3;
    let faces: Vec<Vec3> = (0..tris).map(|t| face(indices[t * 3], indices[t * 3 + 1], indices[t * 3 + 2])).collect();
    let units: Vec<Vec3> = faces.iter().map(|n| n.normalize_or_zero()).collect();
    // Triangles around each vertex, as one flat list (CSR).
    let mut start = vec![0u32; positions.len() + 1];
    indices.iter().for_each(|&v| start[v as usize + 1] += 1);
    (0..positions.len()).for_each(|v| start[v + 1] += start[v]);
    let mut fill = start.clone();
    let mut around = vec![0u32; indices.len()];
    for (corner, &v) in indices.iter().enumerate() {
        around[fill[v as usize] as usize] = (corner / 3) as u32;
        fill[v as usize] += 1;
    }

    let mut out = Generated { positions: Vec::new(), normals: Vec::new(), colors: colors.map(|_| Vec::new()), indices: None };
    let mut first = vec![u32::MAX; positions.len()];
    let mut variants: std::collections::HashMap<(u32, [i32; 3]), u32> = Default::default();
    let mut new_indices = Vec::with_capacity(indices.len());
    for (corner, &v) in indices.iter().enumerate() {
        let own = units[corner / 3];
        let incident = &around[start[v as usize] as usize..start[v as usize + 1] as usize];
        let n = incident
            .iter()
            .filter(|&&t| own == Vec3::ZERO || units[t as usize].dot(own) >= HARD_EDGE_COS)
            .fold(Vec3::ZERO, |acc, &t| acc + faces[t as usize])
            .normalize_or_zero();
        let f = first[v as usize];
        let reuse = if f != u32::MAX && out.normals[f as usize].dot(n) > 0.9999 {
            Some(f)
        } else {
            variants.get(&(v, quantize(n))).copied()
        };
        let index = reuse.unwrap_or_else(|| {
            let index = out.positions.len() as u32;
            out.positions.push(positions[v as usize]);
            out.normals.push(n);
            if let (Some(out), Some(src)) = (&mut out.colors, colors) {
                out.push(src[v as usize]);
            }
            if f == u32::MAX {
                first[v as usize] = index;
            } else {
                variants.insert((v, quantize(n)), index);
            }
            index
        });
        new_indices.push(index);
    }
    out.indices = Some(new_indices);
    out
}

fn quantize(n: Vec3) -> [i32; 3] {
    (n * 4096.0).round().as_ivec3().to_array()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed(positions: Vec<Vec3>, indices: Vec<u32>) -> MeshData {
        MeshData::new(positions).with_indices(Indices::U32(indices.into()))
    }

    #[test]
    fn hard_edges_split_vertices_and_flat_surfaces_share_them() {
        // Two coplanar triangles: one smooth surface, nothing to split.
        let quad = indexed(vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::ONE.with_z(0.0)], vec![0, 1, 2, 2, 1, 3]);
        let g = generate_normals(&quad);
        assert_eq!(g.positions.len(), 4);
        assert!(g.normals.iter().all(|n| (*n - Vec3::Z).length() < 1e-5));

        // A 90° fold: the two shared vertices split, each side stays flat.
        let fold = indexed(vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z], vec![0, 1, 2, 1, 0, 3]);
        let g = generate_normals(&fold);
        assert_eq!(g.positions.len(), 6);
        let idx = g.indices.unwrap();
        assert!(g.normals[idx[0] as usize].dot(g.normals[idx[3] as usize]).abs() < 1e-5);

        // A gentle bend (under 60°) is smoothed across: shared vertices stay shared.
        let bend = indexed(vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::new(0.5, -1.0, 0.3)], vec![0, 1, 2, 1, 0, 3]);
        assert_eq!(generate_normals(&bend).positions.len(), 4);
    }

    #[test]
    fn degenerate_triangles_and_lines_dont_produce_nan() {
        let mut lines = MeshData::new(vec![Vec3::ZERO, Vec3::X]);
        lines.topology = Topology::Lines;
        assert_eq!(generate_normals(&lines).normals, vec![Vec3::ZERO; 2]);

        let sliver = indexed(vec![Vec3::ZERO, Vec3::X, Vec3::X * 2.0], vec![0, 1, 2]);
        assert!(generate_normals(&sliver).normals.iter().all(|n| n.is_finite()));
    }
}
