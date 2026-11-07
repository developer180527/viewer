//! Schema languages written as brace blocks of declarations: Protocol Buffers,
//! Thrift, FlatBuffers, Cap'n Proto, Smithy, GraphQL, Prisma and DBML. One parser,
//! told per language how comments look, what separates declarations and where a
//! declaration's name is. Blocks become nodes (`message Person`), their declarations
//! children named by what they declare (`email`), valued by the declaration as
//! written; a table lists the blocks: the types.

use std::sync::Arc;

use libfileparser_core::{Cell, ParseContext, ParseError, Position, Result, ValueKind};

use crate::nodes::{NodeTable, value};
use crate::tree::{Builder, DataTree, LineCounter};

/// Where a declaration's name is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Name {
    /// The word before `=` (`repeated string email = 3`), before `(` (`rpc Get(…)`,
    /// `void ping(…)`), after a leading keyword (`option`, `reserved`), else the last
    /// word. Protocol Buffers, Thrift.
    BeforeEquals,
    /// The word before the first `:`, `(`, `=` or space. FlatBuffers, GraphQL, Smithy.
    BeforeColon,
    /// The first word. Cap'n Proto, Prisma, DBML.
    First,
}

pub(crate) struct Lang {
    /// `//`, `#`: comments to the end of the line. `/* */` is always a comment.
    pub line_comments: &'static [&'static str],
    /// Whether a line break ends a declaration (GraphQL, Prisma, DBML, Smithy).
    pub newline_ends: bool,
    /// Whether `,` ends a declaration (Thrift, GraphQL, Smithy).
    pub comma_ends: bool,
    pub name: Name,
}

/// What a parse found: the tree, and the block nodes (the types).
pub(crate) struct Blocks {
    pub tree: DataTree,
    pub blocks: Vec<u32>,
}

/// Parses `text` as `lang`.
pub(crate) fn parse(text: &str, lang: &Lang, cx: &ParseContext) -> Result<Blocks> {
    let s = text.as_bytes();
    let mut b = Builder::new(cx);
    let mut lines = LineCounter::new(s);
    let mut blocks = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    let mut stmt = String::new();
    let mut stmt_start: Option<usize> = None;
    // Parentheses and brackets: separators inside them don't end a declaration.
    let mut nesting = 0usize;
    let mut i = 0;
    let flush = |b: &mut Builder, lines: &mut LineCounter, stack: &[u32], stmt: &mut String, start: &mut Option<usize>, end: usize| -> Result<()> {
        let text = squash(stmt);
        stmt.clear();
        if let Some(st) = start.take()
            && !text.is_empty()
        {
            let (first, last) = (lines.line(st), lines.line(end.saturating_sub(1).max(st)));
            let label = member_name(&text, lang.name);
            b.add(stack.last().copied(), stack.len() + 1, label, text, ValueKind::Text, first..last + 1)?;
        }
        Ok(())
    };
    while i < s.len() {
        let c = s[i];
        // Comments.
        if lang.line_comments.iter().any(|lc| s[i..].starts_with(lc.as_bytes())) {
            i = memchr::memchr(b'\n', &s[i..]).map_or(s.len(), |p| i + p);
            continue;
        }
        if s[i..].starts_with(b"/*") {
            i = memchr::memmem::find(&s[i + 2..], b"*/").map_or(s.len(), |p| i + 2 + p + 2);
            continue;
        }
        // Strings, copied whole: `"…"`, `'…'`, GraphQL's `"""…"""`.
        if c == b'"' || c == b'\'' {
            let end = if s[i..].starts_with(b"\"\"\"") {
                memchr::memmem::find(&s[i + 3..], b"\"\"\"").map_or(s.len(), |p| i + 3 + p + 3)
            } else {
                let mut j = i + 1;
                while j < s.len() && s[j] != c && s[j] != b'\n' {
                    j += if s[j] == b'\\' { 2 } else { 1 };
                }
                (j + 1).min(s.len())
            };
            stmt_start.get_or_insert(i);
            stmt.push_str(&String::from_utf8_lossy(&s[i..end]));
            i = end;
            continue;
        }
        match c {
            b'(' | b'[' | b'<' => {
                nesting += 1;
                stmt_start.get_or_insert(i);
                stmt.push(c as char);
            }
            b')' | b']' | b'>' => {
                nesting = nesting.saturating_sub(1);
                stmt.push(c as char);
            }
            b'{' if nesting == 0 => {
                // A block: what came before it is its header.
                let header = squash(&stmt);
                stmt.clear();
                let start = stmt_start.take().unwrap_or(i);
                let line = lines.line(start);
                let (label, kind) = block_header(&header);
                let id = b.add(stack.last().copied(), stack.len() + 1, label, kind, ValueKind::Group, line..line + 1)?;
                blocks.push(id);
                stack.push(id);
            }
            b'}' if nesting == 0 => {
                flush(&mut b, &mut lines, &stack, &mut stmt, &mut stmt_start, i)?;
                match stack.pop() {
                    Some(id) => {
                        let line = lines.line(i);
                        b.end(id, line);
                    }
                    None => return Err(ParseError::corrupt_at("a `}` without a `{`", Position::in_text(s, i))),
                }
            }
            b';' if nesting == 0 => flush(&mut b, &mut lines, &stack, &mut stmt, &mut stmt_start, i)?,
            b',' if nesting == 0 && lang.comma_ends => flush(&mut b, &mut lines, &stack, &mut stmt, &mut stmt_start, i)?,
            b'\n' if nesting == 0 && lang.newline_ends => flush(&mut b, &mut lines, &stack, &mut stmt, &mut stmt_start, i)?,
            c if c.is_ascii_whitespace() => stmt.push(' '),
            _ => {
                stmt_start.get_or_insert(i);
                // A run of plain bytes, at once.
                let end = s[i..].iter().position(|&x| matches!(x, b'(' | b'[' | b'<' | b')' | b']' | b'>' | b'{' | b'}' | b';' | b',' | b'"' | b'\'' | b'/' | b'#') || x.is_ascii_whitespace()).map_or(s.len(), |p| i + p).max(i + 1);
                stmt.push_str(&String::from_utf8_lossy(&s[i..end]));
                i = end;
                continue;
            }
        }
        i += 1;
    }
    flush(&mut b, &mut lines, &stack, &mut stmt, &mut stmt_start, s.len())?;
    if !stack.is_empty() {
        return Err(ParseError::corrupt("a block that never closes"));
    }
    Ok(Blocks { tree: b.finish()?, blocks })
}

