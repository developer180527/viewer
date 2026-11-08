//! Word documents (DOCX, DOCM, DOTX, DOTM), read by `docx-lite` (our own crate: on the
//! real-world corpus it opens more documents than docx-rust, which refuses a whole
//! document over one unexpected value), its events built into formatted text, images
//! read from the package when a viewer asks.

use std::sync::Arc;

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, Limit, ParseContext, ParseError, Reader, ReaderInfo, Result, Role};

use crate::build::Builder;
use crate::lite::ToFlow;
use crate::package::{Package, PackagedFlow};

static INFO: ReaderInfo = ReaderInfo::new("docx", "Word document", Category::Docs)
    .extensions(&["docx", "docm", "dotx", "dotm"])
    .mime_types(&["application/vnd.openxmlformats-officedocument.wordprocessingml.document"]);

pub struct DocxReader;

impl Reader for DocxReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// A zip named as a Word document: certain, so it outranks the zip reader. (A
    /// DOCX can't be told from an XLSX by its first bytes alone.) An OLE file named as
    /// one is a password-protected document, ours to refuse plainly.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if (header.starts_with(b"PK\x03\x04") || header.starts_with(OLE)) && INFO.matches_extension(name) { Detection::new(Confidence::Certain) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let ole = src.header().starts_with(OLE);
        if !ole && !src.header().starts_with(b"PK\x03\x04") {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        if ole {
            return Err(if encrypted(&bytes) { ParseError::unsupported("This document is protected by a password.") } else { ParseError::NotThisFormat });
        }
        let mut sink = ToFlow::new(Builder::new(cx));
        let info = match docx_lite::parse(&bytes, &mut sink, &lite_limits(cx)) {
            Ok(info) => info,
            Err(e) => return Err(error(e, &mut sink)),
        };
        let blocks = sink.b.finish()?;
        let mut doc = Document::new(FormatInfo::new("docx", "Word document", Category::Docs))
            .with_facet(Role::Primary, "Document", Content::Flow(Arc::new(PackagedFlow::new(blocks, Package::open(src, cx)?))));
        for (k, v) in [("Title", info.title), ("Author", info.author), ("Subject", info.subject), ("Last edited by", info.last_edited_by)] {
            if let Some(v) = v {
                doc = doc.with_meta(k, v);
            }
        }
        Ok(doc.with_file_facets(src, None))
    }
}

/// An OLE compound file's first bytes: what an encrypted Office document is stored in.
const OLE: &[u8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";

/// Whether an OLE file holds an encrypted Office package (its `EncryptedPackage`
/// stream, named in UTF-16 in the directory).
fn encrypted(bytes: &[u8]) -> bool {
    let name: Vec<u8> = "EncryptedPackage".encode_utf16().flat_map(u16::to_le_bytes).collect();
    bytes.windows(name.len()).any(|w| w == name)
}

fn lite_limits(cx: &ParseContext) -> docx_lite::Limits {
    let l = &cx.limits;
    docx_lite::Limits::default()
        .with_entries(l.entries.min(usize::MAX as u64) as usize)
        .with_part_bytes(l.allocation)
        .with_unpack(l.unpack_ratio, l.unpack_always)
        .with_depth(l.depth.min(usize::MAX as u64) as usize)
}

fn error(e: docx_lite::Error, sink: &mut ToFlow) -> ParseError {
    match e {
        docx_lite::Error::NotDocx => ParseError::NotThisFormat,
        docx_lite::Error::Stopped => sink.stopped(),
        docx_lite::Error::TooDeep(d) => ParseError::too_large(Limit::Depth, d as u64 + 1, d as u64),
        docx_lite::Error::TooLarge { what, value, max } => ParseError::too_large(if what == "entries" { Limit::Entries } else { Limit::Allocation }, value, max),
        e => ParseError::corrupt(e),
    }
}
