//! Image loaders: everything the `image` crate decodes, plus SVG via resvg.

use std::sync::{Arc, OnceLock};

use anyhow::Context;
use image::ImageFormat;
use viewer_core::{Confidence, Content, Document, FileSource, FormatLoader, Raster, Result};

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
        let img = image::load_from_memory_with_format(src.bytes(), format)?;
        let color = format!("{:?}", img.color());
        let rgba = img.into_rgba8();
        let (width, height) = rgba.dimensions();
        let name = format!("{} image", format.extensions_str()[0].to_ascii_uppercase());
        Ok(Document::new(name, Content::Raster(Raster { width, height, rgba: rgba.into_raw() }))
            .with_meta("Dimensions", format!("{width} × {height}"))
            .with_meta("Color", color))
    }
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
        Ok(Document::new("SVG image", Content::Raster(Raster { width, height, rgba }))
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
        assert_eq!(&r.rgba[..4], &[0, 0, 255, 255]);
    }
}
