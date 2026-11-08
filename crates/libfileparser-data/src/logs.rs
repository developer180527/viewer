//! Logs as tables of entries: syslog (RFC 5424 and BSD/3164), web server access logs
//! (Common and Combined), logfmt, LTSV, CEF, and the most common shape of all, a
//! timestamp, a level and a message. A line that doesn't start an entry (a stack
//! trace) belongs to the entry above it.
//!
//! The file is read through core's line index, so a log opened with
//! `FileSource::open_live` is read a range at a time, never whole. Opening indexes
//! where each entry starts (4 bytes an entry); entries are parsed when their rows are
//! read.
//!
//! **Columns** (contract, docs/design/libfileparser.md §5): fixed per format (below);
//! for logfmt, LTSV and CEF's extension, the keys of the first entries in the order
//! first seen.

use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Bytes, Category, Cell, Column, Content, Count, Document, FileSource, FormatInfo, Limit, LineText, Links, MemoryUse, ParseContext,
    ParseError, Place, Result, Role, TableInfo, TableSource, TextSource, ValueKind,
};

/// Lines sampled to tell a log's format.
const SAMPLE_LINES: usize = 64;
/// Entries sampled for the columns of formats whose columns are their keys.
const KEY_SAMPLE: usize = 1000;
const MAX_COLUMNS: usize = 256;
/// Lines read at a time while indexing.
const BATCH: u64 = 4096;

/// A log's format: how its entries start and what they hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    /// RFC 5424: `<34>1 2024-11-08T10:00:00Z host app 123 ID47 [sd] message`.
    Syslog,
    /// BSD syslog (RFC 3164) and the files syslog daemons write: `Nov  8 10:00:00 host app[123]: message`.
    BsdSyslog,
    /// Common and Combined Log Format: `1.2.3.4 - user [08/Nov/2024:10:00:00 +0000] "GET / HTTP/1.1" 200 512 "ref" "agent"`.
    Access,
    /// `time=… level=info msg="…"`.
    Logfmt,
    /// `2024-11-08 10:00:00.123 [INFO] message`, and its many relatives.
    Stamped,
    /// Labelled tab-separated values: `time:…<TAB>host:…`.
    Ltsv,
    /// ArcSight Common Event Format, alone or after a syslog header.
    Cef,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Syslog => "Syslog",
            Format::BsdSyslog => "Syslog (BSD)",
            Format::Access => "Web server access log",
            Format::Logfmt => "Logfmt log",
            Format::Stamped => "Log",
            Format::Ltsv => "LTSV",
            Format::Cef => "CEF",
        }
    }

    /// Whether `line` starts an entry of this format.
    fn starts(self, line: &str) -> bool {
        match self {
            // Daemons mix the two: RFC 5424 lines among BSD ones.
            Format::Syslog => syslog(line).is_some() || bsd_syslog(line).is_some(),
            Format::BsdSyslog => bsd_syslog(line).is_some(),
            Format::Access => access(line).is_some(),
            Format::Logfmt => logfmt(line).is_some_and(|p| p.len() >= 2),
            Format::Stamped => stamp_len(line).is_some(),
            Format::Ltsv => ltsv(line).is_some(),
            Format::Cef => cef(line).is_some(),
        }
    }

    /// Whether every line is an entry of its own (no continuation lines).
    fn one_line(self) -> bool {
        matches!(self, Format::Access | Format::Ltsv | Format::Cef | Format::Logfmt)
    }
}

/// The format most of `sample`'s lines are in, if any is: `hint` first (the
/// extension's), then the most specific. Continuation lines count against nothing:
/// the first line must start an entry, and at least two in five must.
pub(crate) fn detect(sample: &[&str], hint: Option<Format>) -> Option<Format> {
    let lines: Vec<&str> = sample.iter().copied().filter(|l| !l.trim().is_empty()).take(SAMPLE_LINES).collect();
    let first = *lines.first()?;
    let candidates = [Format::Cef, Format::Syslog, Format::BsdSyslog, Format::Access, Format::Ltsv, Format::Logfmt, Format::Stamped];
    let order = hint.into_iter().chain(candidates.into_iter().filter(|&f| Some(f) != hint));
    let mut best: Option<(Format, usize)> = None;
    for f in order {
        // RFC 5424 only when some line is: it reads BSD lines too.
        if !f.starts(first) || (f == Format::Syslog && !lines.iter().any(|l| syslog(l).is_some())) {
            continue;
        }
        let n = lines.iter().filter(|l| f.starts(l)).count();
        let enough = if f.one_line() { n * 10 >= lines.len() * 8 } else { n * 5 >= lines.len() * 2 };
        if enough && best.is_none_or(|(_, m)| n > m) {
            best = Some((f, n));
        }
    }
    best.map(|(f, _)| f)
}

