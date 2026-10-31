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
            let workers = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4)).min(jobs.len());
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
    };
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width().max(image.height()) > MAX_SIDE {
        // An integer box downscale straight into the output: the filtered resizers keep
        // a float copy of the image (134 MB for a 4K map) while they work.
        image.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        image
    };
    let rgba = image.into_rgba8();
    let (w, h) = rgba.dimensions();
    Some(with_mips(w, h, rgba.into_raw(), color_space))
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
