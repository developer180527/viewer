//! Kindle books (MOBI, AZW, AZW3, PRC, PalmDOC) read by `mobi-lite` (our own decoder
//! of the Palm database and its compression), their text as the HTML it is;
//! and Hangul Word Processor documents (HWP 3 and 5, HWPX) through `unhwp`, written
//! out as Markdown and read by the Markdown reader.

use std::sync::Arc;

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, OwnedFlow, ParseContext, ParseError, Reader, ReaderInfo, Result, Role};

use crate::build::Builder;

static MOBI: ReaderInfo = ReaderInfo::new("mobi", "Kindle book", Category::Docs).extensions(&["mobi", "azw", "azw3", "prc", "pdb"]).mime_types(&["application/x-mobipocket-ebook", "application/vnd.amazon.ebook"]);
static HWP: ReaderInfo = ReaderInfo::new("hwp", "Hangul document", Category::Docs).extensions(&["hwp", "hwpx", "hwt"]).mime_types(&["application/x-hwp", "application/vnd.hancom.hwp", "application/hwp+zip"]);

pub struct MobiReader;
pub struct HwpReader;

impl Reader for MobiReader {
    fn info(&self) -> &ReaderInfo {
        &MOBI
    }

    /// A Palm database of type `BOOKMOBI` (or PalmDOC's `TEXtREAd`) at offset 60.
    fn detect(&self, header: &[u8], _name: &str) -> Detection {
        if mobi_lite::is_mobi(header) { Detection::new(Confidence::Magic) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !mobi_lite::is_mobi(src.header()) {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        let limits = mobi_lite::Limits { max_text: cx.limits.allocation };
        let book = mobi_lite::read(&bytes, &limits, &|| cx.cancel.is_cancelled()).map_err(|e| match e {
            mobi_lite::Error::NotMobi => ParseError::NotThisFormat,
            mobi_lite::Error::Protected => ParseError::unsupported("this Kindle book is protected (DRM)"),
            mobi_lite::Error::Unsupported(c) => ParseError::unsupported(format!("a Kindle book compressed as {c}")),
            mobi_lite::Error::TooLarge { max } => ParseError::too_large(libfileparser_core::Limit::Allocation, max + 1, max),
            mobi_lite::Error::Stopped => ParseError::Cancelled,
            mobi_lite::Error::Damaged(why) => ParseError::corrupt(format!("not a readable Kindle book: {why}")),
        })?;
        cx.cancel.check()?;
        let mut b = Builder::new(cx);
        // Its pictures by their record numbers: `<img recindex="00001">` becomes a
        // reference the flow serves.
        let names: Vec<Option<String>> = book.images.iter().enumerate().map(|(i, bytes)| picture_name(i + 1, bytes)).collect();
        if book.html {
            let text = recindex_to_src(&book.text, &names);
            crate::html::parse(&text, src.name(), &mut b)?;
        } else {
            crate::html::parse(&crate::mail::text_html(&book.text), src.name(), &mut b)?;
        }
        let shown = names.iter().flatten().count();
        let flow = book.images.into_iter().zip(names).filter_map(|(bytes, name)| Some((name?, bytes))).fold(OwnedFlow::new(b.finish()?), |f, (name, bytes)| f.with_resource(name.clone(), FileSource::from_bytes(name, bytes)));
        let mut doc = Document::new(FormatInfo::new("mobi", if book.html { "Kindle book" } else { "PalmDOC book" }, Category::Docs)).with_facet(Role::Primary, "Book", Content::Flow(Arc::new(flow)));
        if !book.title.trim().is_empty() {
            doc = doc.with_meta("Title", book.title.trim());
        }
        for (k, v) in &book.meta {
            doc = doc.with_meta(*k, v.trim());
        }
        if shown > 0 {
            doc = doc.with_meta("Images", shown);
        }
        Ok(doc.with_file_facets(src, None))
    }
}

/// A picture's name in the flow, `image-00001.jpg`, by its number and kind; `None` for
/// a record that isn't one.
fn picture_name(n: usize, bytes: &[u8]) -> Option<String> {
    let ext = if bytes.starts_with(&[0xFF, 0xD8]) {
        "jpg"
    } else if bytes.starts_with(b"\x89PNG") {
        "png"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else if bytes.starts_with(b"BM") {
        "bmp"
    } else {
        return None;
    };
    Some(format!("image-{n:05}.{ext}"))
}

/// Mobipocket's `recindex="00001"` (an `<img>`'s picture by number) as `src="…"`, the
/// name the flow serves it by.
fn recindex_to_src(html: &str, names: &[Option<String>]) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("recindex=") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "recindex=".len()..];
        let quoted = after.starts_with(['"', '\'']);
        let digits: String = after.trim_start_matches(['"', '\'']).chars().take_while(char::is_ascii_digit).collect();
        let len = digits.len() + if quoted { 2 } else { 0 };
        match digits.parse::<usize>().ok().and_then(|n| names.get(n.checked_sub(1)?)).and_then(Option::as_ref) {
            Some(name) if !digits.is_empty() => {
                out.push_str(&format!("src=\"{name}\""));
                rest = &after[len.min(after.len())..];
            }
            _ => {
                out.push_str("recindex=");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

const OLE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const HWPX_MIMETYPE: &[u8] = b"mimetypeapplication/hwp+zip";

impl Reader for HwpReader {
    fn info(&self) -> &ReaderInfo {
        &HWP
    }

    /// HWPX by its `mimetype` part; HWP 3 by its signature; HWP 5 (OLE, shared with
    /// Word and Excel) by its signature with its extension.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if header.starts_with(b"PK\x03\x04") && header.get(30..30 + HWPX_MIMETYPE.len()) == Some(HWPX_MIMETYPE) {
            Detection::new(Confidence::Certain)
        } else if header.starts_with(b"HWP Document File") {
            Detection::new(Confidence::Magic)
        } else if (header.starts_with(OLE) || header.starts_with(b"PK\x03\x04")) && HWP.matches_extension(name) {
            // As Word's and PowerPoint's readers do: the signature with its own extension.
            Detection::new(Confidence::Certain)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let h = src.header();
        if !(h.starts_with(OLE) || h.starts_with(b"PK\x03\x04") || h.starts_with(b"HWP Document File")) {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        // One thread, as the app's context decides threading, and no resources: the
        // text is what's read.
        let opts = unhwp::ParseOptions::new().lenient().sequential().without_resources();
        let hwp = unhwp::parse_bytes_with_options(&bytes, &opts).map_err(|e| {
            let msg = e.to_string();
            if msg.to_ascii_lowercase().contains("encrypt") || msg.contains("password") {
                ParseError::unsupported("this document is password-protected")
            } else if msg.to_ascii_lowercase().contains("not") && msg.to_ascii_lowercase().contains("hwp") {
                ParseError::NotThisFormat
            } else {
                ParseError::corrupt(format!("not a readable HWP document: {msg}"))
            }
        })?;
        cx.cancel.check()?;
        let md = unhwp::render::render_markdown(&hwp, &unhwp::RenderOptions::default()).map_err(ParseError::corrupt)?;
        let mut b = Builder::new(cx);
        for block in crate::markdown::parse(&md, cx)? {
            b.block(block)?;
        }
        let m = &hwp.metadata;
        let mut doc = Document::new(FormatInfo::new("hwp", "Hangul document", Category::Docs)).with_facet(Role::Primary, "Document", Content::Flow(Arc::new(OwnedFlow::new(b.finish()?))));
        for (k, v) in [("Title", &m.title), ("Author", &m.author), ("Subject", &m.subject), ("Created", &m.created), ("Modified", &m.modified), ("Written with", &m.creator_app), ("Version", &m.format_version)] {
            if let Some(v) = v.as_ref().map(|v| v.trim()).filter(|v| !v.is_empty()) {
                doc = doc.with_meta(k, v);
            }
        }
        if m.is_distribution {
            doc = doc.with_meta("Distribution document", "yes");
        }
        Ok(doc.with_file_facets(src, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection() {
        let mut palm = vec![0u8; 78];
        palm[60..68].copy_from_slice(b"BOOKMOBI");
        assert_eq!(MobiReader.detect(&palm, "x").confidence, Confidence::Magic);
        assert_eq!(MobiReader.detect(&[0u8; 78], "a.mobi"), Detection::NONE);
        assert_eq!(HwpReader.detect(b"HWP Document File V3.00 \x1a\x01\x02\x03\x04\x05", "a").confidence, Confidence::Magic);
        assert_eq!(HwpReader.detect(OLE, "a.doc"), Detection::NONE);
        assert_eq!(HwpReader.detect(OLE, "a.hwp").confidence, Confidence::Certain);
    }

    #[test]
    fn pictures_by_record_number() {
        let names = vec![Some("image-00001.jpg".to_string()), None, Some("image-00003.png".to_string())];
        assert_eq!(recindex_to_src(r#"<img recindex="00001"><img recindex='3' alt=x><img recindex="00002"><img recindex="9">"#, &names), r#"<img src="image-00001.jpg"><img src="image-00003.png" alt=x><img recindex="00002"><img recindex="9">"#);
        assert_eq!(picture_name(2, b"\x89PNG\r\n"), Some("image-00002.png".into()));
        assert_eq!(picture_name(2, b"FONT"), None);
    }

    #[test]
    fn broken_books_are_errors_not_panics() {
        let mut palm = vec![0u8; 200];
        palm[60..68].copy_from_slice(b"BOOKMOBI");
        assert!(MobiReader.open(&FileSource::from_bytes("a.mobi", palm), &ParseContext::new()).is_err());
        let mut ole = OLE.to_vec();
        ole.resize(1024, 0);
        assert!(HwpReader.open(&FileSource::from_bytes("a.hwp", ole), &ParseContext::new()).is_err());
    }
}
