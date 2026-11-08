//! XPS and OpenXPS (fixed-layout pages in a zip), through MuPDF with the `mupdf`
//! feature, as PDF is; refused plainly without it.

use libfileparser_core::{Category, Confidence, Detection, Document, FileSource, ParseContext, ParseError, Reader, ReaderInfo, Result};

static INFO: ReaderInfo = ReaderInfo::new("xps", "XPS", Category::Docs).extensions(&["xps", "oxps"]).mime_types(&["application/vnd.ms-xpsdocument", "application/oxps"]);

pub struct XpsReader;

impl Reader for XpsReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// A zip named `.xps` or `.oxps`: certain, so it outranks the zip reader.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if header.starts_with(b"PK\x03\x04") && INFO.matches_extension(name) { Detection::new(Confidence::Certain) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !src.header().starts_with(b"PK\x03\x04") {
            return Err(ParseError::NotThisFormat);
        }
        open_mupdf(src, cx)
    }
}

#[cfg(all(feature = "mupdf", not(windows)))]
fn open_mupdf(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let pages = libfileparser_mupdf::MupdfDocument::open(src.clone(), "document.xps")?;
    let info = pages.info();
    let meta = [("Title", info.title), ("Author", info.author)].into_iter().filter_map(|(k, v)| Some((k.to_string(), v?))).collect();
    crate::pages::document("xps", "XPS document", src, std::sync::Arc::new(pages), meta, cx)
}

#[cfg(not(all(feature = "mupdf", not(windows))))]
fn open_mupdf(_: &FileSource, _: &ParseContext) -> Result<Document> {
    Err(ParseError::unsupported("XPS isn't available in this build (it is read by MuPDF)"))
}
