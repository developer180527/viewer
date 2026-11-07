//! Schemas and interface definitions: the brace-block languages (Protocol Buffers,
//! Thrift, FlatBuffers, Cap'n Proto, Smithy, GraphQL, Prisma, DBML) as a table of
//! their types over a tree of their declarations; Avro schemas as a table of fields;
//! OpenAPI and AsyncAPI as a table of operations; XSD as a table of its top-level
//! definitions.

use std::sync::Arc;

use libfileparser_core::{Cell, Document, FileSource, ParseContext, ParseError, Result, ValueKind};

use crate::blocks::{self, Lang, Name};
use crate::json::{self, Dialect};
use crate::nodes::{self, NodeTable, child, format, local, value};
use crate::tree::{DataTree, TOP};

/// The block languages, by reader id: their name, how they're written, and the
/// language their text is highlighted as.
pub(crate) fn lang(id: &str) -> (&'static str, Lang, &'static str) {
    let l = |line_comments, newline_ends, comma_ends, name| Lang { line_comments, newline_ends, comma_ends, name };
    match id {
        "protobuf" => ("Protocol Buffers", l(&["//"], false, false, Name::BeforeEquals), "protobuf"),
        "thrift" => ("Thrift", l(&["//", "#"], false, true, Name::BeforeEquals), "thrift"),
        "flatbuffers" => ("FlatBuffers schema", l(&["//"], false, false, Name::BeforeColon), "flatbuffers"),
        "capnp" => ("Cap'n Proto schema", l(&["#"], false, false, Name::First), "capnp"),
        "smithy" => ("Smithy model", l(&["//"], true, true, Name::BeforeColon), "smithy"),
        "graphql" => ("GraphQL schema", l(&["#"], true, true, Name::BeforeColon), "graphql"),
        "prisma" => ("Prisma schema", l(&["//"], true, false, Name::First), "prisma"),
        _ => ("DBML", l(&["//"], true, false, Name::First), "dbml"),
    }
}

/// A block language's document: Types, Tree, then the file's facets.
pub(crate) fn block_document(id: &'static str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let (name, lang, language) = lang(id);
    let text = std::str::from_utf8(src.bytes()).map_err(|_| ParseError::corrupt(format!("{name}: not UTF-8 text")))?;
    let parsed = blocks::parse(text.strip_prefix('\u{feff}').unwrap_or(text), &lang, cx)?;
    Ok(nodes::document(format(id, name), "Types", blocks::types_table(parsed), src, Some(language), true))
}

// ---------------------------------------------------------------- Avro

/// An Avro type as one line: `string`, `null | string`, `array<Item>`, a record's name.
fn avro_type(t: &DataTree, id: u32) -> String {
    match t.kind(id) {
        ValueKind::List => t.child_ids(id).iter().map(|&c| avro_type(t, c)).collect::<Vec<_>>().join(" | "),
        ValueKind::Map => {
            let get = |k: &str| t.child(id, k);
            let kind = get("type").map(|k| if t.kind(k) == ValueKind::Text { t.value(k).to_string() } else { avro_type(t, k) }).unwrap_or_default();
            match kind.as_str() {
                "array" => format!("array<{}>", get("items").map(|i| avro_type(t, i)).unwrap_or_default()),
                "map" => format!("map<{}>", get("values").map(|v| avro_type(t, v)).unwrap_or_default()),
                "record" | "enum" | "fixed" | "error" => get("name").map(|n| t.value(n).to_string()).unwrap_or(kind),
                _ => match get("logicalType") {
                    Some(l) => format!("{kind} ({})", t.value(l)),
                    None => kind,
                },
            }
        }
        _ => t.value(id).to_string(),
    }
}