/// A log as a table of its entries.
pub(crate) struct LogTable {
    text: Arc<LineText>,
    format: Format,
    columns: Vec<Column>,
    /// The first line of each entry.
    entries: Vec<u32>,
    lines: u64,
}

impl LogTable {
    pub fn index(src: &FileSource, format: Format, cx: &ParseContext) -> Result<Self> {
        let text = Arc::new(LineText::within(src.clone(), Some("log"), &cx.limits)?);
        let lines = text.info().lines.known();
        let mut entries = Vec::new();
        let mut at = 0;
        while at < lines {
            cx.cancel.check()?;
            let batch = text.lines(at..(at + BATCH).min(lines))?;
            for (i, line) in batch.iter().enumerate() {
                let n = at + i as u64;
                let line = String::from_utf8_lossy(line);
                if line.trim().is_empty() && format.one_line() {
                    continue;
                }
                // The first line always starts an entry: a header before the first stamp.
                if entries.is_empty() || (format.starts(&line) && !line.trim().is_empty()) || (format.one_line() && !line.trim().is_empty()) {
                    entries.push(n as u32);
                }
            }
            cx.limits.check(Limit::Rows, entries.len() as u64)?;
            at += BATCH;
        }
        let mut table = Self { text, format, columns: Vec::new(), entries, lines };
        table.columns = table.make_columns()?;
        Ok(table)
    }

    fn entry_lines(&self, entry: usize) -> Range<u64> {
        let start = self.entries[entry] as u64;
        let end = self.entries.get(entry + 1).map_or(self.lines, |&e| e as u64);
        start..end
    }

    /// The text of entries `range`, each as its lines.
    fn entry_text(&self, range: Range<usize>) -> Result<Vec<Vec<String>>> {
        if range.is_empty() {
            return Ok(Vec::new());
        }
        let first = self.entries[range.start] as u64;
        let last = self.entry_lines(range.end - 1).end;
        let lines: Vec<Bytes> = self.text.lines(first..last)?;
        Ok(range
            .map(|e| {
                let r = self.entry_lines(e);
                lines[(r.start - first) as usize..(r.end - first) as usize]
                    .iter()
                    .map(|l| String::from_utf8_lossy(l).into_owned())
                    .collect()
            })
            .collect())
    }

    fn make_columns(&self) -> Result<Vec<Column>> {
        let fixed = |cols: &[(&str, ValueKind)]| cols.iter().map(|&(n, k)| Column::new(n).with_kind(k)).collect::<Vec<_>>();
        use ValueKind::{Date, Number, Text};
        Ok(match self.format {
            Format::Syslog => fixed(&[("Time", Date), ("Host", Text), ("App", Text), ("Process", Text), ("Message ID", Text), ("Level", Text), ("Data", Text), ("Message", Text)]),
            Format::BsdSyslog => fixed(&[("Time", Date), ("Host", Text), ("App", Text), ("Process", Text), ("Level", Text), ("Message", Text)]),
            Format::Access => fixed(&[
                ("Client", Text),
                ("User", Text),
                ("Time", Date),
                ("Method", Text),
                ("Path", Text),
                ("Protocol", Text),
                ("Status", Number),
                ("Size", Number),
                ("Referrer", Text),
                ("User agent", Text),
            ]),
            Format::Stamped => fixed(&[("Time", Date), ("Level", Text), ("Message", Text)]),
            Format::Cef => {
                let mut cols = fixed(&[
                    ("Time", Date),
                    ("Host", Text),
                    ("Vendor", Text),
                    ("Product", Text),
                    ("Version", Text),
                    ("Signature", Text),
                    ("Name", Text),
                    ("Severity", Text),
                ]);
                cols.extend(self.key_columns(|l| cef(l).map(|c| c.extension))?);
                cols
            }
            Format::Logfmt => self.key_columns(logfmt)?,
            Format::Ltsv => self.key_columns(ltsv)?,
        })
    }

