//! Markdown (CommonMark, with GitHub's tables, strikethrough, task lists and
//! footnotes) into formatted text. How it looks is a viewer's; this says what it is.

use std::sync::Arc;

use libfileparser_core::{Block, Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, OwnedFlow, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, is_text};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::build::{Builder, Open};

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
        let bytes = crate::whole(src, cx)?;
        let text = String::from_utf8_lossy(&bytes);
        let blocks = parse(&text, cx)?;
        let words = text.split_whitespace().count();
        Ok(Document::new(FormatInfo::new("markdown", "Markdown", Category::Docs))
            .with_facet(Role::Primary, "Document", Content::Flow(Arc::new(OwnedFlow::new(blocks))))
            .with_meta("Words", words)
            .with_file_facets(src, Some("markdown")))
    }
}

/// Markdown text into blocks.
pub fn parse(text: &str, cx: &ParseContext) -> Result<Vec<Block>> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_FOOTNOTES;
    let mut b = Builder::new(cx);
    // An image's alt text arrives as text events inside it.
    let mut image: Option<(String, String)> = None;
    let mut html = String::new();
    for (n, event) in Parser::new_ext(text, opts).enumerate() {
        if n % 4096 == 0 {
            cx.cancel.check()?;
        }
        if let Some((_, alt)) = &mut image {
            match event {
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                Event::End(TagEnd::Image) => {
                    let (src, alt) = image.take().unwrap();
                    b.image(&src, &alt)?;
                }
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => b.open(Open::Paragraph)?,
                Tag::Heading { level, .. } => b.open(Open::Heading(heading(level)))?,
                Tag::BlockQuote(_) => b.open(Open::Quote)?,
                Tag::CodeBlock(kind) => b.open(Open::Code(match kind {
                    CodeBlockKind::Fenced(l) if !l.is_empty() => Some(l.split_whitespace().next().unwrap_or("").to_string()),
                    _ => None,
                }))?,
                Tag::List(start) => b.open(Open::List { ordered: start.is_some(), start: start.unwrap_or(1) })?,
                Tag::Item => b.open(Open::Item)?,
                Tag::Table(_) => b.open(Open::Table)?,
                Tag::TableHead => b.open(Open::Row { header: true })?,
                Tag::TableRow => b.open(Open::Row { header: false })?,
                Tag::TableCell => b.open(Open::Cell)?,
                Tag::Emphasis => b.open(Open::Emphasis)?,
                Tag::Strong => b.open(Open::Strong)?,
                Tag::Strikethrough => b.open(Open::Strikethrough)?,
                Tag::Superscript => b.open(Open::Superscript)?,
                Tag::Subscript => b.open(Open::Subscript)?,
                Tag::Link { dest_url, .. } => b.open(Open::Link(dest_url.to_string()))?,
                Tag::Image { dest_url, .. } => image = Some((dest_url.to_string(), String::new())),
                Tag::FootnoteDefinition(label) => {
                    b.open(Open::Quote)?;
                    b.text(&format!("[{label}] "))?;
                }
                Tag::HtmlBlock => html.clear(),
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph => b.close(&Open::Paragraph)?,
                TagEnd::Heading(_) => b.close(&Open::Heading(0))?,
                TagEnd::BlockQuote(_) | TagEnd::FootnoteDefinition => b.close(&Open::Quote)?,
                TagEnd::CodeBlock => b.close(&Open::Code(None))?,
                TagEnd::List(_) => b.close(&Open::List { ordered: false, start: 0 })?,
                TagEnd::Item => b.close(&Open::Item)?,
                TagEnd::Table => b.close(&Open::Table)?,
                TagEnd::TableHead | TagEnd::TableRow => b.close(&Open::Row { header: false })?,
                TagEnd::TableCell => b.close(&Open::Cell)?,
                TagEnd::Emphasis => b.close(&Open::Emphasis)?,
                TagEnd::Strong => b.close(&Open::Strong)?,
                TagEnd::Strikethrough => b.close(&Open::Strikethrough)?,
                TagEnd::Superscript => b.close(&Open::Superscript)?,
                TagEnd::Subscript => b.close(&Open::Subscript)?,
                TagEnd::Link => b.close(&Open::Link(String::new()))?,
                TagEnd::HtmlBlock => b.block(Block::Raw { format: "html".into(), text: std::mem::take(&mut html).trim_end().to_string() })?,
                _ => {}
            },
            Event::Text(t) => b.text(&t)?,
            Event::Code(c) => b.code(&c)?,
            Event::Html(h) => html.push_str(&h),
            Event::InlineHtml(h) => b.text(&h)?,
            Event::SoftBreak => b.text(" ")?,
            Event::HardBreak => b.line_break()?,
            Event::Rule => b.block(Block::Rule)?,
            Event::FootnoteReference(label) => b.note(&label)?,
            Event::TaskListMarker(done) => b.text(if done { "☑ " } else { "☐ " })?,
            Event::InlineMath(m) | Event::DisplayMath(m) => b.code(&m)?,
        }
    }
    b.finish()
}

fn heading(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::Inline;

    #[test]
    fn markdown_into_blocks() {
        let md = "# Title\n\nSome *em* and **strong** with `code` and a [link](x.md).\n\n- one\n- two\n\n1. first\n\n> quoted\n\n```rust\nfn x() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n![alt text](pic.png)\n\n---\n";
        let blocks = parse(md, &ParseContext::new()).unwrap();
        assert_eq!(blocks[0], Block::Heading { level: 1, text: vec![Inline::Text("Title".into())] });
        let Block::Paragraph(p) = &blocks[1] else { panic!("{:?}", blocks[1]) };
        assert!(p.contains(&Inline::Emphasis(vec![Inline::Text("em".into())])));
        assert!(p.contains(&Inline::Code("code".into())));
        assert!(p.iter().any(|i| matches!(i, Inline::Link { target, .. } if target == "x.md")));
        assert!(matches!(&blocks[2], Block::List { ordered: false, items, .. } if items.len() == 2));
        assert!(matches!(&blocks[3], Block::List { ordered: true, start: 1, .. }));
        assert!(matches!(&blocks[4], Block::Quote(_)));
        assert_eq!(blocks[5], Block::Code { language: Some("rust".into()), text: "fn x() {}".into() });
        assert!(matches!(&blocks[6], Block::Table { header_rows: 1, rows } if rows.len() == 2));
        assert!(matches!(&blocks[7], Block::Paragraph(i) if matches!(&i[0], Inline::Image { reference, alt } if reference == "pic.png" && alt == "alt text")));
        assert_eq!(blocks[8], Block::Rule);
    }
}
