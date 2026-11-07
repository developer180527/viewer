//! JSON and its dialects, one parser: JSON (strict, RFC 8259), JSON with comments and
//! trailing commas (JSONC: tsconfig, VS Code settings), JSON5 and Hjson, and JSON
//! Lines. Values become tree nodes on the lines they were written on; numbers are
//! shown as written, never rounded.

use std::sync::Arc;

use libfileparser_core::{
    Category, NodeId, Place, Position, ValueKind, Warning, Content, Document, FileSource, FormatInfo, ParseContext, ParseError, Result, Role,
};

use crate::table::JsonLinesTable;
use crate::tree::{Builder, DataTree, LineCounter, TreeLinks};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    Json,
    Jsonc,
    Json5,
    Hjson,
}

impl Dialect {
    fn comments(self) -> bool {
        self != Dialect::Json
    }

    fn trailing_commas(self) -> bool {
        self != Dialect::Json
    }

    fn name(self) -> &'static str {
        match self {
            Dialect::Json => "JSON",
            Dialect::Jsonc => "JSON with comments",
            Dialect::Json5 => "JSON5",
            Dialect::Hjson => "Hjson",
        }
    }
}

/// Parses `text` as `dialect` into a tree: an object's or array's members at the top,
/// or one node for a single value.
pub(crate) fn parse(text: &[u8], dialect: Dialect, cx: &ParseContext) -> Result<DataTree> {
    let mut b = Builder::new(cx);
    let mut p = Parser {
        s: text,
        i: 0,
        d: dialect,
        lines: LineCounter::new(text),
    };
    p.skip()?;
    if dialect == Dialect::Hjson && p.hjson_braceless_root() {
        p.run(
            &mut b,
            Frame {
                id: None,
                close: None,
                object: true,
                after: false,
            },
            0,
        )?;
    } else {
        match p.peek() {
            Some(c @ (b'{' | b'[')) => {
                p.i += 1;
                let close = if c == b'{' { b'}' } else { b']' };
                p.run(
                    &mut b,
                    Frame {
                        id: None,
                        close: Some(close),
                        object: c == b'{',
                        after: false,
                    },
                    0,
                )?;
            }
            _ => {
                p.value(&mut b, None, 1, "value".into())?;
            }
        }
    }
    p.skip()?;
    if p.i < text.len() {
        return Err(p.error("text after the end of the value"));
    }
    b.finish()
}

/// Bad lines JSON Lines may have before it stops being read as JSON Lines.
const MAX_BAD_LINES: usize = 1000;

/// JSON Lines: one value per line, the lines as the top-level nodes. A line that isn't
/// JSON becomes an error node, and a warning, rather than failing the file, unless
/// it is the first: then the file isn't JSON Lines.
pub(crate) fn parse_lines(text: &[u8], cx: &ParseContext) -> Result<(DataTree, Vec<Warning>)> {
    let mut b = Builder::new(cx);
    let mut p = Parser { s: text, i: 0, d: Dialect::Json, lines: LineCounter::new(text) };
    let mut warnings = Vec::new();
    let mut item = 0usize;
    for (n, line) in text.split(|&c| c == b'\n').enumerate() {
        let end = p.i + line.len();
        if !line.iter().all(u8::is_ascii_whitespace) {
            // A line's value ends with its line.
            p.s = &text[..end];
            let mark = b.len();
            let read = p.skip().and_then(|()| p.value(&mut b, None, 1, item.to_string())).and_then(|_| {
                p.skip_spaces();
                if p.i < end { Err(p.error("text after the line's value")) } else { Ok(()) }
            });
            if let Err(e) = read {
                if item == 0 || !e.try_next_reader() || warnings.len() >= MAX_BAD_LINES {
                    return Err(e);
                }
                b.rollback(mark);
                let why = match &e {
                    ParseError::Corrupt { why, .. } => why.clone(),
                    other => other.to_string(),
                };
                let at = e.position().unwrap_or(Position::line(n as u64 + 1));
                b.add(None, 1, item.to_string(), why.clone(), ValueKind::Error, n as u32..n as u32 + 1)?;
                warnings.push(Warning::new(why).at(at));
            }
            item += 1;
        }
        p.s = text;
        p.i = (end + 1).min(text.len());
    }
    Ok((b.finish()?, warnings))
}

