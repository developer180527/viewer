use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use glam::Mat4;

use crate::error::check;
use crate::{Aabb, Error, Material, MeshData, MeshInfo, SceneLimits, TextureData, TextureInfo};

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl $name {
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id!(MeshId);
id!(MaterialId);
id!(NodeId);
id!(
    /// Reserved before the texture decodes, so materials can point at it right away.
    TextureId
);

/// A placed mesh, or a group (no mesh) that only carries a transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Node {
    pub mesh: Option<MeshId>,
    /// `None` draws with clay.
    pub material: Option<MaterialId>,
    /// Relative to the parent.
    pub transform: Mat4,
    /// Must already exist, so the hierarchy can't contain cycles.
    pub parent: Option<NodeId>,
}

impl Node {
    pub fn group(transform: Mat4, parent: Option<NodeId>) -> Self {
        Self { mesh: None, material: None, transform, parent }
    }

    pub fn mesh(mesh: MeshId, material: Option<MaterialId>, transform: Mat4, parent: Option<NodeId>) -> Self {
        Self { mesh: Some(mesh), material, transform, parent }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryState {
    Loading,
    /// The loader called [`SceneBuilder::finish`]. Textures may still arrive.
    Done,
    /// The builder was dropped without finishing (the loader failed part way); what
    /// arrived is still shown.
    Incomplete,
}

/// Every entry records the scene revision that last changed it, so a renderer that has
/// seen revision `r` uploads only entries with `changed > r`.
#[derive(Debug)]
pub struct MeshEntry {
    pub info: MeshInfo,
    /// `None` once released after upload (docs/3d-renderer.md §11.1).
    pub data: Option<MeshData>,
    pub changed: u64,
    /// Ids of the readers that have uploaded this data; it is freed once every live
    /// reader is in here.
    pub uploaded_by: Vec<u32>,
}

#[derive(Debug)]
pub struct MaterialEntry {
    pub material: Material,
    pub changed: u64,
}

#[derive(Debug)]
pub struct NodeEntry {
    pub node: Node,
    /// 1 for a root.
    pub depth: u32,
    pub changed: u64,
}

#[derive(Debug)]
pub struct TextureEntry {
    /// `None` while reserved but not yet decoded.
    pub info: Option<TextureInfo>,
    pub data: Option<TextureData>,
    pub changed: u64,
    /// Ids of the readers that have uploaded this data; it is freed once every live
    /// reader is in here.
    pub uploaded_by: Vec<u32>,
}

#[derive(Debug)]
struct State {
    revision: u64,
    geometry: GeometryState,
    bounds: Option<Aabb>,
    meshes: Vec<MeshEntry>,
    materials: Vec<MaterialEntry>,
    nodes: Vec<NodeEntry>,
    textures: Vec<TextureEntry>,
    keep_cpu_data: bool,
    /// Ids of live [`ReaderGuard`]s: renderers that upload this scene (one per
    /// `GpuScene`).
    readers: Vec<u32>,
    next_reader: u32,
    // Running totals checked against the limits. They count what arrived, not what is
    // still held, so releasing data can't be used to get past a limit.
    received_bytes: u64,
    vertices: u64,
    primitives: u64,
    drawn_primitives: u64,
}

type OnChange = Arc<dyn Fn() + Send + Sync>;

struct Inner {
    limits: SceneLimits,
    state: RwLock<State>,
    revision: AtomicU64,
    on_change: RwLock<Option<OnChange>>,
}

/// The host's handle to a scene: cheap to clone, `Send + Sync`. Readers (renderers,
/// the UI) see everything the builder has added so far.
#[derive(Clone)]
pub struct CpuScene(Arc<Inner>);

impl CpuScene {
    /// A scene with the default limits, and the builder a loader fills it through.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> (CpuScene, SceneBuilder) {
        Self::with_limits(SceneLimits::default())
    }

    pub fn with_limits(limits: SceneLimits) -> (CpuScene, SceneBuilder) {
        let state = State {
            revision: 0,
            geometry: GeometryState::Loading,
            bounds: None,
            meshes: Vec::new(),
            materials: Vec::new(),
            nodes: Vec::new(),
            textures: Vec::new(),
            keep_cpu_data: false,
            readers: Vec::new(),
            next_reader: 0,
            received_bytes: 0,
            vertices: 0,
            primitives: 0,
            drawn_primitives: 0,
        };
        let scene = CpuScene(Arc::new(Inner {
            limits,
            state: RwLock::new(state),
            revision: AtomicU64::new(0),
            on_change: RwLock::new(None),
        }));
        let builder = SceneBuilder { scene: scene.clone(), finished: false };
        (scene, builder)
    }

    pub fn limits(&self) -> &SceneLimits {
        &self.0.limits
    }

    /// Bumped by every change. Cheap: no lock.
    pub fn revision(&self) -> u64 {
        self.0.revision.load(Ordering::Acquire)
    }

    pub fn read(&self) -> SceneRead<'_> {
        SceneRead(self.0.state.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Called after every change, on the thread that made it, with no lock held.
    /// Hosts use it to wake their event loop.
    pub fn set_on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.0.on_change.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
    }

    /// Editors keep the CPU data as the source of truth; viewers let the renderer free
    /// it after upload. Off by default.
    pub fn set_keep_cpu_data(&self, keep: bool) {
        self.lock().keep_cpu_data = keep;
    }

    /// Registers a reader that uploads this scene and then releases what it uploaded
    /// through the guard (each `GpuScene` holds one). CPU data is freed only once every
    /// live reader has uploaded it, so two windows showing one model both get it. A
    /// reader registered after data was freed can't get it back: register before the
    /// first upload, or keep CPU data.
    pub fn add_reader(&self) -> ReaderGuard {
        let mut s = self.lock();
        let id = s.next_reader;
        s.next_reader = s.next_reader.wrapping_add(1);
        s.readers.push(id);
        ReaderGuard { scene: self.clone(), id }
    }

    /// Frees a mesh's vertex data (its [`MeshInfo`] stays), for hosts that upload
    /// without registering a reader. Does nothing while readers are registered: they
    /// release through their [`ReaderGuard`]. Returns true when the data was freed.
    pub fn release_mesh_data(&self, id: MeshId) -> bool {
        let mut s = self.lock();
        if s.keep_cpu_data || !s.readers.is_empty() {
            return false;
        }
        s.meshes.get_mut(id.index()).is_some_and(|m| m.data.take().is_some())
    }

    /// Like [`release_mesh_data`](Self::release_mesh_data), for a texture's pixels.
    pub fn release_texture_data(&self, id: TextureId) -> bool {
        let mut s = self.lock();
        if s.keep_cpu_data || !s.readers.is_empty() {
            return false;
        }
        s.textures.get_mut(id.index()).is_some_and(|t| t.data.take().is_some())
    }

    fn lock(&self) -> RwLockWriteGuard<'_, State> {
        self.0.state.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `f` with the revision a successful change gets. `f` must check everything
    /// before mutating, so an error leaves the scene untouched.
    fn change<R>(&self, f: impl FnOnce(&mut State, &SceneLimits, u64) -> crate::Result<R>) -> crate::Result<R> {
        let mut s = self.lock();
        let rev = s.revision + 1;
        let out = f(&mut s, &self.0.limits, rev)?;
        s.revision = rev;
        self.0.revision.store(rev, Ordering::Release);
        drop(s);
        let notify = self.0.on_change.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(notify) = notify {
            notify();
        }
        Ok(out)
    }
}

/// A consistent view of the scene. Holds a read lock: keep it short (one upload pass,
/// one frame), since the builder waits for it.
pub struct SceneRead<'a>(RwLockReadGuard<'a, State>);

