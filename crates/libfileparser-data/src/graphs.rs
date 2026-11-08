//! Graphs: Graphviz DOT, GraphML, GEXF, GXL and Graph Modelling Language, each as a
//! table of nodes and a table of edges with their attributes; the XML formats and
//! GML over their tree. (A drawn graph is a view for libviewer to add, on libgui's
//! node graph.)

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Cell, Column, Content, Count, Document, FileSource, Limit, Links, MemoryUse, ParseContext, ParseError, Place, Position, Result, Role,
    TableInfo, TableSource, ValueKind,
};

use crate::nodes::{MAX_PROPERTIES, NodeTable, SAMPLE, child, format, local, named, value};
use crate::tree::{Builder, DataTree, LineCounter, TreeLinks};

// ---------------------------------------------------------------- shared

/// A graph document over a tree: Nodes, Edges, Tree, then the file's facets; rows
/// linked to their elements.
fn tree_document(id: &'static str, name: &str, nodes: NodeTable, edges: NodeTable, directed: Option<bool>, src: &FileSource, language: &str) -> Document {
    let tree = nodes.tree.clone();
    let (n, e) = (nodes.rows.clone(), edges.rows.clone());
    let mut doc = Document::new(format(id, name))
        .with_meta("Nodes", n.len())
        .with_meta("Edges", e.len())
        .with_facet(Role::Primary, "Nodes", Content::Table(Arc::new(nodes)))
        .with_facet(Role::Structure, "Edges", Content::Table(Arc::new(edges)))
        .with_facet(Role::Structure, "Tree", Content::Tree(tree.clone()));
    if let Some(d) = directed {
        doc = doc.with_meta("Directed", if d { "yes" } else { "no" });
    }
    doc.with_links(TreeLinks { tree: tree.clone(), tree_facet: 2, table: Some((0, n)), lines: true })
        .with_links(TreeLinks { tree, tree_facet: 2, table: Some((1, e)), lines: true })
        .with_file_facets(src, Some(language))
}

/// Attribute columns named by `names` (in order), each read by `get`.
fn attribute_columns(mut table: NodeTable, names: Vec<String>, get: impl Fn(&DataTree, u32, &str) -> Cell + Send + Sync + Clone + 'static) -> NodeTable {
    for n in names.into_iter().take(MAX_PROPERTIES) {
        let (get, key) = (get.clone(), n.clone());
        table = table.column(&n, ValueKind::Text, move |t, row| get(t, row, &key));
    }
    table
}

fn opt(t: &DataTree, id: Option<u32>) -> Cell {
    value(t, id)
}

// ---------------------------------------------------------------- GraphML

pub(crate) fn graphml(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().iter().copied().find(|&r| local(tree.label(r)) == "graphml").ok_or_else(|| ParseError::corrupt("GraphML: no <graphml>"))?;
    // `<key id="d0" for="node" attr.name="color"/>`: data keys by id, per element kind.
    let keys: Vec<(String, String, String)> = named(&tree, root, "key")
        .into_iter()
        .map(|k| {
            let get = |a: &str| tree.child(k, a).map(|v| tree.value(v).to_string()).unwrap_or_default();
            let id = get("@id");
            let name = Some(get("@attr.name")).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
            (id, get("@for"), name)
        })
        .collect();
    let directed = named(&tree, root, "graph").first().and_then(|&g| tree.child(g, "@edgedefault")).map(|d| tree.value(d) == "directed");
    let nodes: Vec<u32> = named(&tree, root, "node");
    let edges: Vec<u32> = named(&tree, root, "edge");
    let names = |kind: &str| keys.iter().filter(|(_, f, _)| f == kind || f == "all" || f.is_empty()).map(|(_, _, n)| n.clone()).collect::<Vec<_>>();
    let (node_names, edge_names) = (names("node"), names("edge"));
    let key_of: Arc<Vec<(String, String)>> = Arc::new(keys.iter().map(|(id, _, n)| (n.clone(), id.clone())).collect());
    let tree = Arc::new(tree);
    let data = move |t: &DataTree, row: u32, name: &str| -> Cell {
        let Some(id) = key_of.iter().find(|(n, _)| n == name).map(|(_, id)| id.clone()) else { return Cell::Null };
        let d = t.child_ids(row).iter().copied().find(|&c| local(t.label(c)) == "data" && t.child(c, "@key").is_some_and(|k| t.value(k) == id));
        opt(t, d)
    };
    let node_table = attribute_columns(NodeTable::new(tree.clone(), nodes.into()).column("ID", ValueKind::Text, |t, row| opt(t, t.child(row, "@id"))), node_names, data.clone());
    let edge_table = attribute_columns(
        NodeTable::new(tree, edges.into())
            .column("From", ValueKind::Text, |t, row| opt(t, t.child(row, "@source")))
            .column("To", ValueKind::Text, |t, row| opt(t, t.child(row, "@target")))
            .column("ID", ValueKind::Text, |t, row| opt(t, t.child(row, "@id"))),
        edge_names,
        data,
    );
    Ok(tree_document("graphml", "GraphML", node_table, edge_table, directed, src, "xml"))
}

