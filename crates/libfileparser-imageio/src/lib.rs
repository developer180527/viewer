//! HEIF photos (HEIC, the iPhone's camera format) and AVIF images through Apple's
//! ImageIO: a [`HeifReader`] an app registers on Apple platforms, where the decoder
//! comes with the system. ImageIO assembles a photo's grid of tiles, turns it upright
//! as its orientation says, and converts its colour (Display P3, HDR) to sRGB.
//!
//! ```ignore
//! let lib = libfileparser::Library::new().reader(&libfileparser_imageio::HeifReader);
//! ```
//!
//! Elsewhere the crate is empty: these files fall to the hex view, never to the media
//! reader, which declines them (an MP4 container, but a photo).

#![cfg(target_vendor = "apple")]

use libfileparser_core::{
    Band, Bands, BandSource, Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, Limit, MemoryUse, ParseContext, ParseError, Raster,
    Reader, ReaderInfo, Result, Role, heif_brand,
};
use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, kCGColorSpaceSRGB};
use objc2_image_io::{
    CGImageSource, kCGImagePropertyOrientation, kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth, kCGImageSourceCreateThumbnailFromImageAlways,
    kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceShouldCache, kCGImageSourceThumbnailMaxPixelSize,
};

pub static INFO: ReaderInfo = ReaderInfo::new("heif", "HEIF or AVIF image", Category::Images)
    .extensions(&["heic", "heics", "heif", "heifs", "hif", "avif"])
    .mime_types(&["image/heic", "image/heif", "image/avif"]);

/// HEIC, HEIF and AVIF stills, decoded by ImageIO.
pub struct HeifReader;

impl Reader for HeifReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if heif_brand(header).is_some() {
            Detection::new(Confidence::Magic)
        } else if header.is_empty() && INFO.matches_extension(name) {
            Detection::new(Confidence::Extension)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let brand = heif_brand(src.header()).ok_or(ParseError::NotThisFormat)?;
        // The header alone: ImageIO reads sizes without decoding a pixel.
        let (width, height, orientation) = size(src.bytes())?;
        cx.limits.check(Limit::ImagePixels, width as u64 * height as u64)?;
        let kind = if brand == "avif" { "AVIF image" } else { "HEIC image" };
        let mut doc = Document::new(FormatInfo::new("heif", kind, Category::Images))
            .with_facet(Role::Primary, "Image", Content::Raster(Raster::from_bands(width, height, HeifBands { src: src.clone(), width, height })))
            .with_meta("Dimensions", format!("{width} × {height}"));
        if orientation > 1 {
            doc = doc.with_meta("Orientation", ORIENTATIONS.get(orientation as usize).copied().unwrap_or("other"));
        }
        Ok(doc.with_file_facets(src, None))
    }
}

/// EXIF's orientations, by number: how the stored image is turned to be shown.
const ORIENTATIONS: [&str; 9] =
    ["", "upright", "mirrored", "turned 180°", "mirrored, turned 180°", "mirrored, turned 90° left", "turned 90° right", "mirrored, turned 90° right", "turned 90° left"];

/// The image source for `bytes` (copied: ImageIO keeps no reference into our mapping),
/// told not to keep decoded pixels of its own.
fn source(bytes: &[u8]) -> Result<CFRetained<CGImageSource>> {
    let data = CFData::from_bytes(bytes);
    let options = dict(&[(unsafe { kCGImageSourceShouldCache }, CFBoolean::new(false).as_ref())]);
    // SAFETY: the options are CFString keys to CF values, as ImageIO expects.
    unsafe { CGImageSource::with_data(&data, Some(options.as_opaque())) }.ok_or_else(|| ParseError::corrupt("ImageIO couldn't read this image"))
}

/// The image as shown: width and height once turned upright, and the orientation.
fn size(bytes: &[u8]) -> Result<(u32, u32, u32)> {
    let src = source(bytes)?;
    // SAFETY: plain getters; index 0 is checked by the count.
    if unsafe { src.count() } == 0 {
        return Err(ParseError::corrupt("this file holds no image ImageIO can read"));
    }
    let index = unsafe { src.primary_image_index() };
    let props = unsafe { src.properties_at_index(index, None) }.ok_or_else(|| ParseError::corrupt("ImageIO found no image properties"))?;
    // SAFETY: ImageIO's property dictionaries map CFString keys to CF values.
    let props: &CFDictionary<CFString, CFType> = unsafe { props.cast_unchecked() };
    let number = |key: &CFString| props.get(key).and_then(|v| v.downcast::<CFNumber>().ok()).and_then(|n| n.as_i64());
    let (w, h) = (number(unsafe { kCGImagePropertyPixelWidth }), number(unsafe { kCGImagePropertyPixelHeight }));
    let (Some(w @ 1..=0xFFFF_FFFF), Some(h @ 1..=0xFFFF_FFFF)) = (w, h) else {
        return Err(ParseError::corrupt("ImageIO found no image size"));
    };
    let orientation = number(unsafe { kCGImagePropertyOrientation }).unwrap_or(1).clamp(1, 8) as u32;
    // Orientations 5–8 turn the image a quarter turn.
    Ok(if orientation >= 5 { (h as u32, w as u32, orientation) } else { (w as u32, h as u32, orientation) })
}

