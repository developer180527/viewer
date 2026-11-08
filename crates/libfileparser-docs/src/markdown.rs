//! Markdown: formatted text over its source lines. How it looks is the app's (a
//! Markdown view); this says what it is.

use std::sync::Arc;

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, LineText, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, TextSource, is_text};

static INFO: ReaderInfo = ReaderInfo::new("markdown", "Markdown", Category::Docs).extensions(&["md", "markdown", "mdown", "mkd", "mkdn"]).mime_types(&["text/markdown"]);

pub struct MarkdownReader;

impl Reader for MarkdownReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// By extension only: Markdown has no signature, and any text "looks like" it.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if INFO.matches_extension(name) && is_text(header) { Detection::new(Confidence::Extension).with_language("markdown") } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !is_text(src.header()) {
            return Err(ParseError::NotThisFormat);
        }
        let text = LineText::within(src.clone(), Some("markdown"), &cx.limits)?;
        let lines = text.info().lines;
        let mut doc = Document::new(FormatInfo::new("markdown", "Markdown", Category::Docs))
            .with_facet(Role::Primary, "Document", Content::Markdown(Arc::new(text)))
            .with_meta("Lines", lines);
        // Words, where the file is in memory or mapped (not read by position).
        if !src.is_positional() {
            doc = doc.with_meta("Words", String::from_utf8_lossy(src.bytes()).split_whitespace().count());
        }
        Ok(doc
            .with_file_facets(src, Some("markdown")))
    }
}