    /// Columns from the keys of the first entries, a number where every sampled value is one.
    fn key_columns(&self, pairs: impl Fn(&str) -> Option<Vec<(String, String)>>) -> Result<Vec<Column>> {
        let n = self.entries.len().min(KEY_SAMPLE);
        let mut cols: Vec<(String, bool, bool)> = Vec::new(); // name, all numbers, seen a value
        for entry in self.entry_text(0..n)? {
            for (k, v) in entry.first().and_then(|l| pairs(l)).unwrap_or_default() {
                let i = match cols.iter().position(|c| c.0 == k) {
                    Some(i) => i,
                    None if cols.len() < MAX_COLUMNS => {
                        cols.push((k, true, false));
                        cols.len() - 1
                    }
                    None => continue,
                };
                if !v.is_empty() {
                    cols[i].2 = true;
                    cols[i].1 &= is_number(&v);
                }
            }
        }
        Ok(cols
            .into_iter()
            .map(|(name, numbers, seen)| {
                let kind = if is_time_key(&name) {
                    ValueKind::Date
                } else if numbers && seen {
                    ValueKind::Number
                } else {
                    ValueKind::Text
                };
                Column::new(name).with_kind(kind)
            })
            .collect())
    }

    /// One entry's cells.
    fn cells(&self, lines: &[String]) -> Vec<Cell> {
        let first = lines.first().map_or("", String::as_str);
        // Lines after the first: a stack trace, a multi-line message.
        let more = || lines.get(1..).filter(|l| !l.is_empty()).map(|l| l.join("\n"));
        let with_more = |msg: &str| match more() {
            Some(m) => format!("{msg}\n{m}"),
            None => msg.to_string(),
        };
        let text = |s: Option<&str>| s.filter(|s| !s.is_empty() && *s != "-").map_or(Cell::Null, |s| Cell::Text(s.to_string()));
        let date = |s: Option<&str>| s.filter(|s| !s.is_empty() && *s != "-").map_or(Cell::Null, |s| Cell::Date(s.to_string()));
        let num = |s: Option<&str>| s.filter(|s| is_number(s)).map_or(Cell::Null, Cell::number);
        let row = match self.format {
            Format::Syslog => syslog(first).or_else(|| bsd_syslog(first)).map(|s| {
                vec![
                    date(s.time),
                    text(s.host),
                    text(s.app),
                    text(s.pid),
                    text(s.msgid),
                    text(s.level),
                    text(s.data),
                    Cell::Text(with_more(s.message)),
                ]
            }),
            Format::BsdSyslog => bsd_syslog(first).map(|s| {
                vec![date(s.time), text(s.host), text(s.app), text(s.pid), text(s.level), Cell::Text(with_more(s.message))]
            }),
            Format::Access => access(first).map(|a| {
                vec![
                    text(Some(a.client)),
                    text(Some(a.user)),
                    date(Some(a.time)),
                    text(a.method),
                    text(a.path),
                    text(a.protocol),
                    num(Some(a.status)),
                    num(Some(a.size)),
                    text(a.referrer),
                    text(a.agent),
                ]
            }),
            Format::Stamped => stamped(first).map(|s| vec![date(Some(s.time)), text(s.level), Cell::Text(with_more(s.message))]),
            Format::Cef => cef(first).map(|c| {
                let mut row = vec![
                    date(c.time),
                    text(c.host),
                    text(Some(&c.header[0])),
                    text(Some(&c.header[1])),
                    text(Some(&c.header[2])),
                    text(Some(&c.header[3])),
                    text(Some(&c.header[4])),
                    text(Some(&c.header[5])),
                ];
                row.extend(self.keyed(&self.columns[8..], &c.extension));
                row
            }),
            Format::Logfmt => logfmt(first).map(|p| self.keyed(&self.columns, &p)),
            Format::Ltsv => ltsv(first).map(|p| self.keyed(&self.columns, &p)),
        };
        // Text before the first entry (a header), or a line of another shape: kept,
        // whole, in the last text column.
        row.unwrap_or_else(|| {
            let mut row = vec![Cell::Null; self.columns.len()];
            if let Some(last) = row.last_mut() {
                *last = Cell::Text(lines.join("\n"));
            }
            row
        })
    }

    fn keyed(&self, columns: &[Column], pairs: &[(String, String)]) -> Vec<Cell> {
        columns
            .iter()
            .map(|c| match pairs.iter().find(|(k, _)| *k == c.name) {
                None => Cell::Null,
                Some((_, v)) if c.kind == ValueKind::Number && is_number(v) => Cell::number(v),
                Some((_, v)) if c.kind == ValueKind::Date && !v.is_empty() => Cell::Date(v.clone()),
                Some((_, v)) => Cell::Text(v.clone()),
            })
            .collect()
    }
}

