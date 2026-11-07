//! Tables of records read from a file's lines: a record starts on a line a format
//! recognises (a `>` in FASTA, any data line in BED) and runs to the next. Read
//! through core's line index, so a file opened with `FileSource::open_live` is read
//! a range at a time; opening keeps 4 bytes a record, and records are parsed when
//! their rows are read.

use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Category, Cell, Column, Content, Count, Document, FileSource, FormatInfo, Limit, LineText, Links, MemoryUse, ParseContext, Place,
    Result, Role, TableInfo, TableSource, TextSource, ValueKind,
};

/// Lines read at a time while indexing.
const BATCH: u64 = 4096;

/// A record's cells from its number and its lines.
type Parse = Box<dyn Fn(usize, &[String]) -> Vec<Cell> + Send + Sync>;

pub(crate) struct LineTable {
    text: Arc<LineText>,
    /// The first line of each record.
    starts: Vec<u32>,
    /// Where the last record ends: the file's end, or a line that ends the data.
    end: u64,
    columns: Vec<Column>,
    parse: Parse,
    /// Read at most this many lines of a record to show it: a FASTA record may be a
    /// whole chromosome.
    max_lines: Option<u64>,
}

/// A file's line index and the lines records start on.
pub(crate) struct Index {
    pub text: Arc<LineText>,
    pub starts: Vec<u32>,
    pub end: u64,
}

/// Indexes `src`: `is_start` sees each line (its number from 0, its text) and says
/// whether a record starts there; `Stop` ends the data (a trailer).
pub(crate) fn index(src: &FileSource, language: &str, cx: &ParseContext, mut each: impl FnMut(u64, &str) -> Line) -> Result<Index> {
    let text = Arc::new(LineText::within(src.clone(), Some(language), &cx.limits)?);
    let lines = text.info().lines.known();
    let mut starts = Vec::new();
    let mut at = 0;
    let mut end = lines;
    'read: while at < lines {
        cx.cancel.check()?;
        for (i, line) in text.lines(at..(at + BATCH).min(lines))?.iter().enumerate() {
            let n = at + i as u64;
            match each(n, &String::from_utf8_lossy(line)) {
                Line::Start => starts.push(n as u32),
                Line::Within => {}
                Line::Stop => {
                    end = n;
                    break 'read;
                }
            }
        }
        cx.limits.check(Limit::Rows, starts.len() as u64)?;
        at += BATCH;
    }
    Ok(Index { text, starts, end })
}

/// What a line is to the records.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Line {
    /// A record starts here.
    Start,
    /// Part of the record above, or not a record (a header, a comment).
    Within,
    /// The data ends here.
    Stop,
}

impl LineTable {
    pub fn new(index: Index, columns: Vec<Column>, parse: impl Fn(usize, &[String]) -> Vec<Cell> + Send + Sync + 'static) -> Self {
        Self { text: index.text, starts: index.starts, end: index.end, columns, parse: Box::new(parse), max_lines: None }
    }

    /// Reads no more than `n` lines of a record to show it.
    pub fn max_lines(mut self, n: u64) -> Self {
        self.max_lines = Some(n);
        self
    }

    pub fn len(&self) -> usize {
        self.starts.len()
    }

    /// The same records with other columns: for formats whose columns come from
    /// their first records (read with [`Self::records`] first).
    pub fn with(self, columns: Vec<Column>, parse: impl Fn(usize, &[String]) -> Vec<Cell> + Send + Sync + 'static) -> Self {
        Self { columns, parse: Box::new(parse), ..self }
    }

    pub fn record_lines(&self, r: usize) -> Range<u64> {
        let start = self.starts[r] as u64;
        start..self.starts.get(r + 1).map_or(self.end, |&s| s as u64).max(start + 1)
    }

