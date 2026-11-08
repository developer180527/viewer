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

mod build;
mod comics;
mod docx;
mod epub;
mod html;
mod lite;
mod markdown;
mod odt;
mod org;
mod package;
mod pages;
pub mod pdf;
mod rtf;
mod xps;

use libfileparser_core::{Bytes, Detection, Document, FileSource, Limit, ParseContext, Reader, Result};

pub use comics::ComicReader;
pub use docx::DocxReader;
pub use epub::EpubReader;
pub use markdown::MarkdownReader;
pub use odt::OdtReader;
pub use org::OrgReader;
pub use rtf::RtfReader;
pub use pdf::PdfReader;
pub use xps::XpsReader;

/// Every format this category reads, in the order ties are broken.
pub fn readers() -> &'static [&'static dyn Reader] {
    &[&PdfReader, &XpsReader, &ComicReader, &EpubReader, &DocxReader, &OdtReader, &RtfReader, &OrgReader, &MarkdownReader]
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

/// All of `src`'s bytes, for formats parsed whole: at most `cx.limits.allocation`.
pub(crate) fn whole(src: &FileSource, cx: &ParseContext) -> Result<Bytes> {
    cx.limits.check(Limit::Allocation, src.len())?;
    Ok(src.range(0, src.len() as usize))
}