impl TableSource for LogTable {
    fn info(&self) -> TableInfo {
        TableInfo::new(self.columns.clone(), Count::Exact(self.entries.len() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<Cell>>> {
        let end = range.end.min(self.entries.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(self.entry_text(start..end)?.iter().map(|lines| self.cells(lines)).collect())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.entries.capacity() as u64 * 4) + self.text.memory()
    }
}

/// Links between entries and their lines.
struct EntryLines(Arc<LogTable>);

impl Links for EntryLines {
    fn links(&self, _: &Document, from: &Place, _: &ParseContext) -> Result<Vec<Place>> {
        let t = &self.0;
        Ok(match *from {
            Place::Row { facet: 0, row } if (row as usize) < t.entries.len() => vec![Place::Lines(t.entry_lines(row as usize))],
            Place::Lines(ref r) => match t.entries.partition_point(|&e| e as u64 <= r.start).checked_sub(1) {
                Some(row) => vec![Place::Row { facet: 0, row: row as u64 }],
                None => Vec::new(),
            },
            _ => Vec::new(),
        })
    }
}

/// A log's document: its entries, then its text and bytes.
pub(crate) fn document(src: &FileSource, hint: Option<Format>, cx: &ParseContext) -> Result<Document> {
    let head = String::from_utf8_lossy(src.header());
    let mut sample: Vec<&str> = head.lines().collect();
    // The header may end mid-line.
    if src.len() as usize > src.header().len() {
        sample.pop();
    }
    let format = detect(&sample, hint).ok_or(ParseError::NotThisFormat)?;
    let table = Arc::new(LogTable::index(src, format, cx)?);
    Ok(Document::new(FormatInfo::new("log", format.name(), Category::Data))
        .with_facet(Role::Primary, "Entries", Content::Table(table.clone()))
        .with_meta("Entries", table.entries.len())
        .with_meta("Lines", table.lines)
        .with_links(EntryLines(table))
        .with_file_facets(src, Some("log")))
}

// ---------------------------------------------------------------- line formats

fn is_number(v: &str) -> bool {
    let d = v.strip_prefix('-').unwrap_or(v);
    d.starts_with(|c: char| c.is_ascii_digit()) && v.parse::<f64>().is_ok_and(f64::is_finite)
}

fn is_time_key(k: &str) -> bool {
    matches!(k.to_ascii_lowercase().as_str(), "time" | "ts" | "timestamp" | "@timestamp" | "datetime" | "date" | "t")
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const SEVERITIES: [&str; 8] = ["emerg", "alert", "crit", "err", "warning", "notice", "info", "debug"];

/// `<PRI>` at the start: its severity's name, and the rest.
fn priority(line: &str) -> Option<(&'static str, &str)> {
    let rest = line.strip_prefix('<')?;
    let end = rest.find('>')?;
    let pri: u16 = rest[..end].parse().ok().filter(|_| (1..=3).contains(&end))?;
    (pri <= 191).then(|| (SEVERITIES[(pri % 8) as usize], &rest[end + 1..]))
}

fn digits(s: &str, n: usize) -> bool {
    s.len() >= n && s.as_bytes()[..n].iter().all(u8::is_ascii_digit)
}

/// The length of an ISO 8601 date and time at the start of `s` (`2024-11-08T10:00:00.123Z`,
/// `2024-11-08 10:00:00,123 +01:00`, `2024/11/08 10:00:00`), if there is one.
fn iso_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if !(digits(s, 4) && matches!(b.get(4), Some(b'-' | b'/')) && digits(&s[5..], 2) && b.get(7) == b.get(4) && digits(s.get(8..)?, 2)) {
        return None;
    }
    if !matches!(b.get(10), Some(b'T' | b' ')) || !digits(s.get(11..)?, 2) || b.get(13) != Some(&b':') || !digits(s.get(14..)?, 2) {
        return None;
    }
    let mut i = 16;
    if b.get(i) == Some(&b':') && digits(s.get(i + 1..)?, 2) {
        i += 3;
        if matches!(b.get(i), Some(b'.' | b',')) && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
            i += 1;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
    }
    // A zone: `Z`, `+01:00`, `+0100`, or ` +01:00` after a space.
    let zone = |i: usize| -> Option<usize> {
        match b.get(i) {
            Some(b'Z') => Some(i + 1),
            Some(b'+' | b'-') if digits(s.get(i + 1..)?, 2) => {
                let mut j = i + 3;
                if b.get(j) == Some(&b':') {
                    j += 1;
                }
                if digits(s.get(j..)?, 2) { Some(j + 2) } else { Some(i + 3) }
            }
            _ => None,
        }
    };
    Some(zone(i).or_else(|| if b.get(i) == Some(&b' ') { zone(i + 1) } else { None }).unwrap_or(i))
}

/// `Nov  8 10:00:00` at the start of `s`: its length.
fn bsd_time_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    // Every check is within the first 15 bytes, which must be ASCII.
    if !s.get(..15).is_some_and(str::is_ascii) || !MONTHS.contains(&&s[..3]) || b[3] != b' ' {
        return None;
    }
    let day = s[4..6].trim_start();
    let ok = !day.is_empty() && day.bytes().all(|c| c.is_ascii_digit()) && b[6] == b' ' && digits(&s[7..], 2) && b[9] == b':' && digits(&s[10..], 2) && b[12] == b':' && digits(&s[13..], 2);
    ok.then_some(15)
}

/// The length of a timestamp an entry starts with, in brackets or not. Unix times
/// in seconds or milliseconds count when ten or thirteen digits long.
fn stamp_len(line: &str) -> Option<usize> {
    let (open, inner) = match line.strip_prefix('[') {
        Some(rest) => (1, rest),
        None => (0, line),
    };
    let n = iso_len(inner).or_else(|| bsd_time_len(inner)).or_else(|| {
        let n = inner.bytes().take_while(u8::is_ascii_digit).count();
        (matches!(n, 10 | 13) && inner.as_bytes().get(n).is_none_or(|c| matches!(c, b' ' | b'.' | b']'))).then_some(n)
    })?;
    if open == 1 {
        // To the closing bracket: `[2024-11-08 10:00:00 +0000]`.
        let close = inner.find(']')?;
        (close >= n && close < 40).then_some(close + 2)
    } else {
        Some(n)
    }
}

struct Stamped<'a> {
    time: &'a str,
    level: Option<&'a str>,
    message: &'a str,
}

/// The levels a log entry may name.
fn is_level(word: &str) -> bool {
    const LEVELS: [&str; 22] = [
        "TRACE", "DEBUG", "DBG", "INFO", "INF", "NOTICE", "WARN", "WARNING", "WRN", "ERROR", "ERR", "FATAL", "CRIT", "CRITICAL", "ALERT", "EMERG",
        "SEVERE", "FINE", "FINER", "FINEST", "CONFIG", "PANIC",
    ];
    LEVELS.iter().any(|l| l.eq_ignore_ascii_case(word))
}

/// A timestamp, maybe a level among the next few words, and the message.
fn stamped(line: &str) -> Option<Stamped<'_>> {
    let n = stamp_len(line)?;
    let time = line[..n].trim_matches(['[', ']']);
    let rest = &line[n..];
    // The level: one of the first three words, bare or in brackets; what follows it
    // is the message (`- app - INFO - msg`, `[INFO] msg`, `INFO msg`, `<warn> msg`).
    let mut at = 0;
    for _ in 0..3 {
        let skip = rest[at..].len() - rest[at..].trim_start_matches([' ', '\t', '-', '|', ':']).len();
        at += skip;
        let word_end = rest[at..].find([' ', '\t', ':', '|']).map_or(rest.len(), |e| at + e);
        let word = &rest[at..word_end];
        let bare = word.trim_matches(['[', ']', '(', ')', '<', '>']);
        if is_level(bare) {
            let msg = rest[word_end..].trim_start_matches([' ', '\t', '-', '|', ':', ']']);
            return Some(Stamped { time, level: Some(bare), message: msg });
        }
        if word_end == at {
            break;
        }
        at = word_end;
    }
    Some(Stamped { time, level: None, message: rest.trim_start_matches([' ', '\t', '-', '|', ':']) })
}

