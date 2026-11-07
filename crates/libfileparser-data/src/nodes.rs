//! Tables whose rows are nodes of a file's tree (a feed's items, a schema's types),
//! their cells read from the nodes when the rows are.

use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Category, Cell, Column, Content, Count, Document, FileSource, FormatInfo, MemoryUse, Result, Role, TableInfo, TableSource, ValueKind,
};

use crate::tree::{DataTree, TOP, TreeLinks, cell};

/// Rows sampled for property columns.
pub(crate) const SAMPLE: usize = 1000;
/// At most this many property columns.
pub(crate) const MAX_PROPERTIES: usize = 64;

/// A cell worked out from a feature's node when its row is read.
type CellFn = Box<dyn Fn(&DataTree, u32) -> Cell + Send + Sync>;

/// A table whose rows are nodes of a tree, its cells read from them when asked for.
pub(crate) struct NodeTable {
    pub tree: Arc<DataTree>,
    pub rows: Arc<[u32]>,
    columns: Vec<Column>,
    cells: Vec<CellFn>,
}

impl NodeTable {
    pub fn new(tree: Arc<DataTree>, rows: Arc<[u32]>) -> Self {
        Self { tree, rows, columns: Vec::new(), cells: Vec::new() }
    }

    pub fn column(mut self, name: &str, kind: ValueKind, f: impl Fn(&DataTree, u32) -> Cell + Send + Sync + 'static) -> Self {
        self.columns.push(Column::new(name).with_kind(kind));
        self.cells.push(Box::new(f));
        self
    }

    /// A column per property key of the first features; `props` gives a feature's
    /// property nodes, each labelled with its key.
    pub fn properties(mut self, props: impl Fn(&DataTree, u32) -> Vec<u32> + Send + Sync + Clone + 'static) -> Self {
        let mut keys: Vec<(String, ValueKind)> = Vec::new();
        for &row in self.rows.iter().take(SAMPLE) {
            for p in props(&self.tree, row) {
                let (key, kind) = (self.tree.label(p).to_string(), self.tree.kind(p));
                match keys.iter().position(|(k, _)| *k == key) {
                    Some(i) => keys[i].1 = join(keys[i].1, kind),
                    None if keys.len() < MAX_PROPERTIES => keys.push((key, kind)),
                    None => {}
                }
            }
        }
        for (key, kind) in keys {
            let props = props.clone();
            let kind = if matches!(kind, ValueKind::Map | ValueKind::List | ValueKind::Group) { ValueKind::Text } else { kind };
            let name = key.clone();
            self = self.column(&name, kind, move |t, row| match props(t, row).into_iter().find(|&p| t.label(p) == key) {
                Some(p) => t.cell(p).unwrap_or_else(|| Cell::Text(t.value(p).to_string())),
                None => Cell::Null,
            });
        }
        self
    }
}

fn join(a: ValueKind, b: ValueKind) -> ValueKind {
    match (a, b) {
        (a, b) if a == b => a,
        (ValueKind::Null, b) => b,
        (a, ValueKind::Null) => a,
        _ => ValueKind::Mixed,
    }
}

impl TableSource for NodeTable {
    fn info(&self) -> TableInfo {
        TableInfo::new(self.columns.clone(), Count::Exact(self.rows.len() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<Cell>>> {
        let end = range.end.min(self.rows.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(self.rows[start..end].iter().map(|&row| self.cells.iter().map(|f| f(&self.tree, row)).collect()).collect())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.rows.len() as u64 * 4)
    }
}


/// A document of a node table over its tree: `name` (the table), Tree, then the
/// file's own facets; each row linked to its node (and its lines, when `lines`).
pub(crate) fn document(format: FormatInfo, name: &str, table: NodeTable, src: &FileSource, language: Option<&str>, lines: bool) -> Document {
    let (tree, rows) = (table.tree.clone(), table.rows.clone());
    let nodes = tree.len();
    let links = (!rows.contains(&TOP)).then_some((0, rows.clone()));
    Document::new(format)
        .with_meta(name, rows.len())
        .with_facet(Role::Primary, name, Content::Table(Arc::new(table)))
        .with_facet(Role::Structure, "Tree", Content::Tree(tree.clone()))
        .with_meta("Nodes", nodes)
        .with_links(TreeLinks { tree, tree_facet: 1, table: links, lines })
        .with_file_facets(src, language)
}

/// A leaf's value as a cell, null when absent or empty.
pub(crate) fn value(t: &DataTree, id: Option<u32>) -> Cell {
    match id {
        Some(id) if !t.value(id).is_empty() => cell(t.kind(id), t.value(id)),
        _ => Cell::Null,
    }
}

/// The part of an XML name after its prefix: `gml:pos` → `pos`.
pub(crate) fn local(label: &str) -> &str {
    label.rsplit(':').next().unwrap_or(label)
}

/// Every node at or below `from` whose local name is `name`.
pub(crate) fn named(t: &DataTree, from: u32, name: &str) -> Vec<u32> {
    t.subtree(from).into_iter().filter(|&n| local(t.label(n)) == name).collect()
}

/// The first child of `id` whose local name is `name`.
pub(crate) fn child(t: &DataTree, id: u32, name: &str) -> Option<u32> {
    let ids: Vec<u32> = if id == TOP { t.roots().to_vec() } else { t.child_ids(id).to_vec() };
    ids.into_iter().find(|&c| local(t.label(c)) == name)
}

/// `Category::Data` format info.
pub(crate) fn format(id: &'static str, name: &str) -> FormatInfo {
    FormatInfo::new(id, name, Category::Data)
}

#[allow(dead_code)]
fn _kinds(_: ValueKind) {}
