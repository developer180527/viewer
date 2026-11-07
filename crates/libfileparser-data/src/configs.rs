//! More configuration languages, parsed as written and never evaluated: HCL and
//! Terraform (blocks and attributes), CUE, Dhall and Nickel (records of fields), KDL
//! (nodes with arguments and properties), crontabs (a table of jobs with their
//! schedules in words); and SGML and fixed-width tables, from the base families.

use std::sync::Arc;

use libfileparser_core::{Cell, Column, Document, FileSource, ParseContext, ParseError, Position, Result, ValueKind};

use crate::lines::{self, Line, LineTable, text};
use crate::nodes::{self, NodeTable, format};
use crate::tree::{Builder, DataTree, LineCounter};

/// A run of `s` from `i` to where an expression ends at depth zero: before `stop`
/// characters or a line break, past brackets, strings and heredocs.
fn expression_end(s: &[u8], mut i: usize, stop: &[u8], newline_ends: bool, hash_comments: bool) -> usize {
    let mut depth = 0i32;
    while i < s.len() {
        let c = s[i];
        match c {
            b'"' | b'\'' | b'`' => {
                let mut j = i + 1;
                while j < s.len() && s[j] != c {
                    if s[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            // HCL heredocs: `<<EOF` … `EOF`.
            b'<' if s[i..].starts_with(b"<<") => {
                let start = i + 2 + usize::from(s.get(i + 2) == Some(&b'-'));
                let end = s[start..].iter().position(|b| !(b.is_ascii_alphanumeric() || *b == b'_')).map_or(s.len(), |p| start + p);
                if end > start {
                    let tag = &s[start..end];
                    let mut j = memchr::memchr(b'\n', &s[end..]).map_or(s.len(), |p| end + p + 1);
                    while j < s.len() {
                        let line_end = memchr::memchr(b'\n', &s[j..]).map_or(s.len(), |p| j + p);
                        if s[j..line_end].trim_ascii() == tag {
                            i = line_end;
                            break;
                        }
                        j = line_end + 1;
                    }
                    if j >= s.len() {
                        return s.len();
                    }
                    continue;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth > 0 => depth -= 1,
            b')' | b']' | b'}' => return i,
            b'\n' if depth == 0 && newline_ends => return i,
            c if depth == 0 && stop.contains(&c) => return i,
            b'#' if depth == 0 && hash_comments => return memchr::memchr(b'\n', &s[i..]).map_or(s.len(), |p| i + p).min(if newline_ends { usize::MAX } else { s.len() }),
            b'/' if s[i..].starts_with(b"//") && depth == 0 => return i,
            _ => {}
        }
        i += 1;
    }
    s.len()
}

fn squash(s: &[u8]) -> String {
    String::from_utf8_lossy(s).split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------- HCL

struct Hcl<'a> {
    s: &'a [u8],
    i: usize,
    lines: LineCounter<'a>,
}

impl Hcl<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if c.is_ascii_whitespace() || c == b',' {
                self.i += 1;
            } else if c == b'#' || self.s[self.i..].starts_with(b"//") {
                self.i = memchr::memchr(b'\n', &self.s[self.i..]).map_or(self.s.len(), |p| self.i + p);
            } else if self.s[self.i..].starts_with(b"/*") {
                self.i = memchr::memmem::find(&self.s[self.i + 2..], b"*/").map_or(self.s.len(), |p| self.i + 2 + p + 2);
            } else {
                break;
            }
        }
    }

    /// An identifier or a quoted label.
    fn word(&mut self) -> Option<String> {
        let s = self.s;
        if s.get(self.i) == Some(&b'"') {
            let mut j = self.i + 1;
            while j < s.len() && s[j] != b'"' {
                j += if s[j] == b'\\' { 2 } else { 1 };
            }
            let w = String::from_utf8_lossy(&s[self.i + 1..j.min(s.len())]).into_owned();
            self.i = (j + 1).min(s.len());
            return Some(w);
        }
        let start = self.i;
        while self.i < s.len() && (s[self.i].is_ascii_alphanumeric() || matches!(s[self.i], b'_' | b'-' | b'.')) {
            self.i += 1;
        }
        (self.i > start).then(|| String::from_utf8_lossy(&s[start..self.i]).into_owned())
    }

    /// A body: attributes and blocks, to `}` or the end.
    fn body(&mut self, b: &mut Builder, parent: Option<u32>, depth: usize) -> Result<()> {
        loop {
            self.skip();
            if self.i >= self.s.len() {
                return Ok(());
            }
            if self.s[self.i] == b'}' {
                self.i += 1;
                return Ok(());
            }
            let start = self.i;
            let line = self.lines.line(start);
            let Some(name) = self.word() else {
                return Err(ParseError::corrupt_at("HCL: expected a name", Position::in_text(self.s, self.i)));
            };
            // Spaces only, so a block's labels stay on its line.
            while self.s.get(self.i).is_some_and(|c| *c == b' ' || *c == b'\t') {
                self.i += 1;
            }
            match self.s.get(self.i) {
                Some(b'=' | b':') if self.s.get(self.i + 1) != Some(&b'=') => {
                    self.i += 1;
                    while self.s.get(self.i).is_some_and(|c| *c == b' ' || *c == b'\t') {
                        self.i += 1;
                    }
                    if self.s.get(self.i) == Some(&b'{') {
                        // An object: its keys as children.
                        self.i += 1;
                        let id = b.add(parent, depth, name, "", ValueKind::Map, line..line + 1)?;
                        self.body(b, Some(id), depth + 1)?;
                        let n = b.children(id);
                        b.set_value(id, format!("{{{n}}}"));
                        let last = self.lines.line(self.i.saturating_sub(1));
                        b.end(id, last);
                    } else {
                        let end = expression_end(self.s, self.i, b",", true, true);
                        let value = squash(&self.s[self.i..end]);
                        let last = self.lines.line(end.saturating_sub(1).max(self.i));
                        let kind = expr_kind(&value);
                        let value = if kind == ValueKind::Text { value.trim_matches('"').to_string() } else { value };
                        b.add(parent, depth, name, value, kind, line..last + 1)?;
                        self.i = end;
                    }
                }
                _ => {
                    // A block: labels, then `{`.
                    let mut labels = vec![name.clone()];
                    loop {
                        while self.s.get(self.i).is_some_and(|c| *c == b' ' || *c == b'\t') {
                            self.i += 1;
                        }
                        if self.s.get(self.i) == Some(&b'{') {
                            break;
                        }
                        match self.word() {
                            Some(l) => labels.push(l),
                            None => return Err(ParseError::corrupt_at("HCL: expected `=` or `{`", Position::in_text(self.s, self.i))),
                        }
                    }
                    self.i += 1;
                    let label = labels.iter().map(|l| if l.contains(' ') { format!("\"{l}\"") } else { l.clone() }).collect::<Vec<_>>().join(" ");
                    let id = b.add(parent, depth, label, name, ValueKind::Group, line..line + 1)?;
                    self.body(b, Some(id), depth + 1)?;
                    let last = self.lines.line(self.i.saturating_sub(1));
                    b.end(id, last);
                }
            }
        }
    }
}

/// What a written value is: a string, a number, a boolean, null, or an expression.
fn expr_kind(v: &str) -> ValueKind {
    match v {
        "true" | "false" => ValueKind::Bool,
        "null" => ValueKind::Null,
        _ if v.starts_with('"') && v.ends_with('"') && v.len() >= 2 && !v[1..v.len() - 1].contains('"') => ValueKind::Text,
        _ if v.parse::<f64>().is_ok() => ValueKind::Number,
        _ => ValueKind::Unknown,
    }
}

pub(crate) fn hcl(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let s = src.bytes();
    let mut b = Builder::new(cx);
    let mut p = Hcl { s, i: 0, lines: LineCounter::new(s) };
    p.body(&mut b, None, 1)?;
    let tree = b.finish()?;
    // Top-level blocks: Terraform's resources, variables, modules.
    let blocks: Vec<u32> = tree.roots().iter().copied().filter(|&r| tree.kind(r) == ValueKind::Group).collect();
    let tree = Arc::new(tree);
    let table = NodeTable::new(tree, blocks.into())
        .column("Type", ValueKind::Text, |t, row| Cell::Text(t.value(row).to_string()))
        .column("Name", ValueKind::Text, |t, row| {
            let label = t.label(row);
            let rest: Vec<&str> = label.split_whitespace().skip(1).map(|l| l.trim_matches('"')).collect();
            if rest.is_empty() { Cell::Null } else { Cell::Text(rest.join(".")) }
        })
        .column("Attributes", ValueKind::Number, |t, row| Cell::Int(t.child_ids(row).iter().filter(|&&c| t.kind(c) != ValueKind::Group).count() as i64))
        .column("Blocks", ValueKind::Number, |t, row| Cell::Int(t.child_ids(row).iter().filter(|&&c| t.kind(c) == ValueKind::Group).count() as i64));
    let tf = matches!(src.ext().as_deref(), Some("tf" | "tfvars"));
    Ok(nodes::document(format("hcl", if tf { "Terraform" } else { "HCL" }), "Blocks", table, src, Some("hcl"), true))
}

// ---------------------------------------------------------------- CUE, Dhall, Nickel

/// Records of fields, `key: value` or `key = value`, braces nesting them; other
/// statements (`let x = …`, `import`) kept as written.
struct Fields<'a> {
    s: &'a [u8],
    i: usize,
    lines: LineCounter<'a>,
    comments: &'static [&'static str],
}

