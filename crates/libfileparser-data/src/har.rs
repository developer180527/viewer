//! HTTP Archives (HAR): a browser's recorded requests, as a table of requests over
//! the JSON tree, each row linked to its entry in the tree and its lines.

use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Category, Cell, Column, Content, Count, Document, FileSource, FormatInfo, MemoryUse, ParseContext, ParseError, Result, Role, TableInfo,
    TableSource, ValueKind,
};

use crate::json::{self, Dialect};
use crate::tree::{DataTree, TreeLinks, cell};

/// The requests of a HAR's `log.entries`.
struct Requests {
    tree: Arc<DataTree>,
    entries: Arc<[u32]>,
}

impl Requests {
    const COLUMNS: [(&str, ValueKind); 7] = [
        ("Started", ValueKind::Date),
        ("Method", ValueKind::Text),
        ("URL", ValueKind::Text),
        ("Status", ValueKind::Number),
        ("Type", ValueKind::Text),
        ("Size", ValueKind::Number),
        ("Time (ms)", ValueKind::Number),
    ];

    /// The node at `path` (labels) below `from`.
    fn at(&self, from: u32, path: &[&str]) -> Option<u32> {
        path.iter().try_fold(from, |id, label| self.tree.child_ids(id).iter().copied().find(|&c| self.tree.label(c) == *label))
    }

    fn value(&self, from: u32, path: &[&str]) -> Cell {
        match self.at(from, path) {
            Some(id) => cell(self.tree.kind(id), self.tree.value(id)),
            None => Cell::Null,
        }
    }
}

impl TableSource for Requests {
    fn info(&self) -> TableInfo {
        TableInfo::new(Self::COLUMNS.iter().map(|&(n, k)| Column::new(n).with_kind(k)).collect(), Count::Exact(self.entries.len() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<Cell>>> {
        let end = range.end.min(self.entries.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(self.entries[start..end]
            .iter()
            .map(|&e| {
                // The response body's size where known, else what went over the wire.
                let size = match self.value(e, &["response", "content", "size"]) {
                    Cell::Int(n) if n >= 0 => Cell::Int(n),
                    _ => self.value(e, &["response", "bodySize"]),
                };
                vec![
                    self.value(e, &["startedDateTime"]),
                    self.value(e, &["request", "method"]),
                    self.value(e, &["request", "url"]),
                    self.value(e, &["response", "status"]),
                    self.value(e, &["response", "content", "mimeType"]),
                    size,
                    self.value(e, &["time"]),
                ]
            })
            .collect())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.entries.len() as u64 * 4)
    }
}

pub(crate) fn document(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = json::parse(src.bytes(), Dialect::Json, cx)?;
    // The JSON's top level is its `log` object's members' parent: find `log.entries`.
    let log = tree.roots().iter().copied().find(|&r| tree.label(r) == "log").ok_or_else(|| ParseError::corrupt("HAR: no `log`"))?;
    let entries_node = tree.child_ids(log).iter().copied().find(|&c| tree.label(c) == "entries").ok_or_else(|| ParseError::corrupt("HAR: no `log.entries`"))?;
    let entries: Arc<[u32]> = tree.child_ids(entries_node).into();
    let creator = tree
        .child_ids(log)
        .iter()
        .copied()
        .find(|&c| tree.label(c) == "creator")
        .and_then(|c| tree.child_ids(c).iter().copied().find(|&n| tree.label(n) == "name"))
        .map(|n| tree.value(n).to_string());
    let nodes = tree.len();
    let tree = Arc::new(tree);
    let requests = Requests { tree: tree.clone(), entries: entries.clone() };
    let mut doc = Document::new(FormatInfo::new("har", "HTTP Archive", Category::Data))
        .with_facet(Role::Primary, "Requests", Content::Table(Arc::new(requests)))
        .with_facet(Role::Structure, "Tree", Content::Tree(tree.clone()))
        .with_meta("Requests", entries.len())
        .with_meta("Nodes", nodes);
    if let Some(creator) = creator {
        doc = doc.with_meta("Recorded by", creator);
    }
    Ok(doc.with_links(TreeLinks { tree, tree_facet: 1, table: Some((0, entries)) }).with_file_facets(src, Some("json")))
}
