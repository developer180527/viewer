//! `EXT_meshopt_compression` and `KHR_meshopt_compression`: compressed buffer views,
//! decoded before anything reads them, by zeux's meshoptimizer.

use std::ffi::c_void;
use std::sync::Arc;

use serde_json::Value;

use crate::accessor::BufferData;
use crate::json::Root;

const EXTENSIONS: [&str; 2] = ["EXT_meshopt_compression", "KHR_meshopt_compression"];

/// Decoded data, kept in the words it was decoded into (filters need them aligned)
/// rather than copied out to bytes.
struct Decoded {
    words: Vec<u32>,
    len: usize,
}

impl AsRef<[u8]> for Decoded {
    fn as_ref(&self) -> &[u8] {
        &bytemuck::cast_slice(&self.words)[..self.len]
    }
}

/// Whether a buffer only stands in for compressed views, and so holds no data.
pub fn is_placeholder(extensions: &serde_json::Map<String, Value>) -> bool {
    EXTENSIONS.iter().any(|e| extensions.get(*e).is_some_and(|x| x.get("fallback").and_then(Value::as_bool) == Some(true)))
}

/// Decodes one view's data. Filters read it as 16- and 32-bit values, so it's aligned.
fn decode(ext: &Value, source: &[u8], max: usize) -> Result<Decoded, String> {
    let num = |k: &str| ext.get(k).and_then(Value::as_u64).map(|v| v as usize);
    let count = num("count").ok_or("no count")?;
    let stride = num("byteStride").filter(|s| (1..=256).contains(s)).ok_or("bad byteStride")?;
    let len = count.checked_mul(stride).filter(|&n| n <= max).ok_or("more decoded data than the scene may hold")?;
    let mut words = vec![0u32; len.div_ceil(4)];
    let out: &mut [u8] = bytemuck::cast_slice_mut(&mut words);
    let dest = out.as_mut_ptr() as *mut c_void;
    use meshopt::ffi;
    let mode = ext.get("mode").and_then(Value::as_str).unwrap_or("ATTRIBUTES");
    // Safety: `dest` holds `count * stride` bytes, and the decoders check `source`'s bounds.
    // What they don't check, they assert (debug) or trust (release, writing past `dest`):
    // a vertex stride of 4s, and triangles in threes. Those are checked here.
    let status = unsafe {
        match mode {
            "ATTRIBUTES" if stride.is_multiple_of(4) => ffi::meshopt_decodeVertexBuffer(dest, count, stride, source.as_ptr(), source.len()),
            "TRIANGLES" if matches!(stride, 2 | 4) && count.is_multiple_of(3) => ffi::meshopt_decodeIndexBuffer(dest, count, stride, source.as_ptr(), source.len()),
            "INDICES" if matches!(stride, 2 | 4) => ffi::meshopt_decodeIndexSequence(dest, count, stride, source.as_ptr(), source.len()),
            _ => return Err(format!("mode {mode} with stride {stride} and count {count}")),
        }
    };
    if status != 0 {
        return Err(format!("damaged data ({status})"));
    }
    let filter = ext.get("filter").and_then(Value::as_str).unwrap_or("NONE");
    // Safety: as above; each filter is given the strides it's defined for.
    unsafe {
        match (filter, stride) {
            ("NONE", _) => {}
            ("OCTAHEDRAL", 4 | 8) => ffi::meshopt_decodeFilterOct(dest, count, stride),
            ("QUATERNION", 8) => ffi::meshopt_decodeFilterQuat(dest, count, stride),
            ("EXPONENTIAL", s) if s % 4 == 0 => ffi::meshopt_decodeFilterExp(dest, count, stride),
            ("COLOR", 4 | 8) => ffi::meshopt_decodeFilterColor(dest, count, stride),
            _ => return Err(format!("filter {filter} with stride {stride}")),
        }
    }
    Ok(Decoded { words, len })
}

/// Decodes every compressed view, each into a buffer of its own that the view then names,
/// all of them together within `max` bytes (the scene's byte limit): a small file can
/// claim a lot.
pub fn decompress(root: &mut Root, buffers: &mut Vec<Option<BufferData>>, max: u64, warnings: &mut Vec<String>) {
    let max = usize::try_from(max).unwrap_or(usize::MAX);
    let mut total = 0usize;
    for i in 0..root.buffer_views.len() {
        let view = &root.buffer_views[i];
        let Some(ext) = EXTENSIONS.iter().find_map(|e| view.extensions.get(*e)).cloned() else { continue };
        let num = |k: &str| ext.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
        let at = num("byteOffset");
        let source = ext.get("buffer").and_then(Value::as_u64).and_then(|b| buffers.get(b as usize)?.as_ref());
        let Some(data) = source.and_then(|b| b.slice().get(at..at.checked_add(num("byteLength"))?)) else {
            warnings.push(format!("buffer view {i}: its compressed data isn't there"));
            continue;
        };
        let claimed = num("count").saturating_mul(num("byteStride"));
        if total.saturating_add(claimed) > max {
            warnings.push(format!("buffer view {i}: more compressed data than one file may decode to"));
            continue;
        }
        match decode(&ext, data, max - total) {
            Ok(bytes) => {
                let len = bytes.len;
                let view = &mut root.buffer_views[i];
                view.buffer = buffers.len();
                view.byte_offset = 0;
                view.byte_length = len;
                total += len;
                buffers.push(Some(BufferData::whole(Arc::new(bytes))));
            }
            Err(e) => warnings.push(format!("buffer view {i}: {e}")),
        }
    }
}
