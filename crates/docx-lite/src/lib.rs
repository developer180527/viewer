//! Word documents (DOCX, DOCM, DOTX, DOTM: Office Open XML), read: a small, embeddable
//! reader of a Word document's text and structure.
//!
//! It reads headings (by style, the style a style is based on, or outline level),
//! paragraphs, bold, italic, underline, strikethrough, superscript and subscript,
//! bulleted and numbered lists (from the numbering part, nested by level), tables,
//! links, images (as the package paths of their parts), line and page breaks, footnote
//! marks, and the title, author and subject. Tracked deletions, field instructions and
//! comments aren't text. It is lenient: an element or value it doesn't know is skipped,
//! never a reason to refuse the document.
//!
//! What it reads arrives as events in a [`Sink`] the caller implements, properly nested,
//! so it builds whatever document model the caller has:
//!
//! ```no_run
//! struct Plain(String);
//! impl docx_lite::Sink for Plain {
//!     fn text(&mut self, text: &str) -> Result<(), docx_lite::Stop> {
//!         self.0.push_str(text);
//!         Ok(())
//!     }
//! }
//! let bytes = std::fs::read("report.docx").unwrap();
//! let mut out = Plain(String::new());
//! let info = docx_lite::parse(&bytes, &mut out, &docx_lite::Limits::default()).unwrap();
//! println!("{:?}: {}", info.title, out.0);
//! ```

mod nest;
mod package;

use std::collections::HashMap;
use std::fmt;

use quick_xml::Reader as XmlReader;
use quick_xml::events::{BytesStart, Event};

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
    Row,
    Cell,
    Strong,
    Emphasis,
    Underline,
    Strikethrough,
    Superscript,
    Subscript,
    /// A link: an external address, or `#bookmark`.
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
    fn page_break(&mut self) -> Result<(), Stop> {
        Ok(())
    }
    /// An image: `part` is its path in the package (`word/media/image1.png`).
    fn image(&mut self, part: &str, alt: &str) -> Result<(), Stop> {
        let _ = (part, alt);
        Ok(())
    }
    /// A footnote or endnote mark, by its id.
    fn note(&mut self, id: &str) -> Result<(), Stop> {
        let _ = id;
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

/// What the document says about itself (`docProps/core.xml`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Info {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub last_edited_by: Option<String>,
}

/// Why reading stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A zip, but not a Word document (no main document part).
    NotDocx,
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
            Error::NotDocx => f.write_str("not a Word document"),
            Error::Zip(e) => write!(f, "zip: {e}"),
            Error::Xml { part, message } => write!(f, "{part}: {message}"),
            Error::TooLarge { what, value, max } => write!(f, "too large: {value} {what}, more than {max}"),
            Error::TooDeep(d) => write!(f, "nested deeper than {d}"),
            Error::Stopped => f.write_str("stopped"),
        }
    }
}

impl std::error::Error for Error {}

/// Reads the Word document in `docx` (the file's bytes) into `sink`, within `limits`.
pub fn parse<S: Sink>(docx: &[u8], sink: &mut S, limits: &Limits) -> Result<Info, Error> {
    let pkg = Package::open(docx, limits)?;
    let main = main_part(&pkg)?.ok_or(Error::NotDocx)?;
    let xml = pkg.text(&main)?.ok_or(Error::NotDocx)?;
    let rels = relationships(&pkg, &main)?;
    let styles = Styles::read(&pkg, &main)?;
    let numbering = numbering(&pkg, &main, &rels)?;
    let mut nest = Nest::new(sink, limits.depth);
    body(&xml, &main, &rels, &styles, &numbering, &mut nest)?;
    let mut info = Info::default();
    for (k, v) in core_properties(&pkg)? {
        let slot = match k {
            "Title" => &mut info.title,
            "Author" => &mut info.author,
            "Subject" => &mut info.subject,
            _ => &mut info.last_edited_by,
        };
        *slot = Some(v);
    }
    Ok(info)
}