/// Runs of white space as one space, trimmed.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A block's header (`message Person`, `type Query implements Node`, `Table users as U`):
/// its label (the name) and value (the kind, and anything after the name).
fn block_header(header: &str) -> (String, String) {
    let header = header.trim_end_matches(['=', ':']).trim();
    let mut words = header.split_whitespace();
    match (words.next(), words.next()) {
        (Some(kind), Some(name)) => {
            let name = name.trim_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '"' | '@')));
            let rest: Vec<&str> = words.collect();
            let value = if rest.is_empty() { kind.to_string() } else { format!("{kind} {}", rest.join(" ")) };
            (name.trim_matches('"').to_string(), value)
        }
        (Some(only), None) => (only.to_string(), String::new()),
        _ => ("(block)".to_string(), String::new()),
    }
}

const KEYWORDS: [&str; 10] = ["option", "reserved", "extensions", "import", "package", "syntax", "edition", "namespace", "include", "rpc"];

/// What a declaration declares.
fn member_name(decl: &str, rule: Name) -> String {
    let word = |s: &str| s.trim().rsplit(|c: char| c.is_whitespace()).next().unwrap_or("").trim_matches(['*', '&', ':']).to_string();
    let first = decl.split_whitespace().next().unwrap_or("");
    match rule {
        Name::BeforeEquals => {
            // Thrift's field ids, `1:`.
            let d = match decl.split_once(':') {
                Some((id, rest)) if id.trim().bytes().all(|c| c.is_ascii_digit() || c == b'-') && !id.trim().is_empty() => rest.trim(),
                _ => decl,
            };
            let first = d.split_whitespace().next().unwrap_or("");
            // `syntax = "proto3"`, `package demo`, `option …`: by their keyword.
            if KEYWORDS.contains(&first) && first != "rpc" {
                return first.to_string();
            }
            if first == "rpc" {
                let second = d.split_whitespace().nth(1).unwrap_or(first);
                return second.split(['(', '=', ';']).next().unwrap_or(second).trim_matches('"').to_string();
            }
            let paren = d.find('(');
            let eq = d.find('=');
            match (paren, eq) {
                (Some(p), e) if e.is_none_or(|e| p < e) => word(&d[..p]),
                (_, Some(e)) => word(&d[..e]),
                _ => word(d),
            }
        }
        Name::BeforeColon => {
            if first.starts_with('@') {
                return first.split('(').next().unwrap_or(first).to_string();
            }
            let end = decl.find([':', '(', '=', ' ']).unwrap_or(decl.len());
            decl[..end].trim().to_string()
        }
        Name::First => first.split('(').next().unwrap_or(first).to_string(),
    }
}

