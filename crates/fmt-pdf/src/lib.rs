//! PDF as pages (docs/design/pdf.md). Opening reads the page count only; the view renders.
//!
//! The backend is fixed by the build: MuPDF everywhere but iOS, PDFKit on iOS. On
//! macOS `VIEWER_PDF_BACKEND=pdfkit` switches to PDFKit, for comparing the two.

use std::sync::Arc;

use viewer_core::{Confidence, Content, Document, FileSource, FormatLoader, PageSource, Result};

pub static LOADERS: &[&dyn FormatLoader] = &[&PdfLoader];

pub struct PdfLoader;

/// Whether `header` holds a PDF signature. Readers accept `%PDF-` anywhere in the
/// first kilobyte (some files have junk before it).
pub fn is_pdf(header: &[u8]) -> bool {
    header[..header.len().min(1024)].windows(5).any(|w| w == b"%PDF-")
}

impl FormatLoader for PdfLoader {
    fn id(&self) -> &'static str {
        "pdf"
    }

    fn name(&self) -> &'static str {
        "PDF document"
    }

    fn probe(&self, header: &[u8], ext: Option<&str>, _: &str) -> Confidence {
        match ext {
            _ if is_pdf(header) => Confidence::MAGIC,
            Some("pdf") => Confidence::EXTENSION,
            _ => Confidence::NONE,
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let (pages, meta) = open_backend(src)?;
        let mut doc = Document::new("PDF document", Content::Pages(pages.clone()))
            .with_meta("Pages", pages.page_count())
            .with_meta("Renderer", pages.backend());
        doc.metadata.extend(meta);
        Ok(doc)
    }
}

type Meta = Vec<(String, String)>;

#[cfg(not(any(target_os = "ios", windows)))]
fn open_backend(src: &FileSource) -> Result<(Arc<dyn PageSource>, Meta)> {
    #[cfg(target_vendor = "apple")]
    if std::env::var("VIEWER_PDF_BACKEND").is_ok_and(|b| b.eq_ignore_ascii_case("pdfkit")) {
        return Ok((Arc::new(viewer_pdf_pdfkit::PdfKitDocument::open(src)?), Vec::new()));
    }
    let doc = viewer_pdf_mupdf::MupdfDocument::open(src.clone(), "application/pdf")?;
    let info = doc.info();
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
    Ok((Arc::new(doc), meta))
}

#[cfg(target_os = "ios")]
fn open_backend(src: &FileSource) -> Result<(Arc<dyn PageSource>, Meta)> {
    Ok((Arc::new(viewer_pdf_pdfkit::PdfKitDocument::open(src)?), Vec::new()))
}

#[cfg(windows)]
fn open_backend(_: &FileSource) -> Result<(Arc<dyn PageSource>, Meta)> {
    anyhow::bail!("PDF isn't available on Windows yet")
}

#[cfg(test)]
#[cfg(not(windows))]
mod tests {
    use super::*;
    use viewer_core::testing::pdf;

    #[test]
    fn probes_and_opens() {
        let bytes = pdf(&[(612.0, 792.0, 0), (612.0, 792.0, 0)]);
        assert_eq!(PdfLoader.probe(&bytes, None, "x"), Confidence::MAGIC);
        assert_eq!(PdfLoader.probe(b"hello", Some("pdf"), "x.pdf"), Confidence::EXTENSION);
        assert_eq!(PdfLoader.probe(b"hello", None, "x"), Confidence::NONE);
        let doc = PdfLoader.open(&FileSource::from_bytes("x.pdf", bytes)).unwrap();
        let Content::Pages(p) = &doc.content else { panic!("not pages") };
        assert_eq!(p.page_count(), 2);
        assert!(doc.metadata.iter().any(|(k, v)| k == "Pages" && v == "2"));
    }
}