fn dict(pairs: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let (keys, values): (Vec<&CFString>, Vec<&CFType>) = pairs.iter().copied().unzip();
    CFDictionary::from_slices(&keys, &values)
}

/// A HEIF image, decoded when shown and handed out a band at a time.
struct HeifBands {
    src: FileSource,
    width: u32,
    height: u32,
}

impl BandSource for HeifBands {
    fn bands(&self) -> Result<Bands> {
        let image = decode(self.src.bytes(), self.width.max(self.height))?;
        // The decoded image is ImageIO's; ours are its bands, drawn one at a time.
        let (w, h) = (CGImage::width(Some(&image)) as u32, CGImage::height(Some(&image)) as u32);
        if (w, h) != (self.width, self.height) {
            return Err(ParseError::corrupt(format!("ImageIO decoded {w} × {h}, not the {} × {} the file says", self.width, self.height)));
        }
        let rows = ((16usize << 20) / (w as usize * 4)).clamp(16, h as usize) as u32;
        Ok(Box::new(Drawn { image: Image(image), width: w, height: h, rows, y: 0 }))
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::mapped(self.src.len())
    }
}

/// The whole image, upright, at its full size.
fn decode(bytes: &[u8], side: u32) -> Result<CFRetained<CGImage>> {
    let src = source(bytes)?;
    let side = CFNumber::new_i64(side as i64);
    let yes = CFBoolean::new(true);
    // A "thumbnail" as large as the image: the one call that applies the orientation.
    let options = dict(&[
        (unsafe { kCGImageSourceCreateThumbnailFromImageAlways }, yes.as_ref()),
        (unsafe { kCGImageSourceCreateThumbnailWithTransform }, yes.as_ref()),
        (unsafe { kCGImageSourceThumbnailMaxPixelSize }, side.as_ref()),
        (unsafe { kCGImageSourceShouldCache }, CFBoolean::new(false).as_ref()),
    ]);
    // SAFETY: valid source and options; the index is ImageIO's own.
    let index = unsafe { src.primary_image_index() };
    unsafe { src.thumbnail_at_index(index, Some(options.as_opaque())) }.ok_or_else(|| ParseError::corrupt("ImageIO couldn't decode this image"))
}

struct Image(CFRetained<CGImage>);

// SAFETY: CGImage is immutable and documented as thread-safe.
unsafe impl Send for Image {}

/// Bands drawn from the decoded image into sRGB RGBA, top first.
struct Drawn {
    image: Image,
    width: u32,
    height: u32,
    rows: u32,
    y: u32,
}

impl Iterator for Drawn {
    type Item = Result<Band>;

    fn next(&mut self) -> Option<Result<Band>> {
        if self.y >= self.height {
            return None;
        }
        let rows = self.rows.min(self.height - self.y);
        let band = draw(&self.image.0, self.width, self.height, self.y, rows).map(|rgba| Band::new(self.y, rgba));
        self.y += rows;
        Some(band)
    }
}

/// Rows `y..y + rows` of `image`, as straight RGBA8 in sRGB.
fn draw(image: &CGImage, width: u32, height: u32, y: u32, rows: u32) -> Result<Vec<u8>> {
    let (w, h) = (width as usize, rows as usize);
    let mut rgba = vec![0u8; w * h * 4];
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
    // SAFETY: `rgba` is w * h * 4 bytes and outlives the context, dropped below.
    let ctx = unsafe {
        CGBitmapContextCreate(rgba.as_mut_ptr().cast(), w, h, 8, w * 4, space.as_deref(), CGImageAlphaInfo::PremultipliedLast.0)
    };
    let Some(ctx) = ctx else { return Err(ParseError::corrupt(format!("couldn't make a {w} × {h} bitmap"))) };
    // CG's origin is the bottom left: the band's rows y..y+rows from the top sit at
    // the bottom of a context `rows` high when the image is drawn from
    // −(height − y − rows).
    let below = height as f64 - y as f64 - rows as f64;
    CGContext::draw_image(Some(&ctx), CGRect::new(CGPoint::new(0.0, -below), CGSize::new(width as f64, height as f64)), Some(image));
    drop(ctx);
    unpremultiply(&mut rgba);
    Ok(rgba)
}

/// CG draws premultiplied; a raster's alpha is straight.
fn unpremultiply(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}
