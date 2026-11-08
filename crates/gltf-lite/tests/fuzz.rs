//! gltf-lite on damaged files (docs/design/libfileparser.md §9): glTF JSON and GLB
//! cut, flipped and spliced, loaded into a scene with a resolver that hands back the
//! damaged file for every buffer and image. Errors are fine; panics and hangs aren't.

use std::path::Path;
use std::sync::Arc;

use gltf_lite::{ResolveError, ResolveErrorKind, Resolver};
use scene_data::Bytes;

fn seeds() -> Vec<(String, Vec<u8>)> {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/3d");
    ["cube-gltf/cube.gltf", "missing-texture/cube.gltf", "cube.glb"]
        .iter()
        .map(|f| (f.rsplit('/').next().unwrap().to_string(), std::fs::read(corpus.join(f)).unwrap()))
        .collect()
}

/// Every buffer and image the file names is the file itself, or (for data URIs and
/// odd paths) refused.
struct Echo(Vec<u8>);

impl Resolver for Echo {
    fn resolve(&self, uri: &str) -> Result<Bytes, ResolveError> {
        if uri.len().is_multiple_of(3) {
            return Err(ResolveError::new(ResolveErrorKind::NotFound, "refused"));
        }
        Ok(Arc::new(self.0.clone()))
    }
}

#[test]
fn files_survive_fuzzing() {
    libfileparser_testkit::assert_bytes_fuzz_clean("gltf-lite", &seeds(), |_, bytes| {
        let _ = gltf_lite::glb_chunks(bytes);
        let (scene, mut builder) = scene_data::CpuScene::new();
        let _ = gltf_lite::load(Arc::new(bytes.to_vec()), &Echo(bytes.to_vec()), &mut builder);
        let _ = scene.read().cpu_bytes();
    });
}