impl Fields<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if c.is_ascii_whitespace() || c == b',' || c == b';' {
                self.i += 1;
            } else if self.s[self.i..].starts_with(b"{-") {
                self.i = memchr::memmem::find(&self.s[self.i + 2..], b"-}").map_or(self.s.len(), |p| self.i + 2 + p + 2);
            } else if self.s[self.i..].starts_with(b"/*") {
                self.i = memchr::memmem::find(&self.s[self.i + 2..], b"*/").map_or(self.s.len(), |p| self.i + 2 + p + 2);
            } else if self.comments.iter().any(|c| self.s[self.i..].starts_with(c.as_bytes())) {
                self.i = memchr::memchr(b'\n', &self.s[self.i..]).map_or(self.s.len(), |p| self.i + p);
            } else {
                break;
            }
        }
    }

    fn record(&mut self, b: &mut Builder, parent: Option<u32>, depth: usize) -> Result<()> {
        loop {
            self.skip();
            if self.i >= self.s.len() {
                return Ok(());
            }
            if matches!(self.s[self.i], b'}' | b']' | b')') {
                self.i += 1;
                return Ok(());
            }
            let start = self.i;
            let line = self.lines.line(start);
            // A record on its own (`in { … }`'s, a list item's): a node of its own.
            if self.s[self.i] == b'{' {
                self.i += 1;
                let id = b.add(parent, depth, "(record)", "", ValueKind::Map, line..line + 1)?;
                self.record(b, Some(id), depth + 1)?;
                let n = b.children(id);
                b.set_value(id, format!("{{{n}}}"));
                let last = self.lines.line(self.i.saturating_sub(1));
                b.end(id, last);
                continue;
            }
            // The statement, to a separator at depth zero: is it `key: value`?
            let end = expression_end(self.s, self.i, b",;", true, self.comments.contains(&"#"));
            let stmt = &self.s[start..end];
            let sep = field_separator(stmt);
            match sep {
                Some(at) => {
                    let key = squash(&stmt[..at]);
                    let key = key.trim_matches('"').to_string();
                    let mut v = start + at + 1;
                    while self.s.get(v).is_some_and(|c| c.is_ascii_whitespace() && *c != b'\n') {
                        v += 1;
                    }
                    if self.s.get(v) == Some(&b'{') {
                        self.i = v + 1;
                        let id = b.add(parent, depth, key, "", ValueKind::Map, line..line + 1)?;
                        self.record(b, Some(id), depth + 1)?;
                        let n = b.children(id);
                        b.set_value(id, format!("{{{n}}}"));
                        let last = self.lines.line(self.i.saturating_sub(1));
                        b.end(id, last);
                        // What follows the record (`& Other`, `// a type`) to the separator.
                        self.i = expression_end(self.s, self.i, b",;", true, self.comments.contains(&"#"));
                    } else {
                        let value = squash(&self.s[v.min(end)..end]);
                        let kind = expr_kind(&value);
                        let value = if kind == ValueKind::Text { value.trim_matches('"').to_string() } else { value };
                        let last = self.lines.line(end.saturating_sub(1).max(start));
                        b.add(parent, depth, key, value, kind, line..last + 1)?;
                        self.i = end;
                    }
                }
                None => {
                    let text = squash(stmt);
                    let last = self.lines.line(end.saturating_sub(1).max(start));
                    if !text.is_empty() {
                        let label = text.split_whitespace().next().unwrap_or("").to_string();
                        b.add(parent, depth, label, text, ValueKind::Unknown, line..last + 1)?;
                    }
                    self.i = end.max(start + 1);
                }
            }
        }
    }
}

