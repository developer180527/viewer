//! Graphs: Graphviz DOT, GraphML, GEXF, GXL and Graph Modelling Language, each as a
//! table of nodes and a table of edges with their attributes; the XML formats and
//! GML over their tree. (A drawn graph is a view for libviewer to add, on libgui's
//! node graph.)

use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Cell, Column, Content, Document, FileSource, Limit, Links, OwnedTable, ParseContext, ParseError, Place, Position, Result, Role, ValueKind,
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
    text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).is_some_and(|l| l.starts_with("graph") || l.starts_with("Creator") || l.starts_with("Version"))
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

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Id(String),
    Sym(&'static str),
}

/// DOT's tokens and the byte each starts at.
fn dot_tokens(s: &str) -> Result<Vec<(Tok, usize)>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if s[i..].starts_with("//") || (c == b'#' && (i == 0 || b[i - 1] == b'\n')) {
            i = memchr::memchr(b'\n', &b[i..]).map_or(b.len(), |p| i + p);
        } else if s[i..].starts_with("/*") {
            i = memchr::memmem::find(&b[i + 2..], b"*/").map_or(b.len(), |p| i + 2 + p + 2);
        } else if s[i..].starts_with("->") || s[i..].starts_with("--") {
            out.push((Tok::Sym(if c == b'-' && b[i + 1] == b'>' { "->" } else { "--" }), i));
            i += 2;
        } else if let Some(sym) = ["{", "}", "[", "]", "=", ";", ",", ":"].iter().find(|s| c == s.as_bytes()[0]) {
            out.push((Tok::Sym(sym), i));
            i += 1;
        } else if c == b'"' {
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
            out.push((Tok::Id(v), i));
            i = j + 1;
            // `"a" + "b"`: concatenated.
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
            out.push((Tok::Id(s[i + 1..j.min(b.len())].to_string()), i));
            i = j + 1;
        } else {
            let end = s[i..].find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '.' || (ch == '-' && false) || (!ch.is_ascii() && !ch.is_whitespace()))).map_or(b.len(), |p| i + p);
            let end = if end == i {
                // A numeral like `-1.5`.
                s[i..].find(|ch: char| !(ch.is_ascii_digit() || ch == '.' || ch == '-')).map_or(b.len(), |p| i + p).max(i + 1)
            } else {
                end
            };
            out.push((Tok::Id(s[i..end].to_string()), i));
            i = end;
        }
    }
    Ok(out)
}

/// Attributes as written: `(name, value)`.
type Attrs = Vec<(String, String)>;

struct Dot {
    /// Id, attributes, lines.
    nodes: Vec<(String, Attrs, Range<u64>)>,
    /// From, to, attributes, lines.
    edges: Vec<(String, String, Attrs, Range<u64>)>,
    directed: bool,
    name: Option<String>,
}

struct DotParser<'a> {
    toks: Vec<(Tok, usize)>,
    i: usize,
    lines: LineCounter<'a>,
    out: Dot,
    cx: &'a ParseContext,
}

