//! Finding text in any content model, through the model's own range reads: a 2 GB log
//! or a million-row table is searched a chunk at a time, never held whole
//! (docs/design/libviewer.md, "Find in file").
//!
//! Matching ignores case. Each match is a [`Spot`], in the terms its model's view uses
//! to show it: a line and the bytes in it, a cell, a node and the way to it, a block, a
//! page and the rectangles on it. Matches are handed to `found` in document order as
//! they're found; it returns `false` to stop (an app caps how many it keeps).
//!
//! ```ignore
//! let query = Query::new("needle").unwrap();
//! let mut spots = Vec::new();
//! find::content(&content, &query, &cancel, &mut |s| { spots.push(s); spots.len() < 10_000 })?;
//! ```
//!
//! Searching may take a while (a big PDF's text, a deep tree), so an app runs it off its
//! UI thread; `cancel` stops it between reads.

use std::ops::Range;

use crate::{Block, Cancel, Cell, Container, Content, FileSource, FlowSource, Inline, NodeId, PageRect, PageSource, Result, TableSource, TextSource, TreeSource};

/// What to look for: text, matched without regard to case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    text: String,
    /// The text lowercased, char by char.
    folded: Vec<char>,
    /// The bytes it is as hex digits ("4d 5a", "cafe"), for a file's raw bytes.
    hex: Option<Vec<u8>>,
}

impl Query {
    /// `None` for empty text, which matches nothing.
    pub fn new(text: &str) -> Option<Self> {
        if text.is_empty() {
            return None;
        }
        let folded = text.chars().flat_map(char::to_lowercase).collect();
        Some(Self { text: text.to_owned(), folded, hex: hex_bytes(text) })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where the query appears in `hay`, as byte ranges, without overlapping.
    pub fn find_in(&self, hay: &str) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        if self.text.is_ascii() {
            // ASCII matches only ASCII bytes, so ranges fall on char boundaries.
            let needle = self.text.as_bytes();
            let mut at = 0;
            let bytes = hay.as_bytes();
            while at + needle.len() <= bytes.len() {
                match find_ascii(&bytes[at..], needle) {
                    Some(i) => {
                        out.push(at + i..at + i + needle.len());
                        at += i + needle.len();
                    }
                    None => break,
                }
            }
            return out;
        }
        // Each lowercased char, and the bytes of the char it came from.
        let lowered: Vec<(char, usize, usize)> = hay.char_indices().flat_map(|(i, c)| c.to_lowercase().map(move |l| (l, i, i + c.len_utf8()))).collect();
        let n = self.folded.len();
        let mut i = 0;
        while i + n <= lowered.len() {
            if lowered[i..i + n].iter().zip(&self.folded).all(|((l, ..), f)| l == f) {
                out.push(lowered[i].1..lowered[i + n - 1].2);
                i += n;
            } else {
                i += 1;
            }
        }
        out
    }

    /// Whether the query appears in `hay`.
    pub fn is_in(&self, hay: &str) -> bool {
        if self.text.is_ascii() { find_ascii(hay.as_bytes(), self.text.as_bytes()).is_some() } else { !self.find_in(hay).is_empty() }
    }
}

/// The first place `needle` (ASCII) is in `hay`, ignoring ASCII case.
fn find_ascii(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let (&first, rest) = needle.split_first()?;
    let (lo, up) = (first.to_ascii_lowercase(), first.to_ascii_uppercase());
    let mut at = 0;
    while at + needle.len() <= hay.len() {
        let i = memchr::memchr2(lo, up, &hay[at..=hay.len() - needle.len()])?;
        let start = at + i;
        if hay[start + 1..start + needle.len()].eq_ignore_ascii_case(rest) {
            return Some(start);
        }
        at = start + 1;
    }
    None
}

/// Hex digits, in pairs, spaces allowed between: the bytes they spell.
fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() < 2 || !digits.len().is_multiple_of(2) || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let value = |d: u8| (d as char).to_digit(16).unwrap() as u8;
    Some(digits.chunks(2).map(|p| value(p[0]) << 4 | value(p[1])).collect())
}