// ---------------------------------------------------------------- GEXF

pub(crate) fn gexf(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().iter().copied().find(|&r| local(tree.label(r)) == "gexf").ok_or_else(|| ParseError::corrupt("GEXF: no <gexf>"))?;
    let graph = child(&tree, root, "graph");
    let directed = graph.and_then(|g| tree.child(g, "@defaultedgetype")).map(|d| tree.value(d) == "directed");
    // `<attributes class="node"><attribute id="0" title="url"/>`: titles by id.
    let titles = |class: &str| -> Vec<(String, String)> {
        named(&tree, root, "attributes")
            .into_iter()
            .filter(|&a| tree.child(a, "@class").is_some_and(|c| tree.value(c) == class))
            .flat_map(|a| tree.child_ids(a).iter().copied().filter(|&c| local(tree.label(c)) == "attribute").collect::<Vec<_>>())
            .map(|a| {
                let id = tree.child(a, "@id").map(|v| tree.value(v).to_string()).unwrap_or_default();
                (tree.child(a, "@title").map(|v| tree.value(v).to_string()).unwrap_or_else(|| id.clone()), id)
            })
            .collect()
    };
    let (node_attrs, edge_attrs) = (Arc::new(titles("node")), Arc::new(titles("edge")));
    let nodes = named(&tree, root, "node");
    let edges = named(&tree, root, "edge");
    let tree = Arc::new(tree);
    let attvalue = |attrs: Arc<Vec<(String, String)>>| {
        move |t: &DataTree, row: u32, title: &str| -> Cell {
            let Some(id) = attrs.iter().find(|(n, _)| n == title).map(|(_, id)| id.clone()) else { return Cell::Null };
            let Some(vals) = child(t, row, "attvalues") else { return Cell::Null };
            let v = t.child_ids(vals).iter().copied().find(|&v| t.child(v, "@for").or_else(|| t.child(v, "@id")).is_some_and(|f| t.value(f) == id));
            opt(t, v.and_then(|v| t.child(v, "@value")))
        }
    };
    let node_table = attribute_columns(
        NodeTable::new(tree.clone(), nodes.into()).column("ID", ValueKind::Text, |t, row| opt(t, t.child(row, "@id"))).column("Label", ValueKind::Text, |t, row| opt(t, t.child(row, "@label"))),
        node_attrs.iter().map(|(n, _)| n.clone()).collect(),
        attvalue(node_attrs.clone()),
    );
    let edge_table = attribute_columns(
        NodeTable::new(tree, edges.into())
            .column("From", ValueKind::Text, |t, row| opt(t, t.child(row, "@source")))
            .column("To", ValueKind::Text, |t, row| opt(t, t.child(row, "@target")))
            .column("Label", ValueKind::Text, |t, row| opt(t, t.child(row, "@label")))
            .column("Weight", ValueKind::Number, |t, row| t.child(row, "@weight").map_or(Cell::Null, |w| crate::lines::num(t.value(w)))),
        edge_attrs.iter().map(|(n, _)| n.clone()).collect(),
        attvalue(edge_attrs.clone()),
    );
    Ok(tree_document("gexf", "GEXF", node_table, edge_table, directed, src, "xml"))
}

// ---------------------------------------------------------------- GXL

