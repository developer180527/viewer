//! OpenDocument text (ODT, OTT: LibreOffice, Google Docs and others), read: a small,
//! embeddable reader of an ODT's text and structure.
//!
//! It reads headings, paragraphs, bold, italic, underline, strikethrough, superscript
//! and subscript (from the document's styles, and the styles they're based on),
//! numbered and bulleted lists (by their list style), tables with their header rows,
//! links, images (as the package paths of their parts), line breaks, tabs and spaces,
//! and the title, author, subject, page and word counts. Notes' bodies, tracked
//! deletions and annotations aren't text. Unknown elements are skipped, never a reason
//! to refuse the document.
//!
//! What it reads arrives as events in a [`Sink`] the caller implements, properly
//! nested, so it builds whatever document model the caller has:
//!
//! ```no_run
//! struct Plain(String);
//! impl odt_lite::Sink for Plain {
//!     fn text(&mut self, text: &str) -> Result<(), odt_lite::Stop> {
//!         self.0.push_str(text);
//!         Ok(())
//!     }
//! }
//! let bytes = std::fs::read("letter.odt").unwrap();
//! let mut out = Plain(String::new());
//! let info = odt_lite::parse(&bytes, &mut out, &odt_lite::Limits::default()).unwrap();
//! println!("{:?}: {}", info.title, out.0);
//! ```

mod nest;
mod package;

use std::collections::HashMap;
use std::fmt;

use quick_xml::Reader as XmlReader;
use quick_xml::events::Event;

use nest::Nest;
use package::{Package, attr, local, resolve, unescape};

/// Something opened and later closed. Events nest properly: everything opened inside a
/// tag is closed before it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Tag {
    Paragraph,
    /// A heading, level 1 (largest) to 6.
    Heading(u8),
    List { ordered: bool },
    Item,
    Table,
    /// A table row; `header` rows are the table's header.
    Row { header: bool },
    Cell,
    Strong,
    Emphasis,
    Underline,
    Strikethrough,
    Superscript,
    Subscript,
    Link(String),
}

/// A sink's answer when it wants reading to stop (a limit it keeps, the user closing
/// the document).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop;

/// Where what's read goes. Every method has a default that ignores it.
pub trait Sink {
    fn open(&mut self, tag: Tag) -> Result<(), Stop> {
        let _ = tag;
        Ok(())
    }
    fn close(&mut self, tag: Tag) -> Result<(), Stop> {
        let _ = tag;
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<(), Stop> {
        let _ = text;
        Ok(())
    }
    fn line_break(&mut self) -> Result<(), Stop> {
        Ok(())
    }
    /// An image: `part` is its path in the package (`Pictures/1000.png`), or a URL.
    fn image(&mut self, part: &str, alt: &str) -> Result<(), Stop> {
        let _ = (part, alt);
        Ok(())
    }
}

/// How much a file may cost.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Entries in the zip.
    pub entries: usize,
    /// Bytes of one part, unpacked.
    pub part_bytes: u64,
    /// A part may unpack to this many times its packed size…
    pub unpack_ratio: u64,
    /// …or to this many bytes, whichever is more.
    pub unpack_always: u64,
    /// Nesting of lists, tables and marks.
    pub depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { entries: 100_000, part_bytes: 512 << 20, unpack_ratio: 200, unpack_always: 64 << 20, depth: 512 }
    }
}

impl Limits {
    pub fn with_entries(mut self, n: usize) -> Self {
        self.entries = n;
        self
    }
    pub fn with_part_bytes(mut self, n: u64) -> Self {
        self.part_bytes = n;
        self
    }
    pub fn with_unpack(mut self, ratio: u64, always: u64) -> Self {
        (self.unpack_ratio, self.unpack_always) = (ratio, always);
        self
    }
    pub fn with_depth(mut self, n: usize) -> Self {
        self.depth = n;
        self
    }
}

/// What the document says about itself (`meta.xml`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Info {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub pages: Option<u64>,
    pub words: Option<u64>,
}