struct Syslog<'a> {
    time: Option<&'a str>,
    host: Option<&'a str>,
    app: Option<&'a str>,
    pid: Option<&'a str>,
    msgid: Option<&'a str>,
    level: Option<&'a str>,
    data: Option<&'a str>,
    message: &'a str,
}

/// RFC 5424.
fn syslog(line: &str) -> Option<Syslog<'_>> {
    let (level, rest) = priority(line)?;
    let rest = rest.strip_prefix("1 ")?;
    let mut fields = rest.splitn(6, ' ');
    let mut next = || fields.next().filter(|f| *f != "-");
    let (time, host, app, pid, msgid) = (next(), next(), next(), next(), next());
    let rest = fields.next().unwrap_or("");
    // Structured data: `-`, or `[id k="v"]…`, with `\]` escaped inside values.
    let (data, message) = if let Some(r) = rest.strip_prefix("- ") {
        (None, r)
    } else if rest == "-" {
        (None, "")
    } else if rest.starts_with('[') {
        let b = rest.as_bytes();
        let (mut i, mut depth, mut quoted) = (0, 0, false);
        while i < b.len() {
            match b[i] {
                b'\\' => i += 1,
                b'"' => quoted = !quoted,
                b'[' if !quoted => depth += 1,
                b']' if !quoted => {
                    depth -= 1;
                    if depth == 0 && b.get(i + 1) != Some(&b'[') {
                        i += 1;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        (Some(&rest[..i.min(rest.len())]), rest.get(i..).unwrap_or("").trim_start())
    } else {
        return None;
    };
    Some(Syslog { time, host, app, pid, msgid, level: Some(level), data, message: message.trim_start_matches('\u{feff}') })
}

/// BSD syslog, with or without `<PRI>`: `Nov  8 10:00:00 host app[123]: message`.
fn bsd_syslog(line: &str) -> Option<Syslog<'_>> {
    let (level, rest) = match priority(line) {
        Some((l, r)) => (Some(l), r),
        None => (None, line),
    };
    let (n, iso) = match bsd_time_len(rest) {
        Some(n) => (n, false),
        None => (iso_len(rest)?, true),
    };
    let time = &rest[..n];
    let rest = rest[n..].strip_prefix(' ')?;
    let (host, rest) = rest.split_once(' ')?;
    if is_level(host.trim_matches(['[', ']'])) {
        return None;
    }
    // The tag: `app[123]:`, `app:`, or `app[123]` then the message (macOS).
    let tag_end = rest.find([':', '[', ' ']).unwrap_or(rest.len());
    let app = &rest[..tag_end];
    if app.is_empty() || app.len() > 64 {
        return None;
    }
    let mut rest = &rest[tag_end..];
    let mut pid = None;
    if let Some(r) = rest.strip_prefix('[') {
        let close = r.find(']')?;
        pid = Some(&r[..close]);
        rest = &r[close + 1..];
    }
    // With an ISO time (rsyslog's precise format) the tag must say it is one, or a
    // timestamped log's first words would pass for a host and a tag.
    if iso && pid.is_none() && !rest.starts_with(':') {
        return None;
    }
    let message = rest.strip_prefix(':').unwrap_or(rest).trim_start();
    Some(Syslog { time: Some(time), host: Some(host), app: Some(app), pid, msgid: None, level, data: None, message })
}

struct Access<'a> {
    client: &'a str,
    user: &'a str,
    time: &'a str,
    method: Option<&'a str>,
    path: Option<&'a str>,
    protocol: Option<&'a str>,
    status: &'a str,
    size: &'a str,
    referrer: Option<&'a str>,
    agent: Option<&'a str>,
}

/// A double-quoted field with `\"` escapes at the start of `s`: its inside and the rest.
fn quoted(s: &str) -> Option<(&str, &str)> {
    let s = s.strip_prefix('"')?;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some((&s[..i], &s[i + 1..])),
            _ => i += 1,
        }
    }
    None
}