pub(crate) fn avro(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = json::parse(src.bytes(), Dialect::Json, cx)?;
    // Every field of every record, nested ones too, in order.
    let fields: Vec<u32> = tree
        .all()
        .into_iter()
        .filter(|&n| tree.label(n) == "fields" && tree.kind(n) == ValueKind::List)
        .flat_map(|f| tree.child_ids(f).to_vec())
        .collect();
    let tree = Arc::new(tree);
    let record = |t: &DataTree, row: u32| -> Cell {
        // The record a field is in: its `fields` list's parent object, or the top.
        let r = t.parent(row).and_then(|f| t.parent(f));
        let name = match r {
            Some(r) => t.child(r, "name"),
            None => t.child(TOP, "name"),
        };
        let ns = r.and_then(|r| t.child(r, "namespace")).or_else(|| t.child(TOP, "namespace"));
        match (name, ns) {
            (Some(n), Some(ns)) => Cell::Text(format!("{}.{}", t.value(ns), t.value(n))),
            (Some(n), None) => Cell::Text(t.value(n).to_string()),
            _ => Cell::Null,
        }
    };
    let table = NodeTable::new(tree, fields.into())
        .column("Record", ValueKind::Text, record)
        .column("Field", ValueKind::Text, |t, row| value(t, t.child(row, "name")))
        .column("Type", ValueKind::Text, |t, row| t.child(row, "type").map_or(Cell::Null, |ty| Cell::Text(avro_type(t, ty))))
        .column("Default", ValueKind::Mixed, |t, row| match t.child(row, "default") {
            Some(d) => t.cell(d).unwrap_or_else(|| Cell::Text(t.value(d).to_string())),
            None => Cell::Null,
        })
        .column("Doc", ValueKind::Text, |t, row| value(t, t.child(row, "doc")));
    Ok(nodes::document(format("avro", "Avro schema"), "Fields", table, src, Some("json"), true))
}

// ---------------------------------------------------------------- OpenAPI, AsyncAPI

/// Whether a YAML or JSON file is an API description: its top-level `openapi`,
/// `swagger` or `asyncapi` key.
pub(crate) fn looks_like_api(header: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&header[..header.len().min(2048)]);
    text.lines().take(40).any(|l| {
        let l = l.trim_start_matches(['{', ' ', '\t']).trim_start_matches('"');
        ["openapi", "swagger", "asyncapi"].iter().any(|k| l.starts_with(k) && l[k.len()..].trim_start_matches('"').trim_start().starts_with(':'))
    })
}

const METHODS: [&str; 8] = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];