impl SceneRead<'_> {
    pub fn revision(&self) -> u64 {
        self.0.revision
    }

    pub fn geometry(&self) -> GeometryState {
        self.0.geometry
    }

    pub fn meshes(&self) -> &[MeshEntry] {
        &self.0.meshes
    }

    pub fn mesh(&self, id: MeshId) -> Option<&MeshEntry> {
        self.0.meshes.get(id.index())
    }

    pub fn materials(&self) -> &[MaterialEntry] {
        &self.0.materials
    }

    pub fn material(&self, id: MaterialId) -> Option<&MaterialEntry> {
        self.0.materials.get(id.index())
    }

    /// Parents always come before their children.
    pub fn nodes(&self) -> &[NodeEntry] {
        &self.0.nodes
    }

    pub fn textures(&self) -> &[TextureEntry] {
        &self.0.textures
    }

    pub fn texture(&self, id: TextureId) -> Option<&TextureEntry> {
        self.0.textures.get(id.index())
    }

    /// Bytes of mesh and texture data still held on the CPU (borrowed streams count,
    /// though their pages belong to the file mapping).
    pub fn cpu_bytes(&self) -> u64 {
        let meshes: u64 = self.0.meshes.iter().filter_map(|m| m.data.as_ref()).map(MeshData::byte_len).sum();
        let textures: u64 = self.0.textures.iter().filter_map(|t| t.data.as_ref()).map(TextureData::byte_len).sum();
        meshes + textures
    }

    /// World matrix of every node, in node order.
    pub fn world_transforms(&self) -> Vec<Mat4> {
        let mut world: Vec<Mat4> = Vec::with_capacity(self.0.nodes.len());
        for entry in &self.0.nodes {
            let parent = entry.node.parent.map_or(Mat4::IDENTITY, |p| world[p.index()]);
            world.push(parent * entry.node.transform);
        }
        world
    }

    /// Where the camera should look. While geometry is loading: the loader's early
    /// bounds if it set them (so the camera doesn't jump as meshes arrive). Once it is
    /// done: the measured world bounds of the placed meshes, since a file's stored
    /// min/max can be wrong. `None` while there is nothing to frame.
    pub fn bounds(&self) -> Option<Aabb> {
        let hint = self.0.bounds;
        if self.0.geometry == GeometryState::Loading && hint.is_some() {
            return hint;
        }
        self.measured_bounds().or(hint)
    }

    fn measured_bounds(&self) -> Option<Aabb> {
        let world = self.world_transforms();
        let b = self.0.nodes.iter().zip(&world).fold(Aabb::EMPTY, |acc, (entry, m)| {
            match entry.node.mesh.and_then(|id| self.mesh(id)) {
                Some(mesh) => acc.union(mesh.info.bounds.transformed(m)),
                None => acc,
            }
        });
        b.is_valid().then_some(b)
    }
}