impl DotParser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i).map(|t| &t.0)
    }

    fn sym(&self, s: &str) -> bool {
        self.peek() == Some(&Tok::Sym(match s {
            "{" => "{",
            "}" => "}",
            "[" => "[",
            "]" => "]",
            "=" => "=",
            ";" => ";",
            "," => ",",
            ":" => ":",
            "->" => "->",
            _ => "--",
        }))
    }

    fn id(&mut self) -> Option<String> {
        match self.peek() {
            Some(Tok::Id(v)) => {
                let v = v.clone();
                self.i += 1;
                Some(v)
            }
            _ => None,
        }
    }

    fn line(&mut self) -> u64 {
        let at = self.toks.get(self.i).map_or(0, |t| t.1);
        self.lines.line(at) as u64
    }

    /// `[a=b, c=d][e=f]`.
    fn attrs(&mut self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        while self.sym("[") {
            self.i += 1;
            while !self.sym("]") && self.peek().is_some() {
                let Some(k) = self.id() else {
                    self.i += 1;
                    continue;
                };
                let v = if self.sym("=") {
                    self.i += 1;
                    self.id().unwrap_or_default()
                } else {
                    "true".into()
                };
                out.push((k, v));
                if self.sym(",") || self.sym(";") {
                    self.i += 1;
                }
            }
            self.i += 1;
        }
        out
    }

    fn node(&mut self, id: &str, attrs: Vec<(String, String)>, line: u64) {
        match self.out.nodes.iter_mut().find(|n| n.0 == id) {
            Some(n) => n.1.extend(attrs),
            None => self.out.nodes.push((id.to_string(), attrs, line..line + 1)),
        }
    }

    /// A node id, `id:port`, or a subgraph: the node ids it stands for.
    fn endpoint(&mut self, depth: usize) -> Result<Option<Vec<String>>> {
        if self.sym("{") || matches!(self.peek(), Some(Tok::Id(k)) if k.eq_ignore_ascii_case("subgraph")) {
            return self.subgraph(depth).map(Some);
        }
        let line = self.line();
        let Some(id) = self.id() else { return Ok(None) };
        // Ports: `a:n` or `a:port:n`.
        while self.sym(":") {
            self.i += 1;
            self.id();
        }
        self.node(&id, Vec::new(), line);
        Ok(Some(vec![id]))
    }

    /// `subgraph name? { … }` or `{ … }`: the nodes in it.
    fn subgraph(&mut self, depth: usize) -> Result<Vec<String>> {
        self.cx.limits.check(Limit::Depth, depth as u64)?;
        if matches!(self.peek(), Some(Tok::Id(k)) if k.eq_ignore_ascii_case("subgraph")) {
            self.i += 1;
            if !self.sym("{") {
                self.id();
            }
        }
        if !self.sym("{") {
            return Ok(Vec::new());
        }
        self.i += 1;
        let before = self.out.nodes.len();
        let mentioned = self.statements(depth + 1)?;
        if self.sym("}") {
            self.i += 1;
        }
        let mut ids: Vec<String> = Vec::new();
        for id in mentioned.into_iter().chain(self.out.nodes[before..].iter().map(|n| n.0.clone())) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// Statements to the closing `}`: the node ids they mention.
    fn statements(&mut self, depth: usize) -> Result<Vec<String>> {
        let mut mentioned = Vec::new();
        while self.peek().is_some() && !self.sym("}") {
            if self.sym(";") {
                self.i += 1;
                continue;
            }
            self.cx.limits.check(Limit::Rows, (self.out.nodes.len() + self.out.edges.len()) as u64)?;
            let line = self.line();
            // `graph|node|edge [attrs]`: defaults, not nodes.
            if let Some(Tok::Id(k)) = self.peek()
                && matches!(k.to_ascii_lowercase().as_str(), "graph" | "node" | "edge")
                && self.toks.get(self.i + 1).is_some_and(|t| t.0 == Tok::Sym("["))
            {
                self.i += 1;
                self.attrs();
                continue;
            }
            // `a = b`: a graph attribute.
            if matches!(self.peek(), Some(Tok::Id(_))) && self.toks.get(self.i + 1).is_some_and(|t| t.0 == Tok::Sym("=")) {
                self.i += 2;
                self.id();
                continue;
            }
            let Some(first) = self.endpoint(depth)? else {
                self.i += 1;
                continue;
            };
            mentioned.extend(first.iter().cloned());
            // An edge chain: `a -> b -> {c d} [attrs]`.
            let mut chain = vec![first];
            while self.sym("->") || self.sym("--") {
                self.i += 1;
                match self.endpoint(depth)? {
                    Some(next) => {
                        mentioned.extend(next.iter().cloned());
                        chain.push(next);
                    }
                    None => break,
                }
            }
            let attrs = self.attrs();
            let end = self.line();
            if chain.len() == 1 {
                if let [id] = &chain[0][..] {
                    let id = id.clone();
                    self.node(&id, attrs, line);
                }
            } else {
                for pair in chain.windows(2) {
                    for from in &pair[0] {
                        for to in &pair[1] {
                            self.out.edges.push((from.clone(), to.clone(), attrs.clone(), line..end.max(line) + 1));
                        }
                    }
                }
            }
        }
        Ok(mentioned)
    }
}

fn dot(text: &str, cx: &ParseContext) -> Result<Dot> {
    let toks = dot_tokens(text)?;
    let mut p = DotParser { toks, i: 0, lines: LineCounter::new(text.as_bytes()), out: Dot { nodes: Vec::new(), edges: Vec::new(), directed: false, name: None }, cx };
    // `strict? (graph|digraph) name? {`.
    if matches!(p.peek(), Some(Tok::Id(k)) if k.eq_ignore_ascii_case("strict")) {
        p.i += 1;
    }
    match p.id() {
        Some(k) if k.eq_ignore_ascii_case("digraph") => p.out.directed = true,
        Some(k) if k.eq_ignore_ascii_case("graph") => {}
        _ => return Err(ParseError::corrupt("DOT: expected `graph` or `digraph`")),
    }
    if !p.sym("{") {
        p.out.name = p.id();
    }
    if !p.sym("{") {
        return Err(ParseError::corrupt("DOT: expected `{`"));
    }
    p.i += 1;
    p.statements(1)?;
    Ok(p.out)
}

