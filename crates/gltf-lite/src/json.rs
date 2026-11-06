//! The subset of the glTF 2.0 JSON schema the adapter reads. Unknown fields are
//! ignored; missing ones take the spec's defaults.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Root {
    pub asset: Asset,
    pub scene: Option<usize>,
    pub scenes: Vec<Scene>,
    pub nodes: Vec<Node>,
    pub meshes: Vec<Mesh>,
    pub accessors: Vec<Accessor>,
    pub buffer_views: Vec<BufferView>,
    pub buffers: Vec<Buffer>,
    pub materials: Vec<Material>,
    pub textures: Vec<Texture>,
    pub images: Vec<Image>,
    pub extensions_required: Vec<String>,
    pub extensions: Map<String, Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Asset {
    pub version: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Scene {
    pub nodes: Vec<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Node {
    pub children: Vec<usize>,
    pub mesh: Option<usize>,
    pub matrix: Option<[f32; 16]>,
    pub translation: Option<[f32; 3]>,
    pub rotation: Option<[f32; 4]>,
    pub scale: Option<[f32; 3]>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Mesh {
    pub primitives: Vec<Primitive>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Primitive {
    pub attributes: BTreeMap<String, usize>,
    pub indices: Option<usize>,
    pub material: Option<usize>,
    pub mode: Option<u32>,
    pub extensions: Map<String, Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Accessor {
    pub buffer_view: Option<usize>,
    pub byte_offset: usize,
    pub component_type: u32,
    pub normalized: bool,
    pub count: usize,
    #[serde(rename = "type")]
    pub ty: String,
    pub min: Option<Vec<f32>>,
    pub max: Option<Vec<f32>>,
    pub sparse: Option<Sparse>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Sparse {
    pub count: usize,
    pub indices: SparseIndices,
    pub values: SparseValues,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SparseIndices {
    pub buffer_view: usize,
    pub byte_offset: usize,
    pub component_type: u32,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SparseValues {
    pub buffer_view: usize,
    pub byte_offset: usize,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct BufferView {
    pub buffer: usize,
    pub byte_offset: usize,
    pub byte_length: usize,
    pub byte_stride: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Buffer {
    pub uri: Option<String>,
    pub byte_length: usize,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Material {
    pub name: Option<String>,
    pub pbr_metallic_roughness: Option<Pbr>,
    pub normal_texture: Option<TextureInfo>,
    pub occlusion_texture: Option<TextureInfo>,
    pub emissive_texture: Option<TextureInfo>,
    pub emissive_factor: Option<[f32; 3]>,
    pub alpha_mode: Option<String>,
    pub alpha_cutoff: Option<f32>,
    pub double_sided: bool,
    pub extensions: Map<String, Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Pbr {
    pub base_color_factor: Option<[f32; 4]>,
    pub base_color_texture: Option<TextureInfo>,
    pub metallic_factor: Option<f32>,
    pub roughness_factor: Option<f32>,
    pub metallic_roughness_texture: Option<TextureInfo>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct TextureInfo {
    pub index: usize,
    pub tex_coord: usize,
    /// Normal maps.
    pub scale: Option<f32>,
    /// Occlusion maps.
    pub strength: Option<f32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Texture {
    pub source: Option<usize>,
    pub extensions: Map<String, Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Image {
    pub uri: Option<String>,
    pub buffer_view: Option<usize>,
    pub mime_type: Option<String>,
}
