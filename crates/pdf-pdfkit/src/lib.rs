//! PDF pages through Apple's PDFKit (docs/design/pdf.md §2), behind
//! `viewer_core::PageSource`. The PDF backend on iOS, where MuPDF can't ship; on
//! macOS it builds too, for comparing the two.
//!
//! PDFKit's objects aren't documented as thread-safe, so the document is used only
//! under a mutex, inside an autorelease pool (the render thread has none of its own).

#![cfg(target_vendor = "apple")]

use std::sync::{Mutex, OnceLock};

use anyhow::bail;
use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo};
use objc2_foundation::NSData;
use objc2_pdf_kit::{PDFDisplayBox, PDFDocument, PDFPage};
use viewer_core::{FileSource, PageImage, PageRect, PageSize, PageSource, Result, render_size};

struct Doc(Retained<PDFDocument>);

// SAFETY: the document is only touched under `PdfKitDocument::doc`'s mutex, never
// from two threads at once.
unsafe impl Send for Doc {}

pub struct PdfKitDocument {
    doc: Mutex<Doc>,
    pages: usize,
    sizes: Vec<OnceLock<std::result::Result<PageSize, String>>>,
}

/// The box pages are shown and measured by, as viewers do.
const BOX: PDFDisplayBox = PDFDisplayBox::CropBox;

impl PdfKitDocument {
    pub fn open(source: &FileSource) -> Result<Self> {
        autoreleasepool(|_| {
            // PDFKit wants an NSData; copying keeps it independent of our mapping.
            let data = NSData::with_bytes(source.bytes());
            // SAFETY: a plain initialiser on a fresh allocation.
            let Some(doc) = (unsafe { PDFDocument::initWithData(PDFDocument::alloc(), &data) }) else {
                bail!("PDFKit couldn't read this file as a PDF");
            };
            // SAFETY: plain getters.
            if unsafe { doc.isLocked() } {
                bail!("This document is protected by a password.");
            }
            let pages = unsafe { doc.pageCount() };
            Ok(Self { doc: Mutex::new(Doc(doc)), pages, sizes: (0..pages).map(|_| OnceLock::new()).collect() })
        })
    }

    fn page(doc: &PDFDocument, index: usize) -> Result<Retained<PDFPage>> {
        // SAFETY: `index` is checked against the page count by the callers.
        match unsafe { doc.pageAtIndex(index) } {
            Some(p) => Ok(p),
            None => bail!("page {} couldn't be read", index + 1),
        }
    }

    /// The page's box and rotation, in points.
    fn geometry(page: &PDFPage) -> (CGRect, i64) {
        // SAFETY: plain getters.
        let rect = unsafe { page.boundsForBox(BOX) };
        let rot = (unsafe { page.rotation() } as i64).rem_euclid(360);
        (rect, rot)
    }
}

impl PageSource for PdfKitDocument {
    fn backend(&self) -> &'static str {
        "PDFKit"
    }

    fn page_count(&self) -> usize {
        self.pages
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        let Some(slot) = self.sizes.get(index) else { bail!("no page {}", index + 1) };
        slot.get_or_init(|| {
            let doc = self.doc.lock().unwrap_or_else(|e| e.into_inner());
            autoreleasepool(|_| {
                let page = Self::page(&doc.0, index).map_err(|e| e.to_string())?;
                let (r, rot) = Self::geometry(&page);
                let (w, h) = (r.size.width as f32, r.size.height as f32);
                Ok(if rot == 90 || rot == 270 { PageSize { width: h, height: w } } else { PageSize { width: w, height: h } })
            })
        })
        .clone()
        .map_err(anyhow::Error::msg)
    }

    fn render(&self, index: usize, scale: f32, region: PageRect) -> Result<PageImage> {
        let shown = self.page_size(index)?;
        let (w, h) = render_size(scale, region)?;
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        let doc = self.doc.lock().unwrap_or_else(|e| e.into_inner());
        autoreleasepool(|_| {
            let page = Self::page(&doc.0, index)?;
            let space = CGColorSpace::new_device_rgb();
            // SAFETY: `rgba` is w * h * 4 bytes and outlives the context, which is
            // dropped at the end of this closure.
            let ctx = unsafe {
                CGBitmapContextCreate(
                    rgba.as_mut_ptr().cast(),
                    w as usize,
                    h as usize,
                    8,
                    w as usize * 4,
                    space.as_deref(),
                    CGImageAlphaInfo::PremultipliedLast.0,
                )
            };
            let Some(ctx) = ctx else { bail!("couldn't make a {w} × {h} bitmap") };
            let s = scale as f64;
            CGContext::set_rgb_fill_color(Some(&ctx), 1.0, 1.0, 1.0, 1.0);
            CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64)));
            // CG's origin is the bitmap's bottom left, and PDFKit draws the page as
            // shown (rotated, box at the origin) with y up. A shown point (px, py) with
            // y down lands at x = s·(px − x), y = h − s·(py − y); with py = H − y_up
            // that is translate(−s·x, h − s·(H − y)) then scale(s).
            CGContext::translate_ctm(Some(&ctx), -s * region.x as f64, h as f64 - s * (shown.height - region.y) as f64);
            CGContext::scale_ctm(Some(&ctx), s, s);
            // SAFETY: a valid page drawing into a valid context.
            unsafe { page.drawWithBox_toContext(BOX, &ctx) };
            drop(ctx);
            Ok(())
        })?;
        Ok(PageImage { width: w, height: h, rgba })
    }
}
