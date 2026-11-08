//! PDF (docs/design/pdf.md): recognised always, read by MuPDF with the `mupdf`
//! feature. The pieces here are public so an app's own PDF reader (another engine)
//! detects and shapes documents exactly as this one does.

use std::sync::Arc;

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, Limit, PageSource, ParseContext, Reader, ReaderInfo, Result, Role};

/// What a PDF reader reads, for any engine: id `"pdf"`, so an app's reader replaces
/// this one by id.
pub static INFO: ReaderInfo = ReaderInfo::new("pdf", "PDF", Category::Docs).extensions(&["pdf"]).mime_types(&["application/pdf"]);

/// Whether `header` is a PDF's: `%PDF-` in the first kilobyte (readers accept junk
/// before it).
pub fn is_pdf(header: &[u8]) -> bool {
    header[..header.len().min(1024)].windows(5).any(|w| w == b"%PDF-")
}

/// A PDF reader's detection: certain by signature, else the extension.
pub fn detect(header: &[u8], name: &str) -> Detection {
    if is_pdf(header) {
        Detection::new(Confidence::Magic)
    } else if INFO.matches_extension(name) {
        Detection::new(Confidence::Extension)
    } else {
        Detection::NONE
    }
}

/// A PDF's document from any engine's pages: a Pages facet, the page count, the
/// engine's name, `meta` (version, title, …), then the file's Raw facet. A page count
/// past `cx.limits.pages` is [`ParseError::TooLarge`](libfileparser_core::ParseError::TooLarge).
pub fn document(src: &FileSource, pages: Arc<dyn PageSource>, meta: Vec<(String, String)>, cx: &ParseContext) -> Result<Document> {
    let count = pages.page_count();
    cx.limits.check(Limit::Pages, count)?;
    let engine = pages.engine().to_string();
    let mut doc = Document::new(FormatInfo::new("pdf", "PDF document", Category::Docs))
        .with_facet(Role::Primary, "Pages", Content::Pages(pages))
        .with_meta("Pages", count)
        .with_meta("Rendered by", engine);
    for (k, v) in meta {
        doc = doc.with_meta(k, v);
    }
    Ok(doc.with_file_facets(src, None))
}

/// PDF through MuPDF (`mupdf` feature); without it, PDFs are named and refused.
pub struct PdfReader;

impl Reader for PdfReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        detect(header, name)
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !is_pdf(src.header()) {
            return Err(libfileparser_core::ParseError::NotThisFormat);
        }
        open_mupdf(src, cx)
    }
}

#[cfg(all(feature = "mupdf", not(windows)))]
fn open_mupdf(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let pages = libfileparser_mupdf::MupdfDocument::open(src.clone(), "application/pdf")?;
    let info = pages.info();
    let meta = [
        ("Version", info.format),
        ("Encryption", info.encryption),
        ("Title", info.title),
        ("Author", info.author),
        ("Created with", info.creator),
        ("Producer", info.producer),
    ]
    .into_iter()
    .filter_map(|(k, v)| Some((k.to_string(), v?)))
    .collect();
    document(src, Arc::new(pages), meta, cx)
}

#[cfg(not(all(feature = "mupdf", not(windows))))]
fn open_mupdf(_: &FileSource, _: &ParseContext) -> Result<Document> {
    Err(libfileparser_core::ParseError::unsupported(if cfg!(windows) {
        "PDF isn't available on Windows yet"
    } else {
        "PDF isn't available in this build (libfileparser-docs without its `mupdf` feature)"
    }))
}
