//! Formatted text: the parsed structure of prose (docs/design/libfileparser.md §2.1),
//! never a layout. Markdown, reStructuredText, AsciiDoc, Org, DOCX, ODT, RTF and EPUB
//! all parse into it; how a heading or a table looks is a viewer's choice.
//!
//! A document is a list of [`Block`]s read by range, so a 3,000-page book opens without
//! all of it in memory at once (a reader may still build it whole). Images inside the
//! document (DOCX media, EPUB images) are named by a reference the source resolves with
//! [`FlowSource::resource`].

use std::collections::HashMap;
use std::ops::Range;

use crate::{Count, FileSource, MemoryUse, ParseError, Result};

/// One block of a document.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Block {
    /// A heading, `level` 1 (largest) to 6.
    Heading { level: u8, text: Vec<Inline> },
    Paragraph(Vec<Inline>),
    /// Items, each a list of blocks. `start` is the first number of an ordered list.
    List { ordered: bool, start: u64, items: Vec<Vec<Block>> },
    Quote(Vec<Block>),
    /// Preformatted text, in `language` when known.
    Code { language: Option<String>, text: String },
    /// Rows of cells, each cell a list of inlines; the first `header_rows` are headers.
    Table { header_rows: usize, rows: Vec<Vec<Vec<Inline>>> },
    /// An image on its own line.
    Image { reference: String, alt: String },
    /// A horizontal rule or scene break.
    Rule,
    /// A page or section break the document asks for.
    Break,
    /// Something kept as written because it has no structure here (raw HTML in
    /// Markdown, a LaTeX block), in `format`.
    Raw { format: String, text: String },
}

/// A run of text inside a block, and how it's marked.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Inline {
    Text(String),
    Emphasis(Vec<Inline>),
    Strong(Vec<Inline>),
    Underline(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Superscript(Vec<Inline>),
    Subscript(Vec<Inline>),
    Code(String),
    Link { target: String, text: Vec<Inline> },
    Image { reference: String, alt: String },
    LineBreak,
    /// A footnote or endnote, by its label; its text is a [`Block`] list the source
    /// gives with the rest (as a final section), or the label alone.
    Note(String),
}

impl Inline {
    /// The plain text of inlines, marks dropped.
    pub fn plain(inlines: &[Inline]) -> String {
        let mut out = String::new();
        fn walk(i: &[Inline], out: &mut String) {
            for x in i {
                match x {
                    Inline::Text(t) | Inline::Code(t) => out.push_str(t),
                    Inline::Emphasis(c) | Inline::Strong(c) | Inline::Underline(c) | Inline::Strikethrough(c) | Inline::Superscript(c) | Inline::Subscript(c) => walk(c, out),
                    Inline::Link { text, .. } => walk(text, out),
                    Inline::Image { alt, .. } => out.push_str(alt),
                    Inline::LineBreak => out.push('\n'),
                    Inline::Note(l) => {
                        out.push('[');
                        out.push_str(l);
                        out.push(']');
                    }
                }
            }
        }
        walk(inlines, &mut out);
        out
    }
}

/// Formatted text read by block range.
pub trait FlowSource: Send + Sync {
    fn block_count(&self) -> Count;
    /// Blocks in `range`; fewer past the end.
    fn blocks(&self, range: Range<u64>) -> Result<Vec<Block>>;
    /// The file an image `reference` names (a DOCX media part, an EPUB image), when
    /// the document carries it. [`ParseError::InvalidRequest`] for a reference it
    /// never gave.
    fn resource(&self, reference: &str) -> Result<FileSource> {
        Err(ParseError::invalid(format!("no resource {reference:?}")))
    }
    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
}

/// Formatted text built up front, with the files its images name.
#[derive(Default)]
pub struct OwnedFlow {
    blocks: Vec<Block>,
    resources: HashMap<String, FileSource>,
}

impl OwnedFlow {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self { blocks, resources: HashMap::new() }
    }

    /// Makes `file` what image reference `reference` resolves to.
    pub fn with_resource(mut self, reference: impl Into<String>, file: FileSource) -> Self {
        self.resources.insert(reference.into(), file);
        self
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

impl FlowSource for OwnedFlow {
    fn block_count(&self) -> Count {
        Count::Exact(self.blocks.len() as u64)
    }

    fn blocks(&self, range: Range<u64>) -> Result<Vec<Block>> {
        let end = range.end.min(self.blocks.len() as u64);
        Ok(self.blocks[range.start.min(end) as usize..end as usize].to_vec())
    }

    fn resource(&self, reference: &str) -> Result<FileSource> {
        self.resources.get(reference).cloned().ok_or_else(|| ParseError::invalid(format!("no resource {reference:?}")))
    }

    fn memory(&self) -> MemoryUse {
        fn inl(i: &[Inline]) -> u64 {
            i.iter()
                .map(|x| {
                    48 + match x {
                        Inline::Text(t) | Inline::Code(t) | Inline::Note(t) => t.len() as u64,
                        Inline::Emphasis(c) | Inline::Strong(c) | Inline::Underline(c) | Inline::Strikethrough(c) | Inline::Superscript(c) | Inline::Subscript(c) => inl(c),
                        Inline::Link { target, text } => target.len() as u64 + inl(text),
                        Inline::Image { reference, alt } => (reference.len() + alt.len()) as u64,
                        Inline::LineBreak => 0,
                    }
                })
                .sum()
        }
        fn blk(b: &[Block]) -> u64 {
            b.iter()
                .map(|x| {
                    64 + match x {
                        Block::Heading { text, .. } | Block::Paragraph(text) => inl(text),
                        Block::List { items, .. } => items.iter().map(|i| blk(i)).sum(),
                        Block::Quote(c) => blk(c),
                        Block::Code { text, .. } | Block::Raw { text, .. } => text.len() as u64,
                        Block::Table { rows, .. } => rows.iter().flatten().map(|c| inl(c)).sum(),
                        Block::Image { reference, alt } => (reference.len() + alt.len()) as u64,
                        Block::Rule | Block::Break => 0,
                    }
                })
                .sum()
        }
        MemoryUse::heap(blk(&self.blocks))
    }
}
