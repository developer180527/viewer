//! PowerPoint (PPTX, and PPT from 97–2003) and Word 97–2003 (DOC), read by
//! office_oxide (MIT/Apache): measured on 1,013 real files before it was adopted
//! (docs/design/libfileparser.md §6, "Which parser to use"), it opened every one that
//! wasn't broken or encrypted, and none panicked, hung or used more than 55 MB.
//!
//! Its format-neutral structure becomes formatted text: headings, paragraphs, lists,
//! tables, images (by their alt text), code, notes. A presentation's slides each start
//! with a heading, its speaker notes after the slide as a quote, and a Slides table
//! lists them. DOCX stays `docx-lite`'s, which reads as many real files.

use std::io::Cursor;
use std::sync::Arc;

use libfileparser_core::{
    Block, Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, Inline, Limit, OwnedFlow, OwnedTable, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, ValueKind,
    Warning,
};
use office_oxide::ir::{Element, InlineContent, VerticalAlign};
use office_oxide::{DocumentFormat, options::ParseOptions};

const PK: &[&[u8]] = &[b"PK\x03\x04"];
const OLE: &[&[u8]] = &[b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1"];
/// Word 97 and later in a compound file; Word 2.0 before it, a file of its own.
const WORD: &[&[u8]] = &[b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1", b"\xDB\xA5"];

fn starts(header: &[u8], magic: &[&[u8]]) -> bool {
    magic.iter().any(|m| header.starts_with(m))
}

/// Text runs as our inlines, each with its marks; comments' anchors aren't text.
fn inlines(content: &[InlineContent]) -> Vec<Inline> {
    let mut out = Vec::new();
    for c in content {
        match c {
            InlineContent::Text(span) if !span.text.is_empty() => {
                let mut i = Inline::Text(span.text.clone());
                let wrap = |inner: Inline, f: fn(Vec<Inline>) -> Inline| f(vec![inner]);
                match span.vertical_align {
                    Some(VerticalAlign::Superscript) => i = wrap(i, Inline::Superscript),
                    Some(VerticalAlign::Subscript) => i = wrap(i, Inline::Subscript),
                    _ => {}
                }
                if span.strikethrough {
                    i = wrap(i, Inline::Strikethrough);
                }
                if span.underline.as_ref().is_some_and(|u| !matches!(u, office_oxide::ir::UnderlineStyle::None)) {
                    i = wrap(i, Inline::Underline);
                }
                if span.italic {
                    i = wrap(i, Inline::Emphasis);
                }
                if span.bold {
                    i = wrap(i, Inline::Strong);
                }
                if let Some(target) = span.hyperlink.as_ref().filter(|t| !t.is_empty()) {
                    i = Inline::Link { target: target.clone(), text: vec![i] };
                }
                out.push(i);
            }
            InlineContent::LineBreak => out.push(Inline::LineBreak),
            InlineContent::FootnoteRef(r) | InlineContent::EndnoteRef(r) => out.push(Inline::Note(r.marker.clone().unwrap_or_else(|| r.note_id.to_string()))),
            _ => {}
        }
    }
    out
}

/// What converting adds up: blocks and inlines, against the node limit, and the notes
/// kept for the end.
struct Out {
    blocks: Vec<Block>,
    notes: Vec<Block>,
    count: u64,
    max: u64,
}

impl Out {
    fn charge(&mut self, n: usize) -> Result<()> {
        self.count = self.count.saturating_add(n as u64);
        if self.count > self.max {
            return Err(ParseError::too_large(Limit::Nodes, self.count, self.max));
        }
        Ok(())
    }
}

/// A block's elements as our blocks, notes set aside.
fn blocks(elements: &[Element], out: &mut Out) -> Result<Vec<Block>> {
    let mut v = Vec::new();
    for e in elements {
        out.charge(1)?;
        match e {
            Element::Heading(h) => {
                let text = inlines(&h.content);
                out.charge(text.len())?;
                if !text.is_empty() {
                    v.push(Block::Heading { level: h.level.clamp(1, 6), text });
                }
            }
            Element::Paragraph(p) => {
                let text = inlines(&p.content);
                out.charge(text.len())?;
                if !text.is_empty() {
                    v.push(Block::Paragraph(text));
                }
            }
            Element::Table(t) => {
                let header_rows = t.rows.iter().take_while(|r| r.is_header).count();
                let mut rows = Vec::with_capacity(t.rows.len());
                for r in &t.rows {
                    let mut cells = Vec::with_capacity(r.cells.len());
                    for c in &r.cells {
                        // A cell's paragraphs, one line each.
                        let mut cell = Vec::new();
                        for b in blocks(&c.content, out)? {
                            if !cell.is_empty() {
                                cell.push(Inline::LineBreak);
                            }
                            cell.extend(block_text(b));
                        }
                        cells.push(cell);
                    }
                    rows.push(cells);
                }
                if !rows.is_empty() {
                    v.push(Block::Table { header_rows, rows });
                }
            }
            Element::List(l) => v.push(list(l, out)?),
            Element::Image(i) => v.push(Block::Image { reference: String::new(), alt: i.alt_text.clone().unwrap_or_default() }),
            Element::ThematicBreak => v.push(Block::Rule),
            Element::PageBreak => v.push(Block::Break),
            Element::TextBox(t) => v.extend(blocks(&t.content, out)?),
            Element::CodeBlock(c) => v.push(Block::Code { language: c.language.clone(), text: c.content.clone() }),
            Element::Footnote(n) | Element::Endnote(n) => {
                let mut body = blocks(&n.content, out)?;
                let marker = n.marker.clone().unwrap_or_else(|| n.id.to_string());
                match body.first_mut() {
                    Some(Block::Paragraph(text)) => text.insert(0, Inline::Strong(vec![Inline::Text(format!("{marker} "))])),
                    _ => body.insert(0, Block::Paragraph(vec![Inline::Strong(vec![Inline::Text(marker)])])),
                }
                out.notes.extend(body);
            }
            _ => {}
        }
    }
    Ok(v)
}

fn list(l: &office_oxide::ir::List, out: &mut Out) -> Result<Block> {
    let mut items = Vec::with_capacity(l.items.len());
    for item in &l.items {
        let mut b = blocks(&item.content, out)?;
        if let Some(nested) = &item.nested {
            b.push(list(nested, out)?);
        }
        items.push(b);
    }
    Ok(Block::List { ordered: l.ordered, start: l.start_number.unwrap_or(1) as u64, items })
}

/// A block's text as inlines, for a table cell, which holds inlines only.
fn block_text(b: Block) -> Vec<Inline> {
    match b {
        Block::Heading { text, .. } | Block::Paragraph(text) => text,
        Block::Code { text, .. } => vec![Inline::Code(text)],
        Block::Image { reference, alt } => vec![Inline::Image { reference, alt }],
        Block::List { items, .. } => items.into_iter().flatten().flat_map(block_text).collect(),
        Block::Table { rows, .. } => rows.into_iter().flatten().flatten().collect(),
        Block::Quote(inner) => inner.into_iter().flat_map(block_text).collect(),
        _ => Vec::new(),
    }
}

/// The plain text of some blocks, for counting what a slide holds.
fn text_len(b: &[Block]) -> usize {
    fn inl(i: &[Inline]) -> usize {
        i.iter()
            .map(|x| match x {
                Inline::Text(t) | Inline::Code(t) => t.chars().count(),
                Inline::Emphasis(v) | Inline::Strong(v) | Inline::Underline(v) | Inline::Strikethrough(v) | Inline::Superscript(v) | Inline::Subscript(v) => inl(v),
                Inline::Link { text, .. } => inl(text),
                _ => 0,
            })
            .sum()
    }
    b.iter().map(|b| block_text(b.clone())).map(|i| inl(&i)).sum()
}

/// Our limits as office_oxide's: its parts, package and memory within ours.
fn options(src: &FileSource, cx: &ParseContext) -> ParseOptions {
    let l = &cx.limits;
    let mut o = ParseOptions::default();
    o.limits.part_bytes = o.limits.part_bytes.min(l.allocation);
    o.limits.package_bytes = o.limits.package_bytes.min(l.unpack_cap(src.len()));
    o.limits.package_entries = o.limits.package_entries.min(l.entries.min(usize::MAX as u64) as usize);
    o.limits.inflate_ratio = o.limits.inflate_ratio.min(l.unpack_ratio.max(1));
    o.limits.memory_bytes = o.limits.memory_bytes.min(l.memory);
    o.limits.text_chars = o.limits.text_chars.min(l.memory.min(usize::MAX as u64) as usize);
    o
}

/// The text of blocks, paragraphs and items run together, for finding text in it.
fn plain_text(blocks: &[Block]) -> String {
    paragraphs_of_all(blocks).join("\n")
}

/// Each paragraph's text in a block, lists and quotes opened.
fn paragraphs_of(block: &Block) -> Vec<String> {
    paragraphs_of_all(std::slice::from_ref(block))
}

fn paragraphs_of_all(blocks: &[Block]) -> Vec<String> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Paragraph(t) | Block::Heading { text: t, .. } => out.push(Inline::plain(t).trim().to_string()),
            Block::List { items, .. } => items.iter().for_each(|i| out.extend(paragraphs_of_all(i))),
            Block::Quote(q) => out.extend(paragraphs_of_all(q)),
            Block::Table { rows, .. } => rows.iter().flatten().for_each(|c| out.push(Inline::plain(c).trim().to_string())),
            _ => {}
        }
    }
    out.retain(|t| !t.is_empty());
    out
}