/// Why reading stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A zip, but not an OpenDocument text (no `content.xml`).
    NotOdt,
    /// Not a readable zip.
    Zip(String),
    /// A part's XML is broken.
    Xml { part: String, message: String },
    /// More than [`Limits`] allow.
    TooLarge { what: &'static str, value: u64, max: u64 },
    /// Nesting deeper than [`Limits::depth`].
    TooDeep(usize),
    /// The sink asked to stop.
    Stopped,
}

impl From<Stop> for Error {
    fn from(_: Stop) -> Self {
        Error::Stopped
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotOdt => f.write_str("not an OpenDocument text"),
            Error::Zip(e) => write!(f, "zip: {e}"),
            Error::Xml { part, message } => write!(f, "{part}: {message}"),
            Error::TooLarge { what, value, max } => write!(f, "too large: {value} {what}, more than {max}"),
            Error::TooDeep(d) => write!(f, "nested deeper than {d}"),
            Error::Stopped => f.write_str("stopped"),
        }
    }
}

impl std::error::Error for Error {}

/// Reads the OpenDocument text in `odt` (the file's bytes) into `sink`, within `limits`.
pub fn parse<S: Sink>(odt: &[u8], sink: &mut S, limits: &Limits) -> Result<Info, Error> {
    let pkg = Package::open(odt, limits)?;
    let content = pkg.text("content.xml")?.ok_or(Error::NotOdt)?;
    let mut styles = Styles::default();
    if let Some(s) = pkg.text("styles.xml")? {
        styles.read(&s);
    }
    styles.read(&content);
    let mut nest = Nest::new(sink, limits.depth);
    body(&content, &styles, &mut nest)?;
    let mut info = Info::default();
    if let Some(meta) = pkg.text("meta.xml")? {
        for (k, v) in meta_fields(&meta) {
            match k {
                "Title" => info.title = Some(v),
                "Author" => info.author = Some(v),
                "Subject" => info.subject = Some(v),
                "Pages" => info.pages = v.parse().ok(),
                _ => info.words = v.parse().ok(),
            }
        }
    }
    Ok(info)
}

/// Text styles' marks, paragraph styles that are headings, and list styles that number.
#[derive(Default)]
struct Styles {
    marks: HashMap<String, Vec<Tag>>,
    headings: HashMap<String, u8>,
    ordered: HashMap<String, bool>,
    parents: HashMap<String, String>,
}

