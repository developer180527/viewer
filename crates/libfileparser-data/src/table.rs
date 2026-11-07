//! Tables: delimited text (CSV, TSV and the like), read by row range from an index of
//! where each row starts, and JSON Lines' objects as rows.
//!
//! **Columns** (contract, docs/design/libfileparser.md §5): a delimited file's are its
//! first row, in order, then empty names for cells past it; JSON Lines', the keys of
//! its objects in the order first seen.
//!
//! **Cells:** JSON Lines' are typed as the JSON was (a missing key is null). A
//! delimited file's columns are numbers when every value in its first rows is one
//! (empty cells there are null); else text, as written.

use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Category, Cell, Column, Content, Count, Document, FileSource, FormatInfo, Limit, Links,
    MemoryUse, ParseContext, ParseError, Place, Position, Result, Role, TableInfo, TableSource,
    ValueKind,
};

use crate::tree::DataTree;

/// Rows sampled for JSON Lines' columns.
const COLUMN_SAMPLE: usize = 1000;
/// At most this many columns from JSON Lines' keys.
const MAX_COLUMNS: usize = 256;

pub(crate) struct JsonLinesTable {
    tree: Arc<DataTree>,
    columns: Vec<Column>,
}

impl JsonLinesTable {
    pub fn new(tree: Arc<DataTree>) -> Self {
        let mut columns: Vec<Column> = Vec::new();
        for &row in tree.roots().iter().take(COLUMN_SAMPLE) {
            if tree.kind(row) != ValueKind::Map {
                continue;
            }
            for &cell in tree.child_ids(row) {
                let key = tree.label(cell);
                let kind = tree.kind(cell);
                match columns.iter().position(|c| c.name == key) {
                    Some(i) => columns[i].kind = join(columns[i].kind, kind),
                    None if columns.len() < MAX_COLUMNS => columns.push(Column::new(key).with_kind(kind)),
                    None => {}
                }
            }
        }
        Self { tree, columns }
    }

    pub fn has_columns(&self) -> bool {
        !self.columns.is_empty()
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }
}

/// The kind of a column holding `a` and `b`: nulls fit any kind.
fn join(a: ValueKind, b: ValueKind) -> ValueKind {
    match (a, b) {
        (a, b) if a == b => a,
        (ValueKind::Null, b) => b,
        (a, ValueKind::Null) => a,
        _ => ValueKind::Mixed,
    }
}

