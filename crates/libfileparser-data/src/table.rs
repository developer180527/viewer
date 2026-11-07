//! Tables: delimited text (CSV, TSV and the like), read by row range from an index of
//! where each row starts, and JSON Lines' objects as rows.
//!
//! **Columns** (contract, docs/design/libfileparser.md §5): a delimited file's are its
//! first row, in order, then empty names for cells past it; JSON Lines', the keys of
//! its objects in the order first seen.

use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Category, Content, Count, Document, FileSource, FormatInfo, Limit, Links, MemoryUse,
    ParseContext, ParseError, Place, Result, Role, TableInfo, TableSource,
};

use crate::tree::DataTree;

/// Rows sampled for JSON Lines' columns.
const COLUMN_SAMPLE: usize = 1000;
/// At most this many columns from JSON Lines' keys.
const MAX_COLUMNS: usize = 256;

pub(crate) struct JsonLinesTable {
    tree: Arc<DataTree>,
    columns: Vec<String>,
}

impl JsonLinesTable {
    pub fn new(tree: Arc<DataTree>) -> Self {
        let mut columns: Vec<String> = Vec::new();
        for &row in tree.roots().iter().take(COLUMN_SAMPLE) {
            if !tree.value(row).starts_with('{') {
                continue;
            }
            for &cell in tree.child_ids(row) {
                let key = tree.label(cell);
                if columns.len() < MAX_COLUMNS && !columns.iter().any(|c| c == key) {
                    columns.push(key.to_string());
                }
            }
        }
        Self { tree, columns }
    }

    pub fn has_columns(&self) -> bool {
        !self.columns.is_empty()
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }
}

impl TableSource for JsonLinesTable {
    fn info(&self) -> TableInfo {
        TableInfo::new(
            self.columns.clone(),
            Count::Exact(self.tree.roots().len() as u64),
        )
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<String>>> {
        let roots = self.tree.roots();
        let end = range.end.min(roots.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(roots[start..end]
            .iter()
            .map(|&row| {
                let cells = self.tree.child_ids(row);
                self.columns
                    .iter()
                    .map(|c| {
                        cells
                            .iter()
                            .find(|&&id| self.tree.label(id) == c)
                            .map_or_else(String::new, |&id| cell(self.tree.value(id)))
                    })
                    .collect()
            })
            .collect())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.columns.iter().map(|c| c.len() as u64 + 24).sum())
    }
}

/// A tree value as a cell: strings without their quotes.
fn cell(value: &str) -> String {
    match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\""),
        None => value.to_string(),
    }
}

/// A delimited text file, read by row range.
pub(crate) struct Delimited {
    src: FileSource,
    delimiter: u8,
    columns: Vec<String>,
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
                            cx.limits.check(Limit::Items, starts.len() as u64)?;
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
        cx.limits.check(Limit::Items, starts.len() as u64)?;
        cx.limits.check(Limit::Memory, starts.len() as u64 * 12)?;
        let mut columns = parse_rows(&bytes[..header_end], delimiter)?
            .into_iter()
            .next()
            .unwrap_or_default();
        columns.resize(width.max(columns.len()), String::new());
        starts.push(bytes.len() as u64);
        Ok(Self {
            src: src.clone(),
            delimiter,
            columns,
            starts,
            lines,
        })
    }

    fn row_count(&self) -> usize {
        self.starts.len() - 1
    }
}

impl TableSource for Delimited {
    fn info(&self) -> TableInfo {
        TableInfo::new(self.columns.clone(), Count::Exact(self.row_count() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<String>>> {
        let end = range.end.min(self.row_count() as u64) as usize;
        let start = (range.start as usize).min(end);
        if start == end {
            return Ok(Vec::new());
        }
        let bytes = &self.src.bytes()[self.starts[start] as usize..self.starts[end] as usize];
        let mut rows = parse_rows(bytes, self.delimiter)?;
        rows.truncate(end - start);
        for row in &mut rows {
            row.resize(self.columns.len(), String::new());
        }
        Ok(rows)
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap((self.starts.capacity() * 8 + self.lines.capacity() * 4) as u64)
    }
}

fn parse_rows(bytes: &[u8], delimiter: u8) -> Result<Vec<Vec<String>>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);
    let mut out = Vec::new();
    let mut record = csv::ByteRecord::new();
    while reader
        .read_byte_record(&mut record)
        .map_err(|e| ParseError::corrupt(format!("CSV: {e}")))?
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
        assert_eq!(t.info().columns, ["name", "note", ""]);
        assert_eq!(t.info().rows, Count::Exact(2));
        assert_eq!(t.rows(1..5).unwrap(), [vec!["bob", "x", "extra"]]);
        assert_eq!(
            t.rows(0..1).unwrap(),
            [vec!["ann", "two\nlines, quoted", ""]]
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
