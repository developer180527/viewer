//! Any document into pages or formatted text (docs/design/libfileparser.md §2): PDF
//! as pages, Markdown as formatted text over its source. Comics, EPUB, DOCX and
//! slides come later.
//!
//! **PDF is MuPDF, and only MuPDF.** With the `mupdf` feature (AGPL), PDFs open as
//! pages rendered by MuPDF built from source. Without it, a PDF is still recognised and
//! named, and refused with [`ParseError::Unsupported`]. Any other engine is the app's
//! to wire in: it implements [`Reader`] with id `"pdf"` (using [`pdf::document`] so its
//! documents look the same) and registers it, as `libfileparser-pdfkit` does for
//! Apple's PDFKit.
//!
//! The API is the shape every libfileparser category has: [`readers`], [`detect`],
//! [`open`] and [`open_with`].

mod markdown;
pub mod pdf;

use libfileparser_core::{Detection, Document, FileSource, ParseContext, Reader, Result};

pub use markdown::MarkdownReader;
pub use pdf::PdfReader;

/// Every format this category reads, in the order ties are broken.
pub fn readers() -> &'static [&'static dyn Reader] {
    &[&PdfReader, &MarkdownReader]
}

/// The readers that fit a file, best first.
pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
    libfileparser_core::detect(readers(), header, name)
}

/// Opens `src` with the best reader that succeeds.
pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open(readers(), src, cx)
}

/// Opens `src` with the reader whose id is `id`.
pub fn open_with(id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open_with(readers(), id, src, cx)
}