/// Where a field's `:` or `=` is: after a label (a name, or a quoted one), not `==`
/// or `:=`'s second half, not `::`.
fn field_separator(stmt: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < stmt.len() && stmt[i].is_ascii_whitespace() {
        i += 1;
    }
    if stmt.get(i) == Some(&b'"') {
        i += 1;
        while i < stmt.len() && stmt[i] != b'"' {
            i += 1;
        }
        i += 1;
    } else {
        while i < stmt.len() && (stmt[i].is_ascii_alphanumeric() || matches!(stmt[i], b'_' | b'-' | b'#' | b'$' | b'?' | b'!' | b'.')) {
            i += 1;
        }
    }
    while i < stmt.len() && (stmt[i] == b' ' || stmt[i] == b'\t') {
        i += 1;
    }
    match (stmt.get(i), stmt.get(i + 1)) {
        (Some(b':' | b'='), next) if !matches!(next, Some(b'=' | b':')) => Some(i),
        _ => None,
    }
}

pub(crate) fn fields(id: &'static str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let (name, comments, language): (&str, &'static [&'static str], &str) = match id {
        "cue" => ("CUE", &["//"], "cue"),
        "dhall" => ("Dhall", &["--"], "dhall"),
        _ => ("Nickel", &["#"], "nickel"),
    };
    let s = src.bytes();
    let mut b = Builder::new(cx);
    let mut p = Fields { s, i: 0, lines: LineCounter::new(s), comments };
    p.record(&mut b, None, 1)?;
    let tree = b.finish()?;
    Ok(crate::json::tree_document(format(id, name), tree, src, language))
}

