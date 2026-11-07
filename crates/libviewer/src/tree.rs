//! A tree read a level at a time: a node's children are asked for when it is first
//! expanded, so a tree of a million FBX records shows its top at once.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

use libfileparser_core::{NodeId, TreeSource, ValueKind};
use libgui::*;

/// Children shown per node; past this the rest are summarised in one row.
const MAX_CHILDREN: u64 = 10_000;
/// Keys with this bit are the summary rows, not nodes of the source.
const MORE: u64 = 1 << 63;

/// A tree view's state: what's expanded (libgui's), and what has been read so far.
pub struct TreeView {
    state: TreeState<u64>,
    read: RefCell<Read>,
}

#[derive(Default)]
struct Read {
    /// Label (`key: value`) and whether it has children, by key.
    nodes: HashMap<u64, (String, bool)>,
    /// Children by parent (`None`: the top), as read.
    children: HashMap<Option<u64>, Vec<u64>>,
    /// Summary rows handed out so far.
    more: u64,
}

impl Default for TreeView {
    fn default() -> Self {
        Self { state: TreeState::new(), read: RefCell::new(Read::default()) }
    }
}

struct Lazy<'a> {
    source: &'a dyn TreeSource,
    read: &'a RefCell<Read>,
}

impl Lazy<'_> {
    fn load(&self, parent: Option<u64>, out: &mut Vec<u64>) {
        if let Some(kids) = self.read.borrow().children.get(&parent) {
            out.extend(kids);
            return;
        }
        let id = parent.map(NodeId);
        let count = self.source.child_count(id).map(|c| c.known()).unwrap_or(0);
        let take = count.min(MAX_CHILDREN);
        let nodes = self.source.children(id, 0..take).unwrap_or_default();
        let mut read = self.read.borrow_mut();
        let mut kids = Vec::with_capacity(nodes.len() + 1);
        for n in nodes {
            let value = shown(&n);
            let label = match (n.label.is_empty(), value.is_empty()) {
                (_, true) => n.label,
                (true, false) => value,
                (false, false) => format!("{}: {value}", n.label),
            };
            read.nodes.insert(n.id.0, (label, n.has_children));
            kids.push(n.id.0);
        }
        if count > take {
            read.more += 1;
            let key = MORE | read.more;
            read.nodes.insert(key, (format!("… {} more not shown", count - take), false));
            kids.push(key);
        }
        out.extend(&kids);
        read.children.insert(parent, kids);
    }
}

impl libgui::TreeSource for Lazy<'_> {
    type Key = u64;

    fn roots(&self, out: &mut Vec<u64>) {
        self.load(None, out);
    }

    fn children(&self, node: u64, out: &mut Vec<u64>) {
        if node & MORE == 0 {
            self.load(Some(node), out);
        }
    }

    fn has_children(&self, node: u64) -> bool {
        self.read.borrow().nodes.get(&node).is_some_and(|(_, has)| *has)
    }

    fn label(&self, node: u64) -> Cow<'_, str> {
        Cow::Owned(self.read.borrow().nodes.get(&node).map_or_else(String::new, |(l, _)| l.clone()))
    }
}

pub fn tree(ui: &mut Ui, source: &dyn TreeSource, view: &mut TreeView) {
    let lazy = Lazy { source, read: &view.read };
    ui.container(Layout::column().padding(Insets::xy(8.0, 4.0)), Frame::none(), |ui| {
        ui.tree_view("tree", &mut view.state, &lazy);
    });
}

/// A node's value as one line: text in quotes with its line breaks escaped, so `"1"`
/// and `1` look different; an error marked.
fn shown(n: &libfileparser_core::TreeNode) -> String {
    match n.kind {
        ValueKind::Text => format!("{:?}", n.value),
        ValueKind::Error => format!("⚠ {}", n.value),
        _ => n.value.clone(),
    }
}
