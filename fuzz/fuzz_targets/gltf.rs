//! gltf-lite: a glTF or GLB loaded into a scene, every buffer and image it names being
//! the file itself (or, for some names, refused).
#![no_main]
use std::sync::Arc;
struct Echo(Vec<u8>);
impl gltf_lite::Resolver for Echo {
    fn resolve(&self, uri: &str) -> Result<scene_data::Bytes, gltf_lite::ResolveError> {
        if uri.len().is_multiple_of(3) {
            return Err(gltf_lite::ResolveError::new(gltf_lite::ResolveErrorKind::NotFound, "refused"));
        }
        Ok(Arc::new(self.0.clone()))
    }
}
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = gltf_lite::glb_chunks(data);
    let (scene, mut builder) = scene_data::CpuScene::new();
    let _ = gltf_lite::load(Arc::new(data.to_vec()), &Echo(data.to_vec()), &mut builder);
    let _ = scene.read().cpu_bytes();
});
