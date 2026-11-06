//! Texture decoding, off the reader's critical path: a model's geometry is on screen
//! first, then its images decode as background jobs (`cx.spawn`) and arrive in the
//! scene one by one, each announced through `cx.notify` (docs/design/3d-renderer.md §7.1).
//!
//! Images are scaled to at most [`MAX_SIDE`] and get a full mip chain here, so the
//! renderer only copies bytes. Mips of colour textures are averaged in linear light.

use std::path::{Component, Path};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use libfileparser_core::{Dependencies, FileSource, ParseContext};
use scene_data::{ColorSpace, Stream, TextureData, TextureId, TextureSink};

/// Longest side kept. A 4K base colour map is 64 MB as RGBA8 (85 MB with mips); for a
/// viewer 2048 (21 MB) is sharp enough and a quarter of the memory.
pub const MAX_SIDE: u32 = 2048;
/// Encoded image files larger than this aren't read.
const MAX_FILE: u64 = 256 << 20;

/// Where a texture's image comes from.
#[non_exhaustive]
pub enum ImageSource {
    /// Encoded bytes (embedded in the model, or in a GLB).
    Bytes(Stream<u8>),
    /// A file the model refers to, read through the files hook.
    File(FileSource),
    /// Roughness and metalness from separate images (or channels), packed the glTF way:
    /// roughness in green, metalness in blue. Each is an image and the channel to take
    /// (0–3: R, G, B, A); a missing one is white, so its factor alone applies.
    MetalRough { roughness: Option<(Box<ImageSource>, usize)>, metalness: Option<(Box<ImageSource>, usize)> },
}

/// One image to decode into one of the scene's textures.
pub struct ImageJob {
    texture: TextureId,
    source: ImageSource,
    color_space: ColorSpace,
}

impl ImageJob {
    pub fn new(texture: TextureId, source: ImageSource, color_space: ColorSpace) -> Self {
        Self { texture, source, color_space }
    }
}

/// Queues one background job per image (`cx.spawn`), each setting its result on the
/// scene and announcing it (`cx.notify`). Returns the count of images still pending,
/// which reaches zero when all are done (or failed).
///
/// Jobs hold only a weak reference to the count: once the document that owns it is
/// dropped (its page closed), or `cx.cancel` is set, the rest skip their image instead
/// of decoding a model's textures nobody will see, and nothing more is announced.
pub fn spawn(sink: TextureSink, jobs: Vec<ImageJob>, cx: &ParseContext) -> Arc<AtomicUsize> {
    let pending = Arc::new(AtomicUsize::new(jobs.len()));
    let sink = Arc::new(sink);
    let max_alloc = cx.limits.allocation;
    for job in jobs {
        let (left, sink, cancel, notify) = (Arc::downgrade(&pending), sink.clone(), cx.cancel.clone(), cx.notify.clone());
        cx.spawn_job(move || {
            let wanted = || left.strong_count() > 0 && !cancel.is_cancelled();
            // A failed image leaves its material untextured; so does a decoder that
            // panics, and the count below still goes down.
            if wanted()
                && let Some(data) = ParseContext::catching(|| decode(&job.source, job.color_space, max_alloc))
            {
                let _ = sink.set_texture(job.texture, data);
            }
            if let Some(left) = left.upgrade() {
                left.fetch_sub(1, Ordering::Release);
            }
            if wanted() {
                notify.notify();
            }
        });
    }
    pending
}

fn decode(source: &ImageSource, color_space: ColorSpace, max_alloc: u64) -> Option<TextureData> {
    if let ImageSource::MetalRough { roughness, metalness } = source {
        return metal_rough(roughness.as_ref(), metalness.as_ref(), max_alloc);
    }
    let rgba = load(source, color_space, max_alloc)?;
    let (w, h) = rgba.dimensions();
    Some(with_mips(w, h, rgba.into_raw(), color_space))
}

