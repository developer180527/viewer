//! The tree every structured format becomes, with the lines each node came from, so a
//! node links to its text in the Source facet and a line back to its node.
//!
//! **Paths** (contract, docs/design/libfileparser.md §5): `/`-separated segments from
//! the top, one per node. A segment is the node's label with `~` written `~0`, `/`
//! written `~1` and `[` written `~2`, followed by `[n]` when it is the n-th sibling
//! with that label (n from 2). For JSON that is a JSON Pointer: `/servers/0/port`. XML:
//! `/feed/entry[3]/@id`.

use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Cell, Count, Document, Limit, Links, MemoryUse, NodeId, ParseContext, ParseError, Place,
    Result, TreeNode, TreeSource, ValueKind,
};

const NONE: u32 = u32::MAX;

/// The top level, for [`DataTree::child`]: a file whose one object was hoisted.
pub(crate) const TOP: u32 = u32::MAX;

struct Node {
    parent: u32,
    label: String,
    /// The whole value of a leaf, as text; a summary for a branch.
    value: String,
    kind: ValueKind,
    children: Vec<u32>,
    /// Lines of the file, from 0, end exclusive.
    lines: Range<u32>,
}

/// A parsed file as a tree.
pub(crate) struct DataTree {
    nodes: Vec<Node>,
    roots: Vec<u32>,
}

/// Builds a [`DataTree`] within the context's limits.
pub(crate) struct Builder<'c> {
    tree: DataTree,
    cx: &'c ParseContext,
}

impl<'c> Builder<'c> {
    pub fn new(cx: &'c ParseContext) -> Self {
        Self {
            tree: DataTree {
                nodes: Vec::new(),
                roots: Vec::new(),
            },
            cx,
        }
    }

    /// Adds a node under `parent` at `depth` (top-level nodes are depth 1), on `lines`
    /// (its end can be set later, [`Self::end`]).
    pub fn add(
        &mut self,
        parent: Option<u32>,
        depth: usize,
        label: impl Into<String>,
        value: impl Into<String>,
        kind: ValueKind,
        lines: Range<u32>,
    ) -> Result<u32> {
        let n = self.tree.nodes.len();
        self.cx.limits.check(Limit::Nodes, n as u64 + 1)?;
        self.cx.limits.check(Limit::Depth, depth as u64)?;
        if n.is_multiple_of(4096) {
            self.cx.cancel.check()?;
        }
        let id = n as u32;
        let parent = match parent {
            Some(p) => {
                self.tree.nodes[p as usize].children.push(id);
                p
            }
            None => {
                self.tree.roots.push(id);
                NONE
            }
        };
        self.tree.nodes.push(Node {
            parent,
            label: label.into(),
            value: value.into(),
            kind,
            children: Vec::new(),
            lines,
        });
        Ok(id)
    }

    /// Sets where node `id` ends: `last_line` is its last line, inclusive.
    pub fn end(&mut self, id: u32, last_line: u32) {
        let node = &mut self.tree.nodes[id as usize];
        node.lines.end = node.lines.end.max(last_line + 1);
    }

    pub fn set_value(&mut self, id: u32, value: impl Into<String>) {
        self.tree.nodes[id as usize].value = value.into();
    }

    /// Removes every node added since the builder had `mark` nodes: what a bad line
    /// of JSON Lines added before it failed. Those nodes are top-level or under each other.
    pub fn rollback(&mut self, mark: usize) {
        self.tree.nodes.truncate(mark);
        let mark = mark as u32;
        while self.tree.roots.last().is_some_and(|&r| r >= mark) {
            self.tree.roots.pop();
        }
    }

    pub fn set_kind(&mut self, id: u32, kind: ValueKind) {
        self.tree.nodes[id as usize].kind = kind;
    }

    pub fn set_label(&mut self, id: u32, label: impl Into<String>) {
        self.tree.nodes[id as usize].label = label.into();
    }

    /// Replaces top-level node `id` by its children: a file holding one mapping shows
    /// its members at the top. `id` stays, unreachable and on no line.
    pub fn hoist(&mut self, id: u32) {
        let children = std::mem::take(&mut self.tree.nodes[id as usize].children);
        for &c in &children {
            self.tree.nodes[c as usize].parent = NONE;
        }
        self.tree.nodes[id as usize].lines = 0..0;
        let at = self
            .tree
            .roots
            .iter()
            .position(|&r| r == id)
            .unwrap_or(self.tree.roots.len());
        self.tree
            .roots
            .splice(at..(at + 1).min(self.tree.roots.len()), children);
    }

