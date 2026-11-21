//! Textures made smaller for the GPU: RGBA8 levels encoded as BC7 (4:1), on the
//! decoder's thread, when the process's GPU reads BC7. Sponza's 69 textures, mips and
//! all, went from 362 MB to 90 MB (docs/design/performance.md, step 5).
//!
//! Only BC7's mode 6 is written: one RGBA line a block, 7-bit endpoints with a shared
//! low bit, sixteen steps along it. It keeps alpha and every channel of a normal or
//! packed map, so no shader changes, and it's simple enough to encode quickly. A block of
//! two unrelated colours shows sixteen steps between them, where the full encoder's
//! two-subset modes would fit both; at texture scale that's hard to see.

use std::sync::atomic::{AtomicU8, Ordering};

use crate::{Stream, TextureData, TextureFormat};

/// What the GPU that will draw this process's scenes reads, compressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Compression {
    /// Nothing, or not known yet: textures stay RGBA8.
    None,
    Bc7,
}

static TARGET: AtomicU8 = AtomicU8::new(0);

/// Says what this process's GPU reads; set where the device is made, before files open.
/// Textures decoded before then stay as they are.
pub fn set_compression(c: Compression) {
    TARGET.store(c as u8, Ordering::Relaxed);
}

pub fn compression() -> Compression {
    match TARGET.load(Ordering::Relaxed) {
        1 => Compression::Bc7,
        _ => Compression::None,
    }
}

/// `data` encoded for the GPU, every level; as it was when there's nothing to gain or
/// the GPU couldn't read it (not RGBA8, or a side that isn't whole blocks).
pub fn compress_for_gpu(data: TextureData) -> TextureData {
    if compression() != Compression::Bc7 || data.format != TextureFormat::Rgba8 || !data.width.is_multiple_of(4) || !data.height.is_multiple_of(4) {
        return data;
    }
    let levels = data
        .levels
        .iter()
        .enumerate()
        .map(|(i, level)| {
            let (w, h) = data.level_size(i as u32);
            Stream::from(encode_bc7(w, h, level.as_slice()))
        })
        .collect();
    TextureData::new(data.width, data.height, TextureFormat::Bc7, data.color_space, levels)
}

/// One level of `width`×`height` RGBA8 pixels as BC7 blocks, row by row. Edge blocks
/// of a level that isn't whole blocks (the small mips) repeat the last row and column.
/// A level of a quarter million pixels or more is split by rows of blocks over up to
/// four threads: the same work, done sooner (about 16 ms a 1024² level on one M-series
/// core, three times a JPEG's decode).
pub fn encode_bc7(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let bh = height.div_ceil(4);
    let threads = if width as u64 * height as u64 >= 1 << 18 { std::thread::available_parallelism().map_or(1, |n| n.get()).min(4) as u32 } else { 1 };
    if threads <= 1 {
        return encode_rows(width, height, rgba, 0..bh);
    }
    let per = bh.div_ceil(threads);
    std::thread::scope(|s| {
        let parts: Vec<_> = (0..threads).map(|t| s.spawn(move || encode_rows(width, height, rgba, t * per..((t + 1) * per).min(bh)))).collect();
        parts.into_iter().flat_map(|p| p.join().expect("BC7 encoder thread")).collect()
    })
}

/// Block rows `rows` of a level.
fn encode_rows(width: u32, height: u32, rgba: &[u8], rows: std::ops::Range<u32>) -> Vec<u8> {
    let bw = width.div_ceil(4);
    let mut out = Vec::with_capacity(bw as usize * rows.len() * 16);
    let mut block = [[0u8; 4]; 16];
    for by in rows {
        for bx in 0..bw {
            for (i, p) in block.iter_mut().enumerate() {
                let x = (bx * 4 + i as u32 % 4).min(width - 1);
                let y = (by * 4 + i as u32 / 4).min(height - 1);
                let at = (y as usize * width as usize + x as usize) * 4;
                p.copy_from_slice(&rgba[at..at + 4]);
            }
            out.extend_from_slice(&mode6(&block));
        }
    }
    out
}

/// Mode 6's sixteen weights, out of 64.
const WEIGHTS: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

/// An endpoint as stored: four 7-bit values and the low bit they share.
#[derive(Clone, Copy)]
struct Endpoint {
    q: [u8; 4],
    p: u8,
}

impl Endpoint {
    fn value(self) -> [u32; 4] {
        self.q.map(|q| (q as u32) << 1 | self.p as u32)
    }

    /// The nearest stored endpoint to `e`, trying both low bits. Opaque blocks keep
    /// alpha 255, which needs the low bit set.
    fn nearest(e: [f32; 4], opaque: bool) -> Self {
        let try_p = |p: u8| {
            let q = std::array::from_fn(|c| {
                if opaque && c == 3 { 127 } else { ((e[c] - p as f32) / 2.0).round().clamp(0.0, 127.0) as u8 }
            });
            let ep = Endpoint { q, p };
            let err: f32 = (0..4).map(|c| (ep.value()[c] as f32 - e[c]).powi(2)).sum();
            (err, ep)
        };
        let one = try_p(1);
        if opaque {
            return one.1;
        }
        let zero = try_p(0);
        if zero.0 < one.0 { zero.1 } else { one.1 }
    }
}