fn error(e: office_oxide::OfficeError) -> ParseError {
    let text = e.to_string();
    let lower = text.to_ascii_lowercase();
    if lower.contains("encrypted") || lower.contains("password") {
        ParseError::unsupported("This document is protected by a password.")
    } else if lower.contains("limit exceeded") || lower.contains("memory budget") || lower.contains("text budget") {
        ParseError::too_large(Limit::Memory, 1, 0)
    } else {
        ParseError::corrupt(text)
    }
}

/// Reads `src` as `format`, and makes its document.
fn open(src: &FileSource, cx: &ParseContext, format: DocumentFormat, id: &'static str, name: &'static str) -> Result<Document> {
    cx.cancel.check()?;
    let bytes = crate::whole(src, cx)?.to_vec();
    // SmartArt, which office_oxide leaves out, read from the package first.
    let diagrams = if matches!(format, DocumentFormat::Pptx) { crate::smartart::diagrams(&bytes) } else { Vec::new() };
    let doc = office_oxide::Document::from_reader_with(Cursor::new(bytes), format, &options(src, cx)).map_err(error)?;
    cx.cancel.check()?;
    let ir = doc.to_ir();
    let presentation = matches!(format, DocumentFormat::Pptx | DocumentFormat::Ppt);
    let mut out = Out { blocks: Vec::new(), notes: Vec::new(), count: 0, max: cx.limits.nodes };
    let mut slides = Vec::new();
    for (i, section) in ir.sections.iter().enumerate() {
        cx.cancel.check()?;
        let mut body = blocks(&section.elements, &mut out)?;
        if let Some(d) = diagrams.get(i) {
            // office_oxide reads some diagrams itself (from the drawing saved with them):
            // those are left as it has them.
            let shown = plain_text(&body);
            for diagram in d {
                let texts = paragraphs_of(diagram);
                if !texts.iter().all(|t| shown.contains(t.as_str())) {
                    body.push(diagram.clone());
                }
            }
        }
        if presentation {
            let title = section.title.clone().filter(|t| !t.trim().is_empty());
            let heading = title.clone().map_or_else(|| format!("Slide {}", i + 1), |t| format!("{}. {t}", i + 1));
            out.blocks.push(Block::Heading { level: 2, text: vec![Inline::Text(heading)] });
            let chars = text_len(&body);
            // The title is the heading; it isn't repeated when the slide begins with it.
            let skip = usize::from(title.is_some() && matches!(body.first(), Some(Block::Heading { text, .. } | Block::Paragraph(text)) if block_text(Block::Paragraph(text.clone())).iter().any(|x| matches!(x, Inline::Text(t) if Some(t.trim()) == title.as_deref().map(str::trim)))));
            out.blocks.extend(body.into_iter().skip(skip));
            let notes = match &section.speaker_notes {
                Some(n) if !n.is_empty() => {
                    let n = blocks(n, &mut out)?;
                    let has = !n.is_empty();
                    if has {
                        let mut quote = vec![Block::Paragraph(vec![Inline::Strong(vec![Inline::Text("Speaker notes".into())])])];
                        quote.extend(n);
                        out.blocks.push(Block::Quote(quote));
                    }
                    has
                }
                _ => false,
            };
            slides.push(vec![
                libfileparser_core::Cell::Int(i as i64 + 1),
                title.map_or(libfileparser_core::Cell::Null, libfileparser_core::Cell::Text),
                libfileparser_core::Cell::Int(chars as i64),
                libfileparser_core::Cell::Bool(notes),
            ]);
        } else {
            if i > 0 && !out.blocks.is_empty() {
                out.blocks.push(Block::Break);
            }
            out.blocks.extend(body);
        }
    }
    let mut blocks = std::mem::take(&mut out.blocks);
    if !out.notes.is_empty() {
        blocks.push(Block::Rule);
        blocks.append(&mut out.notes);
    }
    let m = &ir.metadata;
    let mut document = Document::new(FormatInfo::new(id, name, Category::Docs)).with_facet(Role::Primary, "Document", Content::Flow(Arc::new(OwnedFlow::new(blocks))));
    if presentation {
        let cols = [("Slide", ValueKind::Number), ("Title", ValueKind::Text), ("Characters", ValueKind::Number), ("Speaker notes", ValueKind::Bool)];
        let count = slides.len();
        let columns = cols.iter().map(|(n, k)| libfileparser_core::Column::new(*n).with_kind(*k)).collect();
        document = document.with_facet(Role::Structure, "Slides", Content::Table(Arc::new(OwnedTable::new(columns, slides)))).with_meta("Slides", count);
    }
    for (k, v) in [("Title", &m.title), ("Author", &m.author), ("Subject", &m.subject), ("Description", &m.description), ("Created", &m.created), ("Modified", &m.modified)] {
        if let Some(v) = v.as_ref().map(|v| v.trim()).filter(|v| !v.is_empty()) {
            document = document.with_meta(k, v);
        }
    }
    if !m.keywords.is_empty() {
        document = document.with_meta("Keywords", m.keywords.join(", "));
    }
    let damage = doc.damage();
    if !damage.is_empty() {
        document = document.with_warning(Warning::new(format!("The file is damaged in {} place(s); what could be read is shown.", damage.len())));
    }
    Ok(document.with_file_facets(src, None))
}

