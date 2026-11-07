use crate::Topology;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("{what} exceeds the limit of {limit}")]
    LimitExceeded { what: &'static str, limit: u64 },
    #[error("{stream} has {len} elements, expected {expected}")]
    StreamLength { stream: &'static str, len: usize, expected: usize },
    #[error("index {index} is out of range for {vertices} vertices")]
    IndexOutOfRange { index: u32, vertices: usize },
    #[error("{count} vertices don't make whole {topology:?}")]
    PartialPrimitive { count: usize, topology: Topology },
    #[error("unknown {kind} id {id}")]
    UnknownId { kind: &'static str, id: u32 },
    #[error("byte range {start}..{end} doesn't fit a {len}-byte buffer of {elem}-byte elements")]
    ByteRange { start: usize, end: usize, len: usize, elem: usize },
    #[error("bad texture: {0}")]
    BadTexture(&'static str),
    #[error("bad material: {0}")]
    BadMaterial(&'static str),
    #[error("bad node: {0}")]
    BadNode(&'static str),
    #[error("geometry is finished; only textures can still be added")]
    GeometryFinished,
}

/// Hard caps on what a file can make the app allocate or draw (docs/design/3d-renderer.md §12).
/// Going over one is an error for that item; what was already accepted stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SceneLimits {
    /// Primitives (triangles, lines or points) stored across all meshes.
    pub max_triangles: u64,
    /// Primitives drawn across all nodes, so one mesh instanced a million times can't
    /// turn a small file into an unbounded frame.
    pub max_drawn_triangles: u64,
    pub max_vertices: u64,
    pub max_texture_side: u32,
    /// Mesh and texture bytes received. A proxy for GPU memory; the renderer also caps
    /// against the adapter's own budget.
    pub max_bytes: u64,
    pub max_node_depth: u32,
    pub max_nodes: u32,
    pub max_meshes: u32,
    pub max_materials: u32,
    pub max_textures: u32,
}

impl Default for SceneLimits {
    fn default() -> Self {
        Self {
            max_triangles: 50_000_000,
            max_drawn_triangles: 200_000_000,
            max_vertices: 50_000_000,
            max_texture_side: 16_384,
            max_bytes: 2 << 30,
            max_node_depth: 256,
            max_nodes: 1_000_000,
            max_meshes: 1_000_000,
            max_materials: 65_536,
            max_textures: 65_536,
        }
    }
}

/// `Err(LimitExceeded)` when `value > limit`.
pub(crate) fn check(what: &'static str, value: u64, limit: u64) -> crate::Result<()> {
    if value > limit { Err(Error::LimitExceeded { what, limit }) } else { Ok(()) }
}