/// Where a match is, in the terms of its model's view.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Spot {
    /// Text: line `line`, and the bytes of it (as `String::from_utf8_lossy` gives the
    /// line, which is how views show it).
    Line { line: u64, range: Range<usize> },
    /// Raw bytes: `len` of them from `offset`.
    Bytes { offset: u64, len: u64 },
    /// A table's cell.
    Cell { row: u64, column: usize },
    /// A container's entry.
    Entry { index: u64 },
    /// A tree's node, and its ancestors from the top: every node to open to see it.
    Node { path: Vec<NodeId> },
    /// Formatted text: block `block` of the flow.
    Block { block: u64 },
    /// A page, and the rectangles the match covers on it (one per line it runs over),
    /// in points as the page is shown.
    Page { page: u64, rects: Vec<PageRect> },
}

impl Spot {
    /// The line, row, entry, block or page it's in: what a view scrolls to.
    pub fn index(&self) -> u64 {
        match self {
            Spot::Line { line, .. } => *line,
            Spot::Bytes { offset, .. } => *offset,
            Spot::Cell { row, .. } => *row,
            Spot::Entry { index } => *index,
            Spot::Node { path } => path.last().map_or(0, |n| n.0),
            Spot::Block { block } => *block,
            Spot::Page { page, .. } => *page,
        }
    }
}

/// What `found` returns: `true` for more.
pub type Found<'a> = &'a mut dyn FnMut(Spot) -> bool;

/// Finds `query` in any content. Images and 3D scenes have no text; they find nothing.
pub fn content(content: &Content, query: &Query, cancel: &Cancel, found: Found) -> Result<()> {
    match content {
        Content::Text(t) => text(t.as_ref(), query, cancel, found),
        Content::Hex(src) => bytes(src, query, cancel, found),
        Content::Tree(t) => tree(t.as_ref(), query, cancel, found),
        Content::Table(t) => table(t.as_ref(), query, cancel, found),
        Content::Container(c) => container(c.as_ref(), query, cancel, found),
        Content::Flow(f) => flow(f.as_ref(), query, cancel, found),
        Content::Pages(p) => pages(p.as_ref(), query, cancel, found),
        _ => Ok(()),
    }
}

/// Whether a content model has text to find.
pub fn searchable(content: &Content) -> bool {
    matches!(content, Content::Text(_) | Content::Hex(_) | Content::Tree(_) | Content::Table(_) | Content::Container(_) | Content::Flow(_) | Content::Pages(_))
}

/// Lines, rows, entries or blocks read at once.
const BATCH: u64 = 1024;
/// Longer lines are searched only this far (as views show them).
const MAX_LINE_BYTES: usize = 8 * 1024;

/// Text, a batch of lines at a time.
pub fn text(src: &dyn TextSource, query: &Query, cancel: &Cancel, found: Found) -> Result<()> {
    let mut at = 0;
    loop {
        cancel.check()?;
        // The count may grow as a live text is read; ask each batch.
        let end = src.info().lines.known();
        if at >= end {
            return Ok(());
        }
        let lines = src.lines(at..(at + BATCH).min(end))?;
        if lines.is_empty() {
            return Ok(());
        }
        for (k, raw) in lines.iter().enumerate() {
            let line = String::from_utf8_lossy(&raw[..raw.len().min(MAX_LINE_BYTES)]);
            for range in query.find_in(&line) {
                if !found(Spot::Line { line: at + k as u64, range }) {
                    return Ok(());
                }
            }
        }
        at += lines.len() as u64;
    }
}