impl Styles {
    fn read(&mut self, xml: &str) {
        let mut r = XmlReader::from_str(xml);
        let mut style: Option<String> = None;
        let mut list: Option<String> = None;
        loop {
            match r.read_event() {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(&e).as_str() {
                    "style" => {
                        style = attr(&e, "style:name").or_else(|| attr(&e, "name"));
                        if let Some(s) = &style {
                            if let Some(p) = attr(&e, "style:parent-style-name") {
                                self.parents.insert(s.clone(), p);
                            }
                            if let Some(l) = attr(&e, "style:default-outline-level").and_then(|v| v.parse::<u8>().ok()) {
                                self.headings.insert(s.clone(), l.clamp(1, 6));
                            }
                        }
                    }
                    "text-properties" => {
                        if let Some(s) = &style {
                            let mut m = Vec::new();
                            if attr(&e, "fo:font-weight").is_some_and(|v| v == "bold" || v.parse::<u32>().is_ok_and(|w| w >= 600)) {
                                m.push(Tag::Strong);
                            }
                            if attr(&e, "fo:font-style").is_some_and(|v| v == "italic" || v == "oblique") {
                                m.push(Tag::Emphasis);
                            }
                            if attr(&e, "style:text-underline-style").is_some_and(|v| v != "none") {
                                m.push(Tag::Underline);
                            }
                            if attr(&e, "style:text-line-through-style").is_some_and(|v| v != "none") {
                                m.push(Tag::Strikethrough);
                            }
                            match attr(&e, "style:text-position").as_deref().and_then(|v| v.split_whitespace().next().map(str::to_string)).as_deref() {
                                Some("super") => m.push(Tag::Superscript),
                                Some("sub") => m.push(Tag::Subscript),
                                Some(v) if v.trim_end_matches('%').parse::<f32>().is_ok_and(|p| p > 0.0) => m.push(Tag::Superscript),
                                Some(v) if v.trim_end_matches('%').parse::<f32>().is_ok_and(|p| p < 0.0) => m.push(Tag::Subscript),
                                _ => {}
                            }
                            if !m.is_empty() {
                                self.marks.insert(s.clone(), m);
                            }
                        }
                    }
                    "list-style" => list = attr(&e, "style:name").or_else(|| attr(&e, "name")),
                    "list-level-style-number" => {
                        if let Some(l) = &list {
                            self.ordered.entry(l.clone()).or_insert(true);
                        }
                    }
                    "list-level-style-bullet" => {
                        if let Some(l) = &list {
                            self.ordered.entry(l.clone()).or_insert(false);
                        }
                    }
                    _ => {}
                },
                Ok(Event::End(e)) if e.local_name().as_ref() == b"style" => style = None,
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }

    /// A text style's marks, its parents' included.
    fn marks(&self, name: &str) -> Vec<Tag> {
        let mut at = name.to_string();
        for _ in 0..8 {
            if let Some(m) = self.marks.get(&at) {
                return m.clone();
            }
            match self.parents.get(&at) {
                Some(p) => at = p.clone(),
                None => break,
            }
        }
        Vec::new()
    }

    fn heading(&self, name: &str) -> Option<u8> {
        let mut at = name.to_string();
        for _ in 0..8 {
            if let Some(&l) = self.headings.get(&at) {
                return Some(l);
            }
            if let Some(l) = at.strip_prefix("Heading_20_").and_then(|d| d.parse::<u8>().ok()) {
                return Some(l.clamp(1, 6));
            }
            match self.parents.get(&at) {
                Some(p) => at = p.clone(),
                None => break,
            }
        }
        None
    }
}

fn body<S: Sink>(xml: &str, styles: &Styles, b: &mut Nest<S>) -> Result<(), Error> {
    let mut r = XmlReader::from_str(xml);
    r.config_mut().trim_text(false);
    // Marks each open span set, to close at its end.
    let mut spans: Vec<Vec<Tag>> = Vec::new();
    // Inside a note's body, a tracked deletion, or an annotation: not shown.
    let mut hidden = 0usize;
    let mut in_body = false;
    let mut header_rows = false;
    let mut alt = String::new();
    loop {
        let ev = match r.read_event() {
            Ok(e) => e,
            Err(e) => return Err(Error::Xml { part: "content.xml".into(), message: e.to_string() }),
        };
        let empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let name = local(&e);
                if name == "text" && !empty && !in_body && e.name().as_ref().starts_with(b"office:") {
                    in_body = true;
                    continue;
                }
                if !in_body {
                    continue;
                }
                if hidden > 0 {
                    if !empty {
                        hidden += 1;
                    }
                    continue;
                }
                match name.as_str() {
                    "note-body" | "annotation" | "tracked-changes" | "deletion" | "sequence-decls" if !empty => hidden = 1,
                    "h" if !empty => {
                        let level = attr(&e, "text:outline-level").and_then(|v| v.parse::<u8>().ok()).unwrap_or(1);
                        b.open(Tag::Heading(level.clamp(1, 6)))?;
                    }
                    "p" if !empty => match attr(&e, "text:style-name").and_then(|s| styles.heading(&s)) {
                        Some(l) => b.open(Tag::Heading(l))?,
                        None => b.open(Tag::Paragraph)?,
                    },
                    "span" if !empty => {
                        let m = attr(&e, "text:style-name").map(|s| styles.marks(&s)).unwrap_or_default();
                        for o in &m {
                            b.open(o.clone())?;
                        }
                        spans.push(m);
                    }
                    "a" if !empty => b.open(Tag::Link(attr(&e, "xlink:href").unwrap_or_default()))?,
                    "list" if !empty => {
                        let ordered = attr(&e, "text:style-name").and_then(|s| styles.ordered.get(&s).copied()).unwrap_or(false);
                        b.open(Tag::List { ordered })?;
                    }
                    "list-item" | "list-header" if !empty => b.open(Tag::Item)?,
                    "table" if !empty => b.open(Tag::Table)?,
                    "table-header-rows" if !empty => header_rows = true,
                    "table-row" if !empty => b.open(Tag::Row { header: header_rows })?,
                    "table-cell" => b.open(Tag::Cell)?,
                    "line-break" => b.line_break()?,
                    "tab" => b.text("\t")?,
                    "s" => b.text(&" ".repeat(attr(&e, "text:c").and_then(|c| c.parse::<usize>().ok()).unwrap_or(1).min(64)))?,
                    "note-citation" if !empty => {}
                    "title" | "desc" if !empty => {}
                    "frame" => alt = attr(&e, "svg:name").unwrap_or_default(),
                    "image" => {
                        if let Some(href) = attr(&e, "xlink:href")
                            && !href.is_empty()
                        {
                            let reference = if href.contains("://") { href } else { resolve("content.xml", &href) };
                            b.image(&reference, &std::mem::take(&mut alt))?;
                        }
                    }
                    "soft-page-break" => {}
                    _ => {}
                }
            }
            Event::End(e) => {
                let n = e.local_name();
                let name = String::from_utf8_lossy(n.as_ref()).into_owned();
                if hidden > 0 {
                    hidden -= 1;
                    continue;
                }
                if !in_body {
                    continue;
                }
                match name.as_str() {
                    "text" if e.name().as_ref().starts_with(b"office:") => in_body = false,
                    "h" => b.close(&Tag::Heading(0))?,
                    "p" => {
                        b.close(&Tag::Heading(0))?;
                        b.close(&Tag::Paragraph)?;
                    }
                    "span" => {
                        for o in spans.pop().unwrap_or_default().iter().rev() {
                            b.close(o)?;
                        }
                    }
                    "a" => b.close(&Tag::Link(String::new()))?,
                    "list" => b.close(&Tag::List { ordered: false })?,
                    "list-item" | "list-header" => b.close(&Tag::Item)?,
                    "table" => b.close(&Tag::Table)?,
                    "table-header-rows" => header_rows = false,
                    "table-row" => b.close(&Tag::Row { header: false })?,
                    "table-cell" => b.close(&Tag::Cell)?,
                    _ => {}
                }
            }
            Event::Text(t) => {
                if in_body && hidden == 0 {
                    b.text(&unescape(&String::from_utf8_lossy(t.as_ref())))?;
                }
            }
            Event::GeneralRef(g) if in_body && hidden == 0 => b.text(&unescape(&format!("&{};", String::from_utf8_lossy(g.as_ref()))))?,
            _ => {}
        }
    }
    b.finish()
}

/// Title, author, subject, pages and words from `meta.xml`.
fn meta_fields(xml: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let mut r = XmlReader::from_str(xml);
    let mut field: Option<&'static str> = None;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                field = match (e.name().as_ref().split(|&c| c == b':').next().unwrap_or(b""), local(&e).as_str()) {
                    (b"dc", "title") => Some("Title"),
                    (b"dc", "creator") | (b"meta", "initial-creator") => Some("Author"),
                    (b"dc", "subject") => Some("Subject"),
                    _ => None,
                };
            }
            Ok(Event::Empty(e)) if local(&e) == "document-statistic" => {
                for (k, a) in [("Pages", "meta:page-count"), ("Words", "meta:word-count")] {
                    if let Some(v) = attr(&e, a) {
                        out.push((k, v));
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(f) = field.take() {
                    let v = unescape(String::from_utf8_lossy(t.as_ref()).trim());
                    if !v.is_empty() && !out.iter().any(|(k, _)| *k == f) {
                        out.push((f, v));
                    }
                }
            }
            Ok(Event::End(_)) => field = None,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}