/// The index each pixel takes between endpoints `a` and `b`.
fn fit(px: &[[u8; 4]; 16], a: Endpoint, b: Endpoint) -> [u8; 16] {
    let (va, vb) = (a.value(), b.value());
    let palette: [[i32; 4]; 16] = std::array::from_fn(|i| std::array::from_fn(|c| (((64 - WEIGHTS[i]) * va[c] + WEIGHTS[i] * vb[c] + 32) >> 6) as i32));
    // Each pixel's place along the line from a to b says which step it's near; the
    // steps aren't evenly spaced, so the ones either side are tried too.
    let d: [f32; 4] = std::array::from_fn(|c| vb[c] as f32 - va[c] as f32);
    let dd: f32 = d.iter().map(|v| v * v).sum();
    let scale = if dd > 0.0 { 15.0 / dd } else { 0.0 };
    let mut indices = [0u8; 16];
    for (k, p) in px.iter().enumerate() {
        let t: f32 = (0..4).map(|c| (p[c] as f32 - va[c] as f32) * d[c]).sum();
        let guess = (t * scale).round().clamp(0.0, 15.0) as usize;
        let (mut best, mut best_err) = (guess, i32::MAX);
        let first = guess.saturating_sub(1);
        for (i, colour) in palette.iter().enumerate().skip(first).take((guess + 1).min(15) + 1 - first) {
            let err: i32 = (0..4).map(|c| (colour[c] - p[c] as i32).pow(2)).sum();
            if err < best_err {
                (best, best_err) = (i, err);
            }
        }
        indices[k] = best as u8;
    }
    indices
}

/// One block as mode 6: endpoints at the ends of the pixels' principal axis. (Refitting
/// them to the indices by least squares, as fuller encoders do, gained 0.03 dB on
/// Sponza's textures for 60% more time.)
fn mode6(px: &[[u8; 4]; 16]) -> [u8; 16] {
    let opaque = px.iter().all(|p| p[3] == 255);
    if px.iter().all(|p| *p == px[0]) {
        let e = Endpoint::nearest(px[0].map(f32::from), opaque);
        return pack(e, e, [0; 16]);
    }
    let mut mean = [0f32; 4];
    for p in px {
        for c in 0..4 {
            mean[c] += p[c] as f32;
        }
    }
    mean = mean.map(|m| m / 16.0);
    let mut cov = [[0f32; 4]; 4];
    for p in px {
        let d: [f32; 4] = std::array::from_fn(|c| p[c] as f32 - mean[c]);
        for i in 0..4 {
            for j in i..4 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    let cov: [[f32; 4]; 4] = std::array::from_fn(|i| std::array::from_fn(|j| if j < i { cov[j][i] } else { cov[i][j] }));
    // The principal axis: two steps of power iteration from the row of the channel that
    // varies most (more steps changed nothing measurable).
    let start = (0..4).max_by(|&a, &b| cov[a][a].total_cmp(&cov[b][b])).unwrap_or(0);
    let mut axis: [f32; 4] = cov[start];
    for _ in 0..2 {
        axis = std::array::from_fn(|i| (0..4).map(|j| cov[i][j] * axis[j]).sum());
        let len = axis.iter().map(|v| v * v).sum::<f32>().sqrt();
        if len < 1e-9 {
            break;
        }
        axis = axis.map(|v| v / len);
    }
    let len = axis.iter().map(|v| v * v).sum::<f32>().sqrt();
    let (mut lo, mut hi) = (mean, mean);
    if len > 1e-9 {
        axis = axis.map(|v| v / len);
        let t = |p: &[u8; 4]| (0..4).map(|c| (p[c] as f32 - mean[c]) * axis[c]).sum::<f32>();
        let (tmin, tmax) = px.iter().map(t).fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(v), b.max(v)));
        lo = std::array::from_fn(|c| (mean[c] + tmin * axis[c]).clamp(0.0, 255.0));
        hi = std::array::from_fn(|c| (mean[c] + tmax * axis[c]).clamp(0.0, 255.0));
    }
    let (a, b) = (Endpoint::nearest(lo, opaque), Endpoint::nearest(hi, opaque));
    pack(a, b, fit(px, a, b))
}

