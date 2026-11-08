//! Building formatted text ([`Block`]s and [`Inline`]s) from any parser's events:
//! open a block or a mark, add text, close it. Every format's reader (Markdown, XHTML
//! in EPUB, DOCX, ODT, RTF, reStructuredText, AsciiDoc, Org) drives one of these, so
//! nesting rules and limits live in one place.
//!
//! It is lenient, as readers of real files must be: text outside any block starts a
//! paragraph, a block opened inside a paragraph ends it, and closing a kind that isn't
//! open does nothing.

use libfileparser_core::{Block, Inline, Limit, ParseContext, Result};

/// Something opened and later closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    Paragraph,
    Heading(u8),
    Quote,
    List { ordered: bool, start: u64 },
    Item,
    Table,
    /// A table row; `header` rows come first in the table's header count.
    Row { header: bool },
    Cell,
    Code(Option<String>),
    Emphasis,
    Strong,
    Underline,
    Strikethrough,
    Superscript,
    Subscript,
    Link(String),
}

impl Open {
    fn is_mark(&self) -> bool {
        matches!(self, Open::Emphasis | Open::Strong | Open::Underline | Open::Strikethrough | Open::Superscript | Open::Subscript | Open::Link(_))
    }

    /// Holds inlines (text goes straight in).
    fn holds_inlines(&self) -> bool {
        self.is_mark() || matches!(self, Open::Paragraph | Open::Heading(_) | Open::Cell)
    }

    /// Holds blocks.
    fn holds_blocks(&self) -> bool {
        matches!(self, Open::Quote | Open::Item)
    }
}

struct Frame {
    open: Open,
    blocks: Vec<Block>,
    inlines: Vec<Inline>,
    items: Vec<Vec<Block>>,
    rows: Vec<Vec<Vec<Inline>>>,
    header_rows: usize,
    cells: Vec<Vec<Inline>>,
    code: String,
}

impl Frame {
    fn new(open: Open) -> Self {
        Self { open, blocks: Vec::new(), inlines: Vec::new(), items: Vec::new(), rows: Vec::new(), header_rows: 0, cells: Vec::new(), code: String::new() }
    }
}

pub struct Builder<'a> {
    root: Vec<Block>,
    stack: Vec<Frame>,
    count: u64,
    cx: &'a ParseContext,
}

impl<'a> Builder<'a> {
    pub fn new(cx: &'a ParseContext) -> Self {
        Self { root: Vec::new(), stack: Vec::new(), count: 0, cx }
    }

    /// Opens `open`. A block opened inside a paragraph (or other inline holder that
    /// isn't a table cell) closes it first.
    pub fn open(&mut self, open: Open) -> Result<()> {
        self.cx.limits.check(Limit::Depth, self.stack.len() as u64 + 1)?;
        if !open.is_mark() {
            // Blocks don't go inside paragraphs or marks: end them (a cell keeps its own).
            while self.stack.last().is_some_and(|f| f.open.holds_inlines() && f.open != Open::Cell) {
                self.pop()?;
            }
            // A new item, row or cell ends the one before it.
            let top_is = |b: &Self, pred: fn(&Open) -> bool| b.stack.last().is_some_and(|f| pred(&f.open));
            match &open {
                Open::Item => {
                    while top_is(self, |o| o.holds_inlines() || *o == Open::Item) {
                        self.pop()?;
                    }
                    if !top_is(self, |o| matches!(o, Open::List { .. })) {
                        self.stack.push(Frame::new(Open::List { ordered: false, start: 1 }));
                    }
                }
                Open::Row { .. } => {
                    if self.is_open(&Open::Table) {
                        while !top_is(self, |o| *o == Open::Table) {
                            self.pop()?;
                        }
                    } else {
                        self.stack.push(Frame::new(Open::Table));
                    }
                }
                Open::Cell => {
                    if self.is_open(&Open::Row { header: false }) {
                        while !top_is(self, |o| matches!(o, Open::Row { .. })) {
                            self.pop()?;
                        }
                    } else {
                        self.open(Open::Row { header: false })?;
                    }
                }
                _ => {}
            }
        } else {
            self.inline_holder()?;
        }
        self.stack.push(Frame::new(open));
        Ok(())
    }