/// One channel from each image, at the size of the larger, packed as
/// (1, roughness, metalness, 1).
fn metal_rough(
    roughness: Option<&(Box<ImageSource>, usize)>,
    metalness: Option<&(Box<ImageSource>, usize)>,
    max_alloc: u64,
) -> Option<TextureData> {
    // Decoded one after the other and reduced to one channel each, so only one full
    // image is in memory at a time.
    let channel = |src: Option<&(Box<ImageSource>, usize)>| {
        let (src, c) = src?;
        let rgba = load(src, ColorSpace::Linear, max_alloc)?;
        let (w, h) = rgba.dimensions();
        let c = (*c).min(3);
        image::GrayImage::from_raw(w, h, rgba.as_raw().iter().skip(c).step_by(4).copied().collect())
    };
    let (r, m) = (channel(roughness), channel(metalness));
    let (w, h) = match (&r, &m) {
        (Some(a), Some(b)) => (a.width().max(b.width()), a.height().max(b.height())),
        (Some(a), None) | (None, Some(a)) => a.dimensions(),
        (None, None) => return None,
    };
    let fit = |c: Option<image::GrayImage>| {
        c.map(|c| if c.dimensions() == (w, h) { c } else { image::imageops::resize(&c, w, h, image::imageops::FilterType::Triangle) })
    };
    let (r, m) = (fit(r), fit(m));
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for i in 0..(w * h) as usize {
        let get = |c: &Option<image::GrayImage>| c.as_ref().map_or(255, |c| c.as_raw()[i]);
        rgba.extend_from_slice(&[255, get(&r), get(&m), 255]);
    }
    Some(with_mips(w, h, rgba, ColorSpace::Linear))
}

/// Decodes and scales one image to at most [`MAX_SIDE`]. `color_space` says how to
/// store float (EXR) data: sRGB-encoded for colour maps, as-is for data maps.
/// `image`'s limits from the document's: a texture whose header lies about its size
/// fails here instead of in the allocator.
fn image_limits(max_alloc: u64) -> image::Limits {
    let mut l = image::Limits::default();
    l.max_alloc = Some(max_alloc);
    l
}