/// The loader's side of a scene. Geometry goes in until [`finish`](Self::finish);
/// textures can be set any time, from any thread, through a [`TextureSink`].
pub struct SceneBuilder {
    scene: CpuScene,
    finished: bool,
}

impl SceneBuilder {
    pub fn scene(&self) -> &CpuScene {
        &self.scene
    }

    /// Frames the camera before any mesh exists. Invalid bounds are ignored.
    pub fn set_bounds(&mut self, bounds: Aabb) -> crate::Result<()> {
        if !bounds.is_valid() {
            return Ok(());
        }
        self.scene.change(|s, _, _| {
            s.bounds = Some(bounds);
            Ok(())
        })
    }

    /// Validates the mesh and, for triangles without normals, generates them here on
    /// the caller's (loader's) thread, so the renderer never has to.
    pub fn add_mesh(&mut self, data: MeshData) -> crate::Result<MeshId> {
        // The index scan and normal generation are the slow parts; do them before taking
        // the lock. Indices are range-checked before normals read through them.
        let limits = self.scene.limits();
        let mut info = data.validate(limits)?;
        let data = if data.normals.is_none() && data.topology == crate::Topology::Triangles {
            let data = crate::normals::generate(data);
            info = data.validate(limits)?;
            data
        } else {
            data
        };
        self.scene.change(|s, limits, rev| {
            geometry_open(s)?;
            check("meshes", s.meshes.len() as u64 + 1, limits.max_meshes as u64)?;
            check("vertices", s.vertices + info.vertices as u64, limits.max_vertices)?;
            check("triangles", s.primitives + info.primitives, limits.max_triangles)?;
            check("scene bytes", s.received_bytes + info.bytes, limits.max_bytes)?;
            s.vertices += info.vertices as u64;
            s.primitives += info.primitives;
            s.received_bytes += info.bytes;
            s.meshes.push(MeshEntry { info, data: Some(data), changed: rev, uploaded_by: Vec::new() });
            Ok(MeshId(s.meshes.len() as u32 - 1))
        })
    }