    /// How many children `parent` (or the top) has so far.
    pub fn child_count(&self, parent: Option<u32>) -> usize {
        match parent {
            Some(p) => self.tree.nodes[p as usize].children.len(),
            None => self.tree.roots.len(),
        }
    }

    pub fn children(&self, id: u32) -> usize {
        self.tree.nodes[id as usize].children.len()
    }

    pub fn len(&self) -> usize {
        self.tree.nodes.len()
    }

    pub fn finish(self) -> Result<DataTree> {
        let memory = self.tree.memory().total();
        self.cx.limits.check(Limit::Memory, memory)?;
        Ok(self.tree)
    }
}

/// A long value shortened for [`TreeNode::value`]; [`TreeSource::value`] has it whole.
fn preview(value: &str) -> String {
    let mut value = value.to_string();
    if let Some((at, _)) = value.char_indices().nth(TreeNode::PREVIEW_CHARS) {
        value.truncate(at);
        value.push('…');
    }
    value
}

impl DataTree {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn roots(&self) -> &[u32] {
        &self.roots
    }

    pub fn label(&self, id: u32) -> &str {
        &self.nodes[id as usize].label
    }

    pub fn value(&self, id: u32) -> &str {
        &self.nodes[id as usize].value
    }

    pub fn kind(&self, id: u32) -> ValueKind {
        self.nodes[id as usize].kind
    }

    /// Node `id`'s whole value, typed: `None` for a branch.
    pub fn cell(&self, id: u32) -> Option<Cell> {
        let n = &self.nodes[id as usize];
        if !n.children.is_empty() || matches!(n.kind, ValueKind::Map | ValueKind::List | ValueKind::Group) {
            return None;
        }
        Some(cell(n.kind, &n.value))
    }

    /// The first child of `id` labelled `label`; [`TOP`] for the top level.
    pub fn child(&self, id: u32, label: &str) -> Option<u32> {
        let ids = if id == TOP { &self.roots } else { &self.nodes[id as usize].children };
        ids.iter().copied().find(|&c| self.nodes[c as usize].label == label)
    }

    pub fn parent(&self, id: u32) -> Option<u32> {
        match self.nodes[id as usize].parent {
            NONE => None,
            p => Some(p),
        }
    }

