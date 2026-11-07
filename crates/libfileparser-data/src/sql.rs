//! SQL scripts and dumps (`.sql`, `.dump`, Cassandra's `.cql`): a table of their
//! statements, a tree of the schema they create (tables and their columns, views,
//! indexes), and, for each table rows are inserted into (`INSERT … VALUES`, or
//! pg_dump's `COPY … FROM stdin`), a table of that data, read when asked for.
//!
//! Statements are split on `;` outside strings, quoted names and comments, with
//! PostgreSQL's dollar quotes, MySQL's `DELIMITER` and T-SQL's `GO`.

use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Cell, Column, Content, Document, FacetInfo, FileSource, Limit, Model, OwnedTable, ParseContext, ParseError, Result, Role, ValueKind,
};

use crate::lines::{RowLines, columns, text};
use crate::nodes::format;
use crate::tree::{Builder, LineCounter};

/// Data facets at most: one per table that has rows.
const MAX_DATA_FACETS: usize = 32;

struct Statement {
    /// Bytes of the file, the terminator excluded.
    bytes: Range<usize>,
    lines: Range<u64>,
    /// `COPY … FROM stdin`'s data lines that follow it.
    copy: Option<Range<usize>>,
}

/// Splits `s` into statements.
fn statements(s: &[u8], cx: &ParseContext) -> Result<Vec<Statement>> {
    let mut out = Vec::new();
    let mut lines = LineCounter::new(s);
    let mut delimiter: Vec<u8> = b";".to_vec();
    let mut i = 0;
    let mut start: Option<usize> = None;
    let push = |out: &mut Vec<Statement>, lines: &mut LineCounter, from: usize, to: usize| {
        let text = &s[from..to];
        if text.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let (a, b) = (lines.line(from), lines.line(to.saturating_sub(1).max(from)));
        out.push(Statement { bytes: from..to, lines: a as u64..b as u64 + 1, copy: None });
    };
    while i < s.len() {
        let c = s[i];
        // Comments, which may sit between statements.
        if s[i..].starts_with(b"--") || (c == b'#' && start.is_none()) {
            i = memchr::memchr(b'\n', &s[i..]).map_or(s.len(), |p| i + p);
            continue;
        }
        if s[i..].starts_with(b"/*") {
            i = memchr::memmem::find(&s[i + 2..], b"*/").map_or(s.len(), |p| i + 2 + p + 2);
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // At a statement's start: MySQL's `DELIMITER $$` and T-SQL's `GO` lines.
        if start.is_none() {
            let line_end = memchr::memchr(b'\n', &s[i..]).map_or(s.len(), |p| i + p);
            let line = String::from_utf8_lossy(&s[i..line_end]);
            let line = line.trim();
            if line.len() > 10 && line[..10].eq_ignore_ascii_case("DELIMITER ") {
                delimiter = line[10..].trim().as_bytes().to_vec();
                i = line_end;
                continue;
            }
            if line.eq_ignore_ascii_case("GO") {
                i = line_end;
                continue;
            }
            start = Some(i);
        }
        match c {
            b'\'' | b'"' | b'`' => {
                // To the closing quote; doubled quotes and backslashes escape.
                let mut j = i + 1;
                while j < s.len() {
                    if s[j] == b'\\' && c == b'\'' {
                        j += 2;
                        continue;
                    }
                    if s[j] == c {
                        if s.get(j + 1) == Some(&c) {
                            j += 2;
                            continue;
                        }
                        break;
                    }
                    j += 1;
                }
                i = j + 1;
            }
            b'$' => {
                // A dollar quote: `$$ … $$` or `$tag$ … $tag$`.
                let tag_end = s[i + 1..].iter().position(|&b| !(b.is_ascii_alphanumeric() || b == b'_')).map(|p| i + 1 + p);
                match tag_end.filter(|&e| s.get(e) == Some(&b'$')) {
                    Some(e) => {
                        let tag = &s[i..=e];
                        i = memchr::memmem::find(&s[e + 1..], tag).map_or(s.len(), |p| e + 1 + p + tag.len());
                    }
                    None => i += 1,
                }
            }
            _ if s[i..].starts_with(&delimiter) => {
                let from = start.take().unwrap_or(i);
                push(&mut out, &mut lines, from, i);
                i += delimiter.len();
                // pg_dump: `COPY t (a, b) FROM stdin;` then rows to `\.`.
                let head = String::from_utf8_lossy(&s[from..(from + 200).min(i)]).to_ascii_uppercase();
                if head.starts_with("COPY") && head.contains("FROM STDIN") {
                    let data_start = memchr::memchr(b'\n', &s[i..]).map_or(s.len(), |p| i + p + 1);
                    let end = memchr::memmem::find(&s[data_start..], b"\n\\.").map_or(s.len(), |p| data_start + p + 1);
                    if let Some(last) = out.last_mut() {
                        last.copy = Some(data_start..end);
                        last.lines.end = lines.line(end) as u64 + 1;
                    }
                    i = (end + 2).min(s.len());
                }
                if out.len() % 4096 == 0 {
                    cx.cancel.check()?;
                    cx.limits.check(Limit::Rows, out.len() as u64)?;
                }
            }
            _ => i += 1,
        }
    }
    if let Some(from) = start {
        push(&mut out, &mut lines, from, s.len());
    }
    cx.limits.check(Limit::Rows, out.len() as u64)?;
    Ok(out)
}

/// The statement's kind (`CREATE TABLE`, `INSERT`) and the object it names.
fn kind_and_object(stmt: &str) -> (String, Option<String>) {
    let words: Vec<&str> = stmt.split_whitespace().take(12).collect();
    let up: Vec<String> = words.iter().map(|w| w.to_ascii_uppercase()).collect();
    let is = |i: usize, w: &str| up.get(i).is_some_and(|u| u == w);
    // `CREATE [OR REPLACE] [TEMPORARY|UNIQUE|MATERIALIZED …] TABLE|VIEW|INDEX [IF NOT EXISTS] name`
    if is(0, "CREATE") || is(0, "ALTER") || is(0, "DROP") {
        let objects = ["TABLE", "VIEW", "INDEX", "SEQUENCE", "FUNCTION", "PROCEDURE", "TRIGGER", "SCHEMA", "TYPE", "DATABASE", "EXTENSION", "KEYSPACE", "DOMAIN"];
        if let Some(at) = up.iter().position(|u| objects.contains(&u.as_str())) {
            let mut n = at + 1;
            while up.get(n).is_some_and(|u| matches!(u.as_str(), "IF" | "NOT" | "EXISTS" | "CONCURRENTLY")) {
                n += 1;
            }
            let name = words.get(n).map(|w| clean_name(w.split('(').next().unwrap_or(w)));
            return (format!("{} {}", up[0], up[at]), name.filter(|n| !n.is_empty()));
        }
        return (up[0].clone(), None);
    }
    let after = |w: &str| up.iter().position(|u| u == w).and_then(|p| words.get(p + 1)).map(|n| clean_name(n.split('(').next().unwrap_or(n)));
    match up.first().map(String::as_str) {
        Some("INSERT" | "REPLACE") => (up[0].clone(), after("INTO")),
        Some("COPY") => ("COPY".into(), words.get(1).map(|w| clean_name(w.split('(').next().unwrap_or(w)))),
        Some("UPDATE") => ("UPDATE".into(), words.get(1).map(|w| clean_name(w))),
        Some("DELETE") => ("DELETE".into(), after("FROM")),
        Some("SELECT" | "WITH") => (up[0].clone(), after("FROM")),
        Some(other) => (other.to_string(), None),
        None => (String::new(), None),
    }
}

/// A name without its quotes: `"public"."users"` → `public.users`.
fn clean_name(name: &str) -> String {
    name.trim_matches(|c: char| c == ';' || c == ',').split('.').map(|p| p.trim_matches(['"', '`', '[', ']'])).collect::<Vec<_>>().join(".")
}

/// The parts of `s` split on `sep` at nesting depth zero, outside quotes.
fn split_top(s: &str, sep: u8) -> Vec<&str> {
    let b = s.as_bytes();
    let (mut depth, mut quote, mut start) = (0i32, 0u8, 0);
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            if c == b'\\' && quote == b'\'' {
                i += 1;
            } else if c == quote {
                quote = 0;
            }
        } else {
            match c {
                b'\'' | b'"' | b'`' => quote = c,
                b'(' => depth += 1,
                b')' => depth -= 1,
                c if c == sep && depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// The inside of the first parenthesised group at or after `from`, and where it ends.
fn group(s: &str, from: usize) -> Option<(&str, usize)> {
    let open = from + s[from..].find('(')?;
    let b = s.as_bytes();
    let (mut depth, mut quote) = (0i32, 0u8);
    let mut i = open;
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            if c == b'\\' && quote == b'\'' {
                i += 1;
            } else if c == quote {
                quote = 0;
            }
        } else {
            match c {
                b'\'' | b'"' | b'`' => quote = c,
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((&s[open + 1..i], i + 1));
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// A literal as a cell: `NULL`, numbers, `'text'` (with `''` and `\` escapes), booleans;
/// anything else (`now()`) as written.
fn literal(v: &str) -> Cell {
    let v = v.trim();
    if v.eq_ignore_ascii_case("NULL") {
        return Cell::Null;
    }
    if v.eq_ignore_ascii_case("TRUE") || v.eq_ignore_ascii_case("FALSE") {
        return Cell::Bool(v.eq_ignore_ascii_case("TRUE"));
    }
    if let Some(inner) = v.strip_prefix(['E', 'e', 'N']).unwrap_or(v).strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\'' if chars.peek() == Some(&'\'') => {
                    chars.next();
                    out.push('\'');
                }
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('0') => out.push('\0'),
                    Some(o) => out.push(o),
                    None => {}
                },
                c => out.push(c),
            }
        }
        return Cell::Text(out);
    }
    match crate::lines::num(v) {
        Cell::Null => text(v),
        n => n,
    }
}

/// `CREATE TABLE`'s columns: (name, definition).
fn table_columns(stmt: &str) -> Vec<(String, String)> {
    let Some((body, _)) = group(stmt, 0) else { return Vec::new() };
    split_top(body, b',')
        .into_iter()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .filter(|d| {
            let up = d.to_ascii_uppercase();
            !["PRIMARY KEY", "FOREIGN KEY", "UNIQUE", "CONSTRAINT", "KEY ", "INDEX ", "CHECK", "FULLTEXT", "EXCLUDE"].iter().any(|k| up.starts_with(k))
        })
        .map(|d| {
            let (name, rest) = d.split_once(char::is_whitespace).unwrap_or((d, ""));
            (clean_name(name), rest.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .collect()
}

/// `INSERT INTO t (a, b) VALUES (…), (…)`: its column names (if listed) and rows.
fn insert_rows(stmt: &str) -> (Vec<String>, Vec<Vec<Cell>>) {
    let up = stmt.to_ascii_uppercase();
    let Some(values_at) = up.find("VALUES") else { return (Vec::new(), Vec::new()) };
    let head = &stmt[..values_at];
    let names = match head.find('(') {
        Some(_) => group(head, 0).map(|(g, _)| split_top(g, b',').into_iter().map(|n| clean_name(n.trim())).collect()).unwrap_or_default(),
        None => Vec::new(),
    };
    let mut rows = Vec::new();
    let mut at = values_at + 6;
    while let Some((tuple, end)) = group(stmt, at) {
        rows.push(split_top(tuple, b',').into_iter().map(literal).collect());
        at = end;
        // The next tuple follows a comma; anything else (`ON CONFLICT`) ends them.
        let rest = stmt[at..].trim_start();
        if !rest.starts_with(',') {
            break;
        }
    }
    (names, rows)
}

/// pg_dump's COPY rows: tab-separated, `\N` for null, backslash escapes.
fn copy_rows(data: &str) -> Vec<Vec<Cell>> {
    data.lines()
        .map(|line| {
            line.split('\t')
                .map(|f| {
                    if f == "\\N" {
                        return Cell::Null;
                    }
                    let f = f.replace("\\t", "\t").replace("\\n", "\n").replace("\\\\", "\\");
                    match crate::lines::num(&f) {
                        Cell::Null => Cell::Text(f),
                        n => n,
                    }
                })
                .collect()
        })
        .collect()
}

pub(crate) fn looks_like_sql(header: &[u8]) -> bool {
    let text = String::from_utf8_lossy(header);
    let first = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("--") && !l.starts_with("/*") && !l.starts_with('#'));
    first.is_some_and(|l| {
        let w = l.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
        matches!(w.as_str(), "CREATE" | "INSERT" | "BEGIN" | "PRAGMA" | "SET" | "DROP" | "ALTER" | "SELECT" | "USE" | "START" | "COPY" | "LOCK")
    })
}

pub(crate) fn document(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let bytes = src.bytes();
    let text = String::from_utf8_lossy(bytes);
    let stmts = statements(text.as_bytes(), cx)?;
    if stmts.is_empty() {
        return Err(ParseError::corrupt("SQL: no statements"));
    }
    // The schema tree, and which statements hold each table's rows.
    let mut b = Builder::new(cx);
    let mut data: Vec<(String, Vec<usize>)> = Vec::new();
    let mut created: Vec<(String, Vec<String>)> = Vec::new();
    let mut rows = Vec::with_capacity(stmts.len());
    for (n, st) in stmts.iter().enumerate() {
        let body = &text[st.bytes.clone()];
        let (kind, object) = kind_and_object(body);
        let line = st.lines.start as u32;
        let mut count = Cell::Null;
        match (kind.as_str(), &object) {
            ("CREATE TABLE", Some(name)) => {
                let cols = table_columns(body);
                let id = b.add(None, 1, name.clone(), "table", ValueKind::Group, line..st.lines.end as u32)?;
                for (c, def) in &cols {
                    b.add(Some(id), 2, c.clone(), def.clone(), ValueKind::Text, line..st.lines.end as u32)?;
                }
                created.push((name.clone(), cols.into_iter().map(|c| c.0).collect()));
            }
            (k, Some(name)) if k.starts_with("CREATE ") => {
                b.add(None, 1, name.clone(), k.trim_start_matches("CREATE ").to_ascii_lowercase(), ValueKind::Group, line..st.lines.end as u32)?;
            }
            ("INSERT" | "REPLACE" | "COPY", Some(name)) => {
                count = Cell::Int(match &st.copy {
                    Some(r) => text[r.clone()].lines().count(),
                    None => insert_rows(body).1.len(),
                } as i64);
                match data.iter_mut().find(|d| d.0 == *name) {
                    Some(d) => d.1.push(n),
                    None => data.push((name.clone(), vec![n])),
                }
            }
            _ => {}
        }
        let preview: String = body.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
        rows.push(vec![Cell::Text(kind), object.map_or(Cell::Null, Cell::Text), count, Cell::Text(preview)]);
    }
    let lines: Vec<Range<u64>> = stmts.iter().map(|s| s.lines.clone()).collect();
    let count = rows.len();
    let table = OwnedTable::new(columns(&[("Statement", ValueKind::Text), ("Object", ValueKind::Text), ("Rows", ValueKind::Number), ("Text", ValueKind::Text)]), rows);
    let tree = b.finish()?;
    let tables = tree.roots().len();
    let mut doc = Document::new(format("sql", "SQL"))
        .with_facet(Role::Primary, "Statements", Content::Table(Arc::new(table)))
        .with_facet(Role::Structure, "Schema", Content::Tree(Arc::new(tree)))
        .with_meta("Statements", count)
        .with_meta("Schema objects", tables);
    // A table of each table's data, parsed when asked for.
    let stmts = Arc::new(stmts);
    for (name, which) in data.into_iter().take(MAX_DATA_FACETS) {
        let (src2, stmts) = (src.clone(), stmts.clone());
        let declared = created.iter().find(|c| c.0 == name).map(|c| c.1.clone());
        doc = doc.with_lazy_facet(FacetInfo::new(Role::Inspector, format!("Data: {name}"), Model::Table), move |cx| {
            let text = String::from_utf8_lossy(src2.bytes());
            let mut names: Vec<String> = declared.clone().unwrap_or_default();
            let mut rows: Vec<Vec<Cell>> = Vec::new();
            for &n in &which {
                cx.cancel.check()?;
                let st = &stmts[n];
                let body = &text[st.bytes.clone()];
                let (cols, mut r) = match &st.copy {
                    Some(range) => (group(body, 0).map(|(g, _)| split_top(g, b',').into_iter().map(|c| clean_name(c.trim())).collect()).unwrap_or_default(), copy_rows(&text[range.clone()])),
                    None => insert_rows(body),
                };
                if names.is_empty() || (!cols.is_empty() && cols.len() > names.len()) {
                    names = cols;
                }
                rows.append(&mut r);
                cx.limits.check(Limit::Rows, rows.len() as u64)?;
            }
            let width = rows.iter().map(Vec::len).max().unwrap_or(0).max(names.len());
            names.resize(width, String::new());
            for r in &mut rows {
                r.resize(width, Cell::Null);
            }
            let columns: Vec<Column> = names
                .into_iter()
                .enumerate()
                .map(|(i, n)| {
                    let mut kinds = rows.iter().take(1000).map(|r| r[i].kind()).filter(|k| *k != ValueKind::Null);
                    let first = kinds.next();
                    let kind = match first {
                        Some(k) if kinds.all(|x| x == k) => k,
                        Some(_) => ValueKind::Mixed,
                        None => ValueKind::Unknown,
                    };
                    Column::new(n).with_kind(kind)
                })
                .collect();
            Ok(Content::Table(Arc::new(OwnedTable::new(columns, rows))))
        });
    }
    Ok(doc.with_links(RowLines(lines)).with_file_facets(src, Some("sql")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_split_outside_strings_and_quotes() {
        let cx = ParseContext::new();
        let sql = "-- dump\nCREATE TABLE \"users\" (id int PRIMARY KEY, name text NOT NULL, CONSTRAINT x UNIQUE (name));\nINSERT INTO users (id, name) VALUES (1, 'O''Brien; Jr'), (2, NULL);\nCREATE FUNCTION f() RETURNS int AS $$ SELECT 1; $$ LANGUAGE sql;\nDELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; END//\nDELIMITER ;\nSELECT 1";
        let st = statements(sql.as_bytes(), &cx).unwrap();
        let kinds: Vec<(String, Option<String>)> = st.iter().map(|s| kind_and_object(&sql[s.bytes.clone()])).collect();
        assert_eq!(kinds.iter().map(|k| k.0.as_str()).collect::<Vec<_>>(), ["CREATE TABLE", "INSERT", "CREATE FUNCTION", "CREATE PROCEDURE", "SELECT"]);
        assert_eq!(kinds[0].1.as_deref(), Some("users"));
        assert_eq!(table_columns(&sql[st[0].bytes.clone()]), [("id".to_string(), "int PRIMARY KEY".to_string()), ("name".to_string(), "text NOT NULL".to_string())]);
        let (names, rows) = insert_rows(&sql[st[1].bytes.clone()]);
        assert_eq!(names, ["id", "name"]);
        assert_eq!(rows, [vec![Cell::Int(1), Cell::Text("O'Brien; Jr".into())], vec![Cell::Int(2), Cell::Null]]);
        assert_eq!(st[1].lines, 2..3);
    }

    #[test]
    fn pg_dump_copy() {
        let cx = ParseContext::new();
        let sql = "COPY public.t (a, b) FROM stdin;\n1\tx\n2\t\\N\n\\.\n\nSELECT 2;\n";
        let st = statements(sql.as_bytes(), &cx).unwrap();
        assert_eq!(st.len(), 2);
        let data = &sql[st[0].copy.clone().unwrap()];
        assert_eq!(copy_rows(data), [vec![Cell::Int(1), Cell::Text("x".into())], vec![Cell::Int(2), Cell::Null]]);
        assert_eq!(kind_and_object(&sql[st[0].bytes.clone()]).1.as_deref(), Some("public.t"));
    }
}