pub(crate) fn gxl(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().iter().copied().find(|&r| local(tree.label(r)) == "gxl").ok_or_else(|| ParseError::corrupt("GXL: no <gxl>"))?;
    let directed = named(&tree, root, "graph").first().and_then(|&g| tree.child(g, "@edgemode")).map(|m| tree.value(m).starts_with("directed"));
    let (nodes, edges) = (named(&tree, root, "node"), named(&tree, root, "edge"));
    // `<attr name="x"><string>v</string></attr>`.
    let attr_names = |rows: &[u32]| {
        let mut names: Vec<String> = Vec::new();
        for &r in rows.iter().take(SAMPLE) {
            for a in tree.child_ids(r).iter().copied().filter(|&c| local(tree.label(c)) == "attr") {
                if let Some(n) = tree.child(a, "@name").map(|n| tree.value(n).to_string())
                    && !names.contains(&n)
                {
                    names.push(n);
                }
            }
        }
        names
    };
    let (node_names, edge_names) = (attr_names(&nodes), attr_names(&edges));
    let tree = Arc::new(tree);
    let attr = |t: &DataTree, row: u32, name: &str| -> Cell {
        let a = t.child_ids(row).iter().copied().find(|&c| local(t.label(c)) == "attr" && t.child(c, "@name").is_some_and(|n| t.value(n) == name));
        a.and_then(|a| t.child_ids(a).iter().copied().find(|&v| !t.label(v).starts_with('@'))).map_or(Cell::Null, |v| opt(t, Some(v)))
    };
    let node_table = attribute_columns(NodeTable::new(tree.clone(), nodes.into()).column("ID", ValueKind::Text, |t, row| opt(t, t.child(row, "@id"))), node_names, attr);
    let edge_table = attribute_columns(
        NodeTable::new(tree, edges.into())
            .column("From", ValueKind::Text, |t, row| opt(t, t.child(row, "@from")))
            .column("To", ValueKind::Text, |t, row| opt(t, t.child(row, "@to")))
            .column("ID", ValueKind::Text, |t, row| opt(t, t.child(row, "@id"))),
        edge_names,
        attr,
    );
    Ok(tree_document("gxl", "GXL", node_table, edge_table, directed, src, "xml"))
}

// ---------------------------------------------------------------- Graph Modelling Language

/// GML's key-value lists, `graph [ node [ id 1 label "A" ] … ]`, as a tree.
fn gml_tree(text: &str, cx: &ParseContext) -> Result<DataTree> {
    let s = text.as_bytes();
    let mut b = Builder::new(cx);
    let mut lines = LineCounter::new(s);
    let mut stack: Vec<u32> = Vec::new();
    let mut key: Option<(String, usize)> = None;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'#' {
            i = memchr::memchr(b'\n', &s[i..]).map_or(s.len(), |p| i + p);
            continue;
        }
        if c == b']' {
            match stack.pop() {
                Some(id) => {
                    let n = b.children(id);
                    b.set_value(id, format!("[{n}]"));
                    let line = lines.line(i);
                    b.end(id, line);
                }
                None => return Err(ParseError::corrupt_at("GML: a `]` without a `[`", Position::in_text(s, i))),
            }
            i += 1;
            continue;
        }
        let at = i;
        match key.take() {
            None => {
                let end = s[i..].iter().position(|b| b.is_ascii_whitespace() || matches!(b, b'[' | b']')).map_or(s.len(), |p| i + p);
                if end == i {
                    return Err(ParseError::corrupt_at("GML: expected a key", Position::in_text(s, i)));
                }
                key = Some((text[i..end].to_string(), at));
                i = end;
            }
            Some((k, k_at)) => {
                let line = lines.line(k_at);
                if c == b'[' {
                    let id = b.add(stack.last().copied(), stack.len() + 1, k, "", ValueKind::List, line..line + 1)?;
                    stack.push(id);
                    i += 1;
                } else if c == b'"' {
                    let end = memchr::memchr(b'"', &s[i + 1..]).map_or(s.len(), |p| i + 1 + p);
                    let last = lines.line(end);
                    b.add(stack.last().copied(), stack.len() + 1, k, crate::markup::decode(&text[i + 1..end]), ValueKind::Text, line..last + 1)?;
                    i = (end + 1).min(s.len());
                } else {
                    let end = s[i..].iter().position(|b| b.is_ascii_whitespace() || *b == b']').map_or(s.len(), |p| i + p);
                    let v = &text[i..end];
                    let kind = if v.parse::<f64>().is_ok() { ValueKind::Number } else { ValueKind::Text };
                    b.add(stack.last().copied(), stack.len() + 1, k, v, kind, line..line + 1)?;
                    i = end;
                }
            }
        }
    }
    if !stack.is_empty() {
        return Err(ParseError::corrupt("GML: a list that never closes"));
    }
    b.finish()
}

pub(crate) fn looks_like_graph_gml(header: &[u8]) -> bool {
    let text = String::from_utf8_lossy(header);
    // `graph [` (DOT's is `graph name {`), or the `Creator`/`Version` keys before it.
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).take(4096).collect();
    let first = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#'));
    first.is_some_and(|l| l.starts_with("graph") || l.starts_with("Creator") || l.starts_with("Version")) && compact.contains("graph[")
}

