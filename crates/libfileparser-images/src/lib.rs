//! Any image into a document (docs/design/libfileparser.md §2): everything the `image`
//! crate decodes (PNG, JPEG, WebP, GIF, TIFF, BMP, PNM, EXR, HDR, DDS, QOI, ICO, TGA,
//! farbfeld), and SVG through resvg.
//!
//! Opening reads only the header: the size, the colour type, and the limits checked
//! against them. The pixels are a [`BandSource`]: decoded when an app asks, a band of
//! rows at a time, on the app's thread, so a document never holds a decoded image and
//! a 16K panorama can stream to a GPU without its whole RGBA copy ever existing.
//! Non-interlaced PNGs decode row by row; other formats decode whole in their own
//! layout and convert to RGBA a band at a time.
//!
//! The API is the shape every libfileparser category has: [`readers`], [`detect`],
//! [`open`] and [`open_with`].

use std::io::Cursor;
use std::sync::{Arc, OnceLock};

use image::{ImageDecoder, ImageFormat};
use libfileparser_core::{
    Band, BandSource, Bands, Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, Limit, MemoryUse, ParseContext,
    ParseError, Raster, Reader, ReaderInfo, Result, Role,
};

/// Every format this category reads.
pub fn readers() -> &'static [&'static dyn Reader] {
    &[&ImageReader, &SvgReader]
}

pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
    libfileparser_core::detect(readers(), header, name)
}

pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open(readers(), src, cx)
}

pub fn open_with(id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open_with(readers(), id, src, cx)
}

/// Rows per band: about 16 MB of RGBA, at least 16 rows.
pub fn band_rows(width: u32, height: u32) -> u32 {
    const BAND_BYTES: usize = 16 << 20;
    ((BAND_BYTES / (width as usize * 4).max(1)) as u32).clamp(16, height.max(16))
}

fn corrupt(e: impl std::fmt::Display) -> ParseError {
    ParseError::corrupt(e)
}

// ---------------------------------------------------------------- raster formats

/// Everything the `image` crate decodes.
pub struct ImageReader;

static IMAGE: ReaderInfo = ReaderInfo::new("image", "Image", Category::Images)
    .extensions(&[
        "png", "apng", "jpg", "jpeg", "jfif", "gif", "webp", "tif", "tiff", "bmp", "ico", "tga", "qoi", "pbm", "pgm", "ppm", "pam", "pnm", "hdr", "exr",
        "dds", "ff",
    ])
    .mime_types(&["image/png", "image/jpeg", "image/gif", "image/webp", "image/tiff", "image/bmp"]);

fn format_of(src: &FileSource) -> Option<ImageFormat> {
    image::guess_format(src.header())
        .ok()
        .or_else(|| src.ext().as_deref().and_then(ImageFormat::from_extension))
        .filter(|f| f.reading_enabled())
}