macro_rules! reader {
    ($reader:ident, $info:ident, $id:literal, $name:literal, $exts:expr, $mime:expr, $magic:ident, $format:expr) => {
        static $info: ReaderInfo = ReaderInfo::new($id, $name, Category::Docs).extensions($exts).mime_types($mime);

        pub struct $reader;

        impl Reader for $reader {
            fn info(&self) -> &ReaderInfo {
                &$info
            }

            /// Its container, named as one: certain, so it outranks the zip reader or
            /// the other formats in a compound file.
            fn detect(&self, header: &[u8], name: &str) -> Detection {
                if starts(header, $magic) && $info.matches_extension(name) { Detection::new(Confidence::Certain) } else { Detection::NONE }
            }

            fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
                if !starts(src.header(), $magic) {
                    return Err(ParseError::NotThisFormat);
                }
                open(src, cx, $format, $id, $name)
            }
        }
    };
}

reader!(PptxReader, PPTX, "pptx", "PowerPoint presentation", &["pptx", "pptm", "ppsx", "ppsm", "potx", "potm"], &["application/vnd.openxmlformats-officedocument.presentationml.presentation"], PK, DocumentFormat::Pptx);
reader!(PptReader, PPT, "ppt", "PowerPoint 97–2003 presentation", &["ppt", "pps", "pot"], &["application/vnd.ms-powerpoint"], OLE, DocumentFormat::Ppt);
reader!(DocReader, DOC, "doc", "Word 97–2003 document", &["doc", "dot"], &["application/msword"], WORD, DocumentFormat::Doc);