    /// Closes the innermost open `kind` (compared by variant), and everything inside it.
    /// Nothing happens when none is open.
    pub fn close(&mut self, kind: &Open) -> Result<()> {
        let same = |o: &Open| std::mem::discriminant(o) == std::mem::discriminant(kind);
        if let Some(at) = self.stack.iter().rposition(|f| same(&f.open)) {
            while self.stack.len() > at {
                self.pop()?;
            }
        }
        Ok(())
    }

    /// `Cancelled` once the document is closed, for loops over a long input.
    pub fn cancel_check(&self) -> Result<()> {
        self.cx.cancel.check()
    }

    /// Whether a frame of `kind`'s variant is open.
    pub fn is_open(&self, kind: &Open) -> bool {
        self.stack.iter().any(|f| std::mem::discriminant(&f.open) == std::mem::discriminant(kind))
    }

    /// Text, in the current paragraph or mark (or a new paragraph). Inside a code
    /// block it's code, kept exactly.
    pub fn text(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        if let Some(f) = self.stack.last_mut()
            && matches!(f.open, Open::Code(_))
        {
            f.code.push_str(text);
            return Ok(());
        }
        // Whitespace between blocks isn't a paragraph.
        if text.trim().is_empty() && !self.stack.last().is_some_and(|f| f.open.holds_inlines()) {
            return Ok(());
        }
        self.push_inline(Inline::Text(text.to_string()))
    }

    pub fn code(&mut self, code: &str) -> Result<()> {
        self.push_inline(Inline::Code(code.to_string()))
    }

    pub fn line_break(&mut self) -> Result<()> {
        if self.stack.last().is_some_and(|f| f.open.holds_inlines()) { self.push_inline(Inline::LineBreak) } else { Ok(()) }
    }

    pub fn note(&mut self, label: &str) -> Result<()> {
        self.push_inline(Inline::Note(label.to_string()))
    }

    /// An image: inline inside text, a block on its own.
    pub fn image(&mut self, reference: &str, alt: &str) -> Result<()> {
        if self.stack.last().is_some_and(|f| f.open.holds_inlines()) {
            self.push_inline(Inline::Image { reference: reference.into(), alt: alt.into() })
        } else {
            self.block(Block::Image { reference: reference.into(), alt: alt.into() })
        }
    }

    /// A whole block (a rule, a break, raw text, a code block in one piece), ending
    /// any paragraph.
    pub fn block(&mut self, block: Block) -> Result<()> {
        while self.stack.last().is_some_and(|f| f.open.holds_inlines() && f.open != Open::Cell) {
            self.pop()?;
        }
        self.add_block(block)
    }

    /// Everything built, every open frame closed.
    pub fn finish(mut self) -> Result<Vec<Block>> {
        while !self.stack.is_empty() {
            self.pop()?;
        }
        Ok(self.root)
    }

    // ------------------------------------------------------------ inside

    fn push_inline(&mut self, inline: Inline) -> Result<()> {
        self.inline_holder()?;
        self.stack.last_mut().expect("an inline holder is open").inlines.push(inline);
        Ok(())
    }

    /// Makes sure the top frame holds inlines, opening a paragraph (in a list item, a
    /// cell's row…) when it doesn't.
    fn inline_holder(&mut self) -> Result<()> {
        if self.stack.last().is_some_and(|f| f.open.holds_inlines()) {
            return Ok(());
        }
        match self.stack.last().map(|f| &f.open) {
            Some(Open::List { .. }) => self.open(Open::Item)?,
            Some(Open::Table) | Some(Open::Row { .. }) => return self.open(Open::Cell),
            _ => {}
        }
        self.cx.limits.check(Limit::Depth, self.stack.len() as u64 + 1)?;
        self.stack.push(Frame::new(Open::Paragraph));
        Ok(())
    }