    /// The lines of records `range`, each as text.
    pub fn records(&self, range: Range<usize>) -> Result<Vec<Vec<String>>> {
        if range.is_empty() {
            return Ok(Vec::new());
        }
        range
            .map(|r| {
                let l = self.record_lines(r);
                let end = self.max_lines.map_or(l.end, |m| l.end.min(l.start + m));
                Ok(self.text.lines(l.start..end)?.iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect())
            })
            .collect()
    }
}

impl TableSource for LineTable {
    fn info(&self) -> TableInfo {
        TableInfo::new(self.columns.clone(), Count::Exact(self.starts.len() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<Cell>>> {
        let end = range.end.min(self.starts.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(self.records(start..end)?.iter().enumerate().map(|(i, r)| (self.parse)(start + i, r)).collect())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.starts.capacity() as u64 * 4) + self.text.memory()
    }
}

/// Rows of facet 0 and the lines they are on.
pub(crate) struct RecordLines(pub Arc<LineTable>);

impl Links for RecordLines {
    fn links(&self, _: &Document, from: &Place, _: &ParseContext) -> Result<Vec<Place>> {
        let t = &self.0;
        Ok(match *from {
            Place::Row { facet: 0, row } if (row as usize) < t.len() => vec![Place::Lines(t.record_lines(row as usize))],
            Place::Lines(ref r) => match t.starts.partition_point(|&s| s as u64 <= r.start).checked_sub(1) {
                Some(row) if t.record_lines(row).contains(&r.start) => vec![Place::Row { facet: 0, row: row as u64 }],
                _ => Vec::new(),
            },
            _ => Vec::new(),
        })
    }
}

/// Rows and the lines each is on, for tables built up front.
pub(crate) struct RowLines(pub Vec<Range<u64>>);

impl Links for RowLines {
    fn links(&self, _: &Document, from: &Place, _: &ParseContext) -> Result<Vec<Place>> {
        Ok(match *from {
            Place::Row { facet: 0, row } => self.0.get(row as usize).filter(|l| !l.is_empty()).map(|l| vec![Place::Lines(l.clone())]).unwrap_or_default(),
            Place::Lines(ref r) => {
                self.0.iter().position(|l| l.start <= r.start && r.start < l.end).map(|row| vec![Place::Row { facet: 0, row: row as u64 }]).unwrap_or_default()
            }
            _ => Vec::new(),
        })
    }
}

/// A document of one record table: `name` as its primary facet, then the file's.
pub(crate) fn document(id: &'static str, format: &str, name: &str, table: LineTable, src: &FileSource, language: &str) -> Document {
    let table = Arc::new(table);
    Document::new(FormatInfo::new(id, format, Category::Data))
        .with_facet(Role::Primary, name, Content::Table(table.clone()))
        .with_meta(name, table.len())
        .with_links(RecordLines(table))
        .with_file_facets(src, Some(language))
}

/// Columns of `kind`, named.
pub(crate) fn columns(cols: &[(&str, ValueKind)]) -> Vec<Column> {
    cols.iter().map(|&(n, k)| Column::new(n).with_kind(k)).collect()
}

/// A cell of text, null when empty.
pub(crate) fn text(s: &str) -> Cell {
    if s.is_empty() { Cell::Null } else { Cell::Text(s.to_string()) }
}

/// A cell of a number as written, null when empty or not a number.
pub(crate) fn num(s: &str) -> Cell {
    let s = s.trim();
    let d = s.strip_prefix(['-', '+']).unwrap_or(s);
    if d.starts_with(|c: char| c.is_ascii_digit() || c == '.') && s.parse::<f64>().is_ok_and(f64::is_finite) { Cell::number(s) } else { Cell::Null }
}


/// A comma-separated line's fields, `'…'` and `"…"` quoting commas.
pub(crate) fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quote) {
            ('\\', Some(_)) => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (q, Some(open)) if q == open => quote = None,
            ('\'' | '"', None) if cur.trim().is_empty() => {
                cur.clear();
                quote = Some(c);
            }
            (',', None) => out.push(std::mem::take(&mut cur).trim().to_string()),
            (c, _) => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}