pub(crate) fn graph_gml(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let text = String::from_utf8_lossy(src.bytes());
    let tree = gml_tree(&text, cx)?;
    let graph = tree.roots().iter().copied().find(|&r| tree.label(r) == "graph").ok_or_else(|| ParseError::corrupt("GML: no graph"))?;
    let directed = tree.child(graph, "directed").map(|d| tree.value(d) == "1");
    let pick = |label: &str| tree.child_ids(graph).iter().copied().filter(|&c| tree.label(c) == label).collect::<Vec<_>>();
    let (nodes, edges) = (pick("node"), pick("edge"));
    let keys = |rows: &[u32], skip: &[&str]| {
        let mut names: Vec<String> = Vec::new();
        for &r in rows.iter().take(SAMPLE) {
            for &c in tree.child_ids(r) {
                let l = tree.label(c).to_string();
                if !skip.contains(&l.as_str()) && !names.contains(&l) {
                    names.push(l);
                }
            }
        }
        names
    };
    let (node_keys, edge_keys) = (keys(&nodes, &["id", "label"]), keys(&edges, &["source", "target", "label"]));
    let tree = Arc::new(tree);
    let get = |t: &DataTree, row: u32, key: &str| -> Cell { opt(t, t.child(row, key)).max_kind(t, t.child(row, key)) };
    let node_table = attribute_columns(
        NodeTable::new(tree.clone(), nodes.into()).column("ID", ValueKind::Mixed, move |t, row| get(t, row, "id")).column("Label", ValueKind::Text, move |t, row| get(t, row, "label")),
        node_keys,
        get,
    );
    let edge_table = attribute_columns(
        NodeTable::new(tree, edges.into())
            .column("From", ValueKind::Mixed, move |t, row| get(t, row, "source"))
            .column("To", ValueKind::Mixed, move |t, row| get(t, row, "target"))
            .column("Label", ValueKind::Text, move |t, row| get(t, row, "label")),
        edge_keys,
        get,
    );
    Ok(tree_document("graph-gml", "Graph Modelling Language", node_table, edge_table, directed, src, "gml"))
}

/// A value, or a nested list's summary.
trait MaxKind {
    fn max_kind(self, t: &DataTree, id: Option<u32>) -> Cell;
}

impl MaxKind for Cell {
    fn max_kind(self, t: &DataTree, id: Option<u32>) -> Cell {
        match (self, id) {
            (Cell::Null, Some(id)) if t.kind(id) == ValueKind::List => Cell::Text(t.value(id).to_string()),
            (c, _) => c,
        }
    }
}

// ---------------------------------------------------------------- DOT
//
// Memory: a DOT file can hold millions of edges, so ids are borrowed from the text
// while parsing (only quoted ids with escapes are copied), each node's name is kept
// once and edges refer to nodes by number, and the two tables build their rows only
// for the range asked for.

