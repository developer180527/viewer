//! Org (Emacs Org mode), parsed by orgize: its parse, written out as HTML by orgize,
//! becomes formatted text through html5ever (`crate::html`), so headings, lists,
//! tables, source blocks, quotes, links and marks come out as Org defines them.

use std::sync::Arc;

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, OwnedFlow, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, is_text};

use crate::build::Builder;

static INFO: ReaderInfo = ReaderInfo::new("org", "Org", Category::Docs).extensions(&["org"]).mime_types(&["text/org"]);

pub struct OrgReader;

impl Reader for OrgReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// By extension only: Org has no signature.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if INFO.matches_extension(name) && is_text(header) { Detection::new(Confidence::Extension).with_language("org") } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !is_text(src.header()) {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        let text = String::from_utf8_lossy(&bytes);
        let org = orgize::Org::parse(&text);
        let title = org.document().title();
        let mut b = Builder::new(cx);
        crate::html::parse(&org.to_html(), src.name(), &mut b)?;
        let mut doc = Document::new(FormatInfo::new("org", "Org document", Category::Docs)).with_facet(Role::Primary, "Document", Content::Flow(Arc::new(OwnedFlow::new(b.finish()?))));
        if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
            doc = doc.with_meta("Title", t.trim());
        }
        Ok(doc.with_file_facets(src, Some("org")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_through_orgize() {
        let src = FileSource::from_bytes("n.org", b"#+TITLE: Notes\n* Top\n** Sub\nSome *bold*, /it/, =verb= and [[https://x.org][a link]].\n- one\n- two\n#+BEGIN_SRC sh\nls\n#+END_SRC\n| a | b |\n|---+---|\n| 1 | 2 |\n".to_vec());
        let doc = OrgReader.open(&src, &ParseContext::new()).unwrap();
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Notes"));
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..100).unwrap());
        for want in ["Heading { level: 1", "Heading { level: 2", "Strong", "Emphasis", "Code(\"verb\")", "https://x.org", "List", "Code { language", "Table { header_rows: 1"] {
            assert!(s.contains(want), "{want} in {s}");
        }
    }
}
