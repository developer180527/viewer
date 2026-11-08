//! Keeps the events a reader sends properly nested, with the leniency real documents
//! need: a new item ends the one before it, a block ends the paragraph it's in, a row
//! ends the row before it, and closing a kind that isn't open does nothing.

use crate::{Error, Sink, Tag};

pub(crate) struct Nest<'s, S: Sink> {
    s: &'s mut S,
    stack: Vec<Tag>,
    depth: usize,
}

fn same(a: &Tag, b: &Tag) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

fn inline(t: &Tag) -> bool {
    matches!(t, Tag::Strong | Tag::Emphasis | Tag::Underline | Tag::Strikethrough | Tag::Superscript | Tag::Subscript | Tag::Link(_))
}

impl<'s, S: Sink> Nest<'s, S> {
    pub fn new(s: &'s mut S, depth: usize) -> Self {
        Self { s, stack: Vec::new(), depth }
    }

    fn pop(&mut self) -> Result<(), Error> {
        if let Some(t) = self.stack.pop() {
            self.s.close(t)?;
        }
        Ok(())
    }

    fn top_is(&self, f: impl Fn(&Tag) -> bool) -> bool {
        self.stack.last().is_some_and(f)
    }

    pub fn is_open(&self, kind: &Tag) -> bool {
        self.stack.iter().any(|t| same(t, kind))
    }

    pub fn open(&mut self, tag: Tag) -> Result<(), Error> {
        if !inline(&tag) {
            // Blocks don't go inside paragraphs, headings or marks.
            while self.top_is(|t| inline(t) || matches!(t, Tag::Paragraph | Tag::Heading(_))) {
                self.pop()?;
            }
            match &tag {
                Tag::Item => {
                    while self.top_is(|t| *t == Tag::Item) {
                        self.pop()?;
                    }
                }
                Tag::Row if self.is_open(&Tag::Table) => {
                    while !self.top_is(|t| *t == Tag::Table) {
                        self.pop()?;
                    }
                }
                Tag::Cell if self.is_open(&Tag::Row) => {
                    while !self.top_is(|t| *t == Tag::Row) {
                        self.pop()?;
                    }
                }
                _ => {}
            }
        }
        if self.stack.len() >= self.depth {
            return Err(Error::TooDeep(self.depth));
        }
        self.s.open(tag.clone())?;
        self.stack.push(tag);
        Ok(())
    }

    /// Closes the innermost open tag of `kind`'s kind, and everything inside it.
    pub fn close(&mut self, kind: &Tag) -> Result<(), Error> {
        if let Some(at) = self.stack.iter().rposition(|t| same(t, kind)) {
            while self.stack.len() > at {
                self.pop()?;
            }
        }
        Ok(())
    }

    pub fn text(&mut self, text: &str) -> Result<(), Error> {
        Ok(self.s.text(text)?)
    }

    pub fn line_break(&mut self) -> Result<(), Error> {
        Ok(self.s.line_break()?)
    }

    pub fn page_break(&mut self) -> Result<(), Error> {
        while self.top_is(|t| inline(t) || matches!(t, Tag::Paragraph | Tag::Heading(_))) {
            self.pop()?;
        }
        Ok(self.s.page_break()?)
    }

    pub fn image(&mut self, part: &str, alt: &str) -> Result<(), Error> {
        Ok(self.s.image(part, alt)?)
    }

    pub fn note(&mut self, id: &str) -> Result<(), Error> {
        Ok(self.s.note(id)?)
    }

    /// Closes everything still open.
    pub fn finish(&mut self) -> Result<(), Error> {
        while !self.stack.is_empty() {
            self.pop()?;
        }
        Ok(())
    }
}