#[derive(Clone, Debug, PartialEq)]
enum Tok<'a> {
    Id(Cow<'a, str>),
    Sym(&'static str),
}

/// DOT's tokens, one at a time, with the byte each starts at: a big file's tokens
/// are never all held at once.
struct DotLexer<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> DotLexer<'a> {
    fn next(&mut self) -> Option<(Tok<'a>, usize)> {
        let (s, b) = (self.s, self.s.as_bytes());
        let mut i = self.i;
        let tok = loop {
            if i >= b.len() {
                self.i = i;
                return None;
            }
            let c = b[i];
            if c.is_ascii_whitespace() {
                i += 1;
            } else if s[i..].starts_with("//") || (c == b'#' && (i == 0 || b[i - 1] == b'\n')) {
                i = memchr::memchr(b'\n', &b[i..]).map_or(b.len(), |p| i + p);
            } else if s[i..].starts_with("/*") {
                i = memchr::memmem::find(&b[i + 2..], b"*/").map_or(b.len(), |p| i + 2 + p + 2);
            } else if s[i..].starts_with("->") || s[i..].starts_with("--") {
                let t = (Tok::Sym(if c == b'-' && b[i + 1] == b'>' { "->" } else { "--" }), i);
                i += 2;
                break t;
            } else if let Some(sym) = ["{", "}", "[", "]", "=", ";", ",", ":"].iter().find(|s| c == s.as_bytes()[0]) {
                let t = (Tok::Sym(sym), i);
                i += 1;
                break t;
            } else if c == b'"' {
                // Borrowed unless it has an escape to undo.
                let end = memchr::memchr(b'"', &b[i + 1..]).map_or(b.len(), |p| i + 1 + p);
                let plain = &s[i + 1..end];
                if !plain.contains('\\') {
                    let t = (Tok::Id(Cow::Borrowed(plain)), i);
                    i = end + 1;
                    break t;
                }
                let mut v = String::new();
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' && b.get(j + 1) == Some(&b'"') {
                        v.push('"');
                        j += 2;
                    } else if b[j] == b'\\' && b.get(j + 1) == Some(&b'\n') {
                        j += 2;
                    } else {
                        let ch = s[j..].chars().next().unwrap_or(' ');
                        v.push(ch);
                        j += ch.len_utf8();
                    }
                }
                let t = (Tok::Id(Cow::Owned(v)), i);
                i = j + 1;
                break t;
            } else if c == b'<' {
                // An HTML label, nested `<…>`.
                let mut depth = 0;
                let mut j = i;
                while j < b.len() {
                    match b[j] {
                        b'<' => depth += 1,
                        b'>' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let t = (Tok::Id(Cow::Borrowed(&s[i + 1..j.min(b.len())])), i);
                i = j + 1;
                break t;
            } else {
                let end = s[i..].find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '.' || (!ch.is_ascii() && !ch.is_whitespace()))).map_or(b.len(), |p| i + p);
                let end = if end == i {
                    // A numeral like `-1.5`.
                    s[i..].find(|ch: char| !(ch.is_ascii_digit() || ch == '.' || ch == '-')).map_or(b.len(), |p| i + p).max(i + s[i..].chars().next().map_or(1, char::len_utf8))
                } else {
                    end
                };
                let t = (Tok::Id(Cow::Borrowed(&s[i..end])), i);
                i = end;
                break t;
            }
        };
        self.i = i;
        Some(tok)
    }
}

/// Attributes as written: `(name, value)`.
type Attrs = Vec<(String, String)>;

struct DotNode {
    id: Box<str>,
    attrs: Attrs,
    lines: Range<u64>,
}

struct DotEdge {
    /// Indices into [`Dot::nodes`].
    from: u32,
    to: u32,
    attrs: Attrs,
    lines: Range<u64>,
}

struct Dot {
    nodes: Vec<DotNode>,
    edges: Vec<DotEdge>,
    directed: bool,
    name: Option<String>,
}

struct DotParser<'a> {
    lex: DotLexer<'a>,
    /// Tokens read ahead of the parser: at most two.
    ahead: VecDeque<(Tok<'a>, usize)>,
    lines: LineCounter<'a>,
    out: Dot,
    /// Where each node id is in `out.nodes`, keyed by the text's own bytes where it can.
    index: HashMap<Cow<'a, str>, u32>,
    cx: &'a ParseContext,
}

impl<'a> DotParser<'a> {
    /// Token `k` ahead (0 is the next one).
    fn look(&mut self, k: usize) -> Option<&(Tok<'a>, usize)> {
        while self.ahead.len() <= k {
            let t = self.lex.next()?;
            self.ahead.push_back(t);
        }
        self.ahead.get(k)
    }