#[cfg(test)]
mod tests {
    use super::*;
    use office_oxide::ir::{FootnoteRef, Heading, List, ListItem, Note, Paragraph, Table, TableCell, TableRow, TextSpan};

    fn span(text: &str) -> InlineContent {
        InlineContent::Text(TextSpan::plain(text))
    }

    fn para(text: &str) -> Element {
        Element::Paragraph(Paragraph { content: vec![span(text)], ..Default::default() })
    }

    fn out() -> Out {
        Out { blocks: Vec::new(), notes: Vec::new(), count: 0, max: 1000 }
    }

    #[test]
    fn runs_keep_their_marks() {
        let mut s = TextSpan::plain("x");
        (s.bold, s.italic, s.strikethrough, s.hyperlink) = (true, true, true, Some("https://x.org".into()));
        s.underline = Some(office_oxide::ir::UnderlineStyle::Single);
        s.vertical_align = Some(VerticalAlign::Superscript);
        let got = inlines(&[InlineContent::Text(s), InlineContent::LineBreak, InlineContent::FootnoteRef(FootnoteRef { note_id: 4, marker: None }), span("")]);
        let x = Inline::Text("x".into());
        let marked = Inline::Strong(vec![Inline::Emphasis(vec![Inline::Underline(vec![Inline::Strikethrough(vec![Inline::Superscript(vec![x])])])])]);
        assert_eq!(got, [Inline::Link { target: "https://x.org".into(), text: vec![marked] }, Inline::LineBreak, Inline::Note("4".into())]);
        let mut sub = TextSpan::plain("2");
        sub.vertical_align = Some(VerticalAlign::Subscript);
        assert_eq!(inlines(&[InlineContent::Text(sub)]), [Inline::Subscript(vec![Inline::Text("2".into())])]);
    }