impl Reader for ImageReader {
    fn info(&self) -> &ReaderInfo {
        &IMAGE
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        if image::guess_format(header).is_ok_and(|f| f.reading_enabled()) {
            Detection::new(Confidence::Magic)
        } else if ext.as_deref().and_then(ImageFormat::from_extension).is_some_and(|f| f.reading_enabled()) {
            Detection::new(Confidence::Extension)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let format = format_of(src).ok_or(ParseError::NotThisFormat)?;
        // The header alone: size and colour type, nothing decoded.
        let decoder = image::ImageReader::with_format(Cursor::new(src.bytes()), format).into_decoder().map_err(corrupt)?;
        let (width, height) = decoder.dimensions();
        let color = format!("{:?}", decoder.color_type());
        cx.limits.check(Limit::ImagePixels, width as u64 * height as u64)?;
        let source = ImageBands { src: src.clone(), format, width, height, max_alloc: cx.limits.allocation };
        Ok(Document::new(FormatInfo::new("image", format_name(format, src.header()), Category::Images))
            .with_facet(Role::Primary, "Image", Content::Raster(Raster::from_bands(width, height, source)))
            .with_meta("Dimensions", format!("{width} × {height}"))
            .with_meta("Color", color)
            .with_file_facets(src, None))
    }
}

/// "PNG image", "PPM image": PNM files say which of their kinds they are in their
/// second byte, where `image` only knows the family.
fn format_name(format: ImageFormat, header: &[u8]) -> String {
    let ext = match (format, header.get(1)) {
        (ImageFormat::Pnm, Some(b'1' | b'4')) => "PBM",
        (ImageFormat::Pnm, Some(b'2' | b'5')) => "PGM",
        (ImageFormat::Pnm, Some(b'3' | b'6')) => "PPM",
        (ImageFormat::Pnm, Some(b'7')) => "PAM",
        _ => return format!("{} image", format.extensions_str()[0].to_ascii_uppercase()),
    };
    format!("{ext} image")
}

/// How to decode an image file: its bytes (mapped, shared) and format.
struct ImageBands {
    src: FileSource,
    format: ImageFormat,
    width: u32,
    height: u32,
    max_alloc: u64,
}

impl ImageBands {
    /// Decoded whole, with `image`'s allocation limit set from the context's, so a
    /// header that lies about the size fails here instead of in the allocator.
    fn decode(&self) -> Result<image::DynamicImage> {
        let mut reader = image::ImageReader::with_format(Cursor::new(self.src.bytes()), self.format);
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(self.max_alloc);
        reader.limits(limits);
        reader.decode().map_err(corrupt)
    }
}

impl BandSource for ImageBands {
    fn bands(&self) -> Result<Bands> {
        if self.format == ImageFormat::Png {
            let mut decoder = png::Decoder::new(Cursor::new(Shared(self.src.shared_bytes())));
            // 16-bit samples stay 16-bit here and are rounded below, as `image` does
            // (STRIP_16 would truncate).
            decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
            let reader = decoder.read_info().map_err(corrupt)?;
            if !reader.info().interlaced {
                return Ok(Box::new(PngBands { reader, width: self.width, height: self.height, y: 0, eight: Vec::new(), failed: None }));
            }
        }
        let img = self.decode()?;
        let rows = band_rows(self.width, self.height);
        Ok(Box::new(CropBands { img, rows, y: 0 }))
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::mapped(self.src.len())
    }
}

/// The file's bytes, kept alive for whoever decodes them.
struct Shared(Arc<dyn AsRef<[u8]> + Send + Sync>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

/// A decoded image handed out a band at a time, each converted to RGBA as it goes.
struct CropBands {
    img: image::DynamicImage,
    rows: u32,
    y: u32,
}

impl Iterator for CropBands {
    type Item = Result<Band>;

    fn next(&mut self) -> Option<Result<Band>> {
        let height = self.img.height();
        if self.y >= height {
            return None;
        }
        let h = self.rows.min(height - self.y);
        let rgba = self.img.crop_imm(0, self.y, self.img.width(), h).into_rgba8().into_raw();
        let band = Band::new(self.y, rgba);
        self.y += h;
        Some(Ok(band))
    }
}

/// A PNG read row by row: never whole. A damaged one gives the rows before the damage,
/// then the error.
struct PngBands {
    reader: png::Reader<Cursor<Shared>>,
    width: u32,
    height: u32,
    y: u32,
    eight: Vec<u8>,
    /// Why the rows stopped, to give after the rows decoded before it.
    failed: Option<ParseError>,
}

impl PngBands {
    fn band(&mut self) -> Result<Band> {
        let (color, depth) = self.reader.output_color_type();
        let h = band_rows(self.width, self.height).min(self.height - self.y);
        let row_bytes = self.width as usize * 4;
        let mut band = Vec::with_capacity(h as usize * row_bytes);
        let mut decoded = 0;
        for _ in 0..h {
            let row = match self.reader.next_row() {
                Ok(Some(row)) => row,
                Ok(None) => {
                    self.failed = Some(corrupt("the PNG ended early"));
                    break;
                }
                Err(e) => {
                    self.failed = Some(corrupt(e));
                    break;
                }
            };
            decoded += 1;
            let data = if depth == png::BitDepth::Sixteen {
                self.eight.clear();
                self.eight.extend(row.data().as_chunks::<2>().0.iter().map(|&[hi, lo]| ((u16::from_be_bytes([hi, lo]) as u32 + 128) / 257) as u8));
                &self.eight[..]
            } else {
                row.data()
            };
            match color {
                png::ColorType::Rgba => band.extend_from_slice(&data[..row_bytes.min(data.len())]),
                png::ColorType::GrayscaleAlpha => band.extend(data.as_chunks::<2>().0.iter().flat_map(|&[l, a]| [l, l, l, a])),
                png::ColorType::Rgb => band.extend(data.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255])),
                png::ColorType::Grayscale => band.extend(data.iter().flat_map(|&l| [l, l, l, 255])),
                png::ColorType::Indexed => return Err(corrupt("palette PNG not expanded")),
            }
        }
        if decoded == 0 {
            return Err(self.failed.take().unwrap_or_else(|| corrupt("the PNG ended early")));
        }
        band.resize(decoded as usize * row_bytes, 0);
        let band = Band::new(self.y, band);
        self.y += decoded;
        Ok(band)
    }
}