    fn peek(&mut self) -> Option<&Tok<'a>> {
        self.look(0).map(|t| &t.0)
    }

    /// Moves past `n` tokens.
    fn bump(&mut self, n: usize) {
        for _ in 0..n {
            if self.ahead.pop_front().is_none() {
                self.lex.next();
            }
        }
    }

    fn sym(&mut self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Sym(t)) if *t == s)
    }

    fn id(&mut self) -> Option<Cow<'a, str>> {
        match self.peek() {
            Some(Tok::Id(v)) => {
                let v = v.clone();
                self.bump(1);
                Some(v)
            }
            _ => None,
        }
    }

    /// Whether token `k` ahead is an id `pred` accepts.
    fn is_id(&mut self, k: usize, pred: impl Fn(&str) -> bool) -> bool {
        matches!(self.look(k), Some((Tok::Id(v), _)) if pred(v))
    }

    fn is_sym_at(&mut self, k: usize, s: &str) -> bool {
        matches!(self.look(k), Some((Tok::Sym(t), _)) if *t == s)
    }

    fn line(&mut self) -> u64 {
        let at = self.look(0).map_or(0, |t| t.1);
        self.lines.line(at) as u64
    }

    /// `[a=b, c=d][e=f]`.
    fn attrs(&mut self) -> Attrs {
        let mut out = Vec::new();
        while self.sym("[") {
            self.bump(1);
            while !self.sym("]") && self.peek().is_some() {
                let Some(k) = self.id() else {
                    self.bump(1);
                    continue;
                };
                let v = if self.sym("=") {
                    self.bump(1);
                    self.id().unwrap_or_default()
                } else {
                    Cow::Borrowed("true")
                };
                out.push((k.into_owned(), v.into_owned()));
                if self.sym(",") || self.sym(";") {
                    self.bump(1);
                }
            }
            self.bump(1);
        }
        out
    }

    /// The node named `id`, added the first time it's seen.
    fn node(&mut self, id: Cow<'a, str>, line: u64) -> u32 {
        if let Some(&n) = self.index.get(&id) {
            return n;
        }
        let n = self.out.nodes.len() as u32;
        self.out.nodes.push(DotNode { id: id.as_ref().into(), attrs: Vec::new(), lines: line..line + 1 });
        self.index.insert(id, n);
        n
    }

    /// A node id, `id:port`, or a subgraph: the nodes it stands for.
    fn endpoint(&mut self, depth: usize) -> Result<Option<Vec<u32>>> {
        if self.sym("{") || self.is_id(0, |k| k.eq_ignore_ascii_case("subgraph")) {
            return self.subgraph(depth).map(Some);
        }
        let line = self.line();
        let Some(id) = self.id() else { return Ok(None) };
        // Ports: `a:n` or `a:port:n`.
        while self.sym(":") {
            self.bump(1);
            self.id();
        }
        Ok(Some(vec![self.node(id, line)]))
    }

    /// `subgraph name? { … }` or `{ … }`: the nodes in it, each once.
    fn subgraph(&mut self, depth: usize) -> Result<Vec<u32>> {
        self.cx.limits.check(Limit::Depth, depth as u64)?;
        if self.is_id(0, |k| k.eq_ignore_ascii_case("subgraph")) {
            self.bump(1);
            if !self.sym("{") {
                self.id();
            }
        }
        if !self.sym("{") {
            return Ok(Vec::new());
        }
        self.bump(1);
        let before = self.out.nodes.len() as u32;
        let mentioned = self.statements(depth + 1, true)?;
        if self.sym("}") {
            self.bump(1);
        }
        let mut seen = HashSet::new();
        Ok(mentioned.into_iter().chain(before..self.out.nodes.len() as u32).filter(|n| seen.insert(*n)).collect())
    }

    /// Statements to the closing `}`. With `collect`, the nodes they mention (for a
    /// subgraph used as an edge's end); the graph's own body doesn't need them.
    fn statements(&mut self, depth: usize, collect: bool) -> Result<Vec<u32>> {
        let mut mentioned = Vec::new();
        while self.peek().is_some() && !self.sym("}") {
            if self.sym(";") {
                self.bump(1);
                continue;
            }
            self.cx.limits.check(Limit::Rows, (self.out.nodes.len() + self.out.edges.len()) as u64)?;
            let line = self.line();
            // `graph|node|edge [attrs]`: defaults, not nodes.
            if self.is_id(0, |k| matches!(k.to_ascii_lowercase().as_str(), "graph" | "node" | "edge")) && self.is_sym_at(1, "[") {
                self.bump(1);
                self.attrs();
                continue;
            }
            // `a = b`: a graph attribute.
            if matches!(self.peek(), Some(Tok::Id(_))) && self.is_sym_at(1, "=") {
                self.bump(2);
                self.id();
                continue;
            }
            let Some(first) = self.endpoint(depth)? else {
                self.bump(1);
                continue;
            };
            if collect {
                mentioned.extend_from_slice(&first);
            }
            // An edge chain: `a -> b -> {c d} [attrs]`.
            let mut chain = vec![first];
            while self.sym("->") || self.sym("--") {
                self.bump(1);
                match self.endpoint(depth)? {
                    Some(next) => {
                        if collect {
                            mentioned.extend_from_slice(&next);
                        }
                        chain.push(next);
                    }
                    None => break,
                }
            }
            let attrs = self.attrs();
            let end = self.line();
            if chain.len() == 1 {
                if let [n] = chain[0][..] {
                    self.out.nodes[n as usize].attrs.extend(attrs);
                }
            } else {
                for pair in chain.windows(2) {
                    for &from in &pair[0] {
                        for &to in &pair[1] {
                            self.out.edges.push(DotEdge { from, to, attrs: attrs.clone(), lines: line..end.max(line) + 1 });
                        }
                    }
                }
            }
        }
        Ok(mentioned)
    }
}