    fn add_block(&mut self, block: Block) -> Result<()> {
        self.count += 1;
        self.cx.limits.check(Limit::Nodes, self.count)?;
        match self.stack.last_mut() {
            None => self.root.push(block),
            Some(f) if f.open.holds_blocks() => f.blocks.push(block),
            Some(f) if f.open == Open::Cell => {
                // A cell holds inlines: a block in it becomes its text.
                if !f.inlines.is_empty() {
                    f.inlines.push(Inline::LineBreak);
                }
                f.inlines.extend(block_inlines(block));
            }
            Some(f) if matches!(f.open, Open::List { .. }) => f.items.push(vec![block]),
            Some(_) => {
                // Inside a table or row, outside any cell: keep it after the table.
                self.pop()?;
                self.add_block(block)?;
            }
        }
        Ok(())
    }

    fn pop(&mut self) -> Result<()> {
        let Some(f) = self.stack.pop() else { return Ok(()) };
        let mark = |wrap: fn(Vec<Inline>) -> Inline, f: Frame| (!f.inlines.is_empty()).then(|| wrap(f.inlines));
        let inline = match &f.open {
            Open::Emphasis => mark(Inline::Emphasis, f),
            Open::Strong => mark(Inline::Strong, f),
            Open::Underline => mark(Inline::Underline, f),
            Open::Strikethrough => mark(Inline::Strikethrough, f),
            Open::Superscript => mark(Inline::Superscript, f),
            Open::Subscript => mark(Inline::Subscript, f),
            Open::Link(target) => {
                let target = target.clone();
                if f.inlines.is_empty() { None } else { Some(Inline::Link { target, text: f.inlines }) }
            }
            Open::Paragraph => {
                let text = trim(f.inlines);
                return if text.is_empty() { Ok(()) } else { self.add_block(Block::Paragraph(text)) };
            }
            Open::Heading(level) => {
                let text = trim(f.inlines);
                return if text.is_empty() { Ok(()) } else { self.add_block(Block::Heading { level: (*level).clamp(1, 6), text }) };
            }
            Open::Code(language) => {
                let text = f.code.strip_suffix('\n').unwrap_or(&f.code).to_string();
                return self.add_block(Block::Code { language: language.clone(), text });
            }
            Open::Quote => return if f.blocks.is_empty() { Ok(()) } else { self.add_block(Block::Quote(f.blocks)) },
            Open::Item => {
                return match self.stack.last_mut() {
                    Some(p) if matches!(p.open, Open::List { .. }) => {
                        p.items.push(f.blocks);
                        Ok(())
                    }
                    _ => f.blocks.into_iter().try_for_each(|b| self.add_block(b)),
                };
            }
            Open::List { ordered, start } => {
                let (ordered, start) = (*ordered, *start);
                return if f.items.is_empty() { Ok(()) } else { self.add_block(Block::List { ordered, start, items: f.items }) };
            }
            Open::Table => return if f.rows.is_empty() { Ok(()) } else { self.add_block(Block::Table { header_rows: f.header_rows, rows: f.rows }) },
            Open::Row { header } => {
                let header = *header;
                if let Some(t) = self.stack.last_mut()
                    && t.open == Open::Table
                    && !f.cells.is_empty()
                {
                    if header && t.header_rows == t.rows.len() {
                        t.header_rows += 1;
                    }
                    t.rows.push(f.cells);
                }
                return Ok(());
            }
            Open::Cell => {
                if let Some(r) = self.stack.last_mut()
                    && matches!(r.open, Open::Row { .. })
                {
                    r.cells.push(trim(f.inlines));
                }
                return Ok(());
            }
        };
        if let Some(i) = inline {
            self.push_inline(i)?;
        }
        Ok(())
    }
}