/// A file's bytes, a megabyte at a time: the query as text (ASCII case ignored), and
/// as the bytes its hex digits spell when they spell some.
pub fn bytes(src: &FileSource, query: &Query, cancel: &Cancel, found: Found) -> Result<()> {
    const CHUNK: usize = 1 << 20;
    let text = query.text.as_bytes();
    let longest = text.len().max(query.hex.as_ref().map_or(0, Vec::len));
    let len = src.len();
    let mut buf = vec![0u8; CHUNK + longest];
    let mut at = 0u64;
    // Matches end past where the last one started, so none is reported twice.
    let mut next_text = 0u64;
    let mut next_hex = 0u64;
    while at < len {
        cancel.check()?;
        let n = src.read_at(at, &mut buf);
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        let mut spots = Vec::new();
        let starts = |needle: &[u8], insensitive: bool| {
            let mut out = Vec::new();
            let mut i = 0;
            while i + needle.len() <= chunk.len() {
                let hit = if insensitive && needle.is_ascii() { find_ascii(&chunk[i..], needle) } else { memchr::memmem::find(&chunk[i..], needle) };
                let Some(k) = hit else { break };
                out.push(i + k);
                i += k + needle.len();
            }
            out
        };
        // Only matches starting in this chunk's own megabyte; the overlap is the next's.
        let own = |s: usize| s < CHUNK || at + n as u64 >= len;
        for s in starts(text, true).into_iter().filter(|&s| own(s)) {
            let offset = at + s as u64;
            if offset >= next_text {
                spots.push((offset, text.len() as u64));
                next_text = offset + text.len() as u64;
            }
        }
        if let Some(hex) = &query.hex {
            for s in starts(hex, false).into_iter().filter(|&s| own(s)) {
                let offset = at + s as u64;
                if offset >= next_hex && !spots.contains(&(offset, hex.len() as u64)) {
                    spots.push((offset, hex.len() as u64));
                    next_hex = offset + hex.len() as u64;
                }
            }
        }
        spots.sort_unstable();
        for (offset, len) in spots {
            if !found(Spot::Bytes { offset, len }) {
                return Ok(());
            }
        }
        if n <= CHUNK {
            break;
        }
        at += CHUNK as u64;
    }
    Ok(())
}

/// A table's cells, as views show them, a batch of rows at a time.
pub fn table(src: &dyn TableSource, query: &Query, cancel: &Cancel, found: Found) -> Result<()> {
    let mut at = 0;
    loop {
        cancel.check()?;
        let end = src.info().rows.known();
        if at >= end {
            return Ok(());
        }
        let rows = src.rows(at..(at + BATCH).min(end))?;
        if rows.is_empty() {
            return Ok(());
        }
        for (k, row) in rows.iter().enumerate() {
            for (column, cell) in row.iter().enumerate() {
                let shown = match cell {
                    Cell::Null => continue,
                    c => c.to_string(),
                };
                if query.is_in(&shown) && !found(Spot::Cell { row: at + k as u64, column }) {
                    return Ok(());
                }
            }
        }
        at += rows.len() as u64;
    }
}

/// A container's entries, by path.
pub fn container(src: &dyn Container, query: &Query, cancel: &Cancel, found: Found) -> Result<()> {
    let mut at = 0;
    loop {
        cancel.check()?;
        let end = src.entry_count().known();
        if at >= end {
            return Ok(());
        }
        let entries = src.entries(at..(at + BATCH).min(end))?;
        if entries.is_empty() {
            return Ok(());
        }
        for (k, e) in entries.iter().enumerate() {
            if query.is_in(&e.path) && !found(Spot::Entry { index: at + k as u64 }) {
                return Ok(());
            }
        }
        at += entries.len() as u64;
    }
}

/// A tree's labels and values, depth first, as it's shown top to bottom.
pub fn tree(src: &dyn TreeSource, query: &Query, cancel: &Cancel, found: Found) -> Result<()> {
    let mut path: Vec<NodeId> = Vec::new();
    walk(src, None, query, cancel, &mut path, found).map(|_| ())
}