fn load(source: &ImageSource, color_space: ColorSpace, max_alloc: u64) -> Option<image::RgbaImage> {
    let bytes: &[u8] = match source {
        ImageSource::Bytes(s) => s.as_slice(),
        ImageSource::File(file) if file.len() > MAX_FILE => return None,
        ImageSource::File(file) => file.bytes(),
        ImageSource::MetalRough { .. } => return None,
    };
    if bytes.starts_with(&[0x76, 0x2f, 0x31, 0x01])
        && let Some(image) = load_exr(bytes, color_space == ColorSpace::Srgb)
    {
        return Some(image);
    }
    let srgb = color_space == ColorSpace::Srgb;
    if bytes.starts_with(&[0xff, 0xd8])
        && let Some(image) = load_jpeg(bytes, srgb)
    {
        return Some(image);
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    reader.limits(image_limits(max_alloc));
    let image = reader.decode().ok()?;
    if image.width().max(image.height()) <= MAX_SIDE {
        return Some(image.into_rgba8());
    }
    // 8-bit images (nearly all textures) shrink straight from their own layout, in
    // linear light for colour maps; the rest go through `image`.
    let (w, h) = (image.width(), image.height());
    let channels = match &image {
        image::DynamicImage::ImageLuma8(_) => 1,
        image::DynamicImage::ImageLumaA8(_) => 2,
        image::DynamicImage::ImageRgb8(_) => 3,
        image::DynamicImage::ImageRgba8(_) => 4,
        _ => return Some(image.thumbnail(MAX_SIDE, MAX_SIDE).into_rgba8()),
    };
    shrink(image.as_bytes(), w, h, channels, srgb)
}

/// Scales an 8-bit image (1–4 channels) so its longest side is [`MAX_SIDE`], with an
/// area-weighted box filter: every source pixel adds to the one or two output pixels
/// it overlaps in each direction. Colour channels of sRGB maps are averaged in linear
/// light (averaging encoded bytes darkens detail, and disagreed with the mips). Works
/// a row at a time: only three output rows of floats are held, never a float copy of
/// the image.
fn shrink(px: &[u8], w: u32, h: u32, channels: usize, srgb: bool) -> Option<image::RgbaImage> {
    let side = w.max(h) as u64;
    let out = |n: u32| ((n as u64 * MAX_SIDE as u64 + side / 2) / side).max(1) as usize;
    let (ow, oh) = (out(w), out(h));
    if px.len() < w as usize * h as usize * channels {
        return None;
    }
    let lut: Vec<f32> = (0..256).map(|i| if srgb { srgb_to_linear(i as f32 / 255.0) } else { i as f32 / 255.0 }).collect();
    let cols: Vec<Split> = (0..w as usize).map(|x| split(x, w as usize, ow)).collect();
    let (mut row, mut cur, mut next) = (vec![0f32; ow * 4], vec![0f32; ow * 4], vec![0f32; ow * 4]);
    let mut pixels = Vec::with_capacity(ow * oh * 4);
    let emit = |acc: &[f32], pixels: &mut Vec<u8>| {
        for t in acc.as_chunks::<4>().0 {
            for (c, &v) in t.iter().enumerate() {
                let v = if srgb && c < 3 { linear_to_srgb(v) } else { v.clamp(0.0, 1.0) };
                pixels.push((v * 255.0 + 0.5) as u8);
            }
        }
    };
    let mut done = 0;
    for y in 0..h as usize {
        row.fill(0.0);
        let src = &px[y * w as usize * channels..(y + 1) * w as usize * channels];
        for (x, t) in src.chunks_exact(channels).enumerate() {
            let rgba = match *t {
                [l] => [lut[l as usize], lut[l as usize], lut[l as usize], 1.0],
                [l, a] => [lut[l as usize], lut[l as usize], lut[l as usize], a as f32 / 255.0],
                [r, g, b] => [lut[r as usize], lut[g as usize], lut[b as usize], 1.0],
                [r, g, b, a] => [lut[r as usize], lut[g as usize], lut[b as usize], a as f32 / 255.0],
                _ => unreachable!("1–4 channels"),
            };
            let s = cols[x];
            for c in 0..4 {
                row[s.at * 4 + c] += rgba[c] * s.first;
                if s.second > 0.0 {
                    row[(s.at + 1) * 4 + c] += rgba[c] * s.second;
                }
            }
        }
        let s = split(y, h as usize, oh);
        while done < s.at {
            emit(&cur, &mut pixels);
            std::mem::swap(&mut cur, &mut next);
            next.fill(0.0);
            done += 1;
        }
        cur.iter_mut().zip(&row).for_each(|(a, r)| *a += r * s.first);
        if s.second > 0.0 {
            next.iter_mut().zip(&row).for_each(|(a, r)| *a += r * s.second);
        }
    }
    while done < oh {
        emit(&cur, &mut pixels);
        std::mem::swap(&mut cur, &mut next);
        next.fill(0.0);
        done += 1;
    }
    image::RgbaImage::from_raw(ow as u32, oh as u32, pixels)
}

/// How source pixel `i` of `n` spreads over `out` output pixels: weight `first` to
/// output `at`, `second` to `at + 1`. Weights are the overlap in output units, so each
/// output pixel's weights sum to 1.
#[derive(Clone, Copy)]
struct Split {
    at: usize,
    first: f32,
    second: f32,
}

fn split(i: usize, n: usize, out: usize) -> Split {
    let scale = out as f64 / n as f64;
    let (a, b) = (i as f64 * scale, (i + 1) as f64 * scale);
    let at = (a.floor() as usize).min(out - 1);
    let edge = (at + 1) as f64;
    if b <= edge || at + 1 >= out {
        Split { at, first: (b - a) as f32, second: 0.0 }
    } else {
        Split { at, first: (edge - a) as f32, second: (b - edge) as f32 }
    }
}

/// Decodes a JPEG at the smallest of 1, 1/2, 1/4, 1/8 scale that still reaches
/// [`MAX_SIDE`] (the IDCT does the scaling), then finishes with a box filter. A 4K map
/// is decoded at 2K: a quarter of the memory and time.
fn load_jpeg(bytes: &[u8], srgb: bool) -> Option<image::RgbaImage> {
    let mut d = jpeg_decoder::Decoder::new(bytes);
    d.read_info().ok()?;
    let info = d.info()?;
    let side = info.width.max(info.height) as u32;
    if side > MAX_SIDE {
        // `scale` picks the smallest IDCT size at least this large.
        let k = MAX_SIDE as f32 / side as f32;
        d.scale((info.width as f32 * k).ceil() as u16, (info.height as f32 * k).ceil() as u16).ok()?;
    }
    let pixels = d.decode().ok()?;
    let info = d.info()?;
    let (w, h) = (info.width as u32, info.height as u32);
    let channels = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => 3,
        jpeg_decoder::PixelFormat::L8 => 1,
        // CMYK and 16-bit greyscale: let `image` handle them.
        _ => return None,
    };
    if w.max(h) > MAX_SIDE {
        // The IDCT's scaling averages encoded values; the finishing step is in linear light.
        return shrink(&pixels, w, h, channels, srgb);
    }
    let rgba: Vec<u8> = match channels {
        3 => pixels.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
        _ => pixels.iter().flat_map(|&l| [l, l, l, 255]).collect(),
    };
    image::RgbaImage::from_raw(w, h, rgba)
}