/// Common and Combined Log Format.
fn access(line: &str) -> Option<Access<'_>> {
    let mut parts = line.splitn(4, ' ');
    let (client, _ident, user, rest) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let rest = rest.strip_prefix('[')?;
    let close = rest.find(']')?;
    let time = &rest[..close];
    let (request, rest) = quoted(rest[close + 1..].trim_start())?;
    let mut req = request.splitn(3, ' ');
    let (method, path, protocol) = (req.next().filter(|m| !m.is_empty()), req.next(), req.next());
    let mut rest = rest.trim_start().splitn(3, ' ');
    let status = rest.next().filter(|s| s.len() == 3 && s.bytes().all(|c| c.is_ascii_digit()))?;
    let size = rest.next()?;
    let (referrer, agent) = match rest.next().and_then(|r| quoted(r.trim_start())) {
        Some((referrer, r)) => (Some(referrer), quoted(r.trim_start()).map(|(a, _)| a)),
        None => (None, None),
    };
    Some(Access { client, user, time, method, path, protocol, status, size, referrer, agent })
}

/// Logfmt's `key=value` pairs; `None` unless the whole line is pairs.
fn logfmt(line: &str) -> Option<Vec<(String, String)>> {
    let mut pairs = Vec::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        let key_end = rest.find(['=', ' ']).unwrap_or(rest.len());
        let key = &rest[..key_end];
        if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-' | b'@' | b'/')) {
            return None;
        }
        rest = &rest[key_end..];
        let value = if let Some(r) = rest.strip_prefix('=') {
            if r.starts_with('"') {
                let (v, r) = quoted(r)?;
                rest = r;
                v.replace("\\\"", "\"").replace("\\\\", "\\").replace("\\n", "\n")
            } else {
                let end = r.find(' ').unwrap_or(r.len());
                rest = &r[end..];
                r[..end].to_string()
            }
        } else {
            // A bare key is a flag.
            "true".to_string()
        };
        pairs.push((key.to_string(), value));
        rest = rest.trim_start();
    }
    let with_values = pairs.iter().filter(|(_, v)| v != "true").count();
    (with_values >= 1 && !pairs.is_empty()).then_some(pairs)
}