/// The main document part, from the package's relationships (`word/document.xml`
/// almost always).
fn main_part(pkg: &Package) -> Result<Option<String>, Error> {
    if let Some(rels) = pkg.text("_rels/.rels")? {
        let mut r = XmlReader::from_str(&rels);
        loop {
            match r.read_event() {
                Ok(Event::Start(e) | Event::Empty(e)) if local(&e) == "Relationship" => {
                    if attr(&e, "Type").is_some_and(|t| t.ends_with("/officeDocument"))
                        && let Some(t) = attr(&e, "Target")
                    {
                        return Ok(Some(resolve("", &t)));
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    Ok(pkg.has("word/document.xml").then(|| "word/document.xml".to_string()))
}

/// A part's relationships: id → (target, external?, type's last word).
type Rels = HashMap<String, (String, bool, String)>;

fn relationships(pkg: &Package, part: &str) -> Result<Rels, Error> {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    let path = if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") };
    let mut out = HashMap::new();
    if let Some(xml) = pkg.text(&path)? {
        let mut r = XmlReader::from_str(&xml);
        loop {
            match r.read_event() {
                Ok(Event::Start(e) | Event::Empty(e)) if local(&e) == "Relationship" => {
                    if let (Some(id), Some(target)) = (attr(&e, "Id"), attr(&e, "Target")) {
                        let external = attr(&e, "TargetMode").as_deref() == Some("External");
                        let kind = attr(&e, "Type").and_then(|t| t.rsplit('/').next().map(str::to_string)).unwrap_or_default();
                        out.insert(id, (if external { target } else { resolve(part, &target) }, external, kind));
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    Ok(out)
}

/// Paragraph styles: id → heading level (from the style's name, its outline level, or
/// the style it's based on).
struct Styles(HashMap<String, u8>);

impl Styles {
    fn read(pkg: &Package, main: &str) -> Result<Self, Error> {
        let path = resolve(main, "styles.xml");
        let mut levels = HashMap::new();
        let mut based: HashMap<String, String> = HashMap::new();
        if let Some(xml) = pkg.text(&path)? {
            let mut r = XmlReader::from_str(&xml);
            let mut id: Option<String> = None;
            loop {
                match r.read_event() {
                    Ok(Event::Start(e) | Event::Empty(e)) => match local(&e).as_str() {
                        "style" => id = attr(&e, "styleId"),
                        "name" => {
                            if let (Some(id), Some(name)) = (&id, attr(&e, "val"))
                                && let Some(l) = heading_name(&name)
                            {
                                levels.insert(id.clone(), l);
                            }
                        }
                        "outlineLvl" => {
                            if let (Some(id), Some(v)) = (&id, attr(&e, "val").and_then(|v| v.parse::<u8>().ok()))
                                && v < 9
                            {
                                levels.entry(id.clone()).or_insert((v + 1).min(6));
                            }
                        }
                        "basedOn" => {
                            if let (Some(id), Some(v)) = (&id, attr(&e, "val")) {
                                based.insert(id.clone(), v);
                            }
                        }
                        _ => {}
                    },
                    Ok(Event::Eof) | Err(_) => break,
                    _ => {}
                }
            }
        }
        // A style based on a heading is one (a few steps at most, cycles stop).
        for id in based.keys().cloned().collect::<Vec<_>>() {
            let mut at = id.clone();
            for _ in 0..8 {
                if let Some(&l) = levels.get(&at) {
                    levels.entry(id.clone()).or_insert(l);
                    break;
                }
                match based.get(&at) {
                    Some(b) => at = b.clone(),
                    None => break,
                }
            }
        }
        Ok(Self(levels))
    }

    fn level(&self, id: &str) -> Option<u8> {
        self.0.get(id).copied().or_else(|| heading_name(id))
    }
}

/// "heading 2", "Heading2", "Title" → a level.
fn heading_name(name: &str) -> Option<u8> {
    let n = name.to_ascii_lowercase().replace(' ', "");
    if n == "title" {
        return Some(1);
    }
    n.strip_prefix("heading").and_then(|d| d.parse::<u8>().ok()).filter(|l| (1..=9).contains(l)).map(|l| l.min(6))
}

/// Lists: (numId, level) → ordered?
type Numbering = HashMap<(String, u8), bool>;

fn numbering(pkg: &Package, main: &str, rels: &Rels) -> Result<Numbering, Error> {
    let path = rels.values().find(|r| r.2 == "numbering").map(|r| r.0.clone()).unwrap_or_else(|| resolve(main, "numbering.xml"));
    let mut abstracts: HashMap<String, HashMap<u8, bool>> = HashMap::new();
    let mut nums: HashMap<String, String> = HashMap::new();
    if let Some(xml) = pkg.text(&path)? {
        let mut r = XmlReader::from_str(&xml);
        let (mut abs, mut lvl, mut num): (Option<String>, Option<u8>, Option<String>) = (None, None, None);
        loop {
            match r.read_event() {
                Ok(Event::Start(e) | Event::Empty(e)) => match local(&e).as_str() {
                    "abstractNum" => abs = attr(&e, "abstractNumId"),
                    "lvl" => lvl = attr(&e, "ilvl").and_then(|v| v.parse().ok()),
                    "numFmt" => {
                        if let (Some(a), Some(l)) = (&abs, lvl) {
                            let fmt = attr(&e, "val").unwrap_or_default();
                            abstracts.entry(a.clone()).or_default().insert(l, !matches!(fmt.as_str(), "bullet" | "none" | ""));
                        }
                    }
                    "num" => num = attr(&e, "numId"),
                    "abstractNumId" => {
                        if let (Some(n), Some(v)) = (&num, attr(&e, "val")) {
                            nums.insert(n.clone(), v);
                        }
                    }
                    _ => {}
                },
                Ok(Event::End(e)) if e.local_name().as_ref() == b"abstractNum" => abs = None,
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    let mut out = HashMap::new();
    for (num, abs) in nums {
        for (&l, &ordered) in abstracts.get(&abs).into_iter().flatten() {
            out.insert((num.clone(), l), ordered);
        }
    }
    Ok(out)
}

/// A run's marks, as `w:rPr` sets them.
#[derive(Clone, Copy, Default, PartialEq)]
struct Marks {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    sup: bool,
    sub: bool,
}

impl Marks {
    fn opens(&self) -> Vec<Tag> {
        let mut v = Vec::new();
        if self.bold {
            v.push(Tag::Strong);
        }
        if self.italic {
            v.push(Tag::Emphasis);
        }
        if self.underline {
            v.push(Tag::Underline);
        }
        if self.strike {
            v.push(Tag::Strikethrough);
        }
        if self.sup {
            v.push(Tag::Superscript);
        }
        if self.sub {
            v.push(Tag::Subscript);
        }
        v
    }
}

/// `w:b`, `w:i`… are on unless `w:val` says off.
fn on(e: &BytesStart) -> bool {
    !matches!(attr(e, "val").as_deref(), Some("0" | "false" | "off" | "none"))
}

/// Paragraph properties read before the paragraph's first run.
#[derive(Default)]
struct Para {
    style: Option<String>,
    outline: Option<u8>,
    num: Option<(String, u8)>,
    started: bool,
}

fn body<S: Sink>(xml: &str, main: &str, rels: &Rels, styles: &Styles, numbering: &Numbering, b: &mut Nest<S>) -> Result<(), Error> {
    let mut r = XmlReader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut para: Option<Para> = None;
    let mut marks = Marks::default();
    let mut in_rpr = false;
    let mut run_open: Vec<Tag> = Vec::new();
    let mut in_text = false;
    // Deleted text, field instructions and the fallback of alternate content aren't shown.
    let mut hidden = 0usize;
    // The list being continued: its numId, and how deep the builder's lists go.
    let mut list: Option<String> = None;
    let mut list_depth = 0u8;
    let mut image_alt = String::new();
    loop {
        let ev = match r.read_event() {
            Ok(e) => e,
            Err(e) => return Err(Error::Xml { part: main.to_string(), message: e.to_string() }),
        };
        let empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let name = local(&e);
                if hidden > 0 {
                    if !empty {
                        hidden += 1;
                    }
                    continue;
                }
                match name.as_str() {
                    "del" | "instrText" | "delText" | "Fallback" | "footnotes" | "comment" if !empty => hidden = 1,
                    "p" => para = Some(Para::default()),
                    "pStyle" => {
                        if let Some(p) = &mut para {
                            p.style = attr(&e, "val");
                        }
                    }
                    "outlineLvl" => {
                        if let Some(p) = &mut para {
                            p.outline = attr(&e, "val").and_then(|v| v.parse::<u8>().ok()).filter(|v| *v < 9).map(|v| v + 1);
                        }
                    }
                    "ilvl" => {
                        if let Some(p) = &mut para {
                            let l = attr(&e, "val").and_then(|v| v.parse().ok()).unwrap_or(0);
                            p.num = Some((p.num.take().map(|n| n.0).unwrap_or_default(), l));
                        }
                    }
                    "numId" => {
                        if let Some(p) = &mut para {
                            let id = attr(&e, "val").unwrap_or_default();
                            let l = p.num.as_ref().map_or(0, |n| n.1);
                            p.num = if id == "0" { None } else { Some((id, l)) };
                        }
                    }
                    "r" => {
                        start_para(b, &mut para, styles, numbering, &mut list, &mut list_depth)?;
                        marks = Marks::default();
                    }
                    "rPr" if !empty => in_rpr = true,
                    "b" | "bCs" if in_rpr => marks.bold = on(&e),
                    "i" | "iCs" if in_rpr => marks.italic = on(&e),
                    "u" if in_rpr => marks.underline = on(&e),
                    "strike" | "dstrike" if in_rpr => marks.strike = on(&e),
                    "vertAlign" if in_rpr => match attr(&e, "val").as_deref() {
                        Some("superscript") => marks.sup = true,
                        Some("subscript") => marks.sub = true,
                        _ => {}
                    },
                    "t" if !empty => {
                        open_marks(b, &marks, &mut run_open)?;
                        in_text = true;
                    }
                    "tab" if !in_rpr && para.is_some() => {
                        open_marks(b, &marks, &mut run_open)?;
                        b.text("\t")?;
                    }
                    "br" | "cr" => {
                        if attr(&e, "type").as_deref() == Some("page") {
                            close_para(b, &mut para, &mut run_open)?;
                            b.page_break()?;
                        } else {
                            start_para(b, &mut para, styles, numbering, &mut list, &mut list_depth)?;
                            b.line_break()?;
                        }
                    }
                    "hyperlink" => {
                        start_para(b, &mut para, styles, numbering, &mut list, &mut list_depth)?;
                        let target = attr(&e, "id").and_then(|id| rels.get(&id).map(|r| r.0.clone())).or_else(|| attr(&e, "anchor").map(|a| format!("#{a}")));
                        if let Some(t) = target {
                            close_marks(b, &mut run_open)?;
                            b.open(Tag::Link(t))?;
                        }
                    }
                    "docPr" => image_alt = attr(&e, "descr").or_else(|| attr(&e, "title")).unwrap_or_default(),
                    "blip" => {
                        if let Some(target) = attr(&e, "r:embed").or_else(|| attr(&e, "embed")).and_then(|id| rels.get(&id).map(|r| r.0.clone())) {
                            start_para(b, &mut para, styles, numbering, &mut list, &mut list_depth)?;
                            close_marks(b, &mut run_open)?;
                            b.image(&target, &std::mem::take(&mut image_alt))?;
                        }
                    }
                    "footnoteReference" | "endnoteReference" => {
                        start_para(b, &mut para, styles, numbering, &mut list, &mut list_depth)?;
                        b.note(&attr(&e, "id").unwrap_or_default())?;
                    }
                    "tbl" => {
                        close_para(b, &mut para, &mut run_open)?;
                        end_list(b, &mut list, &mut list_depth)?;
                        b.open(Tag::Table)?;
                    }
                    "tr" => b.open(Tag::Row)?,
                    "tblHeader" => {}
                    "tc" => b.open(Tag::Cell)?,
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if hidden > 0 {
                    hidden -= 1;
                    continue;
                }
                match name.as_str() {
                    "rPr" => in_rpr = false,
                    "t" => in_text = false,
                    "r" => close_marks(b, &mut run_open)?,
                    "pPr" => start_para(b, &mut para, styles, numbering, &mut list, &mut list_depth)?,
                    "hyperlink" => {
                        close_marks(b, &mut run_open)?;
                        b.close(&Tag::Link(String::new()))?;
                    }
                    "p" => {
                        // An empty paragraph still starts (and ends) a list item or ends a list.
                        start_para(b, &mut para, styles, numbering, &mut list, &mut list_depth)?;
                        close_para(b, &mut para, &mut run_open)?;
                    }
                    "tc" => b.close(&Tag::Cell)?,
                    "tr" => b.close(&Tag::Row)?,
                    "tbl" => b.close(&Tag::Table)?,
                    "body" => end_list(b, &mut list, &mut list_depth)?,
                    _ => {}
                }
            }
            Event::Text(t) => {
                if in_text && hidden == 0 {
                    b.text(&unescape(&String::from_utf8_lossy(t.as_ref())))?;
                }
            }
            Event::GeneralRef(g) if in_text && hidden == 0 => b.text(&unescape(&format!("&{};", String::from_utf8_lossy(g.as_ref()))))?,
            _ => {}
        }
    }
    end_list(b, &mut list, &mut list_depth)?;
    b.finish()
}

fn open_marks<S: Sink>(b: &mut Nest<S>, marks: &Marks, open: &mut Vec<Tag>) -> Result<(), Error> {
    let want = marks.opens();
    if *open == want {
        return Ok(());
    }
    close_marks(b, open)?;
    for m in &want {
        b.open(m.clone())?;
    }
    *open = want;
    Ok(())
}

fn close_marks<S: Sink>(b: &mut Nest<S>, open: &mut Vec<Tag>) -> Result<(), Error> {
    for m in open.drain(..).rev() {
        b.close(&m)?;
    }
    Ok(())
}

/// Opens the paragraph (or heading, or list item) once its properties are known.
fn start_para<S: Sink>(b: &mut Nest<S>, para: &mut Option<Para>, styles: &Styles, numbering: &Numbering, list: &mut Option<String>, depth: &mut u8) -> Result<(), Error> {
    let Some(p) = para else { return Ok(()) };
    if p.started {
        return Ok(());
    }
    p.started = true;
    let level = p.outline.or_else(|| p.style.as_deref().and_then(|s| styles.level(s)));
    if let (Some((num, lvl)), None) = (&p.num, level) {
        let ordered = numbering.get(&(num.clone(), *lvl)).copied().unwrap_or(false);
        let want = lvl.saturating_add(1).min(8);
        if list.as_deref() != Some(num.as_str()) {
            end_list(b, list, depth)?;
            *list = Some(num.clone());
        }
        // Deeper: open nested lists inside the current item; shallower: close them.
        while *depth > want {
            b.close(&Tag::List { ordered: false })?;
            *depth -= 1;
        }
        while *depth < want {
            if *depth > 0 && !b.is_open(&Tag::Item) {
                b.open(Tag::Item)?;
            }
            b.open(Tag::List { ordered })?;
            *depth += 1;
        }
        b.open(Tag::Item)?;
        return b.open(Tag::Paragraph);
    }
    end_list(b, list, depth)?;
    match level {
        Some(l) => b.open(Tag::Heading(l)),
        None => b.open(Tag::Paragraph),
    }
}

fn close_para<S: Sink>(b: &mut Nest<S>, para: &mut Option<Para>, run_open: &mut Vec<Tag>) -> Result<(), Error> {
    close_marks(b, run_open)?;
    if para.take().is_some() {
        b.close(&Tag::Heading(0))?;
        b.close(&Tag::Paragraph)?;
    }
    Ok(())
}

fn end_list<S: Sink>(b: &mut Nest<S>, list: &mut Option<String>, depth: &mut u8) -> Result<(), Error> {
    while *depth > 0 {
        b.close(&Tag::List { ordered: false })?;
        *depth -= 1;
    }
    *list = None;
    Ok(())
}

/// Title, author, subject from `docProps/core.xml`.
fn core_properties(pkg: &Package) -> Result<Vec<(&'static str, String)>, Error> {
    let mut out = Vec::new();
    if let Some(xml) = pkg.text("docProps/core.xml")? {
        let mut r = XmlReader::from_str(&xml);
        let mut field: Option<&'static str> = None;
        loop {
            match r.read_event() {
                Ok(Event::Start(e)) => {
                    field = match local(&e).as_str() {
                        "title" => Some("Title"),
                        "creator" => Some("Author"),
                        "subject" => Some("Subject"),
                        "lastModifiedBy" => Some("Last edited by"),
                        _ => None,
                    }
                }
                Ok(Event::Text(t)) => {
                    if let Some(f) = field.take() {
                        let v = unescape(String::from_utf8_lossy(t.as_ref()).trim());
                        if !v.is_empty() {
                            out.push((f, v));
                        }
                    }
                }
                Ok(Event::End(_)) => field = None,
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    Ok(out)
}