impl Iterator for PngBands {
    type Item = Result<Band>;

    fn next(&mut self) -> Option<Result<Band>> {
        if let Some(e) = self.failed.take() {
            self.y = self.height;
            return Some(Err(e));
        }
        if self.y >= self.height {
            return None;
        }
        let band = self.band();
        if band.is_err() {
            // An error ends the bands.
            self.y = self.height;
        }
        Some(band)
    }
}

// ---------------------------------------------------------------- SVG

pub struct SvgReader;

static SVG: ReaderInfo = ReaderInfo::new("svg", "SVG", Category::Images).extensions(&["svg", "svgz"]).mime_types(&["image/svg+xml"]);

/// SVGs are rendered at twice their size for crisp zooming, the longest side capped.
const SVG_MAX_SIDE: f32 = 4096.0;

impl Reader for SvgReader {
    fn info(&self) -> &ReaderInfo {
        &SVG
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if SVG.matches_extension(name) {
            return Detection::new(Confidence::Extension).with_language("xml");
        }
        let head = String::from_utf8_lossy(&header[..header.len().min(1024)]);
        if head.contains("<svg") { Detection::new(Confidence::Content).with_language("xml") } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let tree = parse_svg(src)?;
        let size = tree.size();
        let scale = (2.0f32).min(SVG_MAX_SIDE / size.width().max(size.height()));
        let width = (size.width() * scale).ceil().max(1.0) as u32;
        let height = (size.height() * scale).ceil().max(1.0) as u32;
        cx.limits.check(Limit::ImagePixels, width as u64 * height as u64)?;
        Ok(Document::new(FormatInfo::new("svg", "SVG image", Category::Images))
            .with_facet(Role::Primary, "Image", Content::Raster(Raster::from_bands(width, height, SvgBands { src: src.clone(), scale, width, height })))
            .with_meta("Intrinsic size", format!("{} × {}", size.width(), size.height()))
            .with_meta("Rendered at", format!("{width} × {height}"))
            .with_file_facets(src, Some("xml")))
    }
}

fn parse_svg(src: &FileSource) -> Result<resvg::usvg::Tree> {
    // No resources directory: an SVG reads nothing beside itself.
    let opt = resvg::usvg::Options { fontdb: system_fonts(), ..Default::default() };
    // Parsed on whatever thread asks (Viewer's decoder too): a panic in usvg is damage.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| resvg::usvg::Tree::from_data(src.bytes(), &opt)))
        .map_err(|_| corrupt("the SVG couldn't be read"))?
        .map_err(|e| match e {
            resvg::usvg::Error::NotAnUtf8Str | resvg::usvg::Error::ParsingFailed(_) => ParseError::NotThisFormat,
            other => corrupt(other),
        })
}

/// An SVG rendered when asked for, then handed out in bands.
struct SvgBands {
    src: FileSource,
    scale: f32,
    width: u32,
    height: u32,
}

impl BandSource for SvgBands {
    fn bands(&self) -> Result<Bands> {
        let tree = parse_svg(&self.src)?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(self.width, self.height).ok_or_else(|| corrupt("SVG too large"))?;
        // tiny-skia asserts on some degenerate geometry a damaged file can hold: a
        // drawing that fails is a damaged file, not a crash.
        let scale = self.scale;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut())))
            .map_err(|_| corrupt("the SVG's shapes couldn't be drawn"))?;
        let rgba: Vec<u8> = pixmap
            .pixels()
            .iter()
            .flat_map(|p| {
                let c = p.demultiply();
                [c.red(), c.green(), c.blue(), c.alpha()]
            })
            .collect();
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(self.width, self.height, rgba).ok_or_else(|| corrupt("SVG render size"))?);
        Ok(Box::new(CropBands { img, rows: band_rows(self.width, self.height), y: 0 }))
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::mapped(self.src.len())
    }
}