    #[test]
    fn every_element_has_a_block() {
        let header = TableRow { cells: vec![TableCell { content: vec![para("H")], ..Default::default() }], is_header: true, ..Default::default() };
        let row = TableRow { cells: vec![TableCell { content: vec![para("a"), para("b")], ..Default::default() }], ..Default::default() };
        let nested = List { ordered: true, items: vec![ListItem { content: vec![para("inner")], nested: None }], start_number: Some(3), ..Default::default() };
        let elements = vec![
            Element::Heading(Heading { level: 9, content: vec![span("Title")], ..Default::default() }),
            para("text"),
            para(""),
            Element::Table(Table { rows: vec![header, row], ..Default::default() }),
            Element::List(List { ordered: false, items: vec![ListItem { content: vec![para("one")], nested: Some(nested) }], ..Default::default() }),
            Element::Image(office_oxide::ir::Image { alt_text: Some("pic".into()), ..Default::default() }),
            Element::ThematicBreak,
            Element::PageBreak,
            Element::TextBox(office_oxide::ir::TextBox { content: vec![para("grouped")], ..Default::default() }),
            Element::Footnote(Note { id: 1, content: vec![para("a note")], marker: Some("*".into()), author: None }),
        ];
        let mut o = out();
        let got = blocks(&elements, &mut o).unwrap();
        assert_eq!(got[0], Block::Heading { level: 6, text: vec![Inline::Text("Title".into())] });
        assert_eq!(got[1], Block::Paragraph(vec![Inline::Text("text".into())]));
        assert_eq!(got[2], Block::Table { header_rows: 1, rows: vec![vec![vec![Inline::Text("H".into())]], vec![vec![Inline::Text("a".into()), Inline::LineBreak, Inline::Text("b".into())]]] });
        let Block::List { ordered: false, items, .. } = &got[3] else { panic!("{:?}", got[3]) };
        assert!(matches!(&items[0][1], Block::List { ordered: true, start: 3, .. }));
        assert_eq!((got[4].clone(), got[5].clone(), got[6].clone()), (Block::Image { reference: String::new(), alt: "pic".into() }, Block::Rule, Block::Break));
        assert_eq!(got[7], Block::Paragraph(vec![Inline::Text("grouped".into())]));
        assert_eq!(got.len(), 8);
        assert_eq!(o.notes, [Block::Paragraph(vec![Inline::Strong(vec![Inline::Text("* ".into())]), Inline::Text("a note".into())])]);
        // The node limit holds.
        let mut tight = Out { max: 3, ..out() };
        assert!(matches!(blocks(&elements, &mut tight), Err(ParseError::TooLarge(_))));
    }

    #[test]
    fn detection_needs_the_container_and_the_name() {
        let (pk, ole) = (PK[0], OLE[0]);
        assert_eq!(PptxReader.detect(pk, "deck.pptx").confidence, Confidence::Certain);
        assert_eq!(PptxReader.detect(ole, "deck.pptx").confidence, Confidence::None);
        assert_eq!(PptReader.detect(ole, "old.ppt").confidence, Confidence::Certain);
        assert_eq!(DocReader.detect(ole, "letter.doc").confidence, Confidence::Certain);
        // Word 2.0, before compound files.
        assert_eq!(DocReader.detect(b"\xDB\xA5\x2D\x00", "old.doc").confidence, Confidence::Certain);
        assert_eq!(PptReader.detect(b"\xDB\xA5\x2D\x00", "old.ppt").confidence, Confidence::None);
        assert_eq!(DocReader.detect(ole, "book.xls").confidence, Confidence::None);
        assert!(matches!(DocReader.open(&FileSource::from_bytes("x.doc", b"not ole".to_vec()), &ParseContext::new()), Err(ParseError::NotThisFormat)));
        assert!(matches!(DocReader.open(&FileSource::from_bytes("x.doc", [ole, &[0; 600]].concat()), &ParseContext::new()), Err(ParseError::Corrupt { .. })));
    }
}
