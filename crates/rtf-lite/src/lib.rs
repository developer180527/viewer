//! Rich Text Format, read: a small, embeddable RTF reader with no dependencies.
//!
//! It reads paragraphs, headings (from the style sheet's "heading N" styles or
//! `\\outlinelevel`), bold, italic, underline, strikethrough, superscript and
//! subscript, tables, line and page breaks, links (`HYPERLINK` fields), Unicode
//! (`\\u`) and Windows-1252 text, and the document's title. Pictures, fonts, colours,
//! headers, footers and footnotes are skipped.
//!
//! What it reads arrives as events in a [`Sink`] the caller implements, so it builds
//! whatever document model the caller has:
//!
//! ```
//! struct Plain(String);
//! impl rtf_lite::Sink for Plain {
//!     fn text(&mut self, text: &str) -> Result<(), rtf_lite::Stop> {
//!         self.0.push_str(text);
//!         Ok(())
//!     }
//! }
//! let mut out = Plain(String::new());
//! rtf_lite::parse(br"{\rtf1 Hello {\b world}\par}", &mut out, &rtf_lite::Limits::default()).unwrap();
//! assert_eq!(out.0, "Hello world");
//! ```

use std::collections::HashMap;
use std::fmt;

/// Something opened and later closed, in pairs, properly nested.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Tag {
    Paragraph,
    /// A heading, level 1 (largest) to 6.
    Heading(u8),
    Table,
    Row,
    Cell,
    Strong,
    Emphasis,
    Underline,
    Strikethrough,
    Superscript,
    Subscript,
    /// A link to its target. Closed with any `Link`.
    Link(String),
}

/// A sink's answer when it wants reading to stop (a limit it keeps, the user
/// closing the document).
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
}

/// How much a file may cost.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Nesting of `{` groups.
    pub depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { depth: 512 }
    }
}

impl Limits {
    pub fn with_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }
}

/// What the document says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Info {
    pub title: Option<String>,
}

/// Why reading stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// It doesn't start with `{\\rtf`.
    NotRtf,
    /// Groups nest deeper than [`Limits::depth`].
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
            Error::NotRtf => f.write_str("not RTF"),
            Error::TooDeep(d) => write!(f, "groups nest deeper than {d}"),
            Error::Stopped => f.write_str("stopped"),
        }
    }
}

impl std::error::Error for Error {}

/// Character formatting, kept per group.
#[derive(Clone, Default, PartialEq)]
struct Chars {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    sup: bool,
    sub: bool,
}

impl Chars {
    fn opens(&self) -> Vec<Tag> {
        let mut v = Vec::new();
        for (on, o) in [(self.bold, Tag::Strong), (self.italic, Tag::Emphasis), (self.underline, Tag::Underline), (self.strike, Tag::Strikethrough), (self.sup, Tag::Superscript), (self.sub, Tag::Subscript)] {
            if on {
                v.push(o);
            }
        }
        v
    }
}

#[derive(Clone, Default)]
struct Group {
    chars: Chars,
    /// What this group's text is for; `None` is the document's text.
    dest: Option<Dest>,
    /// Characters to skip after a `\u` (the plain-text fallback).
    uc: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum Dest {
    /// Not shown (fonts, colours, pictures, headers…).
    Skip,
    StyleSheet,
    Title,
    /// A field's instruction (`HYPERLINK "…"`).
    FieldInstr,
}

struct State<'a, S: Sink> {
    s: &'a mut S,
    /// A cell has started in the current row.
    in_cell: bool,
    /// How deep the group of the field being read is: its link ends with the group.
    field_depth: Option<usize>,
    /// Marks currently open in the sink, in order.
    open: Vec<Tag>,
    in_para: bool,
    heading: Option<u8>,
    style_headings: HashMap<i64, u8>,
    style_text: String,
    style_index: Option<i64>,
    title: String,
    field: String,
    link: bool,
    in_table: bool,
    in_row: bool,
    codepage: u32,
}