/// Rows of two tables (facets 0 and 1) and their lines.
struct TwoTables(Vec<Range<u64>>, Vec<Range<u64>>);

impl Links for TwoTables {
    fn links(&self, _: &Document, from: &Place, _: &ParseContext) -> Result<Vec<Place>> {
        Ok(match *from {
            Place::Row { facet: 0, row } => self.0.get(row as usize).map(|l| vec![Place::Lines(l.clone())]).unwrap_or_default(),
            Place::Row { facet: 1, row } => self.1.get(row as usize).map(|l| vec![Place::Lines(l.clone())]).unwrap_or_default(),
            Place::Lines(ref r) => {
                let hit = |rows: &Vec<Range<u64>>, facet: usize| rows.iter().position(|l| l.contains(&r.start)).map(|row| Place::Row { facet, row: row as u64 });
                hit(&self.0, 0).into_iter().chain(hit(&self.1, 1)).collect()
            }
            _ => Vec::new(),
        })
    }
}

pub(crate) fn dot_document(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let text = String::from_utf8_lossy(src.bytes());
    let g = dot(&text, cx)?;
    // Attribute columns: label first, then the rest in the order first seen.
    let keys = |attrs: &mut dyn Iterator<Item = &Vec<(String, String)>>| {
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
    let node_keys = keys(&mut g.nodes.iter().map(|n| &n.1));
    let edge_keys = keys(&mut g.edges.iter().map(|e| &e.2));
    let get = |attrs: &[(String, String)], k: &str| attrs.iter().rev().find(|(a, _)| a == k).map_or(Cell::Null, |(_, v)| Cell::Text(v.clone()));
    let mut ncols = vec![Column::new("ID").with_kind(ValueKind::Text), Column::new("Label").with_kind(ValueKind::Text)];
    ncols.extend(node_keys.iter().map(|k| Column::new(k.as_str()).with_kind(ValueKind::Text)));
    let nrows = g.nodes.iter().map(|(id, a, _)| [Cell::Text(id.clone()), get(a, "label")].into_iter().chain(node_keys.iter().map(|k| get(a, k))).collect()).collect();
    let mut ecols = vec![Column::new("From").with_kind(ValueKind::Text), Column::new("To").with_kind(ValueKind::Text), Column::new("Label").with_kind(ValueKind::Text)];
    ecols.extend(edge_keys.iter().map(|k| Column::new(k.as_str()).with_kind(ValueKind::Text)));
    let erows = g.edges.iter().map(|(f, t, a, _)| [Cell::Text(f.clone()), Cell::Text(t.clone()), get(a, "label")].into_iter().chain(edge_keys.iter().map(|k| get(a, k))).collect()).collect();
    let links = TwoTables(g.nodes.iter().map(|n| n.2.clone()).collect(), g.edges.iter().map(|e| e.3.clone()).collect());
    let mut doc = Document::new(format("dot", "Graphviz DOT"))
        .with_meta("Nodes", g.nodes.len())
        .with_meta("Edges", g.edges.len())
        .with_meta("Directed", if g.directed { "yes" } else { "no" })
        .with_facet(Role::Primary, "Nodes", Content::Table(Arc::new(OwnedTable::new(ncols, nrows))))
        .with_facet(Role::Structure, "Edges", Content::Table(Arc::new(OwnedTable::new(ecols, erows))));
    if let Some(n) = g.name {
        doc = doc.with_meta("Name", n);
    }
    Ok(doc.with_links(links).with_file_facets(src, Some("dot")))
}

pub(crate) fn looks_like_dot(header: &[u8]) -> bool {
    let text = String::from_utf8_lossy(header);
    let first = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#') && !l.starts_with("/*"));
    first.is_some_and(|l| {
        let l = l.to_ascii_lowercase();
        let l = l.strip_prefix("strict ").unwrap_or(&l);
        (l.starts_with("digraph") || l.starts_with("graph")) && text.contains('{')
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
        assert_eq!(g.nodes.iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), ["a", "b", "c", "d", "e", "f", "quoted id", "g"]);
        assert_eq!(g.nodes[0].1, [("label".to_string(), "Start".to_string())]);
        let edges: Vec<(&str, &str)> = g.edges.iter().map(|e| (e.0.as_str(), e.1.as_str())).collect();
        assert_eq!(edges, [("a", "b"), ("b", "c"), ("b", "d"), ("f", "e"), ("quoted id", "g")]);
        assert_eq!(g.edges[1].2, [("color".to_string(), "red".to_string())]);
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