impl TableSource for JsonLinesTable {
    fn info(&self) -> TableInfo {
        TableInfo::new(self.columns.clone(), Count::Exact(self.tree.roots().len() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<Cell>>> {
        let t = &self.tree;
        let roots = t.roots();
        let end = range.end.min(roots.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(roots[start..end]
            .iter()
            .map(|&row| {
                // A line that wasn't JSON: marked in its first cell.
                if t.kind(row) == ValueKind::Error {
                    let mut cells = vec![Cell::Null; self.columns.len()];
                    if let Some(first) = cells.first_mut() {
                        *first = Cell::Error(t.value(row).to_string());
                    }
                    return cells;
                }
                let cells = t.child_ids(row);
                self.columns
                    .iter()
                    .map(|c| match cells.iter().find(|&&id| t.label(id) == c.name) {
                        Some(&id) => t.cell(id).unwrap_or_else(|| Cell::Text(t.value(id).to_string())),
                        None => Cell::Null,
                    })
                    .collect()
            })
            .collect())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.columns.iter().map(|c| c.name.len() as u64 + 32).sum())
    }
}

/// A delimited text file, read by row range.
pub(crate) struct Delimited {
    src: FileSource,
    delimiter: u8,
    columns: Vec<Column>,
    /// Where each row after the header starts, and one past the last.
    starts: Vec<u64>,
    /// The line each of those rows starts on (from 0).
    lines: Vec<u32>,
}

impl Delimited {
    /// Indexes `src`'s rows: one pass, quotes respected, nothing but offsets kept.
    pub fn index(src: &FileSource, delimiter: u8, cx: &ParseContext) -> Result<Self> {
        let bytes = src.bytes();
        let mut starts = Vec::new();
        let mut lines = Vec::new();
        let mut width = 0usize;
        let (mut i, mut line, mut row_width, mut quoted) = (0usize, 0u32, 1usize, false);
        let (mut row_start, mut row_line) = (0usize, 0u32);
        let mut first = true;
        let mut header_end = 0usize;
        while i < bytes.len() {
            let Some(p) = memchr::memchr3(b'"', b'\n', delimiter, &bytes[i..]) else {
                break;
            };
            i += p;
            match bytes[i] {
                b'"' => quoted = !quoted,
                b'\n' => {
                    line += 1;
                    if !quoted {
                        let blank = bytes[row_start..i].iter().all(|&c| c == b'\r');
                        if first {
                            header_end = i + 1;
                            first = false;
                        } else if !blank {
                            starts.push(row_start as u64);
                            lines.push(row_line);
                        }
                        width = width.max(row_width);
                        row_width = 1;
                        row_start = i + 1;
                        row_line = line;
                        if starts.len() % 65536 == 0 {
                            cx.cancel.check()?;
                            cx.limits.check(Limit::Rows, starts.len() as u64)?;
                        }
                    }
                }
                _ if !quoted => row_width += 1,
                _ => {}
            }
            i += 1;
        }
        if row_start < bytes.len() && !bytes[row_start..].iter().all(|&c| c == b'\r' || c == b'\n')
        {
            if first {
                header_end = bytes.len();
            } else {
                starts.push(row_start as u64);
                lines.push(row_line);
            }
            width = width.max(row_width);
        }
        cx.limits.check(Limit::Rows, starts.len() as u64)?;
        cx.limits.check(Limit::Memory, starts.len() as u64 * 12)?;
        let mut names = parse_rows(&bytes[..header_end], delimiter, 0)?
            .into_iter()
            .next()
            .unwrap_or_default();
        names.resize(width.max(names.len()), String::new());
        starts.push(bytes.len() as u64);
        let mut table = Self {
            src: src.clone(),
            delimiter,
            columns: names.into_iter().map(|n| Column::new(n).with_kind(ValueKind::Text)).collect(),
            starts,
            lines,
        };
        table.infer_kinds()?;
        Ok(table)
    }

    fn row_count(&self) -> usize {
        self.starts.len() - 1
    }

    /// Columns whose every value in the first rows is a number are numbers.
    fn infer_kinds(&mut self) -> Result<()> {
        let sample = self.text_rows(0..KIND_SAMPLE as u64)?;
        for (i, column) in self.columns.iter_mut().enumerate() {
            let mut values = sample.iter().filter_map(|r| r.get(i)).map(|v| v.trim()).filter(|v| !v.is_empty()).peekable();
            if values.peek().is_some() && values.all(is_number) {
                column.kind = ValueKind::Number;
            }
        }
        Ok(())
    }

    fn text_rows(&self, range: Range<u64>) -> Result<Vec<Vec<String>>> {
        let end = range.end.min(self.row_count() as u64) as usize;
        let start = (range.start as usize).min(end);
        if start == end {
            return Ok(Vec::new());
        }
        let bytes = &self.src.bytes()[self.starts[start] as usize..self.starts[end] as usize];
        let mut rows = parse_rows(bytes, self.delimiter, self.lines[start])?;
        rows.truncate(end - start);
        Ok(rows)
    }
}

/// Rows sampled to tell a column's kind.
const KIND_SAMPLE: usize = 200;

/// A number as a spreadsheet would read it: `42`, `-1.5`, `1e3`; not `0x1F` or `1,000`.
fn is_number(v: &str) -> bool {
    let digits = v.strip_prefix(['-', '+']).unwrap_or(v);
    digits.starts_with(|c: char| c.is_ascii_digit() || c == '.') && v.parse::<f64>().is_ok_and(f64::is_finite)
}

impl TableSource for Delimited {
    fn info(&self) -> TableInfo {
        TableInfo::new(self.columns.clone(), Count::Exact(self.row_count() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<Cell>>> {
        Ok(self
            .text_rows(range)?
            .into_iter()
            .map(|mut row| {
                row.resize(self.columns.len(), String::new());
                row.into_iter()
                    .zip(&self.columns)
                    .map(|(v, c)| match c.kind {
                        ValueKind::Number if v.trim().is_empty() => Cell::Null,
                        ValueKind::Number if is_number(v.trim()) => Cell::number(v.trim()),
                        _ => Cell::Text(v),
                    })
                    .collect()
            })
            .collect())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap((self.starts.capacity() * 8 + self.lines.capacity() * 4) as u64)
    }
}

/// The rows of `bytes`, which start on line `first_line` (from 0) of the file.
fn parse_rows(bytes: &[u8], delimiter: u8, first_line: u32) -> Result<Vec<Vec<String>>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);
    let mut out = Vec::new();
    let mut record = csv::ByteRecord::new();
    while reader
        .read_byte_record(&mut record)
        .map_err(|e| {
            let line = e.position().map_or(1, |p| p.line()) + u64::from(first_line);
            ParseError::corrupt_at(format!("CSV: {e}"), Position::line(line))
        })?
    {
        out.push(
            record
                .iter()
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect(),
        );
    }
    Ok(out)
}

/// The delimiter of a file whose extension doesn't say: the candidate found the same
/// number of times (and at least once) on most of its first lines, else `None`.
pub(crate) fn sniff_delimiter(header: &[u8], candidates: &[u8]) -> Option<u8> {
    let lines: Vec<&[u8]> = header
        .split(|&c| c == b'\n')
        .filter(|l| !l.is_empty())
        .take(20)
        .collect();
    // The header may be cut mid-line.
    let lines = if lines.len() > 1 && header.len() >= 8192 {
        &lines[..lines.len() - 1]
    } else {
        &lines[..]
    };
    let mut best: Option<(u8, usize)> = None;
    for &d in candidates {
        let counts: Vec<usize> = lines.iter().map(|l| count_outside_quotes(l, d)).collect();
        let Some(&first) = counts.first() else {
            continue;
        };
        if first == 0 {
            continue;
        }
        let same = counts.iter().filter(|&&c| c == first).count();
        if same * 10 >= counts.len() * 8 && best.is_none_or(|(_, n)| first > n) {
            best = Some((d, first));
        }
    }
    best.map(|(d, _)| d)
}

fn count_outside_quotes(line: &[u8], d: u8) -> usize {
    let mut quoted = false;
    line.iter()
        .filter(|&&c| {
            if c == b'"' {
                quoted = !quoted;
            }
            !quoted && c == d
        })
        .count()
}

/// Links between a delimited table's rows and the file's lines.
struct RowLines(Arc<Delimited>);

impl Links for RowLines {
    fn links(&self, _: &Document, from: &Place, _: &ParseContext) -> Result<Vec<Place>> {
        let t = &self.0;
        Ok(match *from {
            Place::Row { facet: 0, row } => match t.lines.get(row as usize) {
                Some(&first) => {
                    let next = t.lines.get(row as usize + 1).copied();
                    let last = match next {
                        Some(n) => n.max(first + 1),
                        None => {
                            first
                                + 1
                                + memchr::memchr_iter(
                                    b'\n',
                                    &t.src.bytes()[t.starts[row as usize] as usize..],
                                )
                                .count()
                                .saturating_sub(1) as u32
                        }
                    };
                    vec![Place::Lines(first as u64..last as u64)]
                }
                None => Vec::new(),
            },
            Place::Lines(ref r) => {
                let line = r.start as u32;
                // The last row starting on or before the line.
                match t.lines.partition_point(|&l| l <= line).checked_sub(1) {
                    Some(row) => vec![Place::Row {
                        facet: 0,
                        row: row as u64,
                    }],
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        })
    }
}

/// A delimited file's document.
pub(crate) fn delimited_document(
    src: &FileSource,
    delimiter: u8,
    cx: &ParseContext,
) -> Result<Document> {
    let table = Arc::new(Delimited::index(src, delimiter, cx)?);
    let name = match delimiter {
        b',' => "CSV",
        b'\t' => "Tab-separated values",
        b'|' => "Pipe-separated values",
        b';' => "Semicolon-separated values",
        b' ' => "Space-separated values",
        _ => "Delimited text",
    };
    let shown = match delimiter {
        b'\t' => "tab".to_string(),
        b' ' => "space".to_string(),
        d => (d as char).to_string(),
    };
    Ok(
        Document::new(FormatInfo::new("delimited", name, Category::Data))
            .with_facet(Role::Primary, "Table", Content::Table(table.clone()))
            .with_meta("Rows", table.row_count())
            .with_meta("Columns", table.columns.len())
            .with_meta("Delimiter", shown)
            .with_links(RowLines(table))
            .with_file_facets(src, Some("csv")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_read_by_range_with_quotes_respected() {
        let cx = ParseContext::new();
        let src = FileSource::from_bytes(
            "a.csv",
            b"name,note\nann,\"two\nlines, quoted\"\nbob,x,extra\n\n".to_vec(),
        );
        let t = Delimited::index(&src, b',', &cx).unwrap();
        assert_eq!(t.info().names(), ["name", "note", ""]);
        assert_eq!(t.info().rows, Count::Exact(2));
        assert_eq!(t.rows(1..5).unwrap(), [["bob", "x", "extra"].map(Cell::from).to_vec()]);
        assert_eq!(
            t.rows(0..1).unwrap(),
            [["ann", "two\nlines, quoted", ""].map(Cell::from).to_vec()]
        );
        assert_eq!(t.lines, [1, 3]);
    }

    #[test]
    fn delimiters_are_sniffed_from_consistent_lines() {
        assert_eq!(sniff_delimiter(b"a;b;c\n1;2;3\n", b",;\t|"), Some(b';'));
        assert_eq!(sniff_delimiter(b"a,b\n\"x,y\",2\n", b",;"), Some(b','));
        assert_eq!(
            sniff_delimiter(b"just some prose\nwith words, and a comma\n", b",;\t|"),
            None
        );
    }
}
