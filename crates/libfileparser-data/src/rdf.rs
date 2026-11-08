//! RDF: Turtle, TriG, N-Triples, N-Quads, RDF/XML and OWL (in RDF/XML), TriX and
//! JSON-LD, as a table of their statements (subject, predicate, object) over a tree
//! of subjects and what is said about each; IRIs shortened by the file's prefixes.
//! SPARQL queries as a tree of their prefixes, form and patterns.

use std::collections::HashMap;
use std::sync::Arc;

use libfileparser_core::{Cell, Content, Document, FileSource, Limit, OwnedTable, ParseContext, ParseError, Position, Result, Role, ValueKind};
use oxrdf::{GraphName, NamedOrBlankNode, Term};

use crate::json::{self, Dialect};
use crate::lines::{columns, text};
use crate::nodes::{self, NodeTable, child, format, local, named, value};
use crate::tree::{Builder, DataTree, LineCounter};

/// Prefixes most RDF uses, for files that declare none (N-Triples, RDF/XML).
const COMMON: [(&str, &str); 10] = [
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("foaf", "http://xmlns.com/foaf/0.1/"),
    ("schema", "http://schema.org/"),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("dcterms", "http://purl.org/dc/terms/"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("prov", "http://www.w3.org/ns/prov#"),
];

/// IRIs shortened by prefixes: the longest namespace that fits wins.
struct Prefixes(Vec<(String, String)>);

impl Prefixes {
    fn new(declared: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut all: Vec<(String, String)> = declared.into_iter().collect();
        for (p, ns) in COMMON {
            if !all.iter().any(|(_, n)| n == ns) && !all.iter().any(|(q, _)| q == p) {
                all.push((p.to_string(), ns.to_string()));
            }
        }
        all.sort_by_key(|(_, ns)| std::cmp::Reverse(ns.len()));
        Self(all)
    }

    fn iri(&self, iri: &str) -> String {
        for (p, ns) in &self.0 {
            if let Some(local) = iri.strip_prefix(ns.as_str())
                && local.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
                && !local.ends_with('.')
            {
                return format!("{p}:{local}");
            }
        }
        format!("<{iri}>")
    }

    fn subject(&self, s: &NamedOrBlankNode) -> String {
        match s {
            NamedOrBlankNode::NamedNode(n) => self.iri(n.as_str()),
            other => other.to_string(),
        }
    }

    /// An object as text, and as a typed cell: numbers, booleans and dates as such.
    fn object(&self, o: &Term) -> (String, Cell, Option<String>, Option<String>) {
        match o {
            Term::NamedNode(n) => {
                let s = self.iri(n.as_str());
                (s.clone(), Cell::Text(s), None, None)
            }
            Term::Literal(l) => {
                let dt = l.datatype().as_str();
                let local = dt.rsplit('#').next().unwrap_or(dt);
                let v = l.value();
                let cell = match local {
                    "integer" | "decimal" | "double" | "float" | "int" | "long" | "short" | "nonNegativeInteger" | "positiveInteger" => match crate::lines::num(v) {
                        Cell::Null => Cell::Text(v.to_string()),
                        n => n,
                    },
                    "boolean" => Cell::Bool(v == "true" || v == "1"),
                    "date" | "dateTime" | "time" | "gYear" | "dateTimeStamp" => Cell::Date(v.to_string()),
                    _ => Cell::Text(v.to_string()),
                };
                let plain = dt.ends_with("#string") || dt.ends_with("#langString");
                (v.to_string(), cell, (!plain).then(|| self.iri(dt)), l.language().map(str::to_owned))
            }
            other => {
                let s = other.to_string();
                (s.clone(), Cell::Text(s), None, None)
            }
        }
    }
}

/// One statement: subject, predicate, object (text, cell, datatype, language), graph.
struct Quad {
    subject: String,
    predicate: String,
    object: (String, Cell, Option<String>, Option<String>),
    graph: Option<String>,
}

fn to_quad(p: &Prefixes, s: &NamedOrBlankNode, pred: &str, o: &Term, g: &GraphName) -> Quad {
    Quad {
        subject: p.subject(s),
        predicate: p.iri(pred),
        object: p.object(o),
        graph: match g {
            GraphName::DefaultGraph => None,
            GraphName::NamedNode(n) => Some(p.iri(n.as_str())),
            GraphName::BlankNode(b) => Some(b.to_string()),
        },
    }
}

fn syntax_error(format: &str, e: &oxttl::TurtleSyntaxError) -> ParseError {
    let at = e.location().start;
    ParseError::corrupt_at(format!("{format}: {}", e.message()), Position::line_column(at.line + 1, at.column + 1))
}

/// The base IRI a file's relative IRIs resolve against: the file itself.
fn base_iri(src: &FileSource) -> String {
    let name: String = src.name().chars().map(|c| if c.is_ascii_alphanumeric() || "-._~".contains(c) { c.to_string() } else { format!("%{:02X}", c as u32 & 0xff) }).collect();
    format!("file:///{name}")
}

/// The statements of a Turtle-family file: Turtle and TriG (`trig`), N-Triples and
/// N-Quads (`nquads`); and the prefixes it declared.
fn turtle_family(src: &FileSource, quads_syntax: bool, cx: &ParseContext) -> Result<(Vec<Quad>, Prefixes)> {
    let mut out = Vec::new();
    let mut raw = Vec::new();
    let name = if quads_syntax { "N-Quads" } else { "Turtle" };
    if quads_syntax {
        for q in oxttl::NQuadsParser::new().for_slice(src.bytes()) {
            let q = q.map_err(|e| syntax_error(name, &e))?;
            raw.push(q);
            if raw.len() % 65536 == 0 {
                cx.cancel.check()?;
                cx.limits.check(Limit::Rows, raw.len() as u64)?;
            }
            cx.limits.check(Limit::Rows, raw.len() as u64)?;
        }
        let p = Prefixes::new([]);
        out.extend(raw.iter().map(|q| to_quad(&p, &q.subject, q.predicate.as_str(), &q.object, &q.graph_name)));
        return Ok((out, p));
    }
    // Relative IRIs (`<#me>`) resolve against the file, as RDF tools do.
    let parser = oxttl::TriGParser::new();
    let parser = parser.clone().with_base_iri(base_iri(src)).unwrap_or(parser);
    let mut parser = parser.for_slice(src.bytes());
    for q in parser.by_ref() {
        let q = q.map_err(|e| syntax_error(name, &e))?;
        raw.push(q);
        if raw.len() % 65536 == 0 {
            cx.cancel.check()?;
        }
        cx.limits.check(Limit::Rows, raw.len() as u64)?;
    }
    let p = Prefixes::new(parser.prefixes().map(|(k, v)| (k.to_string(), v.to_string())));
    out.extend(raw.iter().map(|q| to_quad(&p, &q.subject, q.predicate.as_str(), &q.object, &q.graph_name)));
    Ok((out, p))
}

/// RDF/XML's statements; prefixes from its `xmlns:` declarations.
fn rdf_xml(src: &FileSource, cx: &ParseContext) -> Result<Vec<Quad>> {
    let head = String::from_utf8_lossy(src.header());
    let declared = head.split("xmlns:").skip(1).filter_map(|d| {
        let (p, rest) = d.split_once('=')?;
        let rest = rest.trim_start();
        let q = rest.chars().next()?;
        let ns = rest[q.len_utf8()..].split(q).next()?;
        Some((p.trim().to_string(), ns.to_string()))
    });
    let p = Prefixes::new(declared);
    let mut out = Vec::new();
    let parser = oxrdfxml::RdfXmlParser::new();
    let parser = parser.clone().with_base_iri(base_iri(src)).unwrap_or(parser);
    for t in parser.for_slice(src.bytes()) {
        let t = t.map_err(|e| ParseError::corrupt(format!("RDF/XML: {e}")))?;
        out.push(to_quad(&p, &t.subject, t.predicate.as_str(), &t.object, &GraphName::DefaultGraph));
        if out.len() % 65536 == 0 {
            cx.cancel.check()?;
        }
        cx.limits.check(Limit::Rows, out.len() as u64)?;
    }
    Ok(out)
}

/// A document of statements: the table, a tree of subjects, the file's facets.
fn statements_document(id: &'static str, name: &str, quads: Vec<Quad>, src: &FileSource, language: &str, cx: &ParseContext) -> Result<Document> {
    let graphs = quads.iter().any(|q| q.graph.is_some());
    // The tree: each subject, what is said about it.
    let mut b = Builder::new(cx);
    let mut subjects: HashMap<&str, u32> = HashMap::new();
    for q in &quads {
        let s = match subjects.get(q.subject.as_str()) {
            Some(&s) => s,
            None => {
                let s = b.add(None, 1, q.subject.clone(), "", ValueKind::Group, 0..0)?;
                subjects.insert(&q.subject, s);
                s
            }
        };
        let kind = q.object.1.kind();
        b.add(Some(s), 2, q.predicate.clone(), q.object.0.clone(), if kind == ValueKind::Null { ValueKind::Text } else { kind }, 0..0)?;
    }
    for &s in subjects.values() {
        let n = b.children(s);
        b.set_value(s, format!("{n} statements"));
    }
    let tree = b.finish()?;
    let n = quads.len();
    let subject_count = subjects.len();
    let mut cols = vec![("Subject", ValueKind::Text), ("Predicate", ValueKind::Text), ("Object", ValueKind::Mixed), ("Datatype", ValueKind::Text), ("Language", ValueKind::Text)];
    if graphs {
        cols.push(("Graph", ValueKind::Text));
    }
    let rows = quads
        .into_iter()
        .map(|q| {
            let mut row = vec![Cell::Text(q.subject), Cell::Text(q.predicate), q.object.1, q.object.2.map_or(Cell::Null, Cell::Text), q.object.3.map_or(Cell::Null, Cell::Text)];
            if graphs {
                row.push(q.graph.map_or(Cell::Null, Cell::Text));
            }
            row
        })
        .collect();
    let table = OwnedTable::new(columns(&cols), rows);
    Ok(Document::new(format(id, name))
        .with_facet(Role::Primary, "Statements", Content::Table(Arc::new(table)))
        .with_facet(Role::Structure, "Subjects", Content::Tree(Arc::new(tree)))
        .with_meta("Statements", n)
        .with_meta("Subjects", subject_count)
        .with_file_facets(src, Some(language)))
}

pub(crate) fn turtle(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let ext = src.ext().unwrap_or_default();
    let quads_syntax = matches!(ext.as_str(), "nt" | "nq" | "ntriples" | "nquads");
    let (quads, _) = turtle_family(src, quads_syntax, cx)?;
    let name = match ext.as_str() {
        "nt" | "ntriples" => "N-Triples",
        "nq" | "nquads" => "N-Quads",
        "trig" => "TriG",
        _ => "Turtle",
    };
    statements_document("turtle", name, quads, src, "turtle", cx)
}

pub(crate) fn rdfxml(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let quads = rdf_xml(src, cx)?;
    let name = if src.ext().as_deref() == Some("owl") { "OWL (RDF/XML)" } else { "RDF/XML" };
    statements_document("rdfxml", name, quads, src, "xml", cx)
}


// ---------------------------------------------------------------- TriX

pub(crate) fn trix(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let p = Prefixes::new([]);
    let term = |t: &DataTree, n: u32| -> (String, Cell, Option<String>, Option<String>) {
        let v = t.value(n).to_string();
        match local(t.label(n)) {
            "uri" => {
                let s = p.iri(&v);
                (s.clone(), Cell::Text(s), None, None)
            }
            "id" => (format!("_:{v}"), Cell::Text(format!("_:{v}")), None, None),
            "typedLiteral" => {
                let dt = child(t, n, "@datatype").map(|d| p.iri(t.value(d)));
                (v.clone(), Cell::Text(v), dt, None)
            }
            _ => {
                let lang = t.child(n, "@xml:lang").map(|l| t.value(l).to_string());
                (v.clone(), Cell::Text(v), None, lang)
            }
        }
    };
    let mut quads = Vec::new();
    for g in named(&tree, tree.roots().first().copied().unwrap_or(0), "graph") {
        let graph = tree.child_ids(g).iter().copied().find(|&c| local(tree.label(c)) == "uri").map(|u| p.iri(tree.value(u)));
        for tr in tree.child_ids(g).iter().copied().filter(|&c| local(tree.label(c)) == "triple") {
            let parts: Vec<u32> = tree.child_ids(tr).iter().copied().filter(|&c| !tree.label(c).starts_with(['@', '#'])).collect();
            if parts.len() == 3 {
                quads.push(Quad { subject: term(&tree, parts[0]).0, predicate: term(&tree, parts[1]).0, object: term(&tree, parts[2]), graph: graph.clone() });
            }
        }
    }
    statements_document("trix", "TriX", quads, src, "xml", cx)
}

// ---------------------------------------------------------------- JSON-LD

pub(crate) fn jsonld(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = json::parse(src.bytes(), Dialect::Json, cx)?;
    // Every object with an `@id` or `@type`: the nodes it describes (not expanded:
    // terms are shown as the file writes them).
    let rows: Vec<u32> = tree.all().into_iter().filter(|&n| tree.kind(n) == ValueKind::Map && (tree.child(n, "@id").is_some() || tree.child(n, "@type").is_some())).collect();
    let tree = Arc::new(tree);
    let table = NodeTable::new(tree, rows.into())
        .column("ID", ValueKind::Text, |t, row| value(t, t.child(row, "@id")))
        .column("Type", ValueKind::Text, |t, row| match t.child(row, "@type") {
            Some(ty) if t.kind(ty) == ValueKind::List => text(&t.child_ids(ty).iter().map(|&c| t.value(c)).collect::<Vec<_>>().join(", ")),
            ty => value(t, ty),
        })
        .column("Properties", ValueKind::Number, |t, row| Cell::Int(t.child_ids(row).iter().filter(|&&c| !t.label(c).starts_with('@')).count() as i64))
        .column("Name", ValueKind::Text, |t, row| ["name", "schema:name", "rdfs:label", "label", "title"].iter().map(|k| value(t, t.child(row, k))).find(|c| !c.is_null()).unwrap_or(Cell::Null));
    Ok(nodes::document(format("jsonld", "JSON-LD"), "Nodes", table, src, Some("json"), true))
}

// ---------------------------------------------------------------- SPARQL

/// A SPARQL query or update as a tree: its prefixes, its form (`SELECT ?a ?b`), the
/// patterns of its groups (`{ … }`), nested, and what follows (`ORDER BY`, `LIMIT`).
pub(crate) fn sparql(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let text = String::from_utf8_lossy(src.bytes());
    let s = text.as_bytes();
    let mut b = Builder::new(cx);
    let mut lines = LineCounter::new(s);
    let mut stack: Vec<u32> = Vec::new();
    let mut stmt = String::new();
    let mut start: Option<usize> = None;
    let mut i = 0;
    let flush = |b: &mut Builder, lines: &mut LineCounter, stack: &[u32], stmt: &mut String, start: &mut Option<usize>, end: usize| -> Result<()> {
        let t = stmt.split_whitespace().collect::<Vec<_>>().join(" ");
        stmt.clear();
        if let Some(st) = start.take()
            && !t.is_empty()
        {
            let (a, z) = (lines.line(st), lines.line(end.saturating_sub(1).max(st)));
            let up = t.to_ascii_uppercase();
            let label = if up.starts_with("PREFIX") || up.starts_with("BASE") || up.starts_with("SELECT") || up.starts_with("CONSTRUCT") || up.starts_with("ASK") || up.starts_with("DESCRIBE") {
                t.split_whitespace().next().unwrap_or("").to_ascii_uppercase()
            } else if stack.is_empty() {
                "modifier".to_string()
            } else {
                "pattern".to_string()
            };
            b.add(stack.last().copied(), stack.len() + 1, label, t, ValueKind::Text, a..z + 1)?;
        }
        Ok(())
    };
    while i < s.len() {
        let c = s[i];
        match c {
            b'#' => {
                i = memchr::memchr(b'\n', &s[i..]).map_or(s.len(), |p| i + p);
                continue;
            }
            b'<' if s[i..].iter().take(200).position(|&x| x == b'>').is_some_and(|p| !s[i..i + p].iter().any(|x| x.is_ascii_whitespace())) => {
                let end = i + s[i..].iter().position(|&x| x == b'>').unwrap_or(0) + 1;
                start.get_or_insert(i);
                stmt.push_str(&text[i..end]);
                i = end;
                continue;
            }
            b'"' | b'\'' => {
                let mut j = i + 1;
                while j < s.len() && s[j] != c {
                    j += if s[j] == b'\\' { 2 } else { 1 };
                }
                let end = (j + 1).min(s.len());
                start.get_or_insert(i);
                stmt.push_str(&text[i..end]);
                i = end;
                continue;
            }
            b'{' => {
                // What came before the brace (`WHERE`, `OPTIONAL`, `SELECT ?x WHERE`).
                let header = stmt.split_whitespace().collect::<Vec<_>>().join(" ");
                stmt.clear();
                let at = start.take().unwrap_or(i);
                let line = lines.line(at);
                let up = header.to_ascii_uppercase();
                // `SELECT ?x WHERE {`: the form, then the group.
                let (form, group) = match up.find("WHERE") {
                    Some(w) if w > 0 => (Some(header[..w].trim().to_string()), header[w..].to_string()),
                    _ => (None, header.clone()),
                };
                if let Some(f) = form {
                    let label = f.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
                    b.add(stack.last().copied(), stack.len() + 1, label, f, ValueKind::Text, line..line + 1)?;
                }
                let label = if group.is_empty() { "{ }".to_string() } else { group.split_whitespace().next().unwrap_or("").to_ascii_uppercase() };
                let id = b.add(stack.last().copied(), stack.len() + 1, label, group, ValueKind::Group, line..line + 1)?;
                stack.push(id);
            }
            b'}' => {
                flush(&mut b, &mut lines, &stack, &mut stmt, &mut start, i)?;
                if let Some(id) = stack.pop() {
                    let line = lines.line(i);
                    b.end(id, line);
                }
            }
            // A pattern ends with ` .` (not a decimal point).
            b'.' if !stack.is_empty() && !(i > 0 && s[i - 1].is_ascii_digit() && s.get(i + 1).is_some_and(u8::is_ascii_digit)) => {
                flush(&mut b, &mut lines, &stack, &mut stmt, &mut start, i)?;
            }
            b'\n' if stack.is_empty() => flush(&mut b, &mut lines, &stack, &mut stmt, &mut start, i)?,
            c if c.is_ascii_whitespace() => stmt.push(' '),
            c => {
                start.get_or_insert(i);
                stmt.push(c as char);
            }
        }
        i += 1;
    }
    flush(&mut b, &mut lines, &stack, &mut stmt, &mut start, s.len())?;
    let tree = b.finish()?;
    if tree.roots().is_empty() {
        return Err(ParseError::corrupt("SPARQL: empty"));
    }
    Ok(crate::json::tree_document(format("sparql", "SPARQL query"), tree, src, "sparql"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_shorten_iris() {
        let p = Prefixes::new([("ex".to_string(), "http://example.com/".to_string())]);
        assert_eq!(p.iri("http://example.com/Ann"), "ex:Ann");
        assert_eq!(p.iri("http://www.w3.org/2001/XMLSchema#integer"), "xsd:integer");
        assert_eq!(p.iri("http://other.org/a b"), "<http://other.org/a b>");
        assert_eq!(p.iri("http://example.com/a/b"), "<http://example.com/a/b>");
    }
}