/// Reads `rtf` into `sink`, within `limits`. Stops early, with [`Error::Stopped`], when
/// the sink says so.
pub fn parse<S: Sink>(rtf: &[u8], sink: &mut S, limits: &Limits) -> Result<Info, Error> {
    if !rtf.starts_with(b"{\\rtf") {
        return Err(Error::NotRtf);
    }
    let mut st = State { s: sink, in_cell: false, field_depth: None, open: Vec::new(), in_para: false, heading: None, style_headings: HashMap::new(), style_text: String::new(), style_index: None, title: String::new(), field: String::new(), link: false, in_table: false, in_row: false, codepage: 1252 };
    let mut stack: Vec<Group> = vec![Group { uc: 1, ..Default::default() }];
    let mut skip_chars = 0u32;
    let mut i = 0;
    let mut text = Vec::new();
    while i < rtf.len() {
        let c = rtf[i];
        match c {
            b'{' => {
                st.flush_text(&mut text, &stack)?;
                if stack.len() >= limits.depth {
                    return Err(Error::TooDeep(limits.depth));
                }
                let top = stack.last().cloned().unwrap_or_default();
                stack.push(top);
                i += 1;
                // `{\*\dest …}`: an optional destination, skipped unless known.
                if rtf[i..].starts_with(b"\\*") {
                    stack.last_mut().unwrap().dest = Some(Dest::Skip);
                    i += 2;
                }
            }
            b'}' => {
                st.flush_text(&mut text, &stack)?;
                let g = stack.pop().unwrap_or_default();
                if g.dest == Some(Dest::StyleSheet) && stack.last().is_some_and(|p| p.dest == Some(Dest::StyleSheet)) {
                    st.end_style();
                }
                if g.dest == Some(Dest::FieldInstr) && stack.last().map(|p| p.dest) != Some(Some(Dest::FieldInstr)) {
                    st.start_field_result()?;
                }
                if st.field_depth.is_some_and(|d| stack.len() < d) {
                    st.field_depth = None;
                    st.flush_text(&mut text, &stack)?;
                    st.end_link()?;
                }
                if stack.is_empty() {
                    break;
                }
                i += 1;
            }
            b'\\' => {
                i += 1;
                let Some(&n) = rtf.get(i) else { break };
                if n.is_ascii_alphabetic() {
                    let start = i;
                    while i < rtf.len() && rtf[i].is_ascii_alphabetic() && i - start < 32 {
                        i += 1;
                    }
                    let word = std::str::from_utf8(&rtf[start..i]).unwrap_or("");
                    let ns = i;
                    if i < rtf.len() && (rtf[i] == b'-' || rtf[i].is_ascii_digit()) {
                        i += 1;
                        while i < rtf.len() && rtf[i].is_ascii_digit() && i - ns < 11 {
                            i += 1;
                        }
                    }
                    let param = std::str::from_utf8(&rtf[ns..i]).ok().and_then(|p| p.parse::<i64>().ok());
                    if rtf.get(i) == Some(&b' ') {
                        i += 1;
                    }
                    if skip_chars > 0 {
                        skip_chars -= 1;
                        continue;
                    }
                    st.flush_text(&mut text, &stack)?;
                    st.control(word, param, &mut stack, &mut skip_chars, &mut text)?;
                } else if n == b'\'' {
                    let hex = rtf.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                    i += 3;
                    if skip_chars > 0 {
                        skip_chars -= 1;
                    } else if let Some(byte) = hex {
                        push_char(&mut text, decode_byte(byte, st.codepage));
                    }
                } else {
                    i += 1;
                    if skip_chars > 0 {
                        skip_chars -= 1;
                        continue;
                    }
                    match n {
                        b'~' => push_char(&mut text, '\u{a0}'),
                        b'-' => {}
                        b'_' => push_char(&mut text, '\u{2011}'),
                        b'\n' | b'\r' => {
                            st.flush_text(&mut text, &stack)?;
                            st.par()?;
                        }
                        other => push_char(&mut text, other as char),
                    }
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                if skip_chars > 0 {
                    skip_chars -= 1;
                } else {
                    // Plain ASCII; anything else is the code page's.
                    push_char(&mut text, if c < 0x80 { c as char } else { decode_byte(c, st.codepage) });
                }
                i += 1;
            }
        }
    }
    st.flush_text(&mut text, &stack)?;
    st.close_marks()?;
    st.end_para()?;
    let title = st.title.trim().to_string();
    Ok(Info { title: (!title.is_empty()).then_some(title) })
}

fn push_char(text: &mut Vec<char>, c: char) {
    text.push(c);
}

/// A byte in Windows-1252 (the usual `\ansicpg`); others are read as Latin-1.
fn decode_byte(b: u8, codepage: u32) -> char {
    const CP1252: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›',
        'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    if codepage == 1252 && (0x80..0xa0).contains(&b) { CP1252[(b - 0x80) as usize] } else { b as char }
}

impl<S: Sink> State<'_, S> {
    fn flush_text(&mut self, text: &mut Vec<char>, stack: &[Group]) -> Result<(), Error> {
        if text.is_empty() {
            return Ok(());
        }
        let s: String = text.drain(..).collect();
        let g = stack.last().cloned().unwrap_or_default();
        match g.dest {
            Some(Dest::Skip) => {}
            Some(Dest::StyleSheet) => self.style_text.push_str(&s),
            Some(Dest::Title) => self.title.push_str(&s),
            Some(Dest::FieldInstr) => self.field.push_str(&s),
            None => {
                self.start_para()?;
                let want = g.chars.opens();
                if want != self.open {
                    self.close_marks()?;
                    for o in &want {
                        self.s.open(o.clone())?;
                    }
                    self.open = want;
                }
                self.s.text(&s)?;
            }
        }
        Ok(())
    }

    fn close_marks(&mut self) -> Result<(), Error> {
        for o in std::mem::take(&mut self.open).into_iter().rev() {
            self.s.close(o)?;
        }
        Ok(())
    }

    fn start_para(&mut self) -> Result<(), Error> {
        if self.in_para {
            return Ok(());
        }
        self.in_para = true;
        if self.in_table && !self.in_row {
            self.s.open(Tag::Row)?;
            self.in_row = true;
        }
        if self.in_table && !self.in_cell {
            self.s.open(Tag::Cell)?;
            self.in_cell = true;
        }
        match self.heading {
            Some(l) if !self.in_table => self.s.open(Tag::Heading(l))?,
            _ => self.s.open(Tag::Paragraph)?,
        }
        Ok(())
    }

    fn end_para(&mut self) -> Result<(), Error> {
        self.close_marks()?;
        self.end_link()?;
        if self.in_para {
            self.s.close(match self.heading {
                Some(l) if !self.in_table => Tag::Heading(l),
                _ => Tag::Paragraph,
            })?;
            self.in_para = false;
        }
        Ok(())
    }

    /// Ends the link a `HYPERLINK` field opened (at the field's end, or its paragraph's).
    fn end_link(&mut self) -> Result<(), Error> {
        if self.link {
            self.close_marks()?;
            self.s.close(Tag::Link(String::new()))?;
            self.link = false;
        }
        Ok(())
    }

    fn par(&mut self) -> Result<(), Error> {
        if !self.in_para {
            self.start_para()?;
        }
        self.end_para()
    }

    fn end_style(&mut self) {
        if let Some(i) = self.style_index.take() {
            let name = self.style_text.trim().trim_end_matches(';').trim().to_ascii_lowercase();
            if let Some(l) = name.strip_prefix("heading").and_then(|d| d.trim().parse::<u8>().ok()) {
                self.style_headings.insert(i, l.clamp(1, 6));
            } else if name == "title" {
                self.style_headings.insert(i, 1);
            }
        }
        self.style_text.clear();
    }

    /// A field's instruction has ended: a `HYPERLINK "url"` makes its result a link.
    fn start_field_result(&mut self) -> Result<(), Error> {
        let instr = std::mem::take(&mut self.field);
        let instr = instr.trim();
        if let Some(rest) = instr.strip_prefix("HYPERLINK") {
            let url = rest.trim().trim_start_matches("\\l").trim().trim_matches('"').to_string();
            if !url.is_empty() {
                self.start_para()?;
                self.close_marks()?;
                self.s.open(Tag::Link(url))?;
                self.link = true;
            }
        }
        Ok(())
    }

    fn control(&mut self, word: &str, param: Option<i64>, stack: &mut [Group], skip: &mut u32, text: &mut Vec<char>) -> Result<(), Error> {
        let on = param != Some(0);
        let g = stack.last_mut().expect("a group is open");
        match word {
            "ansicpg" => self.codepage = param.unwrap_or(1252).clamp(0, 65535) as u32,
            "par" | "sect" => {
                // In a style sheet, a style name's end; in text, a paragraph's.
                if g.dest.is_none() {
                    self.par()?;
                }
            }
            "line" => {
                self.start_para()?;
                self.s.line_break()?;
            }
            "page" => {
                self.end_para()?;
                self.s.page_break()?;
            }
            "tab" => push_char(text, '\t'),
            "emdash" => push_char(text, '—'),
            "endash" => push_char(text, '–'),
            "bullet" => push_char(text, '•'),
            "lquote" => push_char(text, '‘'),
            "rquote" => push_char(text, '’'),
            "ldblquote" => push_char(text, '“'),
            "rdblquote" => push_char(text, '”'),
            "u" => {
                if let Some(p) = param {
                    let code = if p < 0 { p + 65536 } else { p } as u32;
                    push_char(text, char::from_u32(code).unwrap_or('\u{fffd}'));
                    *skip = g.uc;
                }
            }
            "uc" => g.uc = param.unwrap_or(1).clamp(0, 8) as u32,
            "b" => g.chars.bold = on,
            "i" => g.chars.italic = on,
            "ul" | "uld" | "uldb" | "ulw" => g.chars.underline = on,
            "ulnone" => g.chars.underline = false,
            "strike" | "striked" => g.chars.strike = on,
            "super" => (g.chars.sup, g.chars.sub) = (true, false),
            "sub" => (g.chars.sub, g.chars.sup) = (true, false),
            "nosupersub" => (g.chars.sup, g.chars.sub) = (false, false),
            "plain" => g.chars = Chars::default(),
            "pard" => {
                self.heading = None;
                if !self.in_row {
                    self.in_table = false;
                }
            }
            "s" => {
                if g.dest == Some(Dest::StyleSheet) {
                    self.style_index = param;
                } else if g.dest.is_none() {
                    self.heading = param.and_then(|p| self.style_headings.get(&p).copied());
                }
            }
            "outlinelevel" => self.heading = param.map(|p| (p + 1).clamp(1, 6) as u8),
            "intbl" => self.in_table = true,
            "trowd" => {
                self.end_para()?;
                self.in_table = true;
            }
            "cell" => {
                if !self.in_para {
                    self.start_para()?;
                }
                self.end_para()?;
                if self.in_cell {
                    self.s.close(Tag::Cell)?;
                    self.in_cell = false;
                }
            }
            "row" => {
                self.end_para()?;
                if self.in_row {
                    self.s.close(Tag::Row)?;
                    self.in_row = false;
                }
            }
            "stylesheet" => g.dest = Some(Dest::StyleSheet),
            "title" => g.dest = Some(Dest::Title),
            "fldinst" => g.dest = Some(Dest::FieldInstr),
            "fldrslt" => {}
            "field" => {
                self.end_link()?;
                self.field_depth = Some(stack.len());
            }
            // `\info` holds `\title` (shown) among things that aren't (skipped by name).
            "info" => {}
            "fonttbl" | "colortbl" | "pict" | "object" | "header" | "footer" | "headerl" | "headerr" | "footerl" | "footerr" | "footnote" | "annotation" | "xmlnstbl"
            | "listtable" | "listoverridetable" | "revtbl" | "rsidtbl" | "generator" | "themedata" | "colorschememapping" | "latentstyles" | "datastore" | "bkmkstart"
            | "bkmkend" | "nonshppict" | "shp" | "author" | "operator" | "company" => g.dest = Some(Dest::Skip),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Events as text: `<Tag>`, `</Tag>`, text, `|br|`, `|page|`.
    #[derive(Default)]
    struct Log(String);

    impl Sink for Log {
        fn open(&mut self, tag: Tag) -> Result<(), Stop> {
            self.0 += &format!("<{tag:?}>");
            Ok(())
        }
        fn close(&mut self, tag: Tag) -> Result<(), Stop> {
            self.0 += &format!("</{tag:?}>");
            Ok(())
        }
        fn text(&mut self, text: &str) -> Result<(), Stop> {
            self.0 += text;
            Ok(())
        }
        fn line_break(&mut self) -> Result<(), Stop> {
            self.0 += "|br|";
            Ok(())
        }
        fn page_break(&mut self) -> Result<(), Stop> {
            self.0 += "|page|";
            Ok(())
        }
    }

    #[test]
    fn reads_structure_marks_and_text() {
        let rtf = br#"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Arial;}}{\stylesheet{\s1 heading 1;}}{\info{\title My Doc}}
{\s1 Big\par}
\pard Plain {\b bold} and {\i italic} caf\'e9 \u8212? done\line next\page
{\field{\*\fldinst HYPERLINK "https://x.org"}{\fldrslt link}}\par
\trowd\intbl a\cell b\cell\row
}"#;
        let mut log = Log::default();
        let info = parse(rtf, &mut log, &Limits::default()).unwrap();
        assert_eq!(info.title.as_deref(), Some("My Doc"));
        let l = log.0;
        assert!(l.contains("<Heading(1)>Big</Heading(1)>"), "{l}");
        assert!(l.contains("<Strong>bold</Strong>") && l.contains("<Emphasis>italic</Emphasis>"), "{l}");
        assert!(l.contains("café — done|br|next"), "{l}");
        assert!(l.contains("|page|"), "{l}");
        assert!(l.contains("<Link(\"https://x.org\")>link</Link(\"\")>"), "{l}");
        assert!(l.contains("<Row><Cell><Paragraph>a</Paragraph></Cell><Cell><Paragraph>b</Paragraph></Cell></Row>"), "{l}");
        assert!(!l.contains("Arial"), "fonts aren't text: {l}");
    }

    #[test]
    fn limits_and_stopping() {
        let deep = format!("{{\\rtf1 {}x{}}}", "{".repeat(100), "}".repeat(100));
        assert_eq!(parse(deep.as_bytes(), &mut Log::default(), &Limits::default().with_depth(10)), Err(Error::TooDeep(10)));
        assert_eq!(parse(b"hello", &mut Log::default(), &Limits::default()), Err(Error::NotRtf));
        struct Quit;
        impl Sink for Quit {
            fn text(&mut self, _: &str) -> Result<(), Stop> {
                Err(Stop)
            }
        }
        assert_eq!(parse(b"{\\rtf1 hi\\par}", &mut Quit, &Limits::default()), Err(Error::Stopped));
    }

    #[test]
    fn never_panics_on_damaged_input() {
        let base: &[u8] = br#"{\rtf1\ansi{\fonttbl{\f0 A;}}\u-3913?\'zz\'e9{\b x\u99999999999999? \uc5\u65 y}\cell\row}"#;
        for cut in 0..base.len() {
            let _ = parse(&base[..cut], &mut Log::default(), &Limits::default());
        }
    }
}
