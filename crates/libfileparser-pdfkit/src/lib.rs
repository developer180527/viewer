//! PDF through Apple's PDFKit (docs/design/pdf.md §2): a [`PdfKitReader`] an app
//! registers in place of libfileparser-docs' MuPDF reader (same id, `"pdf"`), and the
//! [`PageSource`] it opens. MIT/Apache, so it can ship where MuPDF's AGPL can't (the
//! App Store). libfileparser-docs bundles only MuPDF; this is wired by the app:
//!
//! ```ignore
//! let lib = libfileparser::Library::new().replace("pdf", &libfileparser_pdfkit::PdfKitReader);
//! ```
//!
//! PDFKit's objects aren't documented as thread-safe, so the document is used only
//! under a mutex, inside an autorelease pool (the render thread has none of its own).

#![cfg(target_vendor = "apple")]

use std::sync::{Arc, Mutex, OnceLock};

use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo};
use objc2_foundation::NSData;
use objc2_pdf_kit::{PDFDisplayBox, PDFDocument, PDFPage};
use libfileparser_core::{Cancel, Detection, Document, FileSource, PageRect, PageSize, PageSource, ParseContext, ParseError, Reader, ReaderInfo, Result, render_size};

/// PDF read by PDFKit: detects and shapes documents as libfileparser-docs' reader does.
pub struct PdfKitReader;

impl Reader for PdfKitReader {
    fn info(&self) -> &ReaderInfo {
        &libfileparser_docs::pdf::INFO
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        libfileparser_docs::pdf::detect(header, name)
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !libfileparser_docs::pdf::is_pdf(src.header()) {
            return Err(ParseError::NotThisFormat);
        }
        let pages = PdfKitDocument::open(src)?;
        let version = pages.version();
        libfileparser_docs::pdf::document(src, Arc::new(pages), vec![("Version".into(), version)], cx)
    }
}

struct Doc(Retained<PDFDocument>);

// SAFETY: the document is only touched under `PdfKitDocument::doc`'s mutex, never
// from two threads at once.
unsafe impl Send for Doc {}

pub struct PdfKitDocument {
    doc: Mutex<Doc>,
    pages: u64,
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
                return Err(ParseError::corrupt("PDFKit couldn't read this file as a PDF"));
            };
            // SAFETY: plain getters.
            if unsafe { doc.isLocked() } {
                return Err(ParseError::unsupported("This document is protected by a password."));
            }
            let pages = unsafe { doc.pageCount() } as u64;
            Ok(Self { doc: Mutex::new(Doc(doc)), pages, sizes: (0..pages).map(|_| OnceLock::new()).collect() })
        })
    }

    /// "PDF 1.7".
    fn version(&self) -> String {
        let doc = self.doc.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain getters.
        let (major, minor) = unsafe { (doc.0.majorVersion(), doc.0.minorVersion()) };
        format!("PDF {major}.{minor}")
    }

    fn page(doc: &PDFDocument, index: u64) -> Result<Retained<PDFPage>> {
        // SAFETY: `index` is checked against the page count by the callers.
        match unsafe { doc.pageAtIndex(index as usize) } {
            Some(p) => Ok(p),
            None => Err(ParseError::corrupt(format!("page {} couldn't be read", index + 1))),
        }
    }

    fn check(&self, index: u64) -> Result<()> {
        if index < self.pages { Ok(()) } else { Err(ParseError::invalid(format!("no page {}", index + 1))) }
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
    fn engine(&self) -> &str {
        "PDFKit"
    }

    fn page_count(&self) -> u64 {
        self.pages
    }

    fn page_size(&self, index: u64) -> Result<PageSize> {
        self.check(index)?;
        self.sizes[index as usize].get_or_init(|| {
            let doc = self.doc.lock().unwrap_or_else(|e| e.into_inner());
            autoreleasepool(|_| {
                let page = Self::page(&doc.0, index).map_err(|e| e.to_string())?;
                let (r, rot) = Self::geometry(&page);
                let (w, h) = (r.size.width as f32, r.size.height as f32);
                Ok(if rot == 90 || rot == 270 { PageSize::new(h, w) } else { PageSize::new(w, h) })
            })
        })
        .clone()
        .map_err(ParseError::corrupt)
    }

    fn render(&self, index: u64, scale: f32, region: PageRect, rgba: &mut Vec<u8>, cancel: &Cancel) -> Result<(u32, u32)> {
        let shown = self.page_size(index)?;
        let (w, h) = render_size(scale, region)?;
        cancel.check()?;
        rgba.clear();
        rgba.resize(w as usize * h as usize * 4, 0);
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
            let Some(ctx) = ctx else { return Err(ParseError::corrupt(format!("couldn't make a {w} × {h} bitmap"))) };
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
        Ok((w, h))
    }
}
