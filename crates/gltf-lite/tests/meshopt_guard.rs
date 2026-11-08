//! Compressed views whose count and stride meshoptimizer's decoders don't accept are
//! refused before the decoder sees them: the decoders assert (debug) or write past
//! their output (release) otherwise.

use std::sync::Arc;

use gltf_lite::{ResolveError, ResolveErrorKind, Resolver};
use scene_data::Bytes;

struct None_;

impl Resolver for None_ {
    fn resolve(&self, _: &str) -> Result<Bytes, ResolveError> {
        Err(ResolveError::new(ResolveErrorKind::NotFound, "none"))
    }
}

fn gltf(mode: &str, count: u32, stride: u32) -> Vec<u8> {
    // 64 bytes of 0xe1 / zeros: a header the decoders take, then nothing useful.
    let mut data = vec![0u8; 64];
    data[0] = if mode == "ATTRIBUTES" { 0xa0 } else { 0xe1 };
    let b64 = base64(&data);
    format!(
        r#"{{"asset":{{"version":"2.0"}},"extensionsUsed":["EXT_meshopt_compression"],
        "buffers":[{{"byteLength":64,"uri":"data:application/octet-stream;base64,{b64}"}}],
        "bufferViews":[{{"buffer":0,"byteLength":64,"extensions":{{"EXT_meshopt_compression":{{"buffer":0,"byteOffset":0,"byteLength":64,"byteStride":{stride},"count":{count},"mode":"{mode}"}}}}}}]}}"#
    )
    .into_bytes()
}

fn base64(b: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() { s.push(T[(n >> (18 - 6 * k) & 63) as usize] as char) } else { s.push('=') }
        }
    }
    s
}

#[test]
fn bad_counts_and_strides_are_refused() {
    for (mode, count, stride) in [("TRIANGLES", 4, 2), ("TRIANGLES", 7, 4), ("ATTRIBUTES", 4, 3), ("ATTRIBUTES", 2, 6)] {
        let (_, mut builder) = scene_data::CpuScene::new();
        let _ = gltf_lite::load(Arc::new(gltf(mode, count, stride)), &None_, &mut builder);
    }
}
