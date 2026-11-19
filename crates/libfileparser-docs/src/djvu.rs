//! DjVu (scanned books and papers) as pages, decoded and rendered by `djvu-rs` (pure
//! Rust: IW44 wavelets, JB2 masks, BZZ). A page is its scan at the resolution it was
//! made, in points; any region renders at any scale. The pages' hidden text layer
//! (OCR, where it was made) is a second reading, as text.

use std::sync::Arc;

use djvu_rs::djvu_render::{RenderOptions, RenderRect, RenderRequest};
use djvu_rs::{DjVuDocument, Rotation};
use libfileparser_core::{Block, Cancel, Category, Confidence, Content, Detection, Document, FileSource, Inline, MemoryUse, OwnedFlow, PageRect, PageSize, PageSource, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, render_size};

static INFO: ReaderInfo = ReaderInfo::new("djvu", "DjVu", Category::Docs).extensions(&["djvu", "djv"]).mime_types(&["image/vnd.djvu", "image/x-djvu"]);

pub struct DjvuReader;

/// `AT&TFORM`, then a single page (`DJVU`) or a bundle (`DJVM`).
fn is_djvu(header: &[u8]) -> bool {
    header.starts_with(b"AT&TFORM") && matches!(header.get(12..16), Some(b"DJVU" | b"DJVM"))
}

impl Reader for DjvuReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    fn detect(&self, header: &[u8], _name: &str) -> Detection {
        if is_djvu(header) { Detection::new(Confidence::Certain) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !is_djvu(src.header()) {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        let doc = DjVuDocument::parse(&bytes).map_err(|e| ParseError::corrupt(format!("not a readable DjVu file: {e}")))?;
        let count = doc.page_count();
        if count == 0 {
            return Err(ParseError::corrupt("a DjVu file with no pages"));
        }
        // The text layer, page by page, where there is one.
        let mut blocks = Vec::new();
        for i in 0..count {
            if i % 16 == 0 {
                cx.cancel.check()?;
            }
            let Ok(page) = doc.page(i) else { continue };
            if let Ok(Some(text)) = page.text() {
                let text = text.trim();
                if !text.is_empty() {
                    blocks.push(Block::Heading { level: 2, text: vec![Inline::Text(format!("Page {}", i + 1))] });
                    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
                        blocks.push(Block::Paragraph(vec![Inline::Text(para.split_whitespace().collect::<Vec<_>>().join(" "))]));
                    }
                }
            }
        }
        let memory = bytes.len() as u64;
        let pages: Arc<dyn PageSource> = Arc::new(Djvu { doc, memory });
        let mut document = crate::pages::document("djvu", "DjVu document", src, pages, Vec::new(), cx)?;
        if !blocks.is_empty() {
            document = document.with_facet(Role::Structure, "Text layer", Content::Flow(Arc::new(OwnedFlow::new(blocks))));
        }
        Ok(document)
    }
}

struct Djvu {
    doc: DjVuDocument,
    memory: u64,
}

fn quarter_turned(r: Rotation) -> bool {
    matches!(r, Rotation::Cw90 | Rotation::Ccw90)
}

impl Djvu {
    fn page(&self, index: u64) -> Result<&djvu_rs::DjVuPage> {
        self.doc.page(index as usize).map_err(|e| ParseError::invalid(format!("page {}: {e}", index + 1)))
    }
}

impl PageSource for Djvu {
    fn page_count(&self) -> u64 {
        self.doc.page_count() as u64
    }

    /// The scan's size at its resolution, in points; turned as the page says.
    fn page_size(&self, index: u64) -> Result<PageSize> {
        let p = self.page(index)?;
        let dpi = p.dpi().max(1) as f32;
        let (w, h) = (p.width() as f32 * 72.0 / dpi, p.height() as f32 * 72.0 / dpi);
        Ok(if quarter_turned(p.rotation()) { PageSize::new(h, w) } else { PageSize::new(w, h) })
    }

    fn render(&self, index: u64, scale: f32, region: PageRect, rgba: &mut Vec<u8>, cancel: &Cancel) -> Result<(u32, u32)> {
        let (w, h) = render_size(scale, region)?;
        cancel.check()?;
        let p = self.page(index)?;
        let dpi = p.dpi().max(1) as f32;
        // The whole page's size at this scale, in its own (unturned) orientation;
        // the region is in the turned page's coordinates.
        let full = |px: u16| ((px as f32 * 72.0 / dpi * scale).round() as u32).max(1);
        let opts = RenderOptions { width: full(p.width()), height: full(p.height()), permissive: true, ..RenderOptions::default() };
        let rect = RenderRect { x: (region.x * scale).round().max(0.0) as u32, y: (region.y * scale).round().max(0.0) as u32, width: w, height: h };
        let pm = RenderRequest::new(opts).region(rect).pixmap(p).map_err(|e| ParseError::corrupt(format!("page {}: {e}", index + 1)))?;
        cancel.check()?;
        if (pm.width, pm.height) == (w, h) {
            // On white, opaque, as pages are drawn.
            rgba.clear();
            rgba.extend(pm.data.chunks_exact(4).flat_map(|px| {
                let a = px[3] as u32;
                let over = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
                [over(px[0]), over(px[1]), over(px[2]), 255]
            }));
        } else {
            let all = PageRect::new(0.0, 0.0, pm.width as f32, pm.height as f32);
            crate::pages::resample(crate::pages::Image { rgba: &pm.data, width: pm.width, height: pm.height }, all, w as f32 / pm.width.max(1) as f32, (w, h), rgba);
        }
        Ok((w, h))
    }

    fn engine(&self) -> &str {
        "djvu-rs"
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_and_refusals() {
        assert_eq!(DjvuReader.detect(b"AT&TFORM\0\0\0\x10DJVUINFO", "x").confidence, Confidence::Certain);
        assert_eq!(DjvuReader.detect(b"AT&TFORM\0\0\0\x10AIFF", "x.djvu"), Detection::NONE);
        let broken = FileSource::from_bytes("b.djvu", b"AT&TFORM\0\0\0\x10DJVUINFO\0\0\0\x02xx".to_vec());
        assert!(DjvuReader.open(&broken, &ParseContext::new()).is_err());
    }
}