/// LTSV's `label:value` fields, tab-separated.
fn ltsv(line: &str) -> Option<Vec<(String, String)>> {
    let pairs: Option<Vec<_>> = line
        .split('\t')
        .map(|f| {
            let (k, v) = f.split_once(':')?;
            (!k.is_empty() && k.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))).then(|| (k.to_string(), v.to_string()))
        })
        .collect();
    pairs.filter(|p| p.len() >= 2)
}

struct Cef<'a> {
    time: Option<&'a str>,
    host: Option<&'a str>,
    /// Vendor, product, version, signature, name, severity.
    header: Vec<String>,
    extension: Vec<(String, String)>,
}

/// CEF: `CEF:0|vendor|product|version|signature|name|severity|key=value …`, maybe
/// after a syslog header that gives the time and host.
fn cef(line: &str) -> Option<Cef<'_>> {
    let at = line.find("CEF:")?;
    let prefix = line[..at].trim();
    let (time, host) = if prefix.is_empty() {
        (None, None)
    } else {
        let p = priority(prefix).map_or(prefix, |(_, r)| r);
        let n = bsd_time_len(p).or_else(|| iso_len(p))?;
        (Some(&p[..n]), p[n..].split_whitespace().next())
    };
    let body = &line[at + 4..];
    // Header fields split on `|` not escaped.
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut chars = body.char_indices();
    let mut ext_start = None;
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                if let Some((_, n)) = chars.next() {
                    cur.push(n);
                }
            }
            '|' => {
                fields.push(std::mem::take(&mut cur));
                if fields.len() == 7 {
                    ext_start = Some(i + 1);
                    break;
                }
            }
            c => cur.push(c),
        }
    }
    let ext = &body[ext_start?..];
    let header = fields[1..].to_vec();
    Some(Cef { time, host, header, extension: cef_extension(ext) })
}