/// Scanning the system's fonts takes tens of milliseconds, so it happens once, on the
/// first SVG, and is shared after that. (Fonts are the platform's, not files the SVG
/// names: nothing the SVG says is read.)
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
    use libfileparser_core::Pixels;

    fn raster(doc: &Document) -> Raster {
        match doc.facet(0, &ParseContext::new()).unwrap() {
            Content::Raster(r) => r,
            _ => panic!("not a raster"),
        }
    }

    /// Decodes an image band by band and checks it against `image`'s full decode.
    fn bands_like_full_decode(img: image::DynamicImage, format: ImageFormat) {
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), format).unwrap();
        let src = FileSource::from_bytes("x", bytes.clone());
        let doc = ImageReader.open(&src, &ParseContext::new()).unwrap();
        // The colour type named the way `image` names what it decodes.
        let want = image::load_from_memory_with_format(&bytes, format).unwrap();
        assert_eq!(doc.metadata().iter().find(|(k, _)| k == "Color").unwrap().1, format!("{:?}", want.color()));
        let r = raster(&doc);
        assert!(r.rgba().is_none(), "nothing decoded at open");
        let rgba = r.decode().unwrap();
        assert!(rgba == want.into_rgba8().into_raw(), "{format:?} {:?} differs", img.color());
    }

    #[test]
    fn bands_match_full_decodes_in_every_colour_type() {
        // Taller than one band at this width (16 MB / (3000 × 4) = 1398 rows), so the
        // bands have to line up.
        let (w, h) = (3000, 3000);
        let rgba = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x % 251) as u8, (y % 241) as u8, ((x ^ y) % 256) as u8, (x % 7 * 36) as u8]));
        let img = image::DynamicImage::ImageRgba8(rgba);
        for i in [img.clone(), img.to_rgb8().into(), img.to_luma8().into(), img.to_luma_alpha8().into(), img.to_rgba16().into(), img.to_luma16().into()] {
            bands_like_full_decode(i, ImageFormat::Png);
        }
        // Not PNG: decoded whole in its own layout, converted per band.
        bands_like_full_decode(img.to_rgb8().into(), ImageFormat::Jpeg);
    }

    #[test]
    fn palette_pngs_expand() {
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
        let doc = ImageReader.open(&FileSource::from_bytes("p.png", out.clone()), &ParseContext::new()).unwrap();
        let rgba = raster(&doc).decode().unwrap();
        assert_eq!(&rgba[..12], [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0]);
        assert!(rgba == image::load_from_memory(&out).unwrap().into_rgba8().into_raw());
    }

    #[test]
    fn decodes_png_and_svg() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255])).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        let src = FileSource::from_bytes("noext", png);
        assert_eq!(ImageReader.detect(src.header(), "").confidence, Confidence::Magic);
        let r = raster(&ImageReader.open(&src, &ParseContext::new()).unwrap());
        assert_eq!((r.width(), r.height()), (3, 2));

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5"><rect width="10" height="5" fill="blue"/></svg>"#;
        let r = raster(&SvgReader.open(&FileSource::from_bytes("a.svg", svg.to_vec()), &ParseContext::new()).unwrap());
        assert_eq!((r.width(), r.height()), (20, 10));
        assert_eq!(&r.decode().unwrap()[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn a_truncated_png_fails_when_decoded() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(64, 64, image::Rgba([9, 9, 9, 255])).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        png.truncate(png.len() / 2);
        // The header is whole, so it opens; its pixels aren't.
        let doc = ImageReader.open(&FileSource::from_bytes("t.png", png), &ParseContext::new()).unwrap();
        let r = raster(&doc);
        assert!(r.decode().is_err());
        // The rows before the damage still come, then the error.
        let Pixels::Bands(source) = r.pixels() else { panic!() };
        let bands: Vec<_> = source.bands().unwrap().collect();
        assert!(bands.len() >= 2 && bands[0].as_ref().is_ok_and(|b| !b.rgba.is_empty()), "{:?}", bands.iter().map(|b| b.is_ok()).collect::<Vec<_>>());
        assert!(bands.last().unwrap().is_err());
    }
}
