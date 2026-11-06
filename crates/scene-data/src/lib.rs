//! What a 3D model is, as plain data: meshes, materials, textures and nodes. Parsers
//! build it (libfileparser-3d, gltf-lite); RendererX draws it. It knows neither.
//!
//! A loader fills a [`SceneBuilder`] on any thread, before any GPU exists; the host
//! keeps the matching [`CpuScene`] handle and hands it to the renderer whenever the
//! GPU is ready. The renderer follows the scene by [revision](CpuScene::revision), so
//! it doesn't matter whether loading or GPU setup finishes first.
//!
//! Everything a file controls is checked against [`SceneLimits`] on the way in:
//! stream lengths, index ranges, counts and sizes. Nothing here panics on bad input.
//!
//! See docs/design/3d-renderer.md §4–§6 and §12.

mod error;
mod material;
mod mesh;
mod normals;
mod scene;
mod stream;
mod texture;

pub use error::{Error, SceneLimits};
pub use glam;
pub use material::{AlphaMode, Material, ParamBlock, ParamValue, ShadingId, ShadingSource, Slot};
pub use mesh::{Aabb, Indices, MeshData, MeshInfo, Topology};
pub use scene::{
    CpuScene, GeometryState, MaterialEntry, MaterialId, MeshEntry, MeshId, Node, NodeEntry, NodeId, ReaderGuard, SceneBuilder,
    SceneRead, TextureEntry, TextureId, TextureSink,
};
pub use stream::{Bytes, Stream};
pub use texture::{ColorSpace, TextureData, TextureFormat, TextureInfo};

pub type Result<T> = std::result::Result<T, Error>;
