//! Paged documents (PDF; later comics, slides, multi-page TIFF): a page list an app
//! lays out from sizes alone and renders a region at a time, into its own buffer
//! (docs/design/libfileparser.md §3, docs/design/pdf.md §5).

use crate::{Cancel, MemoryUse, ParseError, Result};

/// A page's size in points (1/72 inch), already rotated as it is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

impl PageSize {
    pub fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// A region of a page in points, origin at the top left as shown.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct PageRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PageRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    pub fn whole(size: PageSize) -> Self {
        Self { x: 0.0, y: 0.0, width: size.width, height: size.height }
    }
}

/// The largest side of one rendered region, in pixels: a request beyond it is refused
/// rather than allocated (a view renders big pages in tiles).
pub const MAX_RENDER_SIDE: u32 = 8192;

/// The pixel size `region` renders to at `scale` pixels per point, or
/// [`ParseError::InvalidRequest`] if it's empty or bigger than [`MAX_RENDER_SIDE`].
pub fn render_size(scale: f32, region: PageRect) -> Result<(u32, u32)> {
    let w = (region.width * scale).round();
    let h = (region.height * scale).round();
    if !(w >= 1.0 && h >= 1.0) {
        return Err(ParseError::invalid(format!("nothing to render ({w} × {h} px)")));
    }
    if w > MAX_RENDER_SIDE as f32 || h > MAX_RENDER_SIDE as f32 {
        return Err(ParseError::invalid(format!("{w} × {h} px is too big to render at once")));
    }
    Ok((w as u32, h as u32))
}

/// Pages read one at a time. Implementations serialise their own access (a PDF engine
/// is usually single-threaded), so any thread may call; a view renders from one
/// background thread.
pub trait PageSource: Send + Sync {
    fn page_count(&self) -> u64;
    /// Page `index`'s size in points, for layout before anything is rendered. Cheap
    /// after the first call for a page.
    fn page_size(&self, index: u64) -> Result<PageSize>;
    /// Renders `region` of page `index` at `scale` pixels per point into `rgba`:
    /// straight RGBA8, opaque (pages are drawn on white), `width * height * 4` bytes
    /// for the size [`render_size`] gives, resized to fit. May be slow; stops with
    /// [`ParseError::Cancelled`] when `cancel` is set.
    fn render(&self, index: u64, scale: f32, region: PageRect, rgba: &mut Vec<u8>, cancel: &Cancel) -> Result<(u32, u32)>;
    /// The engine's name, for an info panel ("MuPDF", "PDFKit").
    fn engine(&self) -> &str;
    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
}
