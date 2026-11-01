//! Paged documents (PDF first; later comics, multi-page TIFF): a page list the view
//! lays out from sizes alone and renders a region at a time (docs/pdf.md §5).

use crate::Result;

/// A page's size in points (1/72 inch), already rotated as it is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

/// A region of a page in points, origin at the top left as shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PageRect {
    pub fn whole(size: PageSize) -> Self {
        Self { x: 0.0, y: 0.0, width: size.width, height: size.height }
    }
}

/// A rendered region: straight RGBA8, `width * height * 4` bytes, opaque (pages are
/// drawn on white).
pub struct PageImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One backend's open document (MuPDF, PDFKit, …). Implementations serialise their
/// own access, so any thread may call; the view calls from one render thread.
pub trait PageSource: Send + Sync {
    /// The backend's name, for the info panel and tests ("MuPDF", "PDFKit").
    fn backend(&self) -> &'static str;
    fn page_count(&self) -> usize;
    /// Page size in points, for layout before anything is rendered. Cheap after the
    /// first call for a page.
    fn page_size(&self, index: usize) -> Result<PageSize>;
    /// Renders `region` of page `index` at `scale` pixels per point. May be slow.
    fn render(&self, index: usize, scale: f32, region: PageRect) -> Result<PageImage>;
}

/// The largest side of one rendered region, in pixels: a request beyond it is refused
/// rather than allocated (the view renders big pages in tiles).
pub const MAX_RENDER_SIDE: u32 = 8192;

/// The pixel size `region` renders to at `scale`, or an error if it's empty or too big.
pub fn render_size(scale: f32, region: PageRect) -> Result<(u32, u32)> {
    let w = (region.width * scale).round();
    let h = (region.height * scale).round();
    if !(w >= 1.0 && h >= 1.0) {
        anyhow::bail!("nothing to render ({w} × {h} px)");
    }
    if w > MAX_RENDER_SIDE as f32 || h > MAX_RENDER_SIDE as f32 {
        anyhow::bail!("{w} × {h} px is too big to render at once");
    }
    Ok((w as u32, h as u32))
}

/// Small PDFs written by hand, for backends' tests.
#[doc(hidden)]
pub mod testing {
    /// A PDF with one page per `(width, height, rotate)`, each with its top-left
    /// quarter (as the page is defined, before `/Rotate`) filled red.
    pub fn pdf(pages: &[(f32, f32, u32)]) -> Vec<u8> {
        let n = pages.len();
        // Objects: 1 catalog, 2 page tree, then a page and its contents per page.
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Count {n} /Kids [{}] >>", (0..n).map(|i| format!("{} 0 R", 3 + i * 2)).collect::<Vec<_>>().join(" ")),
        ];
        for (i, &(w, h, rot)) in pages.iter().enumerate() {
            let content = format!("1 0 0 rg 0 {} {} {} re f", h / 2.0, w / 2.0, h / 2.0);
            objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Rotate {rot} /Contents {} 0 R >>", 4 + i * 2));
            objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).bytes());
        out
    }

    /// The pixel at (x, y) of an RGBA image `width` wide.
    pub fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = (y * width + x) as usize * 4;
        rgba[i..i + 4].try_into().unwrap()
    }
}
