//! Texture decoding, off the loader's critical path: a model's geometry is on screen
//! first, then its images decode on worker threads and arrive in the scene one by one
//! (docs/3d-renderer.md §7.1).
//!
//! Images are scaled to at most [`MAX_SIDE`] and get a full mip chain here, so the
//! renderer only copies bytes. Mips of colour textures are averaged in linear light.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rendererx_scene::{ColorSpace, Stream, TextureData, TextureId, TextureSink};

/// Longest side kept. A 4K base colour map is 64 MB as RGBA8 (85 MB with mips); for a
/// viewer 2048 (21 MB) is sharp enough and a quarter of the memory.
pub const MAX_SIDE: u32 = 2048;
/// Encoded image files larger than this aren't read.
const MAX_FILE: u64 = 256 << 20;

pub enum ImageSource {
    /// Encoded bytes (embedded in the model, or in a GLB).
    Bytes(Stream<u8>),
    /// A file next to the model, already checked to be inside its folder.
    File(PathBuf),
    /// Roughness and metalness from separate images (or channels), packed the glTF way:
    /// roughness in green, metalness in blue. Each is an image and the channel to take
    /// (0–3: R, G, B, A); a missing one is white, so its factor alone applies.
    MetalRough { roughness: Option<(Box<ImageSource>, usize)>, metalness: Option<(Box<ImageSource>, usize)> },
}

pub struct ImageJob {
    pub texture: TextureId,
    pub source: ImageSource,
    pub color_space: ColorSpace,
}

/// Decodes `jobs` on worker threads and sets each result on the scene. Returns the
/// count of images still pending, which reaches zero when all are done (or failed).
pub fn spawn(sink: TextureSink, jobs: Vec<ImageJob>) -> Arc<AtomicUsize> {
    let pending = Arc::new(AtomicUsize::new(jobs.len()));
    if jobs.is_empty() {
        return pending;
    }
    let left = pending.clone();
    std::thread::Builder::new()
        .name("texture decode".into())
        .spawn(move || {
            // Two at a time: a 4K float EXR is ~256 MB while it decodes.
            let workers = std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 2)).min(jobs.len());
            let next = AtomicUsize::new(0);
            std::thread::scope(|s| {
                for _ in 0..workers {
                    s.spawn(|| {
                        while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                            // A failed image leaves its material untextured.
                            if let Some(data) = decode(&job.source, job.color_space) {
                                let _ = sink.set_texture(job.texture, data);
                            }
                            left.fetch_sub(1, Ordering::Release);
                        }
                    });
                }
            });
        })
        .expect("spawning the texture decoder");
    pending
}

fn decode(source: &ImageSource, color_space: ColorSpace) -> Option<TextureData> {
    if let ImageSource::MetalRough { roughness, metalness } = source {
        return metal_rough(roughness.as_ref(), metalness.as_ref());
    }
    let rgba = load(source, color_space)?;
    let (w, h) = rgba.dimensions();
    Some(with_mips(w, h, rgba.into_raw(), color_space))
}

/// One channel from each image, at the size of the larger, packed as
/// (1, roughness, metalness, 1).
fn metal_rough(roughness: Option<&(Box<ImageSource>, usize)>, metalness: Option<&(Box<ImageSource>, usize)>) -> Option<TextureData> {
    // Decoded one after the other and reduced to one channel each, so only one full
    // image is in memory at a time.
    let channel = |src: Option<&(Box<ImageSource>, usize)>| {
        let (src, c) = src?;
        let rgba = load(src, ColorSpace::Linear)?;
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
fn load(source: &ImageSource, color_space: ColorSpace) -> Option<image::RgbaImage> {
    let file;
    let bytes: &[u8] = match source {
        ImageSource::Bytes(s) => s.as_slice(),
        ImageSource::File(path) => {
            if std::fs::metadata(path).ok()?.len() > MAX_FILE {
                return None;
            }
            file = std::fs::read(path).ok()?;
            &file
        }
        ImageSource::MetalRough { .. } => return None,
    };
    if bytes.starts_with(&[0x76, 0x2f, 0x31, 0x01])
        && let Some(image) = load_exr(bytes, color_space == ColorSpace::Srgb)
    {
        return Some(image);
    }
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width().max(image.height()) > MAX_SIDE {
        // An integer box downscale straight into the output: the filtered resizers keep
        // a float copy of the image (134 MB for a 4K map) while they work.
        image.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        image
    };
    Some(image.into_rgba8())
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
        format: rendererx_scene::TextureFormat::Rgba8,
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

/// Where a model's texture file is, trying the stored relative path, then the file name
/// next to the model and in a `textures/` folder (where exporters and asset libraries
/// usually put them). Only files inside the model's folder are used, so a model can't
/// make the viewer read elsewhere.
pub fn find_file(dir: &Path, relative: &str, absolute: &str) -> Option<PathBuf> {
    let root = dir.canonicalize().ok()?;
    let name = |p: &str| Path::new(&p.replace('\\', "/")).file_name().map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = Vec::new();
    let rel = PathBuf::from(relative.replace('\\', "/"));
    if !relative.is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        candidates.push(rel);
    }
    for file in [name(relative), name(absolute)].into_iter().flatten() {
        candidates.push(file.clone());
        candidates.push(Path::new("textures").join(&file));
    }
    candidates.into_iter().find_map(|c| {
        let real = root.join(c).canonicalize().ok()?;
        (real.starts_with(&root) && real.is_file()).then_some(real)
    })
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
    fn separate_roughness_and_metal_maps_pack_into_green_and_blue() {
        let png = |v: u8, w: u32| {
            let mut out = Vec::new();
            image::GrayImage::from_pixel(w, w, image::Luma([v])).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
            Box::new(ImageSource::Bytes(out.into()))
        };
        // Different sizes: packed at the larger.
        let t = decode(&ImageSource::MetalRough { roughness: Some((png(200, 4), 0)), metalness: Some((png(30, 2), 0)) }, ColorSpace::Linear).unwrap();
        assert_eq!((t.width, t.height, t.color_space), (4, 4, ColorSpace::Linear));
        assert_eq!(&t.levels[0].as_slice()[..4], [255, 200, 30, 255]);
        // Only roughness: metal is white, so the metallic factor alone applies.
        let t = decode(&ImageSource::MetalRough { roughness: Some((png(90, 2), 0)), metalness: None }, ColorSpace::Linear).unwrap();
        assert_eq!(&t.levels[0].as_slice()[..4], [255, 90, 255, 255]);
    }

    #[test]
    fn texture_files_are_found_but_never_outside_the_model_folder() {
        let dir = std::env::temp_dir().join(format!("viewer-fmt-3d-tex-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("textures")).unwrap();
        std::fs::write(dir.join("textures/wood.jpg"), b"x").unwrap();
        let found = find_file(&dir, "", r"C:\Users\artist\Desktop\wood.jpg").unwrap();
        assert!(found.ends_with("textures/wood.jpg"));
        assert!(find_file(&dir, "../outside.jpg", "").is_none());
        assert!(find_file(&dir, "missing.png", "").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