/// The children of `parent`, and theirs; `Ok(false)` once `found` has had enough.
fn walk(src: &dyn TreeSource, parent: Option<NodeId>, query: &Query, cancel: &Cancel, path: &mut Vec<NodeId>, found: Found) -> Result<bool> {
    let count = src.child_count(parent)?.known();
    let mut at = 0;
    while at < count {
        cancel.check()?;
        let nodes = src.children(parent, at..(at + BATCH).min(count))?;
        if nodes.is_empty() {
            break;
        }
        at += nodes.len() as u64;
        for n in nodes {
            path.push(n.id);
            if (query.is_in(&n.label) || query.is_in(&n.value)) && !found(Spot::Node { path: path.clone() }) {
                return Ok(false);
            }
            if n.has_children && !walk(src, Some(n.id), query, cancel, path, found)? {
                return Ok(false);
            }
            path.pop();
        }
    }
    Ok(true)
}

/// Formatted text, block by block: a block that holds the query once or more is one
/// match.
pub fn flow(src: &dyn FlowSource, query: &Query, cancel: &Cancel, found: Found) -> Result<()> {
    let mut at = 0;
    loop {
        cancel.check()?;
        let end = src.block_count().known();
        if at >= end {
            return Ok(());
        }
        let blocks = src.blocks(at..(at + BATCH).min(end))?;
        if blocks.is_empty() {
            return Ok(());
        }
        for (k, b) in blocks.iter().enumerate() {
            let mut text = String::new();
            plain(b, &mut text);
            if query.is_in(&text) && !found(Spot::Block { block: at + k as u64 }) {
                return Ok(());
            }
        }
        at += blocks.len() as u64;
    }
}

/// A block's text, marks dropped, paragraphs on lines of their own.
fn plain(b: &Block, out: &mut String) {
    let inl = |i: &[Inline], out: &mut String| {
        out.push_str(&Inline::plain(i));
        out.push('\n');
    };
    match b {
        Block::Heading { text, .. } | Block::Paragraph(text) => inl(text, out),
        Block::List { items, .. } => items.iter().flatten().for_each(|b| plain(b, out)),
        Block::Quote(c) => c.iter().for_each(|b| plain(b, out)),
        Block::Code { text, .. } | Block::Raw { text, .. } => {
            out.push_str(text);
            out.push('\n');
        }
        Block::Table { rows, .. } => rows.iter().flatten().for_each(|c| inl(c, out)),
        Block::Image { alt, .. } => {
            out.push_str(alt);
            out.push('\n');
        }
        _ => {}
    }
}

