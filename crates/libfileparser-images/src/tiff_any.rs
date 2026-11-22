//! TIFFs that `image` decodes but won't turn into pixels: bit depths other than 8 and
//! 16, floating-point and signed samples, palettes that aren't 8-bit, multiband
//! (scientific) images, CMYK with alpha and Lab. Their samples come from the `tiff`
//! crate (the decoder `image` uses) and are mapped to RGBA here.
//!
//! Integer samples of up to 16 bits are shown at full scale; wider and signed integers,
//! and floats, are data (elevation, intensity) more than colour, so they are stretched
//! from the image's own lowest to highest value.

use std::io::Cursor;

use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

/// Width, height and what the samples are ("Gray(12)", "Multiband { … }").
pub fn size(bytes: &[u8]) -> Option<(u32, u32, String)> {
    let mut d = Decoder::new(Cursor::new(bytes)).ok()?;
    let (w, h) = d.dimensions().ok()?;
    let color = d.colortype().ok()?;
    channels(color)?;
    Some((w, h, format!("{color:?}")))
}

/// The whole image as RGBA8, or `None` when its samples can't be read here either.
pub fn decode(bytes: &[u8], max_alloc: u64) -> Option<image::DynamicImage> {
    let limits = { let mut l = Limits::default(); l.decoding_buffer_size = max_alloc.min(usize::MAX as u64) as usize; l };
    let mut d = Decoder::new(Cursor::new(bytes)).ok()?.with_limits(limits);
    let (w, h) = d.dimensions().ok()?;
    let color = d.colortype().ok()?;
    let spp = channels(color)?;
    let planar = d.find_tag_unsigned::<u16>(Tag::PlanarConfiguration).ok().flatten() == Some(2);
    let palette = matches!(color, ColorType::Palette(_)).then(|| d.find_tag_unsigned_vec::<u16>(Tag::ColorMap).ok().flatten()).flatten();
    let bits = color.bit_depth() as u32;
    let result = d.read_image().ok()?;
    let n = w as usize * h as usize;
    let samples = to_unit(result, bits, w as usize, spp, n, palette.is_some())?;
    if samples.len() < n * spp {
        return None;
    }
    // Sample `c` of pixel `i`, interleaved or in planes.
    let at = |i: usize, c: usize| if planar { samples[c * n + i] } else { samples[i * spp + c] };
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let mut rgba = Vec::with_capacity(n * 4);
    for i in 0..n {
        let px = match color {
            ColorType::Gray(_) | ColorType::Lab(_) => [at(i, 0); 3].map(byte).into_iter().chain([255]).collect::<Vec<_>>(),
            ColorType::GrayA(_) => vec![byte(at(i, 0)), byte(at(i, 0)), byte(at(i, 0)), byte(at(i, 1))],
            ColorType::RGB(_) | ColorType::YCbCr(_) => vec![byte(at(i, 0)), byte(at(i, 1)), byte(at(i, 2)), 255],
            ColorType::RGBA(_) => vec![byte(at(i, 0)), byte(at(i, 1)), byte(at(i, 2)), byte(at(i, 3))],
            ColorType::CMYK(_) | ColorType::CMYKA(_) => {
                let k = 1.0 - at(i, 3);
                let a = if spp > 4 { byte(at(i, 4)) } else { 255 };
                vec![byte((1.0 - at(i, 0)) * k), byte((1.0 - at(i, 1)) * k), byte((1.0 - at(i, 2)) * k), a]
            }
            ColorType::Palette(_) => {
                let map = palette.as_deref()?;
                let entries = map.len() / 3;
                let index = (at(i, 0) as usize).min(entries.saturating_sub(1));
                // TIFF colour maps are 16-bit: all reds, then greens, then blues.
                vec![(map[index] >> 8) as u8, (map[entries + index] >> 8) as u8, (map[2 * entries + index] >> 8) as u8, 255]
            }
            ColorType::Multiband { num_samples, .. } if num_samples >= 3 => vec![byte(at(i, 0)), byte(at(i, 1)), byte(at(i, 2)), 255],
            ColorType::Multiband { .. } => [at(i, 0); 3].map(byte).into_iter().chain([255]).collect(),
            _ => return None,
        };
        rgba.extend_from_slice(&px);
    }
    image::RgbaImage::from_raw(w, h, rgba).map(image::DynamicImage::ImageRgba8)
}

/// Samples a pixel of this colour type has; `None` for YCbCr, whose subsampled planes
/// `tiff` hands over as they're stored.
fn channels(color: ColorType) -> Option<usize> {
    Some(match color {
        ColorType::Gray(_) | ColorType::Palette(_) => 1,
        ColorType::GrayA(_) => 2,
        ColorType::RGB(_) | ColorType::Lab(_) => 3,
        ColorType::RGBA(_) | ColorType::CMYK(_) => 4,
        ColorType::CMYKA(_) => 5,
        ColorType::YCbCr(_) => return None,
        ColorType::Multiband { num_samples, .. } => (num_samples as usize).max(1),
        // A kind added to `tiff` after this was written.
        _ => return None,
    })
}

/// Every sample as 0–1 (or, for a palette, its index): integers of up to 16 bits at
/// full scale, the rest stretched from their lowest to highest value. Samples narrower
/// than a byte arrive packed, each row starting on a byte.
fn to_unit(result: DecodingResult, bits: u32, width: usize, spp: usize, n: usize, index: bool) -> Option<Vec<f32>> {
    let full = |max: f64| move |v: f64| if index { v as f32 } else { (v / max) as f32 };
    let stretch = |vals: Vec<f64>| {
        let (lo, hi) = vals.iter().filter(|v| v.is_finite()).fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        let span = if hi > lo { hi - lo } else { 1.0 };
        vals.into_iter().map(|v| if v.is_finite() { ((v - lo) / span) as f32 } else { 0.0 }).collect()
    };
    Some(match result {
        DecodingResult::U8(v) if bits < 8 => {
            let row = (width * spp * bits as usize).div_ceil(8);
            let max = ((1u32 << bits) - 1) as f64;
            let mut out = Vec::with_capacity(n * spp);
            for r in v.chunks(row) {
                for s in 0..width * spp {
                    let bit = s * bits as usize;
                    let byte = *r.get(bit / 8)? as u32;
                    let shift = 8 - bits as usize - bit % 8;
                    out.push(full(max)(((byte >> shift) & ((1 << bits) - 1)) as f64));
                }
            }
            out
        }
        DecodingResult::U8(v) => v.into_iter().map(|x| full(((1u64 << bits.min(8)) - 1) as f64)(x as f64)).collect(),
        DecodingResult::U16(v) => v.into_iter().map(|x| full(((1u64 << bits.min(16)) - 1) as f64)(x as f64)).collect(),
        DecodingResult::U32(v) => stretch(v.into_iter().map(|x| x as f64).collect()),
        DecodingResult::U64(v) => stretch(v.into_iter().map(|x| x as f64).collect()),
        DecodingResult::I8(v) => stretch(v.into_iter().map(|x| x as f64).collect()),
        DecodingResult::I16(v) => stretch(v.into_iter().map(|x| x as f64).collect()),
        DecodingResult::I32(v) => stretch(v.into_iter().map(|x| x as f64).collect()),
        DecodingResult::I64(v) => stretch(v.into_iter().map(|x| x as f64).collect()),
        DecodingResult::F16(v) => stretch(v.into_iter().map(|x| x.to_f64()).collect()),
        DecodingResult::F32(v) => stretch(v.into_iter().map(|x| x as f64).collect()),
        DecodingResult::F64(v) => stretch(v),
    })
}