fn dot(text: &str, cx: &ParseContext) -> Result<Dot> {
    let mut p = DotParser { lex: DotLexer { s: text, i: 0 }, ahead: VecDeque::new(), lines: LineCounter::new(text.as_bytes()), out: Dot { nodes: Vec::new(), edges: Vec::new(), directed: false, name: None }, index: HashMap::new(), cx };
    // `strict? (graph|digraph) name? {`.
    if p.is_id(0, |k| k.eq_ignore_ascii_case("strict")) {
        p.bump(1);
    }
    match p.id() {
        Some(k) if k.eq_ignore_ascii_case("digraph") => p.out.directed = true,
        Some(k) if k.eq_ignore_ascii_case("graph") => {}
        _ => return Err(ParseError::corrupt("DOT: expected `graph` or `digraph`")),
    }
    if !p.sym("{") {
        p.out.name = p.id().map(Cow::into_owned);
    }
    if !p.sym("{") {
        return Err(ParseError::corrupt("DOT: expected `{`"));
    }
    p.bump(1);
    p.statements(1, false)?;
    Ok(p.out)
}

/// The last value given for attribute `k`.
fn attr(attrs: &[(String, String)], k: &str) -> Cell {
    attrs.iter().rev().find(|(a, _)| a == k).map_or(Cell::Null, |(_, v)| Cell::Text(v.clone()))
}

/// The Nodes or Edges table, its rows made from the parsed graph when asked for.
struct DotTable {
    graph: Arc<Dot>,
    edges: bool,
    columns: Vec<Column>,
    /// Attribute columns after the fixed ones.
    keys: Vec<String>,
}