/// Pages, one at a time, through the engine's own text search
/// ([`PageSource::find_text`]).
pub fn pages(src: &dyn PageSource, query: &Query, cancel: &Cancel, found: Found) -> Result<()> {
    for page in 0..src.page_count() {
        cancel.check()?;
        // A page that can't be read is skipped, as a view shows the rest.
        let Ok(hits) = src.find_text(page, &query.text) else { continue };
        for rects in hits {
            if !found(Spot::Page { page, rects }) {
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{Column, Count, Entry, LineText, OwnedFlow, OwnedTable, OwnedTree};

    fn all(c: &Content, q: &str) -> Vec<Spot> {
        let mut out = Vec::new();
        content(c, &Query::new(q).unwrap(), &Cancel::new(), &mut |s| {
            out.push(s);
            true
        })
        .unwrap();
        out
    }

    #[test]
    fn case_is_ignored_and_matches_dont_overlap() {
        let q = Query::new("aA").unwrap();
        assert_eq!(q.find_in("aaaa xAa"), vec![0..2, 2..4, 6..8]);
        // Beyond ASCII: the ranges are the original's bytes.
        let q = Query::new("ÉTÉ").unwrap();
        assert_eq!(q.find_in("un été, un ÉTÉ"), vec![3..8, 13..18]);
        assert!(Query::new("").is_none());
    }

    #[test]
    fn text_by_line() {
        let src = FileSource::from_bytes("a.txt", "Hello\nno\nsay hello, HELLO\n".as_bytes().to_vec());
        let t = Content::Text(Arc::new(LineText::new(src, None)));
        assert_eq!(all(&t, "hello"), vec![Spot::Line { line: 0, range: 0..5 }, Spot::Line { line: 2, range: 4..9 }, Spot::Line { line: 2, range: 11..16 }]);
    }

    #[test]
    fn bytes_as_text_and_as_hex() {
        let mut data = vec![0u8; 3 << 20];
        data[10..14].copy_from_slice(b"MZab");
        // Across the first chunk's end.
        let edge = (1 << 20) - 2;
        data[edge..edge + 4].copy_from_slice(b"mzMZ");
        let hex = Content::Hex(FileSource::from_bytes("a.bin", data));
        let spots = all(&hex, "mz");
        assert_eq!(spots, vec![Spot::Bytes { offset: 10, len: 2 }, Spot::Bytes { offset: edge as u64, len: 2 }, Spot::Bytes { offset: edge as u64 + 2, len: 2 }]);
        // "4d 5a" is the bytes "MZ", not the text.
        assert_eq!(all(&hex, "4d 5a"), vec![Spot::Bytes { offset: 10, len: 2 }, Spot::Bytes { offset: edge as u64 + 2, len: 2 }]);
    }

    #[test]
    fn cells_entries_nodes_blocks() {
        let t = OwnedTable::text(vec!["a".into(), "b".into()], vec![vec!["x".into(), "Needle".into()], vec!["needles".into(), "y".into()]]);
        assert_eq!(all(&Content::Table(Arc::new(t)), "needle"), vec![Spot::Cell { row: 0, column: 1 }, Spot::Cell { row: 1, column: 0 }]);
        let _ = (Column::new("x"), Count::Exact(0));

        struct Two;
        impl Container for Two {
            fn entry_count(&self) -> Count {
                Count::Exact(2)
            }
            fn entries(&self, r: Range<u64>) -> Result<Vec<Entry>> {
                Ok([Entry::file("docs/readme.md", 1), Entry::file("src/main.rs", 1)][r.start as usize..r.end.min(2) as usize].to_vec())
            }
            fn open(&self, _: u64) -> Result<FileSource> {
                unreachable!()
            }
        }
        assert_eq!(all(&Content::Container(Arc::new(Two)), "MAIN"), vec![Spot::Entry { index: 1 }]);

        let mut tree = OwnedTree::new();
        let root = tree.add(None, "root", "");
        let mid = tree.add(Some(root), "materials", "{2}");
        let leaf = tree.add(Some(mid), "name", "Steel");
        tree.add(None, "steel", "");
        let spots = all(&Content::Tree(Arc::new(tree)), "steel");
        assert_eq!(spots, vec![Spot::Node { path: vec![root, mid, leaf] }, Spot::Node { path: vec![NodeId(3)] }]);

        let flow = OwnedFlow::new(vec![
            Block::Heading { level: 1, text: vec![Inline::Text("Intro".into())] },
            Block::Paragraph(vec![Inline::Strong(vec![Inline::Text("needle".into())])]),
            Block::List { ordered: false, start: 1, items: vec![vec![Block::Paragraph(vec![Inline::Text("a NEEDLE".into())])]] },
        ]);
        assert_eq!(all(&Content::Flow(Arc::new(flow)), "needle"), vec![Spot::Block { block: 1 }, Spot::Block { block: 2 }]);
    }

    #[test]
    fn found_can_stop_it_and_cancel_does() {
        let src = FileSource::from_bytes("a.txt", "a\n".repeat(5000).into_bytes());
        let t = Content::Text(Arc::new(LineText::new(src, None)));
        let mut n = 0;
        content(&t, &Query::new("a").unwrap(), &Cancel::new(), &mut |_| {
            n += 1;
            n < 3
        })
        .unwrap();
        assert_eq!(n, 3);
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(content(&t, &Query::new("a").unwrap(), &cancel, &mut |_| true).is_err());
    }
}
