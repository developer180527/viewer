//! Image loaders: everything the `image` crate decodes, plus SVG via resvg.

use std::sync::{Arc, OnceLock};

use anyhow::Context;
use image::ImageFormat;
use viewer_core::{Band, BandStream, Confidence, Content, Document, FileSource, FormatLoader, Pixels, Raster, Result};

pub static LOADERS: &[&dyn FormatLoader] = &[&ImageLoader, &SvgLoader];

pub struct ImageLoader;

impl FormatLoader for ImageLoader {
    fn id(&self) -> &'static str {
        "image"
    }

    fn name(&self) -> &'static str {
        "Image"
    }

    fn probe(&self, header: &[u8], ext: Option<&str>, _: &str) -> Confidence {
        if image::guess_format(header).is_ok_and(|f| f.reading_enabled()) {
            Confidence::MAGIC
        } else if ext
            .and_then(ImageFormat::from_extension)
            .is_some_and(|f| f.reading_enabled())
        {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let format = image::guess_format(src.header())
            .ok()
            .or_else(|| src.ext().as_deref().and_then(ImageFormat::from_extension))
            .context("unknown image format")?;
        let name = format!("{} image", format.extensions_str()[0].to_ascii_uppercase());
        let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(src.bytes()), format).into_dimensions()?;
        if width as u64 * height as u64 > STREAM_ABOVE_PIXELS {
            let (pixels, color) = stream(src, format, width, height)?;
            return Ok(Document::new(name, Content::Raster(Raster { width, height, pixels }))
                .with_meta("Dimensions", format!("{width} × {height}"))
                .with_meta("Color", color)
                .with_meta("Decoded", "in bands, straight to the GPU"));
        }
        let img = image::load_from_memory_with_format(src.bytes(), format)?;
        let color = format!("{:?}", img.color());
        let rgba = img.into_rgba8();
        let (width, height) = rgba.dimensions();
        Ok(Document::new(name, Content::Raster(Raster::full(width, height, rgba.into_raw())))
            .with_meta("Dimensions", format!("{width} × {height}"))
            .with_meta("Color", color))
    }
}

/// Above this many pixels (64 MB as RGBA) images stream: decoded in bands on a thread
/// and uploaded as they arrive, so the whole RGBA image never sits in memory.
const STREAM_ABOVE_PIXELS: u64 = 16 << 20;
/// Bytes of RGBA per band.
const BAND_BYTES: usize = 16 << 20;

fn band_rows(width: u32, height: u32) -> u32 {
    ((BAND_BYTES / (width as usize * 4).max(1)) as u32).clamp(16, height.max(1))
}

/// The file's bytes, kept alive for the decoder thread.
struct Shared(Arc<dyn AsRef<[u8]> + Send + Sync>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

/// Decodes on a thread, band by band. Non-interlaced PNGs are read row by row (never
/// whole); other formats decode in their own layout (RGB, 16-bit…) and convert to
/// RGBA one band at a time, which still saves the full RGBA copy.
fn stream(src: &FileSource, format: ImageFormat, width: u32, height: u32) -> Result<(Pixels, String)> {
    let bytes = Shared(src.shared_bytes());
    if format == ImageFormat::Png {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(Shared(src.shared_bytes())));
        // 16-bit samples stay 16-bit here and are rounded below, as `image` does
        // (STRIP_16 would truncate).
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
        let reader = decoder.read_info()?;
        if !reader.info().interlaced {
            let color = format!("{:?}", reader.info().color_type);
            return Ok((Pixels::Bands(BandStream::spawn(move |emit| png_bands(reader, width, height, emit))), color));
        }
    }
    let stream = BandStream::spawn(move |emit| {
        let img = image::load_from_memory_with_format(bytes.as_ref(), format).map_err(|e| e.to_string())?;
        let rows = band_rows(width, height);
        let mut y = 0;
        while y < height {
            let h = rows.min(height - y);
            let band = img.crop_imm(0, y, width, h).into_rgba8().into_raw();
            if !emit(Band { y, rgba: band }) {
                return Ok(());
            }
            y += h;
        }
        Ok(())
    });
    Ok((Pixels::Bands(stream), "decoded in full, converted in bands".into()))
}

