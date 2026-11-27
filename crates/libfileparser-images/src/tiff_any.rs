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
///
/// Samples are mapped straight from the decoded buffer into the RGBA output, so the
/// peak is the samples plus the pixels, nothing in between.
pub fn decode(bytes: &[u8], max_alloc: u64) -> Option<image::DynamicImage> {
    let cap = max_alloc.min(usize::MAX as u64) as usize;
    let limits = {
        let mut l = Limits::default();
        l.decoding_buffer_size = cap;
        // A single strip or tile may be the whole image (the default stops at 128 MiB).
        l.intermediate_buffer_size = cap;
        l
    };
    let mut d = Decoder::new(Cursor::new(bytes)).ok()?.with_limits(limits);
    let (w, h) = d.dimensions().ok()?;
    let color = d.colortype().ok()?;
    let spp = channels(color)?;
    let planar = d.find_tag_unsigned::<u16>(Tag::PlanarConfiguration).ok().flatten() == Some(2);
    let palette = matches!(color, ColorType::Palette(_)).then(|| d.find_tag_unsigned_vec::<u16>(Tag::ColorMap).ok().flatten()).flatten();
    let bits = color.bit_depth() as u32;
    let n = w as usize * h as usize;
    let out = Out { color, spp, n, planar, palette: palette.as_deref() };
    let index = out.palette.is_some();
    let rgba = match d.read_image().ok()? {
        DecodingResult::U8(v) if bits < 8 => {
            let v = unpack(&v, bits, w as usize * spp, h as usize)?;
            let max = ((1u32 << bits) - 1) as f32;
            out.paint(&v, |x| if index { x as f32 } else { x as f32 / max })
        }
        DecodingResult::U8(v) => out.paint(&v, |x| if index { x as f32 } else { x as f32 / 255.0 }),
        DecodingResult::U16(v) => {
            let max = ((1u32 << bits.clamp(1, 16)) - 1) as f32;
            out.paint(&v, |x| if index { x as f32 } else { x as f32 / max })
        }
        DecodingResult::U32(v) => out.stretch(&v, |x| x as f64),
        DecodingResult::U64(v) => out.stretch(&v, |x| x as f64),
        DecodingResult::I8(v) => out.stretch(&v, |x| x as f64),
        DecodingResult::I16(v) => out.stretch(&v, |x| x as f64),
        DecodingResult::I32(v) => out.stretch(&v, |x| x as f64),
        DecodingResult::I64(v) => out.stretch(&v, |x| x as f64),
        DecodingResult::F16(v) => out.stretch(&v, |x| x.to_f64()),
        DecodingResult::F32(v) => out.stretch(&v, |x| x as f64),
        DecodingResult::F64(v) => out.stretch(&v, |x| x),
    }?;
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

/// Samples narrower than a byte, one per byte. They arrive packed, each row starting on
/// a byte.
fn unpack(v: &[u8], bits: u32, per_row: usize, rows: usize) -> Option<Vec<u8>> {
    let row = (per_row * bits as usize).div_ceil(8);
    let mask = (1u32 << bits) - 1;
    let mut out = Vec::with_capacity(per_row * rows);
    for r in v.chunks(row).take(rows) {
        for s in 0..per_row {
            let bit = s * bits as usize;
            let byte = *r.get(bit / 8)? as u32;
            let shift = 8 - bits as usize - bit % 8;
            out.push(((byte >> shift) & mask) as u8);
        }
    }
    Some(out)
}

/// What the RGBA pixels are made from.
struct Out<'a> {
    color: ColorType,
    spp: usize,
    n: usize,
    planar: bool,
    palette: Option<&'a [u16]>,
}

impl Out<'_> {
    /// Samples stretched from their lowest to highest finite value; others are 0.
    fn stretch<T: Copy>(&self, v: &[T], f: impl Fn(T) -> f64) -> Option<Vec<u8>> {
        let (lo, hi) = v.iter().map(|&x| f(x)).filter(|x| x.is_finite()).fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(x), b.max(x)));
        let span = if hi > lo { hi - lo } else { 1.0 };
        self.paint(v, |x| {
            let x = f(x);
            if x.is_finite() { ((x - lo) / span) as f32 } else { 0.0 }
        })
    }

    /// RGBA8 from samples that `unit` maps to 0–1 (or, for a palette, to its index).
    fn paint<T: Copy>(&self, v: &[T], unit: impl Fn(T) -> f32) -> Option<Vec<u8>> {
        let (n, spp) = (self.n, self.spp);
        if v.len() < n * spp {
            return None;
        }
        // Sample `c` of pixel `i`, interleaved or in planes.
        let at = |i: usize, c: usize| unit(if self.planar { v[c * n + i] } else { v[i * spp + c] });
        let byte = |x: f32| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let mut rgba = Vec::with_capacity(n * 4);
        for i in 0..n {
            let px = match self.color {
                ColorType::Gray(_) | ColorType::Lab(_) => {
                    let g = byte(at(i, 0));
                    [g, g, g, 255]
                }
                ColorType::GrayA(_) => {
                    let g = byte(at(i, 0));
                    [g, g, g, byte(at(i, 1))]
                }
                ColorType::RGB(_) | ColorType::YCbCr(_) => [byte(at(i, 0)), byte(at(i, 1)), byte(at(i, 2)), 255],
                ColorType::RGBA(_) => [byte(at(i, 0)), byte(at(i, 1)), byte(at(i, 2)), byte(at(i, 3))],
                ColorType::CMYK(_) | ColorType::CMYKA(_) => {
                    let k = 1.0 - at(i, 3);
                    let a = if spp > 4 { byte(at(i, 4)) } else { 255 };
                    [byte((1.0 - at(i, 0)) * k), byte((1.0 - at(i, 1)) * k), byte((1.0 - at(i, 2)) * k), a]
                }
                ColorType::Palette(_) => {
                    let map = self.palette?;
                    let entries = map.len() / 3;
                    let index = (at(i, 0) as usize).min(entries.saturating_sub(1));
                    // TIFF colour maps are 16-bit: all reds, then greens, then blues.
                    [(map[index] >> 8) as u8, (map[entries + index] >> 8) as u8, (map[2 * entries + index] >> 8) as u8, 255]
                }
                ColorType::Multiband { num_samples, .. } if num_samples >= 3 => [byte(at(i, 0)), byte(at(i, 1)), byte(at(i, 2)), 255],
                ColorType::Multiband { .. } => {
                    let g = byte(at(i, 0));
                    [g, g, g, 255]
                }
                _ => return None,
            };
            rgba.extend_from_slice(&px);
        }
        Some(rgba)
    }
}