/// An object or array being read. Nesting is kept here, not on the call stack, so
/// no depth of input can overflow it.
struct Frame {
    /// Its node; `None` for the file's own top-level object or array.
    id: Option<u32>,
    /// `}` or `]`; `None` for Hjson's braceless root, which ends with the file.
    close: Option<u8>,
    object: bool,
    /// A member has just been read: a separator or the close comes next.
    after: bool,
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    d: Dialect,
    lines: LineCounter<'a>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn line(&mut self) -> u32 {
        self.lines.line(self.i)
    }

    fn error(&self, what: &str) -> ParseError {
        ParseError::corrupt_at(format!("{}: {what}", self.d.name()), Position::in_text(self.s, self.i))
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r')) {
            self.i += 1;
        }
    }

    /// Skips white space and, where the dialect has them, comments.
    fn skip(&mut self) -> Result<()> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.i += 1,
                // JSON5 allows other Unicode white space; a BOM at the start is common.
                Some(0xEF) if self.s[self.i..].starts_with(b"\xEF\xBB\xBF") => self.i += 3,
                Some(b'/') if self.d.comments() => match self.s.get(self.i + 1) {
                    Some(b'/') => self.skip_to_line_end(),
                    Some(b'*') => {
                        let end = memchr::memmem::find(&self.s[self.i + 2..], b"*/")
                            .ok_or_else(|| self.error("a comment that never ends"))?;
                        self.i += end + 4;
                    }
                    _ => return Ok(()),
                },
                Some(b'#') if self.d == Dialect::Hjson => self.skip_to_line_end(),
                _ => return Ok(()),
            }
        }
    }

    fn skip_to_line_end(&mut self) {
        self.i = memchr::memchr(b'\n', &self.s[self.i..]).map_or(self.s.len(), |p| self.i + p);
    }

    fn expect(&mut self, c: u8, what: &str) -> Result<()> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.error(what))
        }
    }

    /// Hjson's top-level braces are optional: `key: value` lines are an object.
    fn hjson_braceless_root(&self) -> bool {
        if matches!(self.peek(), Some(b'{' | b'[') | None) {
            return false;
        }
        let mut probe = Parser {
            s: self.s,
            i: self.i,
            d: self.d,
            lines: LineCounter::new(self.s),
        };
        probe.key().is_ok() && {
            probe.skip_spaces();
            probe.peek() == Some(b':')
        }
    }

    /// Reads members until `top` and everything opened inside it are closed. Members
    /// of `top` are at `base + 1`.
    fn run(&mut self, b: &mut Builder, top: Frame, base: usize) -> Result<()> {
        let mut stack = vec![top];
        while let Some(f) = stack.last_mut() {
            if f.after {
                f.after = false;
                if self.separator(f.close.unwrap_or(0))? {
                    continue;
                }
                self.skip()?;
                if !(self.peek() == f.close || (self.peek().is_none() && f.close.is_none())) {
                    return Err(self.error(if f.object {
                        "expected `,` or the end of the object"
                    } else {
                        "expected `,` or `]`"
                    }));
                }
            }
            self.skip()?;
            match (self.peek(), f.close) {
                (Some(c), Some(close)) if c == close => {
                    self.i += 1;
                    let done = stack.pop().expect("a frame");
                    if let Some(id) = done.id {
                        let n = b.children(id);
                        b.set_value(
                            id,
                            if done.object {
                                format!("{{{n}}}")
                            } else {
                                format!("[{n}]")
                            },
                        );
                        let last = self.lines.line(self.i - 1);
                        b.end(id, last);
                    }
                    continue;
                }
                (None, None) => {
                    stack.pop();
                    continue;
                }
                (None, Some(_)) => {
                    return Err(self.error(if f.object {
                        "an object that never closes"
                    } else {
                        "an array that never closes"
                    }));
                }
                _ => {}
            }
            let (parent, object) = (f.id, f.object);
            let label = if object {
                let key = self.key()?;
                self.skip()?;
                self.expect(b':', "expected `:` after a key")?;
                self.skip()?;
                key
            } else {
                b.child_count(parent).to_string()
            };
            f.after = true;
            let depth = base + stack.len();
            match self.peek() {
                Some(c @ (b'{' | b'[')) => {
                    let first = self.line();
                    self.i += 1;
                    let kind = if c == b'{' { ValueKind::Map } else { ValueKind::List };
                    let id = b.add(parent, depth, label, "", kind, first..first + 1)?;
                    let close = if c == b'{' { b'}' } else { b']' };
                    stack.push(Frame {
                        id: Some(id),
                        close: Some(close),
                        object: c == b'{',
                        after: false,
                    });
                }
                _ => {
                    self.scalar(b, parent, depth, label)?;
                }
            }
        }
        Ok(())
    }

    /// After a member: whether another may follow. A comma (and, outside JSON, a
    /// trailing one before `close`); in Hjson a line break serves too.
    fn separator(&mut self, close: u8) -> Result<bool> {
        let start = self.i;
        self.skip()?;
        if self.peek() == Some(b',') {
            self.i += 1;
            if !self.d.trailing_commas() {
                self.skip()?;
                if self.peek() == Some(close) {
                    return Err(self.error(
                        "a comma before the end (allowed in JSONC, JSON5 and Hjson, not JSON)",
                    ));
                }
            }
            return Ok(true);
        }
        // Hjson: a line break separates too.
        let broke = self.d == Dialect::Hjson && self.s[start..self.i].contains(&b'\n');
        Ok(broke && self.peek().is_some() && self.peek() != Some(close))
    }
    fn key(&mut self) -> Result<String> {
        match self.peek() {
            Some(b'"') => self.string(b'"'),
            Some(b'\'') if self.d >= Dialect::Json5 => self.string(b'\''),
            Some(_) if self.d == Dialect::Json5 => {
                let start = self.i;
                while self.peek().is_some_and(|c| {
                    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
                }) {
                    self.i += 1;
                }
                if self.i == start {
                    return Err(self.error("expected a key"));
                }
                Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
            }
            Some(_) if self.d == Dialect::Hjson => {
                let start = self.i;
                while self
                    .peek()
                    .is_some_and(|c| !c.is_ascii_whitespace() && !b",:[]{}".contains(&c))
                {
                    self.i += 1;
                }
                if self.i == start {
                    return Err(self.error("expected a key"));
                }
                Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
            }
            _ => Err(self.error("expected a key in double quotes")),
        }
    }

    /// One value, as a node labelled `label`.
    fn value(
        &mut self,
        b: &mut Builder,
        parent: Option<u32>,
        depth: usize,
        label: String,
    ) -> Result<u32> {
        match self.peek() {
            Some(c @ (b'{' | b'[')) => {
                let first = self.line();
                self.i += 1;
                let kind = if c == b'{' { ValueKind::Map } else { ValueKind::List };
                let id = b.add(parent, depth, label, "", kind, first..first + 1)?;
                let close = if c == b'{' { b'}' } else { b']' };
                self.run(
                    b,
                    Frame {
                        id: Some(id),
                        close: Some(close),
                        object: c == b'{',
                        after: false,
                    },
                    depth,
                )?;
                Ok(id)
            }
            _ => self.scalar(b, parent, depth, label),
        }
    }

    /// A value that isn't an object or array.
    fn scalar(
        &mut self,
        b: &mut Builder,
        parent: Option<u32>,
        depth: usize,
        label: String,
    ) -> Result<u32> {
        let first = self.line();
        match self.peek() {
            Some(b'{' | b'[') => Err(self.error("expected a value")),
            Some(q @ b'"') => {
                let s = self.string(q)?;
                self.leaf(b, parent, depth, label, (s, ValueKind::Text), first)
            }
            Some(q @ b'\'') if self.d >= Dialect::Json5 => {
                if self.d == Dialect::Hjson && self.s[self.i..].starts_with(b"'''") {
                    let s = self.multiline()?;
                    return self.leaf(b, parent, depth, label, (s, ValueKind::Text), first);
                }
                let s = self.string(q)?;
                self.leaf(b, parent, depth, label, (s, ValueKind::Text), first)
            }
            Some(_) if self.d == Dialect::Hjson => {
                let start = self.i;
                let end =
                    memchr::memchr(b'\n', &self.s[start..]).map_or(self.s.len(), |p| start + p);
                let raw = std::str::from_utf8(&self.s[start..end])
                    .map_err(|_| self.error("text that isn't UTF-8"))?;
                // A literal followed only by a separator or comment is that literal;
                // anything else to the end of the line is a string.
                let word_end = raw
                    .char_indices()
                    .find(|&(i, c)| {
                        matches!(c, ',' | ']' | '}' | '#')
                            || c.is_whitespace()
                            || raw[i..].starts_with("//")
                    })
                    .map_or(raw.len(), |(i, _)| i);
                let word = &raw[..word_end];
                let after = raw[word_end..].trim_start();
                let literal = (matches!(word, "true" | "false" | "null") || looks_numeric(word))
                    && (after.is_empty()
                        || after.starts_with([',', ']', '}', '#'])
                        || after.starts_with("//"));
                if literal {
                    self.i = start + word_end;
                    return self.leaf(b, parent, depth, label, (word.to_string(), literal_kind(word)), first);
                }
                if raw.starts_with([',', ':', ']', '}']) {
                    return Err(self.error("expected a value"));
                }
                self.i = end;
                let text = raw.trim_end();
                self.leaf(b, parent, depth, label, (text.to_string(), ValueKind::Text), first)
            }
            Some(_) => {
                let start = self.i;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || b"+-._".contains(&c))
                {
                    self.i += 1;
                }
                let word = std::str::from_utf8(&self.s[start..self.i]).unwrap_or_default();
                let ok = match word {
                    "true" | "false" | "null" => true,
                    "Infinity" | "-Infinity" | "+Infinity" | "NaN" | "-NaN" | "+NaN" => {
                        self.d == Dialect::Json5
                    }
                    w if self.d == Dialect::Json5 => looks_numeric(w),
                    w => strict_number(w),
                };
                if !ok {
                    self.i = start;
                    return Err(self.error("expected a value"));
                }
                self.leaf(b, parent, depth, label, (word.to_string(), literal_kind(word)), first)
            }
            None => Err(self.error("expected a value, found the end")),
        }
    }

    fn leaf(
        &mut self,
        b: &mut Builder,
        parent: Option<u32>,
        depth: usize,
        label: String,
        (value, kind): (String, ValueKind),
        first: u32,
    ) -> Result<u32> {
        let last = self.lines.line(self.i.saturating_sub(1));
        b.add(parent, depth, label, value, kind, first..first.max(last) + 1)
    }

    /// A quoted string, its escapes decoded.
    fn string(&mut self, quote: u8) -> Result<String> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let rest = &self.s[self.i..];
            let Some(p) = memchr::memchr3(quote, b'\\', b'\n', rest) else {
                return Err(self.error("a string that never ends"));
            };
            out.extend_from_slice(&rest[..p]);
            self.i += p;
            match self.s[self.i] {
                c if c == quote => {
                    self.i += 1;
                    break;
                }
                b'\n' => return Err(self.error("a line break in a string")),
                _ => {
                    self.i += 1;
                    let Some(&e) = self.s.get(self.i) else {
                        return Err(self.error("a string that never ends"));
                    };
                    self.i += 1;
                    match e {
                        b'"' | b'\\' | b'/' | b'\'' => out.push(e),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut c = self.hex4()?;
                            if (0xD800..0xDC00).contains(&c) && self.s[self.i..].starts_with(b"\\u")
                            {
                                self.i += 2;
                                let low = self.hex4()?;
                                c = 0x10000
                                    + ((c - 0xD800) << 10)
                                    + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let ch = char::from_u32(c).unwrap_or('\u{FFFD}');
                            out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                        }
                        // JSON5: a line continuation, and the other escapes.
                        b'\n' if self.d >= Dialect::Json5 => {}
                        b'\r' if self.d >= Dialect::Json5 => {
                            if self.peek() == Some(b'\n') {
                                self.i += 1;
                            }
                        }
                        b'0' if self.d >= Dialect::Json5 => out.push(0),
                        b'v' if self.d >= Dialect::Json5 => out.push(11),
                        b'x' if self.d >= Dialect::Json5 => {
                            let h = std::str::from_utf8(
                                self.s.get(self.i..self.i + 2).unwrap_or_default(),
                            )
                            .ok()
                            .and_then(|h| u8::from_str_radix(h, 16).ok());
                            let h = h.ok_or_else(|| self.error("a bad \\x escape"))?;
                            self.i += 2;
                            out.extend_from_slice(
                                char::from(h).encode_utf8(&mut [0; 4]).as_bytes(),
                            );
                        }
                        c if self.d >= Dialect::Json5 && c != b'\n' => out.push(c),
                        _ => return Err(self.error("a bad escape in a string")),
                    }
                }
            }
        }
        String::from_utf8(out).map_err(|_| self.error("a string that isn't UTF-8"))
    }

    fn hex4(&mut self) -> Result<u32> {
        let h = self
            .s
            .get(self.i..self.i + 4)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u32::from_str_radix(h, 16).ok());
        let h = h.ok_or_else(|| self.error("a bad \\u escape"))?;
        self.i += 4;
        Ok(h)
    }

    /// Hjson's `'''` multiline string, its lines' indent up to the opening quotes'
    /// column taken off.
    fn multiline(&mut self) -> Result<String> {
        let indent = self.i
            - self.s[..self.i]
                .iter()
                .rposition(|&c| c == b'\n')
                .map_or(0, |p| p + 1);
        self.i += 3;
        let end = memchr::memmem::find(&self.s[self.i..], b"'''")
            .ok_or_else(|| self.error("a ''' string that never ends"))?;
        let text = String::from_utf8_lossy(&self.s[self.i..self.i + end]).into_owned();
        self.i += end + 3;
        let mut lines: Vec<&str> = text.split('\n').collect();
        if lines.first().is_some_and(|l| l.trim().is_empty()) {
            lines.remove(0);
        }
        if lines.len() > 1 && lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        let strip = |l: &str| -> String {
            let ws = l.len() - l.trim_start_matches([' ', '\t']).len();
            l[ws.min(indent)..].trim_end_matches('\r').to_string()
        };
        Ok(lines.into_iter().map(strip).collect::<Vec<_>>().join("\n"))
    }
}