fn png_bands(mut reader: png::Reader<std::io::Cursor<Shared>>, width: u32, height: u32, emit: &mut dyn FnMut(Band) -> bool) -> std::result::Result<(), String> {
    let (color, depth) = reader.output_color_type();
    let rows = band_rows(width, height);
    let row_bytes = width as usize * 4;
    let mut eight = Vec::new();
    let mut y = 0;
    while y < height {
        let h = rows.min(height - y);
        let mut band = Vec::with_capacity(h as usize * row_bytes);
        for _ in 0..h {
            let row = reader.next_row().map_err(|e| e.to_string())?.ok_or("PNG ended early")?;
            let data = if depth == png::BitDepth::Sixteen {
                eight.clear();
                eight.extend(row.data().as_chunks::<2>().0.iter().map(|&[hi, lo]| ((u16::from_be_bytes([hi, lo]) as u32 + 128) / 257) as u8));
                &eight[..]
            } else {
                row.data()
            };
            match color {
                png::ColorType::Rgba => band.extend_from_slice(&data[..row_bytes.min(data.len())]),
                png::ColorType::GrayscaleAlpha => band.extend(data.as_chunks::<2>().0.iter().flat_map(|&[l, a]| [l, l, l, a])),
                png::ColorType::Rgb => band.extend(data.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255])),
                png::ColorType::Grayscale => band.extend(data.iter().flat_map(|&l| [l, l, l, 255])),
                png::ColorType::Indexed => return Err("palette PNG not expanded".into()),
            }
        }
        band.resize(h as usize * row_bytes, 0);
        if !emit(Band { y, rgba: band }) {
            return Ok(());
        }
        y += h;
    }
    Ok(())
}

pub struct SvgLoader;

/// SVGs are rasterised once at load; the longest side is capped here.
const SVG_MAX_SIDE: f32 = 4096.0;

impl FormatLoader for SvgLoader {
    fn id(&self) -> &'static str {
        "svg"
    }

    fn name(&self) -> &'static str {
        "SVG"
    }

    fn probe(&self, header: &[u8], ext: Option<&str>, _: &str) -> Confidence {
        if matches!(ext, Some("svg" | "svgz")) {
            return Confidence::EXTENSION;
        }
        let head = String::from_utf8_lossy(&header[..header.len().min(1024)]);
        if head.contains("<svg") {
            Confidence::LIKELY
        } else {
            Confidence::NONE
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let opt = resvg::usvg::Options { fontdb: system_fonts(), ..Default::default() };
        let tree = resvg::usvg::Tree::from_data(src.bytes(), &opt)?;
        let size = tree.size();
        // Render at 2x for crisp zooming, within the cap.
        let scale = (2.0f32).min(SVG_MAX_SIDE / size.width().max(size.height()));
        let width = (size.width() * scale).ceil().max(1.0) as u32;
        let height = (size.height() * scale).ceil().max(1.0) as u32;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).context("SVG too large")?;
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        let rgba = pixmap
            .pixels()
            .iter()
            .flat_map(|p| {
                let c = p.demultiply();
                [c.red(), c.green(), c.blue(), c.alpha()]
            })
            .collect();
        Ok(Document::new("SVG image", Content::Raster(Raster::full(width, height, rgba)))
            .with_meta("Intrinsic size", format!("{} × {}", size.width(), size.height()))
            .with_meta("Rendered at", format!("{width} × {height}")))
    }
}