/// Reads an EXR straight into the scaled-down result: each decoded pixel is added to
/// its target texel, so a 4K float map costs ~32 MB of sums instead of the ~256 MB a
/// full float image would. RGB(A) and single-channel (Y) files; others return `None`
/// and go through `image`.
fn load_exr(bytes: &[u8], srgb: bool) -> Option<image::RgbaImage> {
    use exr::prelude::*;
    let rgba = read()
        .no_deep_data()
        .largest_resolution_level()
        .rgba_channels(|size, _| Shrink::new(size.width(), size.height(), srgb), |s: &mut Shrink, at, (r, g, b, a): (f32, f32, f32, f32)| {
            s.add(at.x(), at.y(), [r, g, b, a])
        })
        .first_valid_layer()
        .all_attributes()
        .non_parallel()
        .from_buffered(std::io::Cursor::new(bytes));
    if let Ok(image) = rgba {
        return image.layer_data.channel_data.pixels.finish();
    }
    let grey = read()
        .no_deep_data()
        .largest_resolution_level()
        .specific_channels()
        .required("Y")
        .collect_pixels(|size, _| Shrink::new(size.width(), size.height(), srgb), |s: &mut Shrink, at, (y,): (f32,)| {
            s.add(at.x(), at.y(), [y, y, y, 1.0])
        })
        .first_valid_layer()
        .all_attributes()
        .non_parallel()
        .from_buffered(std::io::Cursor::new(bytes))
        .ok()?;
    grey.layer_data.channel_data.pixels.finish()
}

/// A box-filter downscale by a whole factor, filled one source pixel at a time in any
/// order. Sums of 8-bit values fit u16 for factors up to 16 (65 536 px sources).
struct Shrink {
    width: usize,
    height: usize,
    factor: usize,
    out_w: usize,
    out_h: usize,
    srgb: bool,
    sums: Vec<u16>,
}

impl Shrink {
    fn new(width: usize, height: usize, srgb: bool) -> Self {
        let factor = width.max(height).div_ceil(MAX_SIDE as usize).max(1);
        let (out_w, out_h) = (width.div_ceil(factor), height.div_ceil(factor));
        // Oversized files get an empty buffer and are refused in `finish`.
        let sums = if factor <= 16 { vec![0u16; out_w * out_h * 4] } else { Vec::new() };
        Self { width, height, factor, out_w, out_h, srgb, sums }
    }

    fn add(&mut self, x: usize, y: usize, rgba: [f32; 4]) {
        if self.sums.is_empty() || x >= self.width || y >= self.height {
            return;
        }
        let i = ((y / self.factor) * self.out_w + x / self.factor) * 4;
        for (c, v) in rgba.into_iter().enumerate() {
            let v = if self.srgb && c < 3 { linear_to_srgb(v) } else { v.clamp(0.0, 1.0) };
            let v = if v.is_nan() { 0.0 } else { v };
            self.sums[i + c] += (v * 255.0 + 0.5) as u16;
        }
    }