impl TableSource for DotTable {
    fn info(&self) -> TableInfo {
        let rows = if self.edges { self.graph.edges.len() } else { self.graph.nodes.len() };
        TableInfo::new(self.columns.clone(), Count::Exact(rows as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<Cell>>> {
        let g = &self.graph;
        let len = if self.edges { g.edges.len() } else { g.nodes.len() } as u64;
        let end = range.end.min(len);
        let range = range.start.min(end) as usize..end as usize;
        let row = |fixed: Vec<Cell>, attrs: &Attrs| fixed.into_iter().chain(self.keys.iter().map(|k| attr(attrs, k))).collect();
        Ok(if self.edges {
            g.edges[range]
                .iter()
                .map(|e| {
                    let name = |n: u32| Cell::Text(g.nodes[n as usize].id.to_string());
                    row(vec![name(e.from), name(e.to), attr(&e.attrs, "label")], &e.attrs)
                })
                .collect()
        } else {
            g.nodes[range].iter().map(|n| row(vec![Cell::Text(n.id.to_string()), attr(&n.attrs, "label")], &n.attrs)).collect()
        })
    }

    fn memory(&self) -> MemoryUse {
        // The graph is shared by both tables; each reports half.
        let g = &self.graph;
        let attrs = |a: &Attrs| a.iter().map(|(k, v)| k.len() + v.len() + 48).sum::<usize>();
        let nodes: usize = g.nodes.iter().map(|n| size_of::<DotNode>() + n.id.len() + attrs(&n.attrs)).sum();
        let edges: usize = g.edges.iter().map(|e| size_of::<DotEdge>() + attrs(&e.attrs)).sum();
        MemoryUse::heap(((nodes + edges) / 2) as u64)
    }
}

/// Rows of the two tables (facets 0 and 1) and their lines.
struct DotLinks(Arc<Dot>);

impl Links for DotLinks {
    fn links(&self, _: &Document, from: &Place, _: &ParseContext) -> Result<Vec<Place>> {
        let g = &self.0;
        Ok(match *from {
            Place::Row { facet: 0, row } => g.nodes.get(row as usize).map(|n| vec![Place::Lines(n.lines.clone())]).unwrap_or_default(),
            Place::Row { facet: 1, row } => g.edges.get(row as usize).map(|e| vec![Place::Lines(e.lines.clone())]).unwrap_or_default(),
            Place::Lines(ref r) => {
                let node = g.nodes.iter().position(|n| n.lines.contains(&r.start)).map(|row| Place::Row { facet: 0, row: row as u64 });
                let edge = g.edges.iter().position(|e| e.lines.contains(&r.start)).map(|row| Place::Row { facet: 1, row: row as u64 });
                node.into_iter().chain(edge).collect()
            }
            _ => Vec::new(),
        })
    }
}

pub(crate) fn dot_document(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let text = String::from_utf8_lossy(src.bytes());
    let g = Arc::new(dot(&text, cx)?);
    // Attribute columns: label first, then the rest in the order first seen.
    let keys = |attrs: &mut dyn Iterator<Item = &Attrs>| {
        let mut names: Vec<String> = Vec::new();
        for a in attrs {
            for (k, _) in a {
                if k != "label" && !names.contains(k) && names.len() < MAX_PROPERTIES {
                    names.push(k.clone());
                }
            }
        }
        names
    };
    let text_col = |name: &str| Column::new(name).with_kind(ValueKind::Text);
    let node_keys = keys(&mut g.nodes.iter().map(|n| &n.attrs));
    let edge_keys = keys(&mut g.edges.iter().map(|e| &e.attrs));
    let ncols = [text_col("ID"), text_col("Label")].into_iter().chain(node_keys.iter().map(|k| text_col(k))).collect();
    let ecols = [text_col("From"), text_col("To"), text_col("Label")].into_iter().chain(edge_keys.iter().map(|k| text_col(k))).collect();
    let nodes = DotTable { graph: g.clone(), edges: false, columns: ncols, keys: node_keys };
    let edges = DotTable { graph: g.clone(), edges: true, columns: ecols, keys: edge_keys };
    let mut doc = Document::new(format("dot", "Graphviz DOT"))
        .with_meta("Nodes", g.nodes.len())
        .with_meta("Edges", g.edges.len())
        .with_meta("Directed", if g.directed { "yes" } else { "no" })
        .with_facet(Role::Primary, "Nodes", Content::Table(Arc::new(nodes)))
        .with_facet(Role::Structure, "Edges", Content::Table(Arc::new(edges)));
    if let Some(n) = &g.name {
        doc = doc.with_meta("Name", n);
    }
    Ok(doc.with_links(DotLinks(g)).with_file_facets(src, Some("dot")))
}

pub(crate) fn looks_like_dot(header: &[u8]) -> bool {
    let text = String::from_utf8_lossy(header);
    let first = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#') && !l.starts_with("/*"));
    first.is_some_and(|l| {
        let l = l.to_ascii_lowercase();
        let l = l.strip_prefix("strict ").unwrap_or(&l);
        let rest = l.strip_prefix("digraph").or_else(|| l.strip_prefix("graph"));
        // Then a name or the brace; not TriG's `GRAPH :g {` or `GRAPH <iri> {`.
        rest.is_some_and(|r| r.starts_with([' ', '\t', '{']) && !r.trim_start().starts_with([':', '<', '[', '_'])) && text.contains('{')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_nodes_edges_and_subgraphs() {
        let cx = ParseContext::new();
        let g = dot("strict digraph G {\n  rankdir=LR; node [shape=box];\n  a [label=\"Start\"];\n  a -> b -> {c d} [color=red];\n  subgraph cluster_x { e; f -> e }\n  \"quoted id\":p1 -- g;\n}\n", &cx).unwrap();
        assert!(g.directed);
        assert_eq!(g.nodes.iter().map(|n| &*n.id).collect::<Vec<_>>(), ["a", "b", "c", "d", "e", "f", "quoted id", "g"]);
        assert_eq!(g.nodes[0].attrs, [("label".to_string(), "Start".to_string())]);
        let name = |n: u32| &*g.nodes[n as usize].id;
        let edges: Vec<(&str, &str)> = g.edges.iter().map(|e| (name(e.from), name(e.to))).collect();
        assert_eq!(edges, [("a", "b"), ("b", "c"), ("b", "d"), ("f", "e"), ("quoted id", "g")]);
        assert_eq!(g.edges[1].attrs, [("color".to_string(), "red".to_string())]);
        assert!(dot("graph { a -- ", &cx).is_ok());
        assert!(dot("hello", &cx).is_err());
    }

    #[test]
    fn graph_modelling_language() {
        let cx = ParseContext::new();
        let t = gml_tree("graph [\n directed 1\n node [ id 1 label \"A\" ]\n node [ id 2 label \"B\" graphics [ x 1.5 ] ]\n edge [ source 1 target 2 ]\n]\n", &cx).unwrap();
        let g = t.roots()[0];
        assert_eq!(t.child_ids(g).len(), 4);
        assert!(gml_tree("graph [ node [ id 1 ]", &cx).is_err());
    }
}