/// Scanning the system's fonts takes tens of milliseconds, so it happens once, on
/// the first SVG, and is shared after that.
fn system_fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assembles a streamed image and checks it against `image`'s full decode.
    fn streams_like_full_decode(img: image::DynamicImage, format: ImageFormat) {
        let mut bytes = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut bytes), format).unwrap();
        let src = FileSource::from_bytes("x", bytes.clone());
        let (w, h) = (img.width(), img.height());
        let (pixels, _) = stream(&src, format, w, h).unwrap();
        assert!(matches!(pixels, Pixels::Bands(_)));
        let mut r = Raster { width: w, height: h, pixels };
        r.make_full().unwrap();
        let want = image::load_from_memory_with_format(&bytes, format).unwrap().into_rgba8();
        assert!(r.rgba().unwrap() == want.as_raw().as_slice(), "{format:?} {:?} differs", img.color());
    }

    #[test]
    fn streamed_pngs_match_full_decodes_in_every_colour_type() {
        // Taller than one band at this width (16 MB / (3000 × 4) = 1398 rows), so the
        // bands have to line up.
        let (w, h) = (3000, 3000);
        let rgba = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x % 251) as u8, (y % 241) as u8, ((x ^ y) % 256) as u8, (x % 7 * 36) as u8]));
        let img = image::DynamicImage::ImageRgba8(rgba);
        for i in [img.clone(), img.to_rgb8().into(), img.to_luma8().into(), img.to_luma_alpha8().into(), img.to_rgba16().into(), img.to_luma16().into()] {
            streams_like_full_decode(i, ImageFormat::Png);
        }
        // Not PNG: decoded whole in its own layout, converted per band.
        streams_like_full_decode(img.to_rgb8().into(), ImageFormat::Jpeg);
    }

    #[test]
    fn palette_pngs_expand_when_streamed() {
        let (w, h) = (64u32, 40u32);
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_palette(vec![255, 0, 0, 0, 255, 0, 0, 0, 255]);
            enc.set_trns(vec![255, 128, 0]);
            let mut writer = enc.write_header().unwrap();
            let data: Vec<u8> = (0..w * h).map(|i| (i % 3) as u8).collect();
            writer.write_image_data(&data).unwrap();
        }
        let src = FileSource::from_bytes("p.png", out.clone());
        let (pixels, _) = stream(&src, ImageFormat::Png, w, h).unwrap();
        let mut r = Raster { width: w, height: h, pixels };
        r.make_full().unwrap();
        let rgba = r.rgba().unwrap();
        assert_eq!(&rgba[..12], [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0]);
        assert!(rgba == image::load_from_memory(&out).unwrap().into_rgba8().as_raw().as_slice());
    }

    #[test]
    fn dropping_a_stream_stops_the_decoder() {
        let made = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = made.clone();
        let stream = BandStream::spawn(move |emit| {
            for y in 0..1000 {
                count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if !emit(Band { y, rgba: vec![0; 4] }) {
                    return Ok(());
                }
            }
            Ok(())
        });
        assert!(matches!(stream.wait_next(), viewer_core::Next::Band(_)));
        drop(stream);
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Two may be queued and one in hand when the receiver goes; then it stops.
        assert!(made.load(std::sync::atomic::Ordering::Relaxed) < 10);
    }

    #[test]
    fn decodes_png_and_svg() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let src = FileSource::from_bytes("noext", png);
        assert_eq!(ImageLoader.probe(src.header(), None, ""), Confidence::MAGIC);
        let Content::Raster(r) = ImageLoader.open(&src).unwrap().content else { panic!() };
        assert_eq!((r.width, r.height), (3, 2));

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5"><rect width="10" height="5" fill="blue"/></svg>"#;
        let src = FileSource::from_bytes("a.svg", svg.to_vec());
        let Content::Raster(r) = SvgLoader.open(&src).unwrap().content else { panic!() };
        assert_eq!((r.width, r.height), (20, 10));
        assert_eq!(&r.rgba().unwrap()[..4], &[0, 0, 255, 255]);
    }
}