// ---------------------------------------------------------------- KDL

pub(crate) fn kdl(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let text = String::from_utf8_lossy(src.bytes());
    let s = text.as_bytes();
    let mut b = Builder::new(cx);
    let mut lines = LineCounter::new(s);
    let mut stack: Vec<u32> = Vec::new();
    let mut i = 0;
    // Tokens of one node to its end (a line break, `;`, `{` or `}`).
    let token = |i: &mut usize| -> Option<String> {
        let start = *i;
        match s.get(*i)? {
            b'"' => {
                let mut v = String::new();
                *i += 1;
                while *i < s.len() && s[*i] != b'"' {
                    if s[*i] == b'\\' && *i + 1 < s.len() {
                        *i += 1;
                        v.push(match s[*i] {
                            b'n' => '\n',
                            b't' => '\t',
                            c => c as char,
                        });
                    } else {
                        let ch = text[*i..].chars().next()?;
                        v.push(ch);
                        *i += ch.len_utf8() - 1;
                    }
                    *i += 1;
                }
                *i += 1;
                Some(format!("\"{v}\""))
            }
            b'r' if s.get(*i + 1).is_some_and(|c| *c == b'#' || *c == b'"') => {
                let hashes = s[*i + 1..].iter().take_while(|c| **c == b'#').count();
                let open = *i + 1 + hashes + 1;
                let close: Vec<u8> = std::iter::once(b'"').chain(std::iter::repeat_n(b'#', hashes)).collect();
                let end = memchr::memmem::find(&s[open..], &close).map_or(s.len(), |p| open + p);
                *i = (end + close.len()).min(s.len());
                Some(format!("\"{}\"", &text[open..end]))
            }
            _ => {
                while *i < s.len() && !s[*i].is_ascii_whitespace() && !matches!(s[*i], b';' | b'{' | b'}' | b'"') {
                    *i += 1;
                }
                (*i > start).then(|| text[start..*i].to_string())
            }
        }
    };
    while i < s.len() {
        let c = s[i];
        if c.is_ascii_whitespace() || c == b';' {
            i += 1;
            continue;
        }
        if text[i..].starts_with("//") {
            i = memchr::memchr(b'\n', &s[i..]).map_or(s.len(), |p| i + p);
            continue;
        }
        if text[i..].starts_with("/*") {
            i = memchr::memmem::find(&s[i + 2..], b"*/").map_or(s.len(), |p| i + 2 + p + 2);
            continue;
        }
        if c == b'}' {
            if let Some(id) = stack.pop() {
                let line = lines.line(i);
                b.end(id, line);
            }
            i += 1;
            continue;
        }
        // `/-` comments out the node that follows.
        let skipped = text[i..].starts_with("/-");
        if skipped {
            i += 2;
            while i < s.len() && s[i].is_ascii_whitespace() {
                i += 1;
            }
        }
        let line = lines.line(i);
        let name = token(&mut i).unwrap_or_default();
        let mut parts = Vec::new();
        loop {
            while i < s.len() && (s[i] == b' ' || s[i] == b'\t' || (s[i] == b'\\' && s.get(i + 1).is_some_and(|c| *c == b'\n' || *c == b'\r'))) {
                i += if s[i] == b'\\' { 2 } else { 1 };
            }
            match s.get(i) {
                None | Some(b'\n' | b'\r' | b';' | b'}') => break,
                Some(b'{') => break,
                Some(b'/') if text[i..].starts_with("//") => break,
                Some(b'/') if text[i..].starts_with("/-") => {
                    i += 2;
                    token(&mut i);
                }
                _ => match token(&mut i) {
                    Some(t) => parts.push(t),
                    None => i += 1,
                },
            }
        }
        let opens = s.get(i) == Some(&b'{');
        if opens {
            i += 1;
        }
        let last = lines.line(i.saturating_sub(1));
        let name = name.trim_matches('"').to_string();
        // A type annotation, `(u8)`, is dropped from the name.
        let name = name.rsplit(')').next().unwrap_or(&name).to_string();
        if skipped {
            if opens {
                // Skip the commented-out children too.
                let mut depth = 1;
                while i < s.len() && depth > 0 {
                    match s[i] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            continue;
        }
        let value = parts.join(" ");
        let kind = if parts.len() == 1 { expr_kind(parts[0].trim_start_matches('#')) } else { ValueKind::Unknown };
        let value = if kind == ValueKind::Text { value.trim_matches('"').to_string() } else { value };
        let id = b.add(stack.last().copied(), stack.len() + 1, name, value, if opens { ValueKind::Group } else { kind }, line..last + 1)?;
        if opens {
            stack.push(id);
        }
    }
    let tree = b.finish()?;
    Ok(crate::json::tree_document(format("kdl", "KDL"), tree, src, "kdl"))
}

// ---------------------------------------------------------------- cron

/// A cron field in words: `*/15` → "every 15", `1-5` → "1 to 5".
fn field_words(f: &str, unit: &str) -> Option<String> {
    Some(match f {
        "*" => return None,
        f if f.starts_with("*/") => format!("every {} {unit}s", &f[2..]),
        f => format!("{unit} {}", f.replace(',', ", ").replace('-', " to ")),
    })
}

/// A schedule in words, roughly: "at 02:30, on weekday 1 to 5".
pub(crate) fn describe(fields: &[&str]) -> String {
    if let Some(at) = fields.first().filter(|f| f.starts_with('@')) {
        return match *at {
            "@reboot" => "at startup".into(),
            "@yearly" | "@annually" => "once a year".into(),
            "@monthly" => "once a month".into(),
            "@weekly" => "once a week".into(),
            "@daily" | "@midnight" => "once a day".into(),
            "@hourly" => "once an hour".into(),
            other => other.into(),
        };
    }
    let [min, hour, dom, mon, dow] = [0, 1, 2, 3, 4].map(|i| fields.get(i).copied().unwrap_or("*"));
    let mut parts = Vec::new();
    let plain = |f: &str| f.bytes().all(|c| c.is_ascii_digit());
    match (min, hour) {
        ("*", "*") => parts.push("every minute".to_string()),
        (m, h) if plain(m) && plain(h) => parts.push(format!("at {:0>2}:{:0>2}", h, m)),
        (m, "*") if plain(m) => parts.push(format!("at minute {m} of every hour")),
        (m, h) => {
            parts.extend(field_words(m, "minute"));
            parts.extend(field_words(h, "hour"));
        }
    }
    parts.extend(field_words(dom, "day"));
    parts.extend(field_words(mon, "month"));
    parts.extend(field_words(dow, "weekday"));
    parts.join(", ")
}

pub(crate) fn cron(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let index = lines::index(src, "crontab", cx, |_, line| {
        let l = line.trim();
        // Comments and `NAME=value` settings aren't jobs.
        if l.is_empty() || l.starts_with('#') || l.split_whitespace().next().is_some_and(|w| w.contains('=')) { Line::Within } else { Line::Start }
    })?;
    let cols = [("Minute", ValueKind::Text), ("Hour", ValueKind::Text), ("Day", ValueKind::Text), ("Month", ValueKind::Text), ("Weekday", ValueKind::Text), ("Command", ValueKind::Text), ("Schedule", ValueKind::Text)];
    let table = LineTable::new(index, lines::columns(&cols), |_, lines| {
        let l = lines.first().map_or("", |l| l.trim());
        let words: Vec<&str> = l.split_whitespace().collect();
        let n = if words.first().is_some_and(|w| w.starts_with('@')) { 1 } else { 5 };
        let fields: Vec<&str> = words.iter().take(n).copied().collect();
        // The command: the rest of the line as written.
        let mut rest = l;
        for w in &fields {
            rest = rest.trim_start().strip_prefix(w).unwrap_or(rest);
        }
        let mut row: Vec<Cell> = if n == 1 { vec![Cell::Null; 5] } else { fields.iter().map(|f| text(f)).chain(std::iter::repeat(Cell::Null)).take(5).collect() };
        row.push(text(rest.trim()));
        row.push(Cell::Text(describe(&fields)));
        row
    });
    Ok(lines::document("cron", "Crontab", "Jobs", table, src, "crontab"))
}

// ---------------------------------------------------------------- SGML

pub(crate) fn sgml(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::markup::parse(src.bytes(), cx, false)?;
    if tree.roots().is_empty() {
        return Err(ParseError::corrupt("SGML: no element"));
    }
    Ok(crate::json::tree_document(format("sgml", "SGML"), tree, src, "sgml"))
}

// ---------------------------------------------------------------- fixed-width

/// Columns of fixed-width text: character ranges where some sampled line has text,
/// split by runs of positions blank in every line.
fn fixed_columns(sample: &[Vec<char>]) -> Vec<std::ops::Range<usize>> {
    let width = sample.iter().map(Vec::len).max().unwrap_or(0);
    let used: Vec<bool> = (0..width).map(|c| sample.iter().any(|l| l.get(c).is_some_and(|ch| !ch.is_whitespace()))).collect();
    let mut cols = Vec::new();
    let mut start = None;
    for (c, &u) in used.iter().enumerate() {
        match (u, start) {
            (true, None) => start = Some(c),
            (false, Some(s)) => {
                cols.push(s..c);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        cols.push(s..width);
    }
    // A column runs to where the next starts, so ragged values aren't cut.
    let n = cols.len();
    for i in 0..n.saturating_sub(1) {
        cols[i].end = cols[i + 1].start;
    }
    if let Some(last) = cols.last_mut() {
        last.end = usize::MAX;
    }
    cols
}

pub(crate) fn fixed_width(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let head = String::from_utf8_lossy(src.header());
    let mut sample: Vec<&str> = head.lines().filter(|l| !l.trim().is_empty()).take(200).collect();
    if src.len() as usize > src.header().len() {
        sample.pop();
    }
    let chars: Vec<Vec<char>> = sample.iter().map(|l| l.chars().collect()).collect();
    let cols = fixed_columns(&chars);
    if cols.len() < 2 {
        return Err(ParseError::NotThisFormat);
    }
    let cut = |line: &[char], r: &std::ops::Range<usize>| -> String { line.get(r.start.min(line.len())..r.end.min(line.len())).unwrap_or(&[]).iter().collect::<String>().trim().to_string() };
    // A first line of words over columns of numbers is a header.
    let is_num = |v: &str| !v.is_empty() && v.replace([',', ' '], "").parse::<f64>().is_ok();
    let header = chars.first().map(|l| cols.iter().map(|c| cut(l, c)).collect::<Vec<_>>()).filter(|h| h.iter().all(|v| !v.is_empty() && !is_num(v)));
    let body = &chars[usize::from(header.is_some())..];
    let kinds: Vec<ValueKind> = cols
        .iter()
        .map(|c| {
            let mut vals = body.iter().map(|l| cut(l, c)).filter(|v| !v.is_empty()).peekable();
            if vals.peek().is_some() && vals.all(|v| is_num(&v)) { ValueKind::Number } else { ValueKind::Text }
        })
        .collect();
    let names: Vec<String> = match &header {
        Some(h) => h.clone(),
        None => (1..=cols.len()).map(|i| format!("Column {i}")).collect(),
    };
    let skip_first = header.is_some();
    let mut first = true;
    let index = lines::index(src, "text", cx, |_, line| {
        if line.trim().is_empty() {
            return Line::Within;
        }
        if first {
            first = false;
            if skip_first {
                return Line::Within;
            }
        }
        Line::Start
    })?;
    let columns: Vec<Column> = names.iter().zip(&kinds).map(|(n, k)| Column::new(n.as_str()).with_kind(*k)).collect();
    let table = LineTable::new(index, columns, move |_, lines| {
        let line: Vec<char> = lines.first().map_or_else(Vec::new, |l| l.chars().collect());
        cols.iter().zip(&kinds).map(|(c, k)| {
            let v = cut(&line, c);
            match k {
                ValueKind::Number => lines::num(&v.replace(',', "")),
                _ => text(&v),
            }
        }).collect()
    });
    Ok(lines::document("fixed-width", "Fixed-width table", "Rows", table, src, "text"))
}

#[allow(dead_code)]
fn _tree(_: &DataTree) {}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::TreeSource;

    fn top(t: &DataTree) -> Vec<(String, String)> {
        t.children(None, 0..50).unwrap().into_iter().map(|n| (n.label, n.value)).collect()
    }

    #[test]
    fn hcl_blocks_and_attributes() {
        let cx = ParseContext::new();
        let s = b"# infra\nterraform { required_version = \">= 1.5\" }\nresource \"aws_s3_bucket\" \"logs\" {\n  bucket = \"my-logs\"\n  tags = {\n    env = \"prod\"\n  }\n  count = 2\n  policy = <<EOF\n{ \"a\": 1 }\nEOF\n  list = [1,\n    2]\n}\n";
        let mut b = Builder::new(&cx);
        let mut p = Hcl { s, i: 0, lines: LineCounter::new(s) };
        p.body(&mut b, None, 1).unwrap();
        let t = b.finish().unwrap();
        assert_eq!(top(&t), [("terraform".to_string(), "terraform".to_string()), ("resource aws_s3_bucket logs".to_string(), "resource".to_string())]);
        let r = t.find("/resource aws_s3_bucket logs").unwrap().unwrap();
        let kids: Vec<(String, String)> = t.children(Some(r), 0..9).unwrap().into_iter().map(|n| (n.label, n.value)).collect();
        assert_eq!(kids[0], ("bucket".to_string(), "my-logs".to_string()));
        assert_eq!(kids[1], ("tags".to_string(), "{1}".to_string()));
        assert_eq!(kids[2], ("count".to_string(), "2".to_string()));
        assert_eq!(kids[3].0, "policy");
        assert_eq!(kids[4], ("list".to_string(), "[1, 2]".to_string()));
    }

    #[test]
    fn schedules_in_words() {
        assert_eq!(describe(&["30", "2", "*", "*", "*"]), "at 02:30");
        assert_eq!(describe(&["*/15", "*", "*", "*", "1-5"]), "every 15 minutes, weekday 1 to 5");
        assert_eq!(describe(&["@reboot"]), "at startup");
        assert_eq!(describe(&["0", "*", "1", "*", "*"]), "at minute 0 of every hour, day 1");
    }

    #[test]
    fn fixed_width_columns() {
        let lines: Vec<Vec<char>> = ["NAME    AGE  CITY", "Ann      42  Paris", "Bob       7  New York"].iter().map(|l| l.chars().collect()).collect();
        let cols = fixed_columns(&lines);
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0], 0..8);
    }
}