    fn finish(self) -> Option<image::RgbaImage> {
        if self.sums.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(self.out_w * self.out_h * 4);
        for ty in 0..self.out_h {
            let rows = self.factor.min(self.height - ty * self.factor);
            for tx in 0..self.out_w {
                let count = (rows * self.factor.min(self.width - tx * self.factor)) as u32;
                let i = (ty * self.out_w + tx) * 4;
                out.extend(self.sums[i..i + 4].iter().map(|&s| ((s as u32 + count / 2) / count) as u8));
            }
        }
        image::RgbaImage::from_raw(self.out_w as u32, self.out_h as u32, out)
    }
}

/// Level 0 plus every smaller level down to 1×1, each a 2×2 box filter of the one
/// above (edge texels repeat for odd sizes).
pub fn with_mips(width: u32, height: u32, rgba: Vec<u8>, color_space: ColorSpace) -> TextureData {
    let srgb = color_space == ColorSpace::Srgb;
    let to_linear: Vec<f32> = (0..256).map(|i| if srgb { srgb_to_linear(i as f32 / 255.0) } else { i as f32 / 255.0 }).collect();
    let mut levels: Vec<Vec<u8>> = vec![rgba];
    let (mut w, mut h) = (width, height);
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let src = levels.last().expect("level 0");
        let mut out = Vec::with_capacity((nw * nh * 4) as usize);
        for y in 0..nh {
            for x in 0..nw {
                let texel = |dx: u32, dy: u32, c: usize| {
                    let (sx, sy) = ((x * 2 + dx).min(w - 1), (y * 2 + dy).min(h - 1));
                    src[((sy * w + sx) * 4) as usize + c]
                };
                for c in 0..4 {
                    let sum: u32 = [(0, 0), (1, 0), (0, 1), (1, 1)].iter().map(|&(dx, dy)| texel(dx, dy, c) as u32).sum();
                    if c == 3 || !srgb {
                        out.push(((sum + 2) / 4) as u8);
                    } else {
                        let lin: f32 = [(0, 0), (1, 0), (0, 1), (1, 1)].iter().map(|&(dx, dy)| to_linear[texel(dx, dy, c) as usize]).sum::<f32>() / 4.0;
                        out.push((linear_to_srgb(lin) * 255.0 + 0.5) as u8);
                    }
                }
            }
        }
        levels.push(out);
        (w, h) = (nw, nh);
    }
    TextureData {
        width,
        height,
        format: scene_data::TextureFormat::Rgba8,
        color_space,
        levels: levels.into_iter().map(Stream::from).collect(),
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// Where a model's texture file may be, in order: the stored relative path, then the
/// file name, alone and in a `textures/` folder (where exporters and asset libraries
/// usually put them), in each of `dirs` (relative to the model; `""` is its own
/// folder). A stored path that climbs out (`..`) or is absolute is only used by name,
/// so a model can't name a file outside the folders it was given.
pub fn candidates(relative: &str, absolute: &str, dirs: &[&str]) -> Vec<String> {
    let name = |p: &str| Path::new(&p.replace('\\', "/")).file_name().map(|n| n.to_string_lossy().into_owned());
    let rel = relative.replace('\\', "/");
    let rel_ok = !rel.is_empty() && Path::new(&rel).components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    let mut out: Vec<String> = Vec::new();
    for dir in dirs {
        let at = |p: &str| if dir.is_empty() { p.to_owned() } else { format!("{dir}/{p}") };
        if rel_ok {
            out.push(at(rel.trim_start_matches("./")));
        }
        for file in [name(relative), name(absolute)].into_iter().flatten() {
            out.push(at(&file));
            out.push(at(&format!("textures/{file}")));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert(c.clone()));
    out
}

/// The texture a model names, found through the files hook and recorded once in
/// `deps` under the file name it was stored with. `relative` and `absolute` are the
/// paths the model stored (either may be empty); each of `dirs` (relative to the model,
/// `""` for its own folder) is tried with the stored relative path, then the file name
/// alone and in a `textures/` folder. A path that climbs out or is absolute is only
/// tried by name.
pub fn find(cx: &ParseContext, deps: &Dependencies, from: &FileSource, relative: &str, absolute: &str, dirs: &[&str]) -> Option<FileSource> {
    let report = report_name(relative, absolute);
    if report.is_empty() {
        return None;
    }
    deps.resolve_any(cx, from, &report, &candidates(relative, absolute, dirs))
}

/// The name to report a texture under: the file name it was stored with.
pub fn report_name(relative: &str, absolute: &str) -> String {
    let pick = if relative.is_empty() { absolute } else { relative };
    pick.rsplit(['/', '\\']).next().unwrap_or(pick).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mips_go_down_to_one_texel_and_average_in_linear_light() {
        // Black and white stripes: the linear average is 50% grey, which is 188 in
        // sRGB, not 128.
        let mut px = Vec::new();
        for _ in 0..2 {
            px.extend_from_slice(&[0, 0, 0, 255, 255, 255, 255, 255]);
        }
        let t = with_mips(2, 2, px, ColorSpace::Srgb);
        assert_eq!(t.levels.len(), 2);
        assert_eq!(t.levels[1].as_slice(), [188, 188, 188, 255]);
        assert!(t.validate(&Default::default()).is_ok());

        let t = with_mips(5, 3, vec![10; 5 * 3 * 4], ColorSpace::Linear);
        assert_eq!(t.levels.len(), 3, "5×3, 2×1, 1×1");
        assert!(t.validate(&Default::default()).is_ok());
    }

    #[test]
    fn exr_style_shrinking_averages_whole_blocks_and_partial_edges() {
        // 4099 wide: factor 3 (to 1367), so the last column's block has one pixel.
        let mut s = Shrink::new(4099, 2, false);
        assert_eq!((s.factor, s.out_w, s.out_h), (3, 1367, 1));
        for y in 0..2 {
            for x in 0..4099 {
                let v = if x < 3 { [1.0, 0.0, 0.5, 1.0] } else { [0.2, 0.2, 0.2, 1.0] };
                s.add(x, y, v);
            }
        }
        s.add(9999, 0, [1.0; 4]); // out of range: ignored
        let img = s.finish().unwrap();
        assert_eq!(img.dimensions(), (1367, 1));
        assert_eq!(img.get_pixel(0, 0).0, [255, 0, 128, 255]);
        assert_eq!(img.get_pixel(1366, 0).0, [51, 51, 51, 255], "the 1-pixel-wide edge block averages its 2 pixels");
        // Colour maps stored as linear floats are sRGB-encoded.
        let mut c = Shrink::new(1, 1, true);
        c.add(0, 0, [0.2158, 0.0, 1.0, 1.0]); // sRGB 128
        assert_eq!(c.finish().unwrap().get_pixel(0, 0).0, [128, 0, 255, 255]);
    }

    #[test]
    fn big_images_shrink_to_max_side_in_linear_light() {
        // 4096×2 black/white columns → 2048×1, each texel half black, half white:
        // 50% linear is 188 in sRGB (a gamma-space average would give 128).
        let px: Vec<u8> = (0..2).flat_map(|_| (0..4096).flat_map(|x| if x % 2 == 0 { [0u8; 3] } else { [255u8; 3] })).collect();
        let img = shrink(&px, 4096, 2, 3, true).unwrap();
        assert_eq!(img.dimensions(), (2048, 1));
        assert_eq!(img.get_pixel(5, 0).0, [188, 188, 188, 255]);
        // Data maps average as stored.
        assert_eq!(shrink(&px, 4096, 2, 3, false).unwrap().get_pixel(5, 0).0, [128, 128, 128, 255]);
        // A non-integer ratio still reaches the full size (3000 → 2048, not 1500).
        let grey = vec![77u8; 3000 * 10];
        let img = shrink(&grey, 3000, 10, 1, true).unwrap();
        assert_eq!(img.dimensions(), (2048, 7));
        assert!(img.pixels().all(|p| p.0 == [77, 77, 77, 255]), "a flat image stays flat");
    }

    #[test]
    fn decoding_stops_once_the_document_is_dropped_or_cancelled() {
        use libfileparser_testkit::{CountingNotify, QueueSpawn};
        let png = |v: u8| {
            let mut out = Vec::new();
            image::RgbaImage::from_pixel(8, 8, image::Rgba([v; 4])).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
            out
        };
        for close in ["drop", "cancel"] {
            let (scene, mut b) = scene_data::CpuScene::new();
            let jobs: Vec<ImageJob> = (0..20)
                .map(|i| ImageJob { texture: b.reserve_texture().unwrap(), source: ImageSource::Bytes(png(i as u8).into()), color_space: ColorSpace::Srgb })
                .collect();
            let (spawn_q, notify) = (QueueSpawn::default(), CountingNotify::default());
            let cx = ParseContext::new().with_spawn(spawn_q.clone()).with_notify(notify.clone());
            let pending = spawn(b.texture_sink(), jobs, &cx);
            assert_eq!(spawn_q.queued(), 20, "one job per image, through cx.spawn");
            match close {
                "drop" => drop(pending),
                _ => cx.cancel.cancel(),
            }
            spawn_q.run_all();
            let decoded = scene.read().textures().iter().filter(|t| t.info.is_some()).count();
            assert_eq!((decoded, notify.count()), (0, 0), "{close}: nothing decoded or announced after closing");
        }
    }

    #[test]
    fn separate_roughness_and_metal_maps_pack_into_green_and_blue() {
        let png = |v: u8, w: u32| {
            let mut out = Vec::new();
            image::GrayImage::from_pixel(w, w, image::Luma([v])).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
            Box::new(ImageSource::Bytes(out.into()))
        };
        // Different sizes: packed at the larger.
        let t = decode(&ImageSource::MetalRough { roughness: Some((png(200, 4), 0)), metalness: Some((png(30, 2), 0)) }, ColorSpace::Linear, u64::MAX).unwrap();
        assert_eq!((t.width, t.height, t.color_space), (4, 4, ColorSpace::Linear));
        assert_eq!(&t.levels[0].as_slice()[..4], [255, 200, 30, 255]);
        // Only roughness: metal is white, so the metallic factor alone applies.
        let t = decode(&ImageSource::MetalRough { roughness: Some((png(90, 2), 0)), metalness: None }, ColorSpace::Linear, u64::MAX).unwrap();
        assert_eq!(&t.levels[0].as_slice()[..4], [255, 90, 255, 255]);
    }

    #[test]
    fn texture_candidates_never_climb_out_of_their_folders() {
        assert_eq!(
            candidates("", r"C:\Users\artist\Desktop\wood.jpg", &[""]),
            ["wood.jpg", "textures/wood.jpg"],
            "an exporter's absolute path is tried by name"
        );
        assert_eq!(candidates("../outside.jpg", "", &[""]), ["outside.jpg", "textures/outside.jpg"], "a climbing path only by name");
        assert_eq!(candidates("maps/a.png", "", &["", "sub"]), ["maps/a.png", "a.png", "textures/a.png", "sub/maps/a.png", "sub/a.png", "sub/textures/a.png"]);
    }

    #[test]
    fn found_textures_are_recorded_once_as_read_blocked_or_missing() {
        use libfileparser_core::DependencyStatus::{Blocked, Missing, Read};
        use libfileparser_testkit::{DenyFiles, MemFiles};
        let model = FileSource::from_bytes("m.obj", Vec::new());
        let deps = Dependencies::new();
        let cx = ParseContext::new().with_files(MemFiles::new([("textures/wood.jpg".to_string(), b"x".to_vec())]));
        assert!(find(&cx, &deps, &model, "", r"C:\art\wood.jpg", &[""]).is_some());
        assert!(find(&cx, &deps, &model, "stone.png", "", &[""]).is_none());
        let cx = ParseContext::new().with_files(DenyFiles);
        assert!(find(&cx, &deps, &model, "maps/moss.png", "", &[""]).is_none());
        let status = |s| deps.with_status(s);
        assert_eq!((status(Read), status(Missing), status(Blocked)), (vec!["wood.jpg".to_string()], vec!["stone.png".to_string()], vec!["moss.png".to_string()]));
    }
}