    pub fn add_material(&mut self, material: Material) -> crate::Result<MaterialId> {
        material.validate()?;
        self.scene.change(|s, limits, rev| {
            geometry_open(s)?;
            check("materials", s.materials.len() as u64 + 1, limits.max_materials as u64)?;
            for (_, tex) in &material.textures {
                if tex.index() >= s.textures.len() {
                    return Err(Error::UnknownId { kind: "texture", id: tex.0 });
                }
            }
            s.materials.push(MaterialEntry { material, changed: rev });
            Ok(MaterialId(s.materials.len() as u32 - 1))
        })
    }

    pub fn add_node(&mut self, node: Node) -> crate::Result<NodeId> {
        if !node.transform.is_finite() {
            return Err(Error::BadNode("transform is not finite"));
        }
        self.scene.change(|s, limits, rev| {
            geometry_open(s)?;
            check("nodes", s.nodes.len() as u64 + 1, limits.max_nodes as u64)?;
            let depth = match node.parent {
                Some(p) => s.nodes.get(p.index()).ok_or(Error::UnknownId { kind: "node", id: p.0 })?.depth + 1,
                None => 1,
            };
            check("node depth", depth as u64, limits.max_node_depth as u64)?;
            if let Some(m) = node.material
                && m.index() >= s.materials.len()
            {
                return Err(Error::UnknownId { kind: "material", id: m.0 });
            }
            let drawn = match node.mesh {
                Some(m) => s.meshes.get(m.index()).ok_or(Error::UnknownId { kind: "mesh", id: m.0 })?.info.primitives,
                None => 0,
            };
            check("drawn triangles", s.drawn_primitives + drawn, limits.max_drawn_triangles)?;
            s.drawn_primitives += drawn;
            s.nodes.push(NodeEntry { node, depth, changed: rev });
            Ok(NodeId(s.nodes.len() as u32 - 1))
        })
    }

    /// An id for a texture that will be decoded later.
    pub fn reserve_texture(&mut self) -> crate::Result<TextureId> {
        self.scene.change(|s, limits, rev| {
            check("textures", s.textures.len() as u64 + 1, limits.max_textures as u64)?;
            s.textures.push(TextureEntry { info: None, data: None, changed: rev, uploaded_by: Vec::new() });
            Ok(TextureId(s.textures.len() as u32 - 1))
        })
    }

    pub fn set_texture(&self, id: TextureId, data: TextureData) -> crate::Result<()> {
        self.texture_sink().set_texture(id, data)
    }

    /// A handle for decoder threads; outlives the builder.
    pub fn texture_sink(&self) -> TextureSink {
        TextureSink(self.scene.clone())
    }

    /// No more geometry. Readers see [`GeometryState::Done`].
    pub fn finish(&mut self) {
        if !self.finished {
            self.finished = true;
            let _ = self.scene.change(|s, _, _| {
                s.geometry = GeometryState::Done;
                Ok(())
            });
        }
    }
}