/// Mode 6's 128 bits, lowest first: the mode (six 0s and a 1), each channel's two
/// endpoints, the two low bits, then the indices, the first with its top bit left out
/// (it must be 0, so the endpoints swap when it wouldn't be).
fn pack(mut a: Endpoint, mut b: Endpoint, mut indices: [u8; 16]) -> [u8; 16] {
    if indices[0] >= 8 {
        std::mem::swap(&mut a, &mut b);
        indices = indices.map(|i| 15 - i);
    }
    let mut bits = 0u128;
    let mut at = 0;
    let mut put = |value: u32, width: u32| {
        bits |= (value as u128) << at;
        at += width;
    };
    put(1 << 6, 7);
    for c in 0..4 {
        put(a.q[c] as u32, 7);
        put(b.q[c] as u32, 7);
    }
    put(a.p as u32, 1);
    put(b.p as u32, 1);
    put(indices[0] as u32, 3);
    for &i in &indices[1..] {
        put(i as u32, 4);
    }
    bits.to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mode 6 block decoded the way the BC7 specification lays it out.
    fn decode(block: [u8; 16]) -> [[u8; 4]; 16] {
        let bits = u128::from_le_bytes(block);
        let mut at = 0;
        let mut take = |width: u32| {
            let v = (bits >> at) as u32 & ((1 << width) - 1);
            at += width;
            v
        };
        assert_eq!(take(7), 1 << 6, "mode 6");
        let pairs: [(u32, u32); 4] = std::array::from_fn(|_| (take(7), take(7)));
        let e = [pairs.map(|p| p.0), pairs.map(|p| p.1)];
        let (p0, p1) = (take(1), take(1));
        let e: [[u32; 4]; 2] = [e[0].map(|q| q << 1 | p0), e[1].map(|q| q << 1 | p1)];
        let indices: Vec<u32> = (0..16).map(|k| take(if k == 0 { 3 } else { 4 })).collect();
        std::array::from_fn(|k| std::array::from_fn(|c| (((64 - WEIGHTS[indices[k] as usize]) * e[0][c] + WEIGHTS[indices[k] as usize] * e[1][c] + 32) >> 6) as u8))
    }

    fn worst(px: &[[u8; 4]; 16]) -> i32 {
        let back = decode(mode6(px));
        px.iter().zip(&back).flat_map(|(a, b)| (0..4).map(move |c| (a[c] as i32 - b[c] as i32).abs())).max().unwrap()
    }

    #[test]
    fn a_flat_block_comes_back_within_a_step() {
        for colour in [[0, 0, 0, 255], [255, 255, 255, 255], [200, 17, 90, 255], [10, 20, 30, 0], [128, 128, 255, 77]] {
            assert!(worst(&[colour; 16]) <= 1, "{colour:?}");
        }
    }

    #[test]
    fn gradients_come_back_close_and_opaque_stays_opaque() {
        let ramp: [[u8; 4]; 16] = std::array::from_fn(|i| [(i * 16) as u8, 255 - (i * 16) as u8, (i * 8) as u8, 255]);
        assert!(worst(&ramp) <= 6, "{}", worst(&ramp));
        assert!(decode(mode6(&ramp)).iter().all(|p| p[3] == 255));
        let fade: [[u8; 4]; 16] = std::array::from_fn(|i| [40, 80, 120, (i * 17) as u8]);
        assert!(worst(&fade) <= 6, "{}", worst(&fade));
    }

    #[test]
    fn the_first_index_always_fits_in_three_bits() {
        // Bright first, dark after: the encoder must swap its endpoints.
        let px: [[u8; 4]; 16] = std::array::from_fn(|i| if i < 8 { [250, 250, 250, 255] } else { [5, 5, 5, 255] });
        assert!(worst(&px) <= 2);
    }

    #[test]
    fn a_photo_like_level_keeps_most_of_its_detail() {
        // Smooth shapes with noise on top, the kind of content textures have.
        let (w, h) = (64u32, 64u32);
        let mut seed = 1u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as i32 % 24 - 12
        };
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let base = [(x * 4) as i32, (y * 4) as i32, ((x + y) * 2) as i32, 255];
                rgba.extend(base.iter().enumerate().map(|(c, &v)| if c == 3 { 255 } else { (v + noise()).clamp(0, 255) as u8 }));
            }
        }
        let blocks = encode_bc7(w, h, &rgba);
        assert_eq!(blocks.len(), (w * h) as usize);
        let mut se = 0f64;
        for (b, block) in blocks.as_chunks::<16>().0.iter().enumerate() {
            let back = decode(*block);
            let (bx, by) = (b as u32 % (w / 4), b as u32 / (w / 4));
            for (k, p) in back.iter().enumerate() {
                let (x, y) = (bx * 4 + k as u32 % 4, by * 4 + k as u32 / 4);
                let at = ((y * w + x) * 4) as usize;
                se += (0..3).map(|c| (p[c] as f64 - rgba[at + c] as f64).powi(2)).sum::<f64>();
            }
        }
        let psnr = 10.0 * (255.0f64.powi(2) / (se / (w * h * 3) as f64)).log10();
        assert!(psnr > 30.0, "PSNR {psnr:.1} dB");

    }

    #[test]
    fn small_levels_are_one_padded_block() {
        assert_eq!(encode_bc7(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]).len(), 16);
        assert_eq!(encode_bc7(1, 1, &[9, 9, 9, 9]).len(), 16);
    }
}
