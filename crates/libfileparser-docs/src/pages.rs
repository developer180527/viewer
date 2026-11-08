//! Paged documents in general: a document around any [`PageSource`], and resampling
//! a decoded page image into the region and scale a view asks for.

use std::sync::Arc;

use libfileparser_core::{Category, Content, Document, FileSource, FormatInfo, Limit, PageRect, PageSource, ParseContext, Result, Role};

/// A paged document: a Pages facet, the page count, the engine, `meta`, then the
/// file's Raw facet. A page count past `cx.limits.pages` is `TooLarge`.
pub(crate) fn document(id: &'static str, name: &str, src: &FileSource, pages: Arc<dyn PageSource>, meta: Vec<(String, String)>, cx: &ParseContext) -> Result<Document> {
    let count = pages.page_count();
    cx.limits.check(Limit::Pages, count)?;
    let engine = pages.engine().to_string();
    let mut doc = Document::new(FormatInfo::new(id, name, Category::Docs))
        .with_facet(Role::Primary, "Pages", Content::Pages(pages))
        .with_meta("Pages", count)
        .with_meta("Rendered by", engine);
    for (k, v) in meta {
        doc = doc.with_meta(k, v);
    }
    Ok(doc.with_file_facets(src, None))
}

/// A decoded image: straight RGBA8.
pub(crate) struct Image<'a> {
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// `region` (in source pixels, which comics count as points) of an RGBA image
/// `w` × `h`, at `scale`, into `out` (`ow` × `oh`): bilinear, averaging over the
/// source pixels each output pixel covers when shrinking, so text stays legible.
pub(crate) fn resample(image: Image, region: PageRect, scale: f32, (ow, oh): (u32, u32), out: &mut Vec<u8>) {
    let Image { rgba, width: w, height: h } = image;
    out.clear();
    out.resize(ow as usize * oh as usize * 4, 255);
    if w == 0 || h == 0 {
        return;
    }
    let px = |x: i64, y: i64| -> [f32; 4] {
        let (x, y) = (x.clamp(0, w as i64 - 1) as usize, y.clamp(0, h as i64 - 1) as usize);
        let i = (y * w as usize + x) * 4;
        [rgba[i] as f32, rgba[i + 1] as f32, rgba[i + 2] as f32, rgba[i + 3] as f32]
    };
    let bilinear = |sx: f32, sy: f32| -> [f32; 4] {
        let (x0, y0) = ((sx - 0.5).floor(), (sy - 0.5).floor());
        let (fx, fy) = (sx - 0.5 - x0, sy - 0.5 - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, b, c, d) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
        std::array::from_fn(|k| (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy)
    };
    // How many samples per output pixel, per side: about one per source pixel covered.
    let n = ((1.0 / scale).ceil() as usize).clamp(1, 8);
    for oy in 0..oh {
        for ox in 0..ow {
            let mut acc = [0f32; 4];
            for j in 0..n {
                for i in 0..n {
                    let sx = region.x + (ox as f32 + (i as f32 + 0.5) / n as f32) / scale;
                    let sy = region.y + (oy as f32 + (j as f32 + 0.5) / n as f32) / scale;
                    let p = bilinear(sx, sy);
                    for k in 0..4 {
                        acc[k] += p[k];
                    }
                }
            }
            let o = (oy as usize * ow as usize + ox as usize) * 4;
            let count = (n * n) as f32;
            // On white, as pages are drawn: straight alpha composited, then opaque.
            let a = acc[3] / count / 255.0;
            for k in 0..3 {
                out[o + k] = (acc[k] / count * a + 255.0 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
            }
            out[o + 3] = 255;
        }
    }
}