/// `key=value` pairs where a value runs to the next ` key=`; `\=`, `\\`, `\n` escaped.
fn cef_extension(ext: &str) -> Vec<(String, String)> {
    let b = ext.as_bytes();
    // Where each key starts: after a space (or at the start), word characters, then `=`.
    let mut keys = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if i == 0 || b[i - 1] == b' ' {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'.') {
                j += 1;
            }
            if j > i && b.get(j) == Some(&b'=') && (j == 0 || b[j - 1] != b'\\') {
                keys.push((i, j));
                i = j + 1;
                continue;
            }
        }
        if b[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    keys.iter()
        .enumerate()
        .map(|(n, &(k, eq))| {
            let end = keys.get(n + 1).map_or(ext.len(), |&(next, _)| next);
            let value = ext[eq + 1..end].trim_end().replace("\\=", "=").replace("\\n", "\n").replace("\\\\", "\\");
            (ext[k..eq].to_string(), value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        for (s, n) in [
            ("2024-11-08T10:00:00Z x", 20),
            ("2024-11-08 10:00:00,123 - x", 23),
            ("2024-11-08T10:00:00.5+01:00 x", 27),
            ("2024/11/08 10:00:00 x", 19),
            ("2024-11-08 10:00:00 +0100 x", 25),
            ("[08/Nov/2024:10:00:00 +0000] x", 0),
            ("[2024-11-08 10:00:00] x", 21),
            ("1731060000 x", 10),
            ("Nov  8 10:00:00 host", 15),
        ] {
            assert_eq!(stamp_len(s).unwrap_or(0), n, "{s}");
        }
        assert!(stamp_len("Hello 2024-11-08").is_none());
        assert!(stamp_len("12345 apples").is_none());
    }

    #[test]
    fn stamped_lines_find_their_level() {
        let s = stamped("2024-11-08 10:00:00,123 - app.db - WARNING - slow query").unwrap();
        assert_eq!((s.time, s.level, s.message), ("2024-11-08 10:00:00,123", Some("WARNING"), "slow query"));
        let s = stamped("[2024-11-08T10:00:00Z] [error] boom").unwrap();
        assert_eq!((s.time, s.level, s.message), ("2024-11-08T10:00:00Z", Some("error"), "boom"));
        let s = stamped("2024-11-08T10:00:00Z just text").unwrap();
        assert_eq!((s.level, s.message), (None, "just text"));
    }

    #[test]
    fn syslog_both_kinds() {
        let s = syslog(r#"<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut="3" eventSource="App\]"] An application event"#).unwrap();
        assert_eq!((s.level, s.host, s.app, s.pid, s.msgid), (Some("notice"), Some("mymachine.example.com"), Some("evntslog"), None, Some("ID47")));
        assert_eq!(s.message, "An application event");
        assert!(s.data.unwrap().ends_with("App\\]\"]"));
        let b = bsd_syslog("<34>Oct 11 22:14:15 mymachine su: 'su root' failed for lonvick on /dev/pts/8").unwrap();
        assert_eq!((b.level, b.host, b.app, b.message), (Some("crit"), Some("mymachine"), Some("su"), "'su root' failed for lonvick on /dev/pts/8"));
        let mac = bsd_syslog("Nov  8 10:00:00 MacBook kernel[0] <Notice>: hello").unwrap();
        assert_eq!((mac.app, mac.pid, mac.message), (Some("kernel"), Some("0"), "<Notice>: hello"));
    }

    #[test]
    fn access_logs() {
        let a = access(r#"127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /apache_pb.gif HTTP/1.0" 200 2326 "http://www.example.com/start.html" "Mozilla/4.08 [en] (Win98; I ;Nav)""#).unwrap();
        assert_eq!((a.client, a.user, a.method, a.path, a.status, a.size), ("127.0.0.1", "frank", Some("GET"), Some("/apache_pb.gif"), "200", "2326"));
        assert_eq!(a.agent, Some("Mozilla/4.08 [en] (Win98; I ;Nav)"));
        assert!(access(r#"1.2.3.4 - - [10/Oct/2000:13:55:36 -0700] "-" 400 -"#).is_some());
    }

    #[test]
    fn keyed_formats() {
        let p = logfmt(r#"time=2024-11-08T10:00:00Z level=info msg="hello \"you\"" took=12ms ok"#).unwrap();
        assert_eq!(p[2], ("msg".into(), "hello \"you\"".into()));
        assert_eq!(p[4], ("ok".into(), "true".into()));
        assert!(logfmt("just some words here").is_none());
        assert_eq!(ltsv("host:127.0.0.1\tstatus:200").unwrap()[1], ("status".into(), "200".into()));
        let c = cef(r"Sep 19 08:26:10 host CEF:0|Security|threatmanager|1.0|100|worm successfully stopped|10|src=10.0.0.1 dst=2.1.2.2 msg=a \= b spt=1232").unwrap();
        assert_eq!((c.time, c.host), (Some("Sep 19 08:26:10"), Some("host")));
        assert_eq!(c.header, ["Security", "threatmanager", "1.0", "100", "worm successfully stopped", "10"]);
        assert_eq!(c.extension[2], ("msg".into(), "a = b".into()));
        assert_eq!(c.extension.len(), 4);
    }

    #[test]
    fn formats_are_told_apart() {
        let lines = |s: &'static str| s.lines().collect::<Vec<_>>();
        assert_eq!(detect(&lines("2024-11-08 10:00:00 INFO a\n  at x\n  at y\n2024-11-08 10:00:01 ERROR b"), None), Some(Format::Stamped));
        assert_eq!(detect(&lines("level=info msg=a\nlevel=warn msg=b"), None), Some(Format::Logfmt));
        assert_eq!(detect(&lines("Nov  8 10:00:00 h a: x\nNov  8 10:00:01 h a: y"), None), Some(Format::BsdSyslog));
        assert_eq!(detect(&lines("just\nsome\nprose"), None), None);
        let mixed = lines("<34>Nov  8 10:00:00 h su: x\n<165>1 2024-11-08T10:00:07Z db pg 1 ID - y");
        assert_eq!(detect(&mixed, None), Some(Format::Syslog));
        assert_eq!(detect(&lines("time=2024-11-08 level=info"), Some(Format::Ltsv)), Some(Format::Logfmt));
    }
}
