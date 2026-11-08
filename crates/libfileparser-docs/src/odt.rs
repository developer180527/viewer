//! OpenDocument text (ODT, OTT), read by `odt-lite` (our own crate: no Rust library
//! reads ODT), its events built into formatted text, images read from the package
//! when a viewer asks.

use std::sync::Arc;

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, Limit, ParseContext, ParseError, Reader, ReaderInfo, Result, Role};

use crate::build::Builder;
use crate::lite::ToFlow;
use crate::package::{Package, PackagedFlow};

static INFO: ReaderInfo = ReaderInfo::new("odt", "OpenDocument text", Category::Docs).extensions(&["odt", "ott"]).mime_types(&["application/vnd.oasis.opendocument.text"]);

pub struct OdtReader;

const MIMETYPE: &[u8] = b"mimetypeapplication/vnd.oasis.opendocument.text";

impl Reader for OdtReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// Certain from its `mimetype` part, stored first; a zip named `.odt` is tried.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if header.starts_with(b"PK\x03\x04") && header.get(30..30 + MIMETYPE.len()) == Some(MIMETYPE) {
            Detection::new(Confidence::Certain)
        } else if header.starts_with(b"PK\x03\x04") && INFO.matches_extension(name) {
            Detection::new(Confidence::Magic)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !src.header().starts_with(b"PK\x03\x04") {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        let l = &cx.limits;
        let limits = odt_lite::Limits::default()
            .with_entries(l.entries.min(usize::MAX as u64) as usize)
            .with_part_bytes(l.allocation)
            .with_unpack(l.unpack_ratio, l.unpack_always)
            .with_depth(l.depth.min(usize::MAX as u64) as usize);
        let mut sink = ToFlow::new(Builder::new(cx));
        let info = match odt_lite::parse(&bytes, &mut sink, &limits) {
            Ok(info) => info,
            Err(odt_lite::Error::NotOdt) => return Err(ParseError::NotThisFormat),
            Err(odt_lite::Error::Stopped) => return Err(sink.stopped()),
            Err(odt_lite::Error::TooDeep(d)) => return Err(ParseError::too_large(Limit::Depth, d as u64 + 1, d as u64)),
            Err(odt_lite::Error::TooLarge { what, value, max }) => return Err(ParseError::too_large(if what == "entries" { Limit::Entries } else { Limit::Allocation }, value, max)),
            Err(e) => return Err(ParseError::corrupt(e)),
        };
        let blocks = sink.b.finish()?;
        let mut doc = Document::new(FormatInfo::new("odt", "OpenDocument text", Category::Docs))
            .with_facet(Role::Primary, "Document", Content::Flow(Arc::new(PackagedFlow::new(blocks, Package::open(src, cx)?))));
        for (k, v) in [("Title", info.title), ("Author", info.author), ("Subject", info.subject)] {
            if let Some(v) = v {
                doc = doc.with_meta(k, v);
            }
        }
        for (k, v) in [("Pages", info.pages), ("Words", info.words)] {
            if let Some(v) = v {
                doc = doc.with_meta(k, v);
            }
        }
        Ok(doc.with_file_facets(src, None))
    }
}