impl PartialOrd for Dialect {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some((*self as u8).cmp(&(*other as u8)))
    }
}

/// What a bare word is: `true`, `false`, `null`, or a number.
fn literal_kind(word: &str) -> ValueKind {
    match word {
        "true" | "false" => ValueKind::Bool,
        "null" => ValueKind::Null,
        _ => ValueKind::Number,
    }
}

/// RFC 8259's number: `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`.
fn strict_number(w: &str) -> bool {
    let b = w.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let digits = |i: &mut usize| {
        let s = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i - s
    };
    match b.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            digits(&mut i);
        }
        _ => return false,
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if digits(&mut i) == 0 {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

/// JSON5's and Hjson's looser numbers: hex, a leading or trailing point, a sign.
fn looks_numeric(w: &str) -> bool {
    let w = w.strip_prefix(['+', '-']).unwrap_or(w);
    if let Some(h) = w.strip_prefix("0x").or_else(|| w.strip_prefix("0X")) {
        return !h.is_empty() && h.bytes().all(|c| c.is_ascii_hexdigit());
    }
    !w.is_empty() && w.bytes().any(|c| c.is_ascii_digit()) && w.parse::<f64>().is_ok()
}

/// A document of `tree`, with its links to lines.
pub(crate) fn tree_document(
    format: FormatInfo,
    tree: DataTree,
    src: &FileSource,
    language: &str,
) -> Document {
    let nodes = tree.len();
    let tree = Arc::new(tree);
    Document::new(format)
        .with_facet(Role::Primary, "Tree", Content::Tree(tree.clone()))
        .with_meta("Nodes", nodes)
        .with_links(TreeLinks { lines: true,
            tree,
            tree_facet: 0,
            table: None,
        })
        .with_file_facets(src, Some(language))
}

/// A JSON Lines document: a table of the lines when they are objects (their keys as
/// columns), and the tree of every line.
pub(crate) fn lines_document(tree: DataTree, warnings: Vec<Warning>, src: &FileSource) -> Document {
    let nodes = tree.len();
    let rows = tree.roots().len();
    let tree = Arc::new(tree);
    let table = JsonLinesTable::new(tree.clone());
    let format = FormatInfo::new("json-lines", "JSON Lines", Category::Data);
    let doc = Document::new(format);
    let (doc, tree_facet, table_facet) = if table.has_columns() {
        let columns = table.columns().len();
        let doc = doc
            .with_facet(Role::Primary, "Table", Content::Table(Arc::new(table)))
            .with_meta("Columns", columns);
        (
            doc.with_facet(Role::Structure, "Tree", Content::Tree(tree.clone())),
            1,
            Some(0),
        )
    } else {
        (
            doc.with_facet(Role::Primary, "Tree", Content::Tree(tree.clone())),
            0,
            None,
        )
    };
    // Each warning is a bad line: its row, or its node.
    let bad: Vec<usize> = (0..rows).filter(|&r| tree.kind(tree.roots()[r]) == ValueKind::Error).collect();
    let warnings = warnings.into_iter().zip(bad).map(|(w, row)| match table_facet {
        Some(facet) => w.on(Place::Row { facet, row: row as u64 }),
        None => w.on(Place::Node { facet: tree_facet, node: NodeId(tree.roots()[row] as u64) }),
    });
    doc.with_meta("Lines", rows)
        .with_meta("Nodes", nodes)
        .with_warnings(warnings)
        .with_links(TreeLinks { lines: true,
            table: table_facet.map(|f| (f, tree.roots().into())),
            tree,
            tree_facet,
        })
        .with_file_facets(src, Some("json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::{NodeId, TreeSource};

    fn top(t: &DataTree) -> Vec<(String, String)> {
        t.children(None, 0..100)
            .unwrap()
            .into_iter()
            .map(|n| (n.label.clone(), crate::tree::shown(&n)))
            .collect()
    }

    fn kids(t: &DataTree, path: &str) -> Vec<(String, String)> {
        let id = t.find(path).unwrap().unwrap();
        t.children(Some(id), 0..100)
            .unwrap()
            .into_iter()
            .map(|n| (n.label.clone(), crate::tree::shown(&n)))
            .collect()
    }

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn json_keeps_order_numbers_and_lines() {
        let cx = ParseContext::new();
        let t = parse(
            b"{\n  \"z\": 1.50,\n  \"a\": [true,\n null],\n \"s\": \"\\u00e9\\n\"\n}",
            Dialect::Json,
            &cx,
        )
        .unwrap();
        assert_eq!(
            top(&t),
            pairs(&[("z", "1.50"), ("a", "[2]"), ("s", "\"é\\n\"")])
        );
        assert_eq!(kids(&t, "/a"), pairs(&[("0", "true"), ("1", "null")]));
        let a = t.find("/a").unwrap().unwrap();
        assert_eq!(t.lines(a.0 as u32), 2..4);
        assert_eq!(
            t.path(NodeId(t.find("/a/1").unwrap().unwrap().0))
                .unwrap()
                .unwrap(),
            "/a/1"
        );
    }

    #[test]
    fn strict_json_refuses_what_dialects_allow() {
        let cx = ParseContext::new();
        for bad in [
            &b"{\"a\":1,}"[..],
            b"[1,]",
            b"{a:1}",
            b"// c\n{}",
            b"[01]",
            b"[1.]",
            b"['a']",
            b"[1] 2",
        ] {
            assert!(
                parse(bad, Dialect::Json, &cx).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        let err = parse(b"{\n  \"a\": nope\n}", Dialect::Json, &cx)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("line 2, column 8"), "{err}");
        let t = parse(
            b"// settings\n{\n  /* x */ \"a\": [1,],\n}",
            Dialect::Jsonc,
            &cx,
        )
        .unwrap();
        assert_eq!(kids(&t, "/a"), pairs(&[("0", "1")]));
    }

    #[test]
    fn json5() {
        let cx = ParseContext::new();
        let t = parse(
            b"{unquoted: 'single', hex: 0xFF, lead: .5, inf: -Infinity, s: 'a\\\nb',}",
            Dialect::Json5,
            &cx,
        )
        .unwrap();
        assert_eq!(
            top(&t),
            pairs(&[
                ("unquoted", "\"single\""),
                ("hex", "0xFF"),
                ("lead", ".5"),
                ("inf", "-Infinity"),
                ("s", "\"ab\"")
            ])
        );
    }

    #[test]
    fn hjson() {
        let cx = ParseContext::new();
        let text = b"# config\nname: My App, really\nport: 8080\nlist: [\n  a\n  2\n]\nok: true // yes\ntext:\n  '''\n  two\n  lines\n  '''\n";
        let t = parse(text, Dialect::Hjson, &cx).unwrap();
        assert_eq!(
            top(&t),
            pairs(&[
                ("name", "\"My App, really\""),
                ("port", "8080"),
                ("list", "[2]"),
                ("ok", "true"),
                ("text", "\"two\\nlines\"")
            ])
        );
        assert_eq!(kids(&t, "/list"), pairs(&[("0", "\"a\""), ("1", "2")]));
    }

    #[test]
    fn json_lines_and_limits() {
        let cx = ParseContext::new();
        let t = parse_lines(b"{\"a\":1}\n\n[2]\n\"x\"\n", &cx).unwrap().0;
        assert_eq!(
            top(&t),
            pairs(&[("0", "{1}"), ("1", "[1]"), ("2", "\"x\"")])
        );
        // A bad first line: not JSON Lines. A bad later line: an error node and a warning.
        assert!(parse_lines(b"{bad\n{}\n", &cx).is_err());
        let (t, warnings) = parse_lines(b"{}\n{bad\n[3]\n", &cx).unwrap();
        assert_eq!(t.roots().len(), 3);
        assert_eq!(t.kind(t.roots()[1]), ValueKind::Error);
        assert_eq!(t.children(t.find("/2").unwrap(), 0..1).unwrap()[0].value, "3");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].at.and_then(|p| p.line), Some(2));
        let err = parse(b"{\n  \"a\": nope\n}", Dialect::Json, &cx).err().unwrap();
        assert_eq!(err.position().map(|p| (p.line, p.column)), Some((Some(2), Some(8))));
        let mut limits = libfileparser_core::Limits::desktop();
        limits.depth = 3;
        let tiny = ParseContext::new().with_limits(limits);
        assert!(parse(b"[[[[1]]]]", Dialect::Json, &tiny).is_err());
        assert!(parse(b"[[1]]", Dialect::Json, &tiny).is_ok());
    }
}
