//! Our own format libraries (`rtf-lite`, `docx-lite`, `odt-lite`) send events; this
//! builds them into formatted text. A limit or cancellation the builder hits stops the
//! library, and the error is kept to return.

use libfileparser_core::{Block, ParseError, Result};

use crate::build::{Builder, Open};

pub(crate) struct ToFlow<'a> {
    pub b: Builder<'a>,
    pub error: Option<ParseError>,
    n: u64,
}

impl<'a> ToFlow<'a> {
    pub fn new(b: Builder<'a>) -> Self {
        Self { b, error: None, n: 0 }
    }

    /// The error a stopped library leaves: the builder's, else cancellation.
    pub fn stopped(&mut self) -> ParseError {
        self.error.take().unwrap_or(ParseError::Cancelled)
    }

    fn go<S>(&mut self, r: Result<()>, stop: S) -> std::result::Result<(), S> {
        self.n += 1;
        let r = if self.n.is_multiple_of(4096) { r.and_then(|_| self.b.cancel_check()) } else { r };
        r.map_err(|e| {
            self.error = Some(e);
            stop
        })
    }
}

macro_rules! sink {
    ($lib:ident, $map:ident) => {
        impl $lib::Sink for ToFlow<'_> {
            fn open(&mut self, tag: $lib::Tag) -> std::result::Result<(), $lib::Stop> {
                let r = self.b.open($map(tag));
                self.go(r, $lib::Stop)
            }
            fn close(&mut self, tag: $lib::Tag) -> std::result::Result<(), $lib::Stop> {
                let r = self.b.close(&$map(tag));
                self.go(r, $lib::Stop)
            }
            fn text(&mut self, text: &str) -> std::result::Result<(), $lib::Stop> {
                let r = self.b.text(text);
                self.go(r, $lib::Stop)
            }
            fn line_break(&mut self) -> std::result::Result<(), $lib::Stop> {
                let r = self.b.line_break();
                self.go(r, $lib::Stop)
            }
            sink!(@extra $lib);
        }
    };
    (@page $lib:ident) => {
        fn page_break(&mut self) -> std::result::Result<(), $lib::Stop> {
            let r = self.b.block(Block::Break);
            self.go(r, $lib::Stop)
        }
    };
    (@image $lib:ident) => {
        fn image(&mut self, part: &str, alt: &str) -> std::result::Result<(), $lib::Stop> {
            let r = self.b.image(part, alt);
            self.go(r, $lib::Stop)
        }
    };
    (@extra rtf_lite) => {
        sink!(@page rtf_lite);
    };
    (@extra docx_lite) => {
        sink!(@page docx_lite);
        sink!(@image docx_lite);
        fn note(&mut self, id: &str) -> std::result::Result<(), docx_lite::Stop> {
            let r = self.b.note(id);
            self.go(r, docx_lite::Stop)
        }
    };
    (@extra odt_lite) => {
        sink!(@image odt_lite);
    };
}

fn rtf_open(tag: rtf_lite::Tag) -> Open {
    use rtf_lite::Tag;
    match tag {
        Tag::Heading(l) => Open::Heading(l),
        Tag::Table => Open::Table,
        Tag::Row => Open::Row { header: false },
        Tag::Cell => Open::Cell,
        Tag::Strong => Open::Strong,
        Tag::Emphasis => Open::Emphasis,
        Tag::Underline => Open::Underline,
        Tag::Strikethrough => Open::Strikethrough,
        Tag::Superscript => Open::Superscript,
        Tag::Subscript => Open::Subscript,
        Tag::Link(t) => Open::Link(t),
        _ => Open::Paragraph,
    }
}

fn docx_open(tag: docx_lite::Tag) -> Open {
    use docx_lite::Tag;
    match tag {
        Tag::Heading(l) => Open::Heading(l),
        Tag::List { ordered } => Open::List { ordered, start: 1 },
        Tag::Item => Open::Item,
        Tag::Table => Open::Table,
        Tag::Row => Open::Row { header: false },
        Tag::Cell => Open::Cell,
        Tag::Strong => Open::Strong,
        Tag::Emphasis => Open::Emphasis,
        Tag::Underline => Open::Underline,
        Tag::Strikethrough => Open::Strikethrough,
        Tag::Superscript => Open::Superscript,
        Tag::Subscript => Open::Subscript,
        Tag::Link(t) => Open::Link(t),
        _ => Open::Paragraph,
    }
}

fn odt_open(tag: odt_lite::Tag) -> Open {
    use odt_lite::Tag;
    match tag {
        Tag::Heading(l) => Open::Heading(l),
        Tag::List { ordered } => Open::List { ordered, start: 1 },
        Tag::Item => Open::Item,
        Tag::Table => Open::Table,
        Tag::Row { header } => Open::Row { header },
        Tag::Cell => Open::Cell,
        Tag::Strong => Open::Strong,
        Tag::Emphasis => Open::Emphasis,
        Tag::Underline => Open::Underline,
        Tag::Strikethrough => Open::Strikethrough,
        Tag::Superscript => Open::Superscript,
        Tag::Subscript => Open::Subscript,
        Tag::Link(t) => Open::Link(t),
        _ => Open::Paragraph,
    }
}

sink!(rtf_lite, rtf_open);
sink!(docx_lite, docx_open);
sink!(odt_lite, odt_open);