/// The table of a parse's blocks: kind, name, declarations, the block they are in.
pub(crate) fn types_table(parsed: Blocks) -> NodeTable {
    let tree = Arc::new(parsed.tree);
    let rows: Arc<[u32]> = parsed.blocks.into();
    NodeTable::new(tree, rows)
        .column("Name", ValueKind::Text, |t, row| Cell::Text(t.label(row).to_string()))
        .column("Kind", ValueKind::Text, |t, row| {
            let v = t.value(row);
            if v.is_empty() { Cell::Null } else { Cell::Text(v.split_whitespace().next().unwrap_or(v).to_string()) }
        })
        .column("Members", ValueKind::Number, |t, row| {
            Cell::Int(t.child_ids(row).iter().filter(|&&c| t.kind(c) != ValueKind::Group).count() as i64)
        })
        .column("In", ValueKind::Text, |t: &DataTree, row| match t.parent(row) {
            Some(p) => Cell::Text(t.label(p).to_string()),
            None => value(t, None),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::TreeSource;

    fn names(t: &DataTree, path: &str) -> Vec<(String, String)> {
        let id = t.find(path).unwrap().unwrap_or_else(|| panic!("{path}"));
        t.children(Some(id), 0..50).unwrap().into_iter().map(|n| (n.label, n.value)).collect()
    }

    #[test]
    fn protobuf() {
        let cx = ParseContext::new();
        let lang = Lang { line_comments: &["//"], newline_ends: false, comma_ends: false, name: Name::BeforeEquals };
        let p = parse(
            "syntax = \"proto3\";\npackage demo;\n// a person\nmessage Person {\n  string name = 1;\n  repeated string emails = 2 [packed = true];\n  enum Kind { HUMAN = 0; ROBOT = 1; }\n  map<string, int32> scores = 4;\n}\nservice People { rpc Get(Req) returns (Person); }\n",
            &lang,
            &cx,
        )
        .unwrap();
        let top: Vec<String> = p.tree.children(None, 0..9).unwrap().into_iter().map(|n| n.label).collect();
        assert_eq!(top, ["syntax", "package", "Person", "People"]);
        let person = names(&p.tree, "/Person");
        assert_eq!(person[0], ("name".into(), "string name = 1".into()));
        assert_eq!(person[1].0, "emails");
        assert_eq!(person[2], ("Kind".into(), "enum".into()));
        assert_eq!(person[3].0, "scores");
        assert_eq!(names(&p.tree, "/People")[0].0, "Get");
        assert_eq!(p.blocks.len(), 3);
    }

    #[test]
    fn graphql_thrift_prisma() {
        let cx = ParseContext::new();
        let gql = Lang { line_comments: &["#"], newline_ends: true, comma_ends: true, name: Name::BeforeColon };
        let p = parse("\"\"\"A user\"\"\"\ntype User implements Node {\n  id: ID!\n  posts(first: Int, after: String): [Post!]!\n}\n", &gql, &cx).unwrap();
        assert_eq!(names(&p.tree, "/User").iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), ["id", "posts"]);
        assert_eq!(p.tree.children(None, 0..9).unwrap().last().unwrap().value, "type implements Node");
        let thrift = Lang { line_comments: &["//", "#"], newline_ends: false, comma_ends: true, name: Name::BeforeEquals };
        let p = parse("struct User {\n  1: required string name,\n  2: optional i32 age = 0\n}\nservice S { void ping(1: i32 x) }", &thrift, &cx).unwrap();
        assert_eq!(names(&p.tree, "/User").iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), ["name", "age"]);
        assert_eq!(names(&p.tree, "/S")[0].0, "ping");
        let prisma = Lang { line_comments: &["//"], newline_ends: true, comma_ends: false, name: Name::First };
        let p = parse("model User {\n  id    Int    @id @default(autoincrement())\n  email String @unique\n  @@index([email])\n}\n", &prisma, &cx).unwrap();
        assert_eq!(names(&p.tree, "/User").iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), ["id", "email", "@@index"]);
        assert!(parse("message A {", &prisma, &cx).is_err());
    }
}