impl Drop for SceneBuilder {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.scene.change(|s, _, _| {
                s.geometry = GeometryState::Incomplete;
                Ok(())
            });
        }
    }
}

/// A registered reader of a scene ([`CpuScene::add_reader`]). It records what this
/// reader has uploaded; data is freed once every live reader has. Dropping it
/// unregisters, and frees data that every remaining reader has already uploaded.
pub struct ReaderGuard {
    scene: CpuScene,
    id: u32,
}

impl ReaderGuard {
    /// Records that this reader has uploaded a mesh, and frees its vertex data once
    /// every live reader has (its [`MeshInfo`] stays). Returns true when it was freed.
    pub fn release_mesh_data(&self, mesh: MeshId) -> bool {
        let mut s = self.scene.lock();
        let s = &mut *s;
        let Some(m) = s.meshes.get_mut(mesh.index()).filter(|m| m.data.is_some()) else { return false };
        release(&mut m.uploaded_by, &mut m.data, self.id, &s.readers, s.keep_cpu_data)
    }

    /// Like [`release_mesh_data`](Self::release_mesh_data), for a texture's pixels.
    pub fn release_texture_data(&self, texture: TextureId) -> bool {
        let mut s = self.scene.lock();
        let s = &mut *s;
        let Some(t) = s.textures.get_mut(texture.index()).filter(|t| t.data.is_some()) else { return false };
        release(&mut t.uploaded_by, &mut t.data, self.id, &s.readers, s.keep_cpu_data)
    }
}

/// Marks `reader` as done with an entry and drops `data` once every live reader is.
fn release<T>(uploaded_by: &mut Vec<u32>, data: &mut Option<T>, reader: u32, readers: &[u32], keep: bool) -> bool {
    if !uploaded_by.contains(&reader) {
        uploaded_by.push(reader);
    }
    let all = readers.iter().all(|r| uploaded_by.contains(r));
    !keep && all && data.take().is_some()
}

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        let mut s = self.scene.lock();
        let s = &mut *s;
        s.readers.retain(|&r| r != self.id);
        let readers = &s.readers;
        // Forget this reader's uploads (so a stale id can't stand in for a live one),
        // then free what every remaining reader already has. With no reader left the
        // data stays: a reader attached later may still need it.
        s.meshes.iter_mut().for_each(|m| m.uploaded_by.retain(|r| readers.contains(r)));
        s.textures.iter_mut().for_each(|t| t.uploaded_by.retain(|r| readers.contains(r)));
        if s.keep_cpu_data || readers.is_empty() {
            return;
        }
        for m in s.meshes.iter_mut().filter(|m| readers.iter().all(|r| m.uploaded_by.contains(r))) {
            m.data = None;
        }
        for t in s.textures.iter_mut().filter(|t| readers.iter().all(|r| t.uploaded_by.contains(r))) {
            t.data = None;
        }
    }
}

impl std::fmt::Debug for ReaderGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReaderGuard")
    }
}

/// Sets decoded textures on a scene, from any thread. Setting a texture again replaces
/// it (a sharper version arriving after a preview).
#[derive(Clone)]
pub struct TextureSink(CpuScene);

impl TextureSink {
    pub fn set_texture(&self, id: TextureId, data: TextureData) -> crate::Result<()> {
        let info = data.validate(self.0.limits())?;
        self.0.change(|s, limits, rev| {
            check("scene bytes", s.received_bytes + info.bytes, limits.max_bytes)?;
            let entry = s.textures.get_mut(id.index()).ok_or(Error::UnknownId { kind: "texture", id: id.0 })?;
            *entry = TextureEntry { info: Some(info), data: Some(data), changed: rev, uploaded_by: Vec::new() };
            s.received_bytes += info.bytes;
            Ok(())
        })
    }
}

fn geometry_open(s: &State) -> crate::Result<()> {
    if s.geometry == GeometryState::Loading { Ok(()) } else { Err(Error::GeometryFinished) }
}