/// A block's text as inlines (for a block that lands in a table cell).
fn block_inlines(b: Block) -> Vec<Inline> {
    match b {
        Block::Heading { text, .. } | Block::Paragraph(text) => text,
        Block::Code { text, .. } | Block::Raw { text, .. } => vec![Inline::Code(text)],
        Block::Image { reference, alt } => vec![Inline::Image { reference, alt }],
        Block::List { items, .. } => items.into_iter().flatten().flat_map(block_inlines).collect(),
        Block::Quote(bs) => bs.into_iter().flat_map(block_inlines).collect(),
        Block::Table { rows, .. } => rows.into_iter().flatten().flatten().collect(),
        _ => Vec::new(),
    }
}

/// Inlines with leading and trailing whitespace (and line breaks) taken off, and
/// neighbouring text joined.
fn trim(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::with_capacity(inlines.len());
    for i in inlines {
        match (out.last_mut(), i) {
            (Some(Inline::Text(a)), Inline::Text(b)) => a.push_str(&b),
            (_, i) => out.push(i),
        }
    }
    while matches!(out.first(), Some(Inline::LineBreak)) {
        out.remove(0);
    }
    while matches!(out.last(), Some(Inline::LineBreak)) {
        out.pop();
    }
    if let Some(Inline::Text(t)) = out.first_mut() {
        *t = t.trim_start().to_string();
    }
    if let Some(Inline::Text(t)) = out.last_mut() {
        *t = t.trim_end().to_string();
    }
    out.retain(|i| !matches!(i, Inline::Text(t) if t.is_empty()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Vec<Inline> {
        vec![Inline::Text(s.into())]
    }

    #[test]
    fn nesting_and_leniency() {
        let cx = ParseContext::new();
        let mut b = Builder::new(&cx);
        b.open(Open::Heading(1)).unwrap();
        b.text("Title").unwrap();
        b.close(&Open::Heading(0)).unwrap();
        b.text("loose ").unwrap();
        b.open(Open::Strong).unwrap();
        b.text("bold").unwrap();
        b.close(&Open::Strong).unwrap();
        // An item with no list opens one; a block inside a paragraph ends it.
        b.open(Open::Item).unwrap();
        b.text("one").unwrap();
        b.close(&Open::Item).unwrap();
        b.open(Open::Item).unwrap();
        b.text("two").unwrap();
        b.finish_test(vec![
            Block::Heading { level: 1, text: text("Title") },
            Block::Paragraph(vec![Inline::Text("loose ".into()), Inline::Strong(text("bold"))]),
            Block::List { ordered: false, start: 1, items: vec![vec![Block::Paragraph(text("one"))], vec![Block::Paragraph(text("two"))]] },
        ]);
    }

    #[test]
    fn tables_and_code() {
        let cx = ParseContext::new();
        let mut b = Builder::new(&cx);
        b.open(Open::Row { header: true }).unwrap();
        b.open(Open::Cell).unwrap();
        b.text("a").unwrap();
        b.open(Open::Cell).unwrap();
        b.text("b").unwrap();
        b.open(Open::Row { header: false }).unwrap();
        b.open(Open::Cell).unwrap();
        b.text("1").unwrap();
        b.close(&Open::Table).unwrap();
        b.open(Open::Code(Some("rust".into()))).unwrap();
        b.text("fn x() {}\n").unwrap();
        b.finish_test(vec![
            Block::Table { header_rows: 1, rows: vec![vec![text("a"), text("b")], vec![text("1")]] },
            Block::Code { language: Some("rust".into()), text: "fn x() {}".into() },
        ]);
    }

    #[test]
    fn depth_is_limited() {
        let mut limits = libfileparser_core::Limits::desktop();
        limits.depth = 3;
        let cx = ParseContext::new().with_limits(limits);
        let mut b = Builder::new(&cx);
        let r: Result<()> = (0..10).try_for_each(|_| b.open(Open::Quote));
        assert!(matches!(r, Err(libfileparser_core::ParseError::TooLarge(_))));
    }

    impl Builder<'_> {
        fn finish_test(self, want: Vec<Block>) {
            assert_eq!(self.finish().unwrap(), want);
        }
    }
}