    /// `id` and every node below it, in document order.
    pub fn subtree(&self, id: u32) -> Vec<u32> {
        if id == TOP {
            return self.all();
        }
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.nodes[n as usize].children.iter().rev());
        }
        out
    }

    /// Every node, in document order (top-level nodes and what is below each).
    pub fn all(&self) -> Vec<u32> {
        self.roots.iter().flat_map(|&r| self.subtree(r)).collect()
    }

    pub fn child_ids(&self, id: u32) -> &[u32] {
        &self.nodes[id as usize].children
    }

    pub fn lines(&self, id: u32) -> Range<u32> {
        self.nodes[id as usize].lines.clone()
    }

    fn siblings(&self, parent: u32) -> &[u32] {
        if parent == NONE {
            &self.roots
        } else {
            &self.nodes[parent as usize].children
        }
    }

    fn ids(&self, parent: Option<NodeId>) -> Result<&[u32]> {
        match parent {
            None => Ok(&self.roots),
            Some(p) => self
                .nodes
                .get(p.0 as usize)
                .map(|n| n.children.as_slice())
                .ok_or_else(|| ParseError::invalid(format!("no node {}", p.0))),
        }
    }

    fn tree_node(&self, id: u32) -> TreeNode {
        let n = &self.nodes[id as usize];
        TreeNode::new(NodeId(id as u64), n.label.clone(), preview(&n.value), !n.children.is_empty())
            .with_kind(n.kind)
    }

    /// The node on `line` most closely: the smallest one whose lines hold it.
    fn node_at(&self, line: u32) -> Option<u32> {
        let mut best: Option<(u32, u32)> = None;
        for (i, n) in self.nodes.iter().enumerate() {
            if n.lines.start <= line && line < n.lines.end {
                let size = n.lines.end - n.lines.start;
                if best.is_none_or(|(_, s)| size <= s) {
                    best = Some((i as u32, size));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    /// The path segment of `id` among its siblings.
    fn segment(&self, id: u32) -> String {
        let n = &self.nodes[id as usize];
        let mut seg = escape(&n.label);
        let siblings = self.siblings(n.parent);
        let nth = siblings
            .iter()
            .take_while(|&&s| s != id)
            .filter(|&&s| self.nodes[s as usize].label == n.label)
            .count();
        if nth > 0 {
            seg.push_str(&format!("[{}]", nth + 1));
        }
        seg
    }
}

fn escape(label: &str) -> String {
    label
        .replace('~', "~0")
        .replace('/', "~1")
        .replace('[', "~2")
}

fn unescape(segment: &str) -> String {
    segment
        .replace("~2", "[")
        .replace("~1", "/")
        .replace("~0", "~")
}

impl TreeSource for DataTree {
    fn child_count(&self, parent: Option<NodeId>) -> Result<Count> {
        Ok(Count::Exact(self.ids(parent)?.len() as u64))
    }

    fn children(&self, parent: Option<NodeId>, range: Range<u64>) -> Result<Vec<TreeNode>> {
        let ids = self.ids(parent)?;
        let end = (range.end.min(ids.len() as u64)) as usize;
        let start = (range.start as usize).min(end);
        Ok(ids[start..end]
            .iter()
            .map(|&id| self.tree_node(id))
            .collect())
    }

    fn memory(&self) -> MemoryUse {
        let text: usize = self
            .nodes
            .iter()
            .map(|n| n.label.capacity() + n.value.capacity() + n.children.capacity() * 4)
            .sum();
        MemoryUse::heap(
            (text + self.nodes.capacity() * size_of::<Node>() + self.roots.capacity() * 4) as u64,
        )
    }

    fn value(&self, node: NodeId) -> Result<Option<Cell>> {
        match u32::try_from(node.0).ok().filter(|&i| (i as usize) < self.nodes.len()) {
            Some(id) => Ok(self.cell(id)),
            None => Err(ParseError::invalid(format!("no node {}", node.0))),
        }
    }

    fn find(&self, path: &str) -> Result<Option<NodeId>> {
        let Some(rest) = path.strip_prefix('/') else {
            return Ok(None);
        };
        let mut at = NONE;
        for seg in rest.split('/') {
            // `label[n]`: the n-th sibling with that label. Escaped labels never hold `[`.
            let (label, nth) = match seg.strip_suffix(']').and_then(|s| s.rsplit_once('[')) {
                Some((l, n)) => match n.parse::<usize>() {
                    Ok(n) if n >= 2 => (l, n),
                    _ => return Ok(None),
                },
                None => (seg, 1),
            };
            let label = unescape(label);
            let siblings = self.siblings(at);
            // Array items are labelled by index: go straight there.
            let direct = label
                .parse::<usize>()
                .ok()
                .filter(|_| nth == 1)
                .and_then(|i| siblings.get(i))
                .filter(|&&s| self.nodes[s as usize].label == label);
            let found = match direct {
                Some(&s) => Some(s),
                None => siblings
                    .iter()
                    .copied()
                    .filter(|&s| self.nodes[s as usize].label == label)
                    .nth(nth - 1),
            };
            match found {
                Some(s) => at = s,
                None => return Ok(None),
            }
        }
        Ok((at != NONE).then_some(NodeId(at as u64)))
    }

    fn path(&self, node: NodeId) -> Result<Option<String>> {
        let Some(mut id) = u32::try_from(node.0)
            .ok()
            .filter(|&i| (i as usize) < self.nodes.len())
        else {
            return Ok(None);
        };
        let mut segments = Vec::new();
        loop {
            segments.push(self.segment(id));
            match self.nodes[id as usize].parent {
                NONE => break,
                p => id = p,
            }
        }
        segments.reverse();
        Ok(Some(format!("/{}", segments.join("/"))))
    }
}

/// A value of `kind` written as `text`, as a cell.
pub(crate) fn cell(kind: ValueKind, text: &str) -> Cell {
    match kind {
        ValueKind::Null => Cell::Null,
        ValueKind::Bool => Cell::Bool(text == "true"),
        ValueKind::Number => Cell::number(text),
        ValueKind::Date => Cell::Date(text.to_string()),
        ValueKind::Error => Cell::Error(text.to_string()),
        _ => Cell::Text(text.to_string()),
    }
}

/// Line numbers of byte offsets, counted from the last offset asked, so offsets that
/// mostly go forward (a parser's) cost one pass over the file.
pub(crate) struct LineCounter<'a> {
    text: &'a [u8],
    at: usize,
    line: u32,
}

impl<'a> LineCounter<'a> {
    pub fn new(text: &'a [u8]) -> Self {
        Self {
            text,
            at: 0,
            line: 0,
        }
    }

    /// The line (from 0) byte `offset` is on.
    pub fn line(&mut self, offset: usize) -> u32 {
        let offset = offset.min(self.text.len());
        if offset >= self.at {
            self.line += memchr::memchr_iter(b'\n', &self.text[self.at..offset]).count() as u32;
        } else {
            self.line -= memchr::memchr_iter(b'\n', &self.text[offset..self.at]).count() as u32;
        }
        self.at = offset;
        self.line
    }
}

/// Links between a tree facet's nodes and the file's lines, and, when the document
/// has one, between a table's rows and the tree nodes they are (JSON Lines' lines,
/// HAR's entries).
pub(crate) struct TreeLinks {
    pub tree: Arc<DataTree>,
    pub tree_facet: usize,
    /// A table facet whose row r is node `rows[r]`.
    pub table: Option<(usize, Arc<[u32]>)>,
    /// Whether the tree's lines are the file's (not a file inside it, as a KMZ's KML).
    pub lines: bool,
}

impl Links for TreeLinks {
    fn links(&self, _: &Document, from: &Place, _: &ParseContext) -> Result<Vec<Place>> {
        let t = &self.tree;
        let lines = |id: u32| {
            let l = t.lines(id);
            (self.lines && l.start < l.end).then_some(Place::Lines(l.start as u64..l.end as u64))
        };
        let node = |id: u32| Place::Node { facet: self.tree_facet, node: NodeId(id as u64) };
        // The row of the nearest node above `id` (or `id` itself) that is a row.
        let row_of = |id: u32| {
            let (facet, rows) = self.table.as_ref()?;
            let mut at = id;
            loop {
                if let Some(row) = rows.iter().position(|&r| r == at) {
                    return Some(Place::Row { facet: *facet, row: row as u64 });
                }
                match t.nodes[at as usize].parent {
                    NONE => return None,
                    p => at = p,
                }
            }
        };
        Ok(match *from {
            Place::Node { facet, node: n } if facet == self.tree_facet => match u32::try_from(n.0).ok().filter(|&i| (i as usize) < t.len()) {
                Some(id) => lines(id).into_iter().chain(row_of(id)).collect(),
                None => Vec::new(),
            },
            Place::Row { facet, row } if self.table.as_ref().is_some_and(|(f, _)| *f == facet) => {
                match self.table.as_ref().and_then(|(_, rows)| rows.get(row as usize).copied()) {
                    Some(id) => lines(id).into_iter().chain([node(id)]).collect(),
                    None => Vec::new(),
                }
            }
            Place::Lines(_) if !self.lines => Vec::new(),
            Place::Lines(ref r) => match u32::try_from(r.start).ok().and_then(|l| t.node_at(l)) {
                Some(id) => [node(id)].into_iter().chain(row_of(id)).collect(),
                None => Vec::new(),
            },
            _ => Vec::new(),
        })
    }
}

/// A node as a view shows it: text in quotes, escaped, so tests see kinds too.
#[cfg(test)]
pub(crate) fn shown(n: &TreeNode) -> String {
    if n.kind == ValueKind::Text { format!("{:?}", n.value) } else { n.value.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_round_trip_through_find() {
        let cx = ParseContext::new();
        let mut b = Builder::new(&cx);
        let a = b.add(None, 1, "a/b", "{2}", ValueKind::Map, 0..1).unwrap();
        b.add(Some(a), 2, "item", "1", ValueKind::Number, 0..1).unwrap();
        let second = b.add(Some(a), 2, "item", "2", ValueKind::Number, 0..1).unwrap();
        b.add(None, 1, "x[0]~", "", ValueKind::Text, 1..2).unwrap();
        let t = b.finish().unwrap();
        for id in 0..t.len() as u64 {
            let path = t.path(NodeId(id)).unwrap().unwrap();
            assert_eq!(t.find(&path).unwrap(), Some(NodeId(id)), "{path}");
        }
        assert_eq!(
            t.path(NodeId(second as u64)).unwrap().unwrap(),
            "/a~1b/item[2]"
        );
        assert_eq!(t.path(NodeId(3)).unwrap().unwrap(), "/x~20]~0");
        assert_eq!(t.find("/nope").unwrap(), None);
        assert_eq!(t.find("a").unwrap(), None);
    }

    #[test]
    fn lines_counted_both_ways() {
        let mut lc = LineCounter::new(b"a\nb\nc\n");
        assert_eq!(lc.line(4), 2);
        assert_eq!(lc.line(1), 0);
        assert_eq!(lc.line(2), 1);
        assert_eq!(lc.line(99), 3);
    }
}