pub(crate) fn api(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let json = src.header().iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{');
    let tree = if json {
        json::parse(src.bytes(), Dialect::Json, cx)?
    } else {
        crate::yaml::parse(std::str::from_utf8(src.bytes()).map_err(|_| ParseError::corrupt("API description: not UTF-8"))?, cx)?
    };
    let spec = ["openapi", "swagger", "asyncapi"].iter().find_map(|k| tree.child(TOP, k).map(|v| (*k, tree.value(v).to_string())));
    let Some((kind, version)) = spec else { return Err(ParseError::NotThisFormat) };
    let async_api = kind == "asyncapi";
    let rows: Vec<u32> = if async_api {
        // 2.x: channels/<name>/{publish,subscribe}; 3.x: operations/<id>.
        let mut ops: Vec<u32> = tree.child(TOP, "channels").map(|c| tree.child_ids(c).iter().flat_map(|&ch| tree.child_ids(ch).iter().copied().filter(|&o| matches!(tree.label(o), "publish" | "subscribe")).collect::<Vec<_>>()).collect()).unwrap_or_default();
        ops.extend(tree.child(TOP, "operations").map(|o| tree.child_ids(o).to_vec()).unwrap_or_default());
        ops
    } else {
        tree.child(TOP, "paths").map(|p| tree.child_ids(p).iter().flat_map(|&path| tree.child_ids(path).iter().copied().filter(|&m| METHODS.contains(&tree.label(m))).collect::<Vec<_>>()).collect()).unwrap_or_default()
    };
    let info = tree.child(TOP, "info");
    let title = info.and_then(|i| tree.child(i, "title")).map(|t| tree.value(t).to_string());
    let api_version = info.and_then(|i| tree.child(i, "version")).map(|t| tree.value(t).to_string());
    let tree = Arc::new(tree);
    let joined = |t: &DataTree, id: Option<u32>| -> Cell {
        let names: Vec<String> = id.map(|i| t.child_ids(i).iter().map(|&c| if t.kind(c) == ValueKind::Text { t.value(c).to_string() } else { t.label(c).to_string() }).collect()).unwrap_or_default();
        if names.is_empty() { Cell::Null } else { Cell::Text(names.join(", ")) }
    };
    let table = NodeTable::new(tree, rows.into())
        .column(if async_api { "Action" } else { "Method" }, ValueKind::Text, move |t, row| {
            if async_api && t.parent(row).is_some_and(|p| t.label(p) == "operations") {
                return value(t, t.child(row, "action"));
            }
            Cell::Text(if async_api { t.label(row).to_string() } else { t.label(row).to_ascii_uppercase() })
        })
        .column(if async_api { "Channel" } else { "Path" }, ValueKind::Text, move |t, row| {
            if async_api && t.parent(row).is_some_and(|p| t.label(p) == "operations") {
                // 3.x: `channel: {$ref: '#/channels/name'}`.
                return t.child(row, "channel").and_then(|c| t.child(c, "$ref")).map_or(Cell::Null, |r| Cell::Text(t.value(r).rsplit('/').next().unwrap_or("").to_string()));
            }
            t.parent(row).map_or(Cell::Null, |p| Cell::Text(t.label(p).to_string()))
        })
        .column("Operation ID", ValueKind::Text, |t, row| value(t, t.child(row, "operationId")))
        .column("Summary", ValueKind::Text, |t, row| match value(t, t.child(row, "summary")) {
            Cell::Null => value(t, t.child(row, "description")),
            c => c,
        })
        .column("Tags", ValueKind::Text, move |t, row| {
            // OpenAPI tags are names; AsyncAPI's are objects with a name.
            let tags = t.child(row, "tags");
            let names: Vec<String> = tags.map(|g| t.child_ids(g).iter().map(|&c| t.child(c, "name").map_or_else(|| t.value(c).to_string(), |n| t.value(n).to_string())).collect()).unwrap_or_default();
            if names.is_empty() { Cell::Null } else { Cell::Text(names.join(", ")) }
        })
        .column("Responses", ValueKind::Text, move |t, row| joined(t, t.child(row, "responses")));
    let name = match kind {
        "asyncapi" => format!("AsyncAPI {version}"),
        "swagger" => format!("Swagger {version}"),
        _ => format!("OpenAPI {version}"),
    };
    let mut doc = nodes::document(format("openapi", &name), "Operations", table, src, Some(if json { "json" } else { "yaml" }), true);
    if let Some(t) = title {
        doc = doc.with_meta("Title", t);
    }
    if let Some(v) = api_version {
        doc = doc.with_meta("API version", v);
    }
    Ok(doc)
}

// ---------------------------------------------------------------- XSD

const XSD_DEFINITIONS: [&str; 7] = ["element", "complexType", "simpleType", "attribute", "group", "attributeGroup", "notation"];

pub(crate) fn xsd(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().iter().copied().find(|&r| local(tree.label(r)) == "schema").ok_or_else(|| ParseError::corrupt("XSD: no <schema>"))?;
    let target = child(&tree, root, "@targetNamespace").map(|n| tree.value(n).to_string());
    let defs: Vec<u32> = tree.child_ids(root).iter().copied().filter(|&c| XSD_DEFINITIONS.contains(&local(tree.label(c)))).collect();
    let tree = Arc::new(tree);
    let table = NodeTable::new(tree, defs.into())
        .column("Kind", ValueKind::Text, |t, row| Cell::Text(local(t.label(row)).to_string()))
        .column("Name", ValueKind::Text, |t, row| value(t, t.child(row, "@name")))
        .column("Type", ValueKind::Text, |t, row| {
            // `type="…"`, or what a restriction or extension builds on.
            if let Some(ty) = t.child(row, "@type") {
                return value(t, Some(ty));
            }
            t.subtree(row).into_iter().find(|&n| matches!(local(t.label(n)), "restriction" | "extension")).and_then(|n| t.child(n, "@base")).map_or(Cell::Null, |b| value(t, Some(b)))
        })
        .column("Documentation", ValueKind::Text, |t, row| {
            t.subtree(row).into_iter().find(|&n| local(t.label(n)) == "documentation").map_or(Cell::Null, |d| value(t, Some(d)))
        });
    let mut doc = nodes::document(format("xsd", "XML Schema"), "Definitions", table, src, Some("xml"), true);
    if let Some(t) = target {
        doc = doc.with_meta("Target namespace", t);
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_descriptions_are_recognised() {
        assert!(looks_like_api(b"openapi: 3.1.0\ninfo:\n  title: x\n"));
        assert!(looks_like_api(b"{\n  \"swagger\": \"2.0\",\n"));
        assert!(looks_like_api(b"# API\nasyncapi: '2.6.0'\n"));
        assert!(!looks_like_api(b"name: app\nversion: 1\n"));
    }
}
