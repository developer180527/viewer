//! RTF, read by `rtf-lite` (our own crate: no mature RTF reader was more reliable on the
//! real-world corpus), its events built into formatted text.

use std::sync::Arc;

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, Limit, OwnedFlow, ParseContext, ParseError, Reader, ReaderInfo, Result, Role};
use crate::build::Builder;
use crate::lite::ToFlow;

static INFO: ReaderInfo = ReaderInfo::new("rtf", "RTF", Category::Docs).extensions(&["rtf"]).mime_types(&["application/rtf", "text/rtf"]);

pub struct RtfReader;

impl Reader for RtfReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    fn detect(&self, header: &[u8], _: &str) -> Detection {
        if header.starts_with(b"{\\rtf") { Detection::new(Confidence::Magic) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !src.header().starts_with(b"{\\rtf") {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        let mut sink = ToFlow::new(Builder::new(cx));
        let limits = rtf_lite::Limits::default().with_depth(cx.limits.depth.min(usize::MAX as u64) as usize);
        let info = match rtf_lite::parse(&bytes, &mut sink, &limits) {
            Ok(info) => info,
            Err(rtf_lite::Error::Stopped) => return Err(sink.stopped()),
            Err(rtf_lite::Error::TooDeep(d)) => return Err(ParseError::too_large(Limit::Depth, d as u64 + 1, d as u64)),
            Err(e) => return Err(ParseError::corrupt(e)),
        };
        let blocks = sink.b.finish()?;
        let mut doc = Document::new(FormatInfo::new("rtf", "Rich Text Format", Category::Docs)).with_facet(Role::Primary, "Document", Content::Flow(Arc::new(OwnedFlow::new(blocks))));
        if let Some(t) = info.title {
            doc = doc.with_meta("Title", t);
        }
        Ok(doc.with_file_facets(src, Some("rtf")))
    }
}
