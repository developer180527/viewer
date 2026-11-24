//! Plain-text markup languages into formatted text. Each is read by the most mature
//! parser there is for it, written out as HTML, and that HTML becomes formatted text
//! through html5ever (`crate::html`), as Org does: one pipeline for every markup's
//! structure (headings, lists, tables, code, quotes, links, marks).
//!
//! | Format | Parser |
//! |---|---|
//! | reStructuredText | `rst_parser`; its tree is written out by us (its own writer is unfinished) |
//! | Djot | `jotdown`, John MacFarlane's language's reference port |
//! | AsciiDoc | `asciidoc-parser` (Asciidoctor's language, secure mode: nothing is included or fetched) |
//! | Typst | `typst-syntax`, Typst's own parser; markup is shown, code isn't run |
//! | Textile | ours (no maintained crate): the common block and phrase syntax |
//!
//! None of them runs anything: a Typst file's code, an AsciiDoc include or a reST
//! directive that would read another file are skipped.

use std::sync::Arc;

use libfileparser_core::{Category, Content, Document, FileSource, FormatInfo, OwnedFlow, ParseContext, ParseError, Result, Role, Warning, is_text};

use crate::build::Builder;

/// What a markup's parser made of a file.
#[derive(Default)]
pub(crate) struct Rendered {
    pub html: String,
    pub title: Option<String>,
    pub warnings: Vec<String>,
    /// Files the HTML's images name, by that name (a FictionBook's pictures).
    pub resources: Vec<(String, FileSource)>,
}

pub(crate) use markup_lite::{esc, strip_tags};

impl From<markup_lite::Html> for Rendered {
    fn from(h: markup_lite::Html) -> Self {
        Rendered { html: h.html, title: h.title, warnings: h.warnings, resources: Vec::new() }
    }
}

/// One markup reader: its description and how its text becomes HTML (`fn(&str) ->
/// Result<Rendered>`). Shared by every plain-text markup in the crate.
macro_rules! markup {
    ($(#[$doc:meta])* $reader:ident, $info:ident, $id:literal, $name:literal, $kind:literal, [$($ext:literal),*], [$($mime:literal),*], $render:path) => {
        $crate::markup::markup!($(#[$doc])* $reader, $info, $id, $name, $kind, [$($ext),*], [$($mime),*], $render, guess = $crate::markup::no_guess);
    };
    // `guess`: a file this markup claims by its content when another owns the name
    // (Vimwiki's default `.wiki` is MediaWiki's too).
    ($(#[$doc:meta])* $reader:ident, $info:ident, $id:literal, $name:literal, $kind:literal, [$($ext:literal),*], [$($mime:literal),*], $render:path, guess = $guess:path) => {
        static $info: libfileparser_core::ReaderInfo = libfileparser_core::ReaderInfo::new($id, $name, libfileparser_core::Category::Docs).extensions(&[$($ext),*]).mime_types(&[$($mime),*]);

        $(#[$doc])*
        pub struct $reader;

        impl libfileparser_core::Reader for $reader {
            fn info(&self) -> &libfileparser_core::ReaderInfo {
                &$info
            }

            /// By extension, as none of these markups has a signature, or by `guess`.
            fn detect(&self, header: &[u8], name: &str) -> libfileparser_core::Detection {
                if !libfileparser_core::is_text(header) {
                    libfileparser_core::Detection::NONE
                } else if $guess(header, name) {
                    libfileparser_core::Detection::new(libfileparser_core::Confidence::Content).with_language($id)
                } else if $info.matches_extension(name) {
                    libfileparser_core::Detection::new(libfileparser_core::Confidence::Extension).with_language($id)
                } else {
                    libfileparser_core::Detection::NONE
                }
            }

            fn open(&self, src: &libfileparser_core::FileSource, cx: &libfileparser_core::ParseContext) -> libfileparser_core::Result<libfileparser_core::Document> {
                $crate::markup::read(src, cx, $id, $kind, $render)
            }
        }
    };
}
pub(crate) use markup;

/// For markups claimed by extension alone.
pub(crate) fn no_guess(_: &[u8], _: &str) -> bool {
    false
}

pub(crate) fn read(src: &FileSource, cx: &ParseContext, id: &'static str, kind: &'static str, render: fn(&str) -> Result<Rendered>) -> Result<Document> {
    cx.cancel.check()?;
    if !is_text(src.header()) {
        return Err(ParseError::NotThisFormat);
    }
    let bytes = crate::whole(src, cx)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let rendered = render(text)?;
    cx.cancel.check()?;
    let mut b = Builder::new(cx);
    crate::html::parse(&rendered.html, src.name(), &mut b)?;
    let flow = rendered.resources.into_iter().fold(OwnedFlow::new(b.finish()?), |f, (name, file)| f.with_resource(name, file));
    let mut doc = Document::new(FormatInfo::new(id, kind, Category::Docs)).with_facet(Role::Primary, "Document", Content::Flow(Arc::new(flow)));
    if let Some(t) = rendered.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        doc = doc.with_meta("Title", t);
    }
    for w in rendered.warnings {
        doc = doc.with_warning(Warning::new(w));
    }
    Ok(doc.with_file_facets(src, Some(id)))
}

// ---------------------------------------------------------------- reStructuredText

markup!(
    /// reStructuredText (Python's documentation markup), through `rst_parser`.
    RstReader, RST, "rst", "reStructuredText", "reStructuredText document", ["rst", "rest"], ["text/x-rst"], rst
);

fn rst(text: &str) -> Result<Rendered> {
    // rst_parser's grammar wants a final newline, and tabs as spaces (as docutils does).
    let mut text = text.replace('\t', "        ");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let (text, long) = tame_rst(&text);
    let (text, tables, literals) = rst_tables(&text);
    // A directive option rst_parser's grammar refuses (`:alt:` with spaces) loses the
    // whole document; without the option, the rest reads. A few, then give up.
    let mut text = text;
    let mut dropped = 0;
    let doc = loop {
        match rst_parser::parse(&text) {
            Ok(doc) => break doc,
            Err(e) => {
                let msg = e.to_string();
                let at = msg.split("--> ").nth(1).and_then(|r| r.split(':').next()).and_then(|n| n.trim().parse::<usize>().ok());
                let lines: Vec<&str> = text.split_inclusive('\n').collect();
                let option = at.filter(|&n| n >= 1 && n <= lines.len()).filter(|&n| {
                    let l = lines[n - 1];
                    l.starts_with([' ', '\t']) && l.trim_start().starts_with(':') && l.trim_start()[1..].contains(':')
                });
                match option {
                    Some(n) if dropped < 32 => {
                        dropped += 1;
                        text = lines.iter().enumerate().filter(|&(k, _)| k != n - 1).map(|(_, l)| *l).collect();
                    }
                    _ => return Err(ParseError::invalid(format!("not reStructuredText this parser reads: {}", first_line(&msg)))),
                }
            }
        }
    };
    // The tree as JSON (document_tree derives Serialize), walked generically: rst_parser's
    // own HTML writer, rst_renderer, panics on 13 kinds of element.
    let tree = serde_json::to_value(&doc).map_err(ParseError::invalid)?;
    let mut w = RstHtml { out: String::with_capacity(text.len() * 2), sections: 0, title: None, depth: 0 };
    w.children(&tree);
    // Each literal block, then each table, where its placeholder paragraph is.
    for (n, code) in literals.iter().enumerate() {
        let mark = literal_placeholder(n);
        let html = format!("<pre>{}</pre>", esc(code));
        if w.out.contains(&format!("<p>{mark}</p>")) {
            w.out = w.out.replacen(&format!("<p>{mark}</p>"), &html, 1);
        } else {
            w.out = w.out.replacen(&mark, &html, 1);
        }
    }
    for (n, table) in tables.iter().enumerate() {
        let mark = table_placeholder(n);
        let html = table_html(table);
        if w.out.contains(&format!("<p>{mark}</p>")) {
            w.out = w.out.replacen(&format!("<p>{mark}</p>"), &html, 1);
        } else {
            w.out = w.out.replacen(&mark, &html, 1);
        }
    }
    let mut warnings = Vec::new();
    if dropped > 0 {
        warnings.push(format!("{dropped} directive option(s) this parser doesn't read left out (an image's alt text, say)."));
    }
    if !long.is_empty() {
        // Each long line back where its placeholder is, as written.
        for (n, line) in long.iter().enumerate() {
            w.out = w.out.replacen(&placeholder(n), &esc(line.trim()), 1);
        }
        warnings.push(format!("{} line(s) over {LONG_LINE} characters shown without their inline formatting: reading them as reStructuredText takes too long.", long.len()));
    }
    let title = w.title.map(|t| long.iter().enumerate().fold(t, |t, (n, line)| t.replacen(&placeholder(n), line.trim(), 1)));
    Ok(Rendered { html: w.out, title, warnings, ..Rendered::default() })
}

/// Characters in one line past which rst_parser is too slow: its parse grows with the
/// square of a line's length (4,000 characters take seconds), and it can't be
/// cancelled. Found by fuzzing; many short lines stay linear.
const LONG_LINE: usize = 800;

/// A word standing in for long line `n` while rst_parser reads the rest.
fn placeholder(n: usize) -> String {
    format!("lfplongline{n}x")
}

/// `text` with every line over [`LONG_LINE`] characters swapped for a placeholder (its
/// indent kept, so it stays in the same block), and those lines.
fn tame_rst(text: &str) -> (String, Vec<String>) {
    if !text.lines().any(|l| l.chars().count() > LONG_LINE) {
        return (text.to_string(), Vec::new());
    }
    let mut long = Vec::new();
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        if body.chars().count() > LONG_LINE {
            let indent = &body[..body.len() - body.trim_start().len()];
            out.push_str(indent);
            out.push_str(&placeholder(long.len()));
            out.push('\n');
            long.push(body.to_string());
        } else {
            out.push_str(line);
        }
    }
    (out, long)
}

/// A table read from reStructuredText's grid or simple syntax, which rst_parser
/// doesn't read: its rows of cells, as written, and how many rows are headers.
struct RstTable {
    rows: Vec<Vec<String>>,
    header_rows: usize,
}

fn table_placeholder(n: usize) -> String {
    format!("lfptable{n}x")
}

/// `text` with each grid table (`+---+`), simple table (`=== ===`) and literal block
/// (the indented lines after `::`) swapped for a placeholder paragraph at its indent,
/// and those tables and blocks. Tables inside a literal block or a directive's content
/// stay as written.
fn rst_tables(text: &str) -> (String, Vec<RstTable>, Vec<String>) {
    let (mut tables, mut literals) = (Vec::new(), Vec::new());
    let out = rst_tables_into(text, &mut tables, &mut literals, 0);
    (out, tables, literals)
}

fn rst_tables_into(text: &str, tables: &mut Vec<RstTable>, literals: &mut Vec<String>, depth: usize) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let blank = |l: &str| l.trim().is_empty();
    let grid_border = |l: &str| {
        let t = l.trim();
        t.len() > 2 && t.starts_with('+') && t.ends_with('+') && t.chars().all(|c| matches!(c, '+' | '-' | '=' | ':'))
    };
    let simple_border = |l: &str| {
        let t = l.trim();
        t.contains(' ') && t.split_whitespace().count() >= 2 && t.split_whitespace().all(|r| r.chars().all(|c| c == '='))
    };
    if !lines.iter().any(|l| grid_border(l) || simple_border(l) || l.contains("::")) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    // Indent under which lines are a literal block or a directive's content.
    let mut literal: Option<usize> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(at) = literal {
            if blank(line) || indent(line) > at {
                out.push_str(line);
                i += 1;
                continue;
            }
            literal = None;
        }
        let starts_block = i == 0 || blank(lines[i - 1]);
        let table = if starts_block && grid_border(line) {
            grid_table(&lines[i..])
        } else if starts_block && simple_border(line) {
            simple_table(&lines[i..])
        } else {
            None
        };
        if let Some((table, used)) = table {
            out.push_str(&" ".repeat(indent(line)));
            out.push_str(&table_placeholder(tables.len()));
            out.push('\n');
            tables.push(table);
            i += used;
            continue;
        }
        let t = line.trim_end();
        // `.. table:: caption`, which rst_parser doesn't know: its caption, then its
        // content read as the rest is.
        if let Some(caption) = t.trim_start().strip_prefix(".. table::")
            && depth < 4
        {
            let at = indent(line);
            let mut end = i + 1;
            while end < lines.len() && (blank(lines[end]) || indent(lines[end]) > at) {
                end += 1;
            }
            let body: Vec<&str> = lines[i + 1..end].to_vec();
            let cut = body.iter().filter(|l| !blank(l)).map(|l| indent(l)).min().unwrap_or(0);
            let inner: String = body.iter().map(|l| if blank(l) { "\n" } else { &l[cut.min(l.len())..] }).collect();
            if !caption.trim().is_empty() {
                out.push_str(&" ".repeat(at));
                out.push_str(caption.trim());
                out.push_str("\n\n");
            }
            let inner = rst_tables_into(inner.trim_start_matches('\n'), tables, literals, depth + 1);
            for l in inner.split_inclusive('\n') {
                if !blank(l) {
                    out.push_str(&" ".repeat(at));
                }
                out.push_str(l);
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
            i = end;
            continue;
        }
        // `.. list-table::`: rows as a two-level list (`* - a` then `  - b`).
        if let Some(caption) = t.trim_start().strip_prefix(".. list-table::") {
            let at = indent(line);
            let mut end = i + 1;
            while end < lines.len() && (blank(lines[end]) || indent(lines[end]) > at) {
                end += 1;
            }
            if let Some(table) = list_table(&lines[i + 1..end]) {
                if !caption.trim().is_empty() {
                    out.push_str(&" ".repeat(at));
                    out.push_str(caption.trim());
                    out.push_str("\n\n");
                }
                out.push_str(&" ".repeat(at));
                out.push_str(&table_placeholder(tables.len()));
                out.push_str(if end < lines.len() { "\n\n" } else { "\n" });
                tables.push(table);
                i = end;
                continue;
            }
        }
        let directive = t.trim_start().starts_with(".. ") && t.contains("::");
        if t.ends_with("::") && !t.trim_start().starts_with(".. ") {
            // A literal block: the indented lines after it, as written (rst_parser
            // doesn't read them). The paragraph keeps one colon, or none after a space.
            let at = indent(line);
            let mut j = i + 1;
            while j < lines.len() && blank(lines[j]) {
                j += 1;
            }
            let mut end = j;
            while end < lines.len() && (blank(lines[end]) || indent(lines[end]) > at) {
                end += 1;
            }
            while end > j && blank(lines[end - 1]) {
                end -= 1;
            }
            if j > i + 1 && end > j {
                let head = t.trim_start();
                let head = if head == "::" { "" } else if let Some(h) = head.strip_suffix(" ::") { h } else { &t.trim_start()[..head.len() - 1] };
                if !head.is_empty() {
                    out.push_str(&" ".repeat(at));
                    out.push_str(head.trim_end());
                    out.push_str("\n\n");
                }
                let cut = lines[j..end].iter().filter(|l| !blank(l)).map(|l| indent(l)).min().unwrap_or(0);
                let body: Vec<&str> = lines[j..end].iter().map(|l| l.trim_end_matches(['\n', '\r']).get(cut..).unwrap_or("")).collect();
                out.push_str(&" ".repeat(at));
                out.push_str(&literal_placeholder(literals.len()));
                out.push_str(if end < lines.len() { "\n\n" } else { "\n" });
                literals.push(body.join("\n"));
                i = end;
                continue;
            }
            literal = Some(at);
        } else if directive {
            literal = Some(indent(line));
        }
        out.push_str(line);
        i += 1;
    }
    out
}

fn literal_placeholder(n: usize) -> String {
    format!("lfpliteral{n}x")
}

/// A grid table from its top border: its cells split where the border has `+`, rows
/// ended by borders, the rows above a `=` border its header. Returns the lines read.
fn grid_table(lines: &[&str]) -> Option<(RstTable, usize)> {
    let top = lines[0];
    let start = top.len() - top.trim_start().len();
    let cols: Vec<usize> = top.char_indices().filter(|&(_, c)| c == '+').map(|(i, _)| i).collect();
    if cols.len() < 2 {
        return None;
    }
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut header_rows = 0;
    let mut cells: Vec<Vec<String>> = vec![Vec::new(); cols.len() - 1];
    let mut used = 1;
    for line in &lines[1..] {
        let body = line.trim_end_matches(['\n', '\r']);
        let t = body.trim_start();
        if !(t.starts_with('+') || t.starts_with('|')) || body.len() - t.len() != start {
            break;
        }
        used += 1;
        if t.starts_with('+') {
            // A border: the row is done (a partial one inside a row span ends it too).
            if cells.iter().any(|c| !c.is_empty()) {
                rows.push(cells.iter().map(|c| c.join("\n")).collect());
            }
            cells = vec![Vec::new(); cols.len() - 1];
            if t.contains('=') {
                header_rows = rows.len();
            }
            continue;
        }
        // Text between the column lines that have a `|` here; a span keeps going.
        let chars: Vec<char> = body.chars().collect();
        let mut from = cols[0] + 1;
        let mut col = 0;
        for (j, &edge) in cols.iter().enumerate().skip(1) {
            if chars.get(edge) == Some(&'|') || j == cols.len() - 1 {
                let piece: String = chars.get(from..edge.min(chars.len())).map(|s| s.iter().collect()).unwrap_or_default();
                cells[col].push(piece.trim_end().to_string());
                from = edge + 1;
                col = j;
            }
        }
    }
    if rows.is_empty() {
        return None;
    }
    // Each cell's lines, their shared indent removed.
    let rows = rows
        .into_iter()
        .map(|r| {
            r.into_iter()
                .map(|c| {
                    let lines: Vec<&str> = c.lines().collect();
                    let cut = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
                    lines.iter().map(|l| l.get(cut..).unwrap_or("").to_string()).collect::<Vec<_>>().join("\n").trim().to_string()
                })
                .collect()
        })
        .collect();
    Some((RstTable { rows, header_rows }, used))
}

/// A simple table from its top border: columns where the border has `=`s (the last
/// running to the line's end), a line with its first column empty continuing the row
/// above, a border before the last its header's end.
fn simple_table(lines: &[&str]) -> Option<(RstTable, usize)> {
    let top = lines[0].trim_end();
    let mut cols: Vec<(usize, usize)> = Vec::new();
    let mut at = None;
    for (i, c) in top.char_indices().chain([(top.len(), ' ')]) {
        match (c == '=', at) {
            (true, None) => at = Some(i),
            (false, Some(s)) => {
                cols.push((s, i));
                at = None;
            }
            _ => {}
        }
    }
    if cols.len() < 2 {
        return None;
    }
    let is_border = |l: &str| {
        let t = l.trim();
        !t.is_empty() && t.chars().all(|c| c == '=' || c == ' ')
    };
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut header_rows = 0;
    let mut used = 1;
    let mut ended = false;
    for (k, line) in lines[1..].iter().enumerate() {
        used += 1;
        let body = line.trim_end_matches(['\n', '\r']);
        if is_border(body) {
            let next = lines.get(k + 2);
            if next.is_none_or(|n| n.trim().is_empty()) {
                ended = true;
                break;
            }
            header_rows = rows.len();
            continue;
        }
        if body.trim().is_empty() || body.trim().chars().all(|c| c == '-' || c == ' ') {
            continue;
        }
        let chars: Vec<char> = body.chars().collect();
        let cell = |j: usize| -> String {
            let s = cols[j].0;
            let e = if j + 1 == cols.len() { chars.len() } else { cols[j + 1].0.min(chars.len()) };
            chars.get(s.min(chars.len())..e.max(s).min(chars.len())).map(|x| x.iter().collect::<String>()).unwrap_or_default().trim().to_string()
        };
        let row: Vec<String> = (0..cols.len()).map(cell).collect();
        match rows.last_mut() {
            Some(last) if row[0].is_empty() => {
                for (c, add) in last.iter_mut().zip(row) {
                    if !add.is_empty() {
                        if !c.is_empty() {
                            c.push(' ');
                        }
                        c.push_str(&add);
                    }
                }
            }
            _ => rows.push(row),
        }
    }
    (ended && !rows.is_empty()).then_some((RstTable { rows, header_rows }, used))
}

/// A list table's content: its `:header-rows:`, then rows (`* -`) of cells (`-`),
/// each cell's text running on over the lines indented under it.
fn list_table(lines: &[&str]) -> Option<RstTable> {
    let mut header_rows = 0;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut cell_indent = usize::MAX;
    for line in lines {
        let body = line.trim_end_matches(['\n', '\r']);
        let t = body.trim_start();
        let ind = body.len() - t.len();
        if t.is_empty() {
            if let Some(c) = rows.last_mut().and_then(|r| r.last_mut()) {
                c.push('\n');
            }
            continue;
        }
        if rows.is_empty() && t.starts_with(':') {
            if let Some(n) = t.strip_prefix(":header-rows:") {
                header_rows = n.trim().parse().unwrap_or(0);
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("* -").or_else(|| (t == "* -").then_some("")) {
            rows.push(vec![rest.trim().to_string()]);
            cell_indent = ind + 2;
        } else if let Some(rest) = t.strip_prefix("- ").or_else(|| (t == "-").then_some("")).filter(|_| ind == cell_indent && !rows.is_empty()) {
            rows.last_mut()?.push(rest.trim().to_string());
        } else {
            // More of the cell above; text before any row isn't a list table.
            let c = rows.last_mut().and_then(|r| r.last_mut())?;
            if !c.is_empty() && !c.ends_with('\n') {
                c.push('\n');
            }
            c.push_str(t);
        }
    }
    let rows: Vec<Vec<String>> = rows.into_iter().map(|r| r.into_iter().map(|c| c.trim().to_string()).collect()).collect();
    (!rows.is_empty()).then(|| RstTable { header_rows: header_rows.min(rows.len()), rows })
}

/// A table as HTML, each cell read as reStructuredText itself.
fn table_html(t: &RstTable) -> String {
    let cell = |text: &str| -> String {
        if text.is_empty() {
            return String::new();
        }
        let html = rst(text).map(|r| r.html).unwrap_or_else(|_| esc(text));
        // One paragraph is the cell's text.
        match html.strip_prefix("<p>").and_then(|h| h.strip_suffix("</p>")) {
            Some(inner) if !inner.contains("<p>") => inner.to_string(),
            _ => html,
        }
    };
    let mut out = String::from("<table>");
    for (i, row) in t.rows.iter().enumerate() {
        let tag = if i < t.header_rows { "th" } else { "td" };
        if i == 0 && t.header_rows > 0 {
            out.push_str("<thead>");
        }
        if i == t.header_rows && t.header_rows > 0 {
            out.push_str("</thead><tbody>");
        }
        out.push_str("<tr>");
        for c in row {
            out.push_str(&format!("<{tag}>{}</{tag}>", cell(c)));
        }
        out.push_str("</tr>");
    }
    if t.header_rows > 0 {
        out.push_str("</tbody>");
    }
    out.push_str("</table>");
    out
}

struct RstHtml {
    out: String,
    /// Sections open: a title's heading level.
    sections: usize,
    title: Option<String>,
    depth: usize,
}

impl RstHtml {
    fn children(&mut self, v: &serde_json::Value) {
        if let Some(kids) = v.get("children").and_then(|c| c.as_array()) {
            for k in kids {
                self.node(k, None);
            }
        }
    }

    /// One child: a string, an element tagged with its kind (`{"Paragraph": {…}}`,
    /// inside transparent category wrappers), or an untagged element (a list item).
    fn node(&mut self, v: &serde_json::Value, tag: Option<&str>) {
        use serde_json::Value;
        self.depth += 1;
        if self.depth > 256 {
            self.depth -= 1;
            return;
        }
        match v {
            Value::String(t) => self.out.push_str(&esc(t)),
            Value::Object(m) if m.len() == 1 && tag.is_none() && m.keys().all(|k| k.starts_with(|c: char| c.is_ascii_uppercase())) => {
                let (k, inner) = m.iter().next().expect("one entry");
                match k.as_str() {
                    "SubStructure" | "StructuralSubElement" | "BodyElement" | "TextOrInlineElement" | "SubSection" | "SubTopic" | "SubSidebar" | "SubLineBlock" | "SubBlockQuote" | "SubFootnote" | "SubFigure" | "SubTable" | "SubTableGroup" | "SubDLItem" | "SubField" | "SubOptionListItem" | "SubOption" => self.node(inner, None),
                    "String" => self.node(inner, Some("String")),
                    kind => self.element(kind, inner),
                }
            }
            Value::Object(_) => self.element(tag.unwrap_or(""), v),
            Value::Array(a) => a.iter().for_each(|x| self.node(x, None)),
            _ => {}
        }
        self.depth -= 1;
    }

    fn wrap(&mut self, tag: &str, v: &serde_json::Value) {
        self.out.push_str(&format!("<{tag}>"));
        self.children(v);
        self.out.push_str(&format!("</{tag}>"));
    }

    fn element(&mut self, kind: &str, v: &serde_json::Value) {
        let field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        let classes: Vec<String> = v.get("classes").and_then(|c| c.as_array()).map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect()).unwrap_or_default();
        match kind {
            "Section" => {
                self.sections += 1;
                self.children(v);
                self.sections -= 1;
            }
            "Title" => {
                let level = self.sections.clamp(1, 6);
                let start = self.out.len();
                self.wrap(&format!("h{level}"), v);
                if self.title.is_none() && level == 1 {
                    self.title = Some(strip_tags(&self.out[start..]));
                }
            }
            "Subtitle" => self.wrap("h2", v),
            "Paragraph" | "Line" | "Rubric" | "Attribution" => self.wrap("p", v),
            "Emphasis" | "TitleReference" => self.wrap("em", v),
            "Strong" => self.wrap("strong", v),
            "Subscript" => self.wrap("sub", v),
            "Superscript" => self.wrap("sup", v),
            "Literal" | "Math" => {
                let mut text = String::new();
                collect_text(v, &mut text);
                self.out.push_str(&format!("<code>{}</code>", esc(&text)));
            }
            "Reference" => match field("refuri") {
                Some(uri) => {
                    self.out.push_str(&format!("<a href=\"{}\">", esc(&uri)));
                    self.children(v);
                    self.out.push_str("</a>");
                }
                None => self.children(v),
            },
            "FootnoteReference" | "CitationReference" => {
                let mut label = String::new();
                collect_text(v, &mut label);
                self.out.push_str(&format!("<sup>[{}]</sup>", esc(&label)));
            }
            "BulletList" | "EnumeratedList" => {
                let tag = if kind == "BulletList" { "ul" } else { "ol" };
                self.out.push_str(&format!("<{tag}>"));
                for item in v.get("children").and_then(|c| c.as_array()).into_iter().flatten() {
                    self.out.push_str("<li>");
                    self.children(item);
                    self.out.push_str("</li>");
                }
                self.out.push_str(&format!("</{tag}>"));
            }
            "LiteralBlock" | "DoctestBlock" | "MathBlock" => {
                let mut text = String::new();
                collect_text(v, &mut text);
                let lang = match classes.as_slice() {
                    [c, l, ..] if c == "code" => Some(l.as_str()),
                    _ if kind == "DoctestBlock" => Some("pycon"),
                    _ if kind == "MathBlock" => Some("math"),
                    _ => None,
                };
                match lang.filter(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(c))) {
                    Some(l) => self.out.push_str(&format!("<pre><code class=\"language-{l}\">{}</code></pre>", esc(text.trim_end_matches('\n')))),
                    None => self.out.push_str(&format!("<pre>{}</pre>", esc(text.trim_end_matches('\n')))),
                }
            }
            "BlockQuote" | "Topic" | "Sidebar" | "Compound" | "Container" => self.wrap("blockquote", v),
            "Attention" | "Caution" | "Danger" | "Error" | "Hint" | "Important" | "Note" | "Tip" | "Warning" | "AdmonitionElement" => {
                self.out.push_str(&format!("<blockquote><p><strong>{}</strong></p>", if kind == "AdmonitionElement" { "Admonition" } else { kind }));
                self.children(v);
                self.out.push_str("</blockquote>");
            }
            "Footnote" | "Citation" => {
                self.out.push_str("<div>");
                self.children(v);
                self.out.push_str("</div>");
            }
            "Label" => {
                let mut label = String::new();
                collect_text(v, &mut label);
                self.out.push_str(&format!("<p><sup>[{}]</sup></p>", esc(&label)));
            }
            "Image" => {
                let uri = field("uri").unwrap_or_default();
                let alt = field("alt").unwrap_or_default();
                self.out.push_str(&format!("<img src=\"{}\" alt=\"{}\">", esc(&uri), esc(alt.trim())));
            }
            "Caption" | "Legend" => {
                self.out.push_str("<p><em>");
                self.children(v);
                self.out.push_str("</em></p>");
            }
            "Transition" => self.out.push_str("<hr>"),
            "LineBlock" => self.wrap("div", v),
            "DefinitionList" | "FieldList" | "OptionList" => self.wrap("div", v),
            "Term" | "FieldName" | "OptionGroup" => {
                self.out.push_str("<p><strong>");
                self.children(v);
                self.out.push_str("</strong></p>");
            }
            "Definition" | "FieldBody" | "Description" => self.wrap("blockquote", v),
            "Table" => self.wrap("table", v),
            "TableHead" => self.wrap("thead", v),
            "TableRow" => self.wrap("tr", v),
            "TableEntry" => self.wrap("td", v),
            "Comment" | "Target" | "SubstitutionDefinition" | "Raw" | "Pending" | "SystemMessage" | "Decoration" | "Header" | "Footer" | "ColSpec" | "Generated" => {}
            _ => self.children(v),
        }
    }
}

/// Every string under `v`.
fn collect_text(v: &serde_json::Value, out: &mut String) {
    match v {
        serde_json::Value::String(s) => out.push_str(s),
        serde_json::Value::Object(m) => {
            if let Some(c) = m.get("children") {
                collect_text(c, out);
            } else if m.len() == 1 {
                m.values().for_each(|x| collect_text(x, out));
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_text(x, out)),
        _ => {}
    }
}

fn first_line(s: &str) -> &str {
    let s = s.trim();
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s);
    &line[..line.floor_char_boundary(200)]
}

// ---------------------------------------------------------------- Djot

markup!(
    /// Djot (John MacFarlane's successor to CommonMark), through `jotdown`.
    DjotReader, DJOT, "djot", "Djot", "Djot document", ["dj", "djot"], ["text/x-djot"], djot
);

fn djot(text: &str) -> Result<Rendered> {
    Ok(Rendered { html: jotdown::html::render_to_string(jotdown::Parser::new(text)), ..Rendered::default() })
}

// ---------------------------------------------------------------- AsciiDoc

markup!(
    /// AsciiDoc (Asciidoctor's language), through `asciidoc-parser` in secure mode.
    AsciidocReader, ASCIIDOC, "asciidoc", "AsciiDoc", "AsciiDoc document", ["adoc", "asciidoc", "asc.txt"], ["text/asciidoc"], asciidoc
);

fn asciidoc(text: &str) -> Result<Rendered> {
    use asciidoc_parser::blocks::FindBlocks;
    let doc = asciidoc_parser::Parser::new().parse(text);
    let mut out = String::with_capacity(text.len() * 2);
    let title = doc.doctitle().map(str::to_string);
    if let Some(t) = &title {
        out.push_str(&format!("<h1>{t}</h1>"));
    }
    // The header's author and revision lines, under the title as Asciidoctor shows them.
    let authors: Vec<String> = doc
        .authors()
        .iter()
        .map(|a| match a.email() {
            Some(e) => format!("{} &lt;<a href=\"mailto:{1}\">{1}</a>&gt;", esc(a.name()), esc(e)),
            None => esc(a.name()),
        })
        .collect();
    if !authors.is_empty() {
        out.push_str(&format!("<p>{}</p>", authors.join(", ")));
    }
    if let Some(r) = doc.header().revision_line() {
        let parts: Vec<String> = [r.revnumber().map(|n| format!("version {n}")), Some(r.revdate().to_string()).filter(|d| !d.is_empty()), r.revremark().map(str::to_string)].into_iter().flatten().collect();
        if !parts.is_empty() {
            out.push_str(&format!("<p><em>{}</em></p>", esc(&parts.join(", "))));
        }
    }
    let mut warnings = Vec::new();
    adoc_blocks(doc.child_blocks(), &mut out, 0, &mut warnings);
    let title = doc.doctitle_sanitized().or(title);
    Ok(Rendered { html: out, title, warnings, ..Rendered::default() })
}

fn adoc_blocks<'a>(blocks: impl Iterator<Item = &'a asciidoc_parser::blocks::Block<'a>>, out: &mut String, depth: usize, warnings: &mut Vec<String>) {
    for b in blocks {
        adoc_block(b, out, depth, warnings);
    }
}

fn adoc_block<'a>(block: &'a asciidoc_parser::blocks::Block<'a>, out: &mut String, depth: usize, warnings: &mut Vec<String>) {
    use asciidoc_parser::blocks::{Block, BreakType, ContentModel, FindBlocks, IsBlock, ListType, MediaType, TableCellContent};
    if depth > 64 {
        return;
    }
    // A block's own title (".Title" above it), except a section's, which is its heading.
    if !matches!(block, Block::Section(_))
        && let Some(t) = block.title() {
            out.push_str(&format!("<p><strong>{t}</strong></p>"));
        }
    let context = block.resolved_context();
    match block {
        Block::Section(s) => {
            let level = (s.level() + 1).min(6);
            out.push_str(&format!("<h{level}>{}</h{level}>", s.section_title()));
            adoc_blocks(s.child_blocks(), out, depth + 1, warnings);
        }
        Block::List(l) => {
            let tag = if matches!(l.type_(), ListType::Ordered) { "ol" } else { "ul" };
            out.push_str(&format!("<{tag}>"));
            adoc_blocks(l.child_blocks(), out, depth + 1, warnings);
            out.push_str(&format!("</{tag}>"));
        }
        Block::ListItem(item) => {
            out.push_str("<li>");
            if let Some(c) = item.checkbox() {
                out.push_str(if c { "☑ " } else { "☐ " });
            }
            if let Some(html) = item.rendered_html_content() {
                out.push_str(&format!("<p>{html}</p>"));
            }
            adoc_blocks(item.child_blocks(), out, depth + 1, warnings);
            out.push_str("</li>");
        }
        Block::Media(m) => match m.type_() {
            MediaType::Image => out.push_str(&format!("<img src=\"{}\" alt=\"{}\">", esc(m.resolved_target()), esc(block.title().unwrap_or("")))),
            _ => out.push_str(&format!("<p>[{}: {}]</p>", context, esc(m.resolved_target()))),
        },
        Block::Break(b) => {
            if matches!(b.type_(), BreakType::Thematic) {
                out.push_str("<hr>");
            }
        }
        Block::Table(t) => {
            out.push_str("<table>");
            let cells = |row: &'a asciidoc_parser::blocks::TableRow<'a>, tag: &str, out: &mut String, warnings: &mut Vec<String>| {
                out.push_str("<tr>");
                for cell in row.cells() {
                    out.push_str(&format!("<{tag}>"));
                    match cell.content() {
                        TableCellContent::Simple(c) => out.push_str(c.rendered_html()),
                        TableCellContent::AsciiDoc(d) => {
                            // A cell's blocks become its text: our tables hold inlines.
                            let mut inner = String::new();
                            adoc_blocks(d.blocks().iter(), &mut inner, depth + 1, warnings);
                            out.push_str(&inner.replace("<p>", "").replace("</p>", " "));
                        }
                    }
                    out.push_str(&format!("</{tag}>"));
                }
                out.push_str("</tr>");
            };
            if let Some(h) = t.header_row() {
                out.push_str("<thead>");
                cells(h, "th", out, warnings);
                out.push_str("</thead>");
            }
            for r in t.body_rows() {
                cells(r, "td", out, warnings);
            }
            if let Some(f) = t.footer_row() {
                cells(f, "td", out, warnings);
            }
            out.push_str("</table>");
        }
        Block::Admonition(a) => {
            out.push_str(&format!("<blockquote><p><strong>{}</strong></p>", esc(a.label())));
            if let Some(c) = a.content() {
                out.push_str(&format!("<p>{}</p>", c.rendered_html()));
            }
            adoc_blocks(a.child_blocks(), out, depth + 1, warnings);
            out.push_str("</blockquote>");
        }
        Block::Toc(_) | Block::DocumentAttribute(_) => {}
        _ => match block.content_model() {
            ContentModel::Compound => {
                let quote = matches!(&*context, "quote" | "verse" | "example" | "sidebar");
                out.push_str(if quote { "<blockquote>" } else { "<div>" });
                if let Some(html) = block.rendered_html_content() {
                    out.push_str(&format!("<p>{html}</p>"));
                }
                adoc_blocks(block.child_blocks(), out, depth + 1, warnings);
                out.push_str(if quote { "</blockquote>" } else { "</div>" });
            }
            ContentModel::Verbatim => {
                let html = block.rendered_html_content().unwrap_or("");
                let lang = block.attrlist().and_then(|a| a.nth_attribute(2)).map(|a| a.value().to_string());
                match lang.filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(c))) {
                    Some(l) => out.push_str(&format!("<pre><code class=\"language-{l}\">{html}</code></pre>")),
                    None => out.push_str(&format!("<pre>{html}</pre>")),
                }
            }
            ContentModel::Raw => {
                // A passthrough's HTML, as Asciidoctor would emit it; ours only reads it.
                if let Some(html) = block.rendered_html_content() {
                    out.push_str(html);
                }
            }
            ContentModel::Simple => {
                let html = block.rendered_html_content().unwrap_or("");
                match &*context {
                    "verse" | "quote" => out.push_str(&format!("<blockquote><p>{}</p></blockquote>", html.replace('\n', "<br>"))),
                    "literal" | "listing" => out.push_str(&format!("<pre>{html}</pre>")),
                    _ => out.push_str(&format!("<p>{html}</p>")),
                }
            }
            ContentModel::Empty | ContentModel::Table => {}
        },
    }
}

// ---------------------------------------------------------------- Typst

markup!(
    /// Typst's markup, through `typst-syntax` (Typst's own parser). Text, headings,
    /// lists, emphasis, raw text, links and math are shown; code (`#let`, `#set`,
    /// `#show`, imports) isn't run, and of a function call only its content is shown.
    TypstReader, TYPST, "typst", "Typst", "Typst document", ["typ"], ["text/vnd.typst"], typst
);

fn typst(text: &str) -> Result<Rendered> {
    let root = typst_syntax::parse(text);
    let mut t = Typst { out: String::with_capacity(text.len() * 2), para: false, lists: Vec::new(), depth: 0, title: None };
    t.markup(&root);
    t.close_lists(0);
    t.end_para();
    let (errors, _) = root.errors_and_warnings();
    let warnings = if errors.is_empty() { Vec::new() } else { vec![format!("{} syntax error(s); the text around them is shown as Typst read it.", errors.len())] };
    Ok(Rendered { html: t.out, title: t.title, warnings, ..Rendered::default() })
}

struct Typst {
    out: String,
    /// A paragraph is open.
    para: bool,
    /// Open lists, outermost first: `true` for numbered.
    lists: Vec<bool>,
    depth: usize,
    title: Option<String>,
}

impl Typst {
    fn inline(&mut self) {
        if !self.para {
            self.close_lists(0);
            self.out.push_str("<p>");
            self.para = true;
        }
    }

    fn end_para(&mut self) {
        if self.para {
            self.out.push_str("</p>");
            self.para = false;
        }
    }

    fn close_lists(&mut self, keep: usize) {
        while self.lists.len() > keep {
            let ordered = self.lists.pop().unwrap_or(false);
            self.out.push_str(if ordered { "</ol>" } else { "</ul>" });
        }
    }

    fn markup(&mut self, node: &typst_syntax::SyntaxNode) {
        use typst_syntax::SyntaxKind as K;
        self.depth += 1;
        if self.depth > 200 {
            self.depth -= 1;
            return;
        }
        let kids: Vec<_> = node.children().collect();
        let mut i = 0;
        while i < kids.len() {
            let n = kids[i];
            match n.kind() {
                K::Raw if self.is_block_raw(n) => {
                    self.end_para();
                    self.close_lists(0);
                    self.raw(n);
                }
                K::Equation if n.children().any(|c| c.kind() == K::Space && c.leaf_text().contains('\n')) || is_display_math(n) => {
                    self.end_para();
                    self.close_lists(0);
                    self.out.push_str(&format!("<pre><code class=\"language-math\">{}</code></pre>", esc(n.full_text().trim_matches('$').trim())));
                }
                K::Space => {
                    if self.para {
                        self.out.push(' ');
                    }
                }
                K::Parbreak => {
                    self.end_para();
                }
                K::Heading => {
                    self.end_para();
                    self.close_lists(0);
                    let level = n.children().find(|c| c.kind() == K::HeadingMarker).map_or(1, |m| m.leaf_text().len()).clamp(1, 6);
                    self.out.push_str(&format!("<h{level}>"));
                    let start = self.out.len();
                    self.para = true;
                    for c in n.children().filter(|c| c.kind() == K::Markup) {
                        self.markup(c);
                    }
                    self.para = false;
                    if level == 1 && self.title.is_none() {
                        self.title = Some(strip_tags(&self.out[start..]));
                    }
                    self.out.push_str(&format!("</h{level}>"));
                }
                K::ListItem | K::EnumItem | K::TermItem => {
                    self.end_para();
                    let ordered = n.kind() == K::EnumItem;
                    // Items at this markup level share one list; a nested list is
                    // inside the item's own markup.
                    if self.lists.last() != Some(&ordered) || self.lists.is_empty() {
                        self.close_lists(self.lists.len().saturating_sub(usize::from(!self.lists.is_empty())));
                        self.lists.push(ordered);
                        self.out.push_str(if ordered { "<ol>" } else { "<ul>" });
                    }
                    let saved = std::mem::take(&mut self.lists);
                    self.out.push_str("<li>");
                    let markups: Vec<_> = n.children().filter(|c| c.kind() == K::Markup).collect();
                    for (k, m) in markups.iter().enumerate() {
                        if k == 1 {
                            self.end_para();
                            self.out.push_str("<p>");
                            self.para = true;
                        }
                        if k == 0 && n.kind() == K::TermItem {
                            self.inline();
                            self.out.push_str("<strong>");
                            self.markup(m);
                            self.out.push_str("</strong>");
                        } else {
                            self.markup(m);
                        }
                    }
                    self.end_para();
                    self.close_lists(0);
                    self.lists = saved;
                    self.out.push_str("</li>");
                }
                K::Hash => {
                    // Embedded code: the next node is the expression.
                    if let Some(expr) = kids.get(i + 1) {
                        self.code(expr);
                        i += 1;
                    }
                }
                K::FuncCall | K::ContentBlock => self.code(n),
                K::LineComment | K::BlockComment | K::Label | K::Error => {}
                _ => self.leafish(n),
            }
            i += 1;
        }
        self.depth -= 1;
    }

    fn is_block_raw(&self, n: &typst_syntax::SyntaxNode) -> bool {
        n.kind() == typst_syntax::SyntaxKind::Raw && n.children().next().is_some_and(|d| d.leaf_text().len() >= 3) && n.full_text().contains('\n')
    }

    /// Inline content: text, marks, links, inline raw and math.
    fn leafish(&mut self, n: &typst_syntax::SyntaxNode) {
        use typst_syntax::SyntaxKind as K;
        match n.kind() {
            K::Text => {
                self.inline();
                self.out.push_str(&esc(n.leaf_text()));
            }
            K::Escape => {
                self.inline();
                let t = n.leaf_text().trim_start_matches('\\');
                let c = t.strip_prefix("u{").and_then(|h| h.strip_suffix('}')).and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32);
                self.out.push_str(&esc(&c.map_or_else(|| t.to_string(), String::from)));
            }
            K::Shorthand => {
                self.inline();
                self.out.push_str(match n.leaf_text().as_str() {
                    "~" => "\u{a0}",
                    "---" => "—",
                    "--" => "–",
                    "..." => "…",
                    "-?" => "\u{ad}",
                    "-" => "−",
                    other => other,
                });
            }
            K::SmartQuote => {
                self.inline();
                self.out.push_str(if n.leaf_text() == "'" { "’" } else { "\u{201d}" });
            }
            K::Linebreak => {
                self.inline();
                self.out.push_str("<br>");
            }
            K::Strong | K::Emph => {
                self.inline();
                let tag = if n.kind() == K::Strong { "strong" } else { "em" };
                self.out.push_str(&format!("<{tag}>"));
                for c in n.children().filter(|c| c.kind() == K::Markup) {
                    self.markup(c);
                }
                self.out.push_str(&format!("</{tag}>"));
            }
            K::Link => {
                self.inline();
                let url = n.leaf_text();
                self.out.push_str(&format!("<a href=\"{0}\">{0}</a>", esc(url)));
            }
            K::Ref => {
                self.inline();
                self.out.push_str(&esc(&n.full_text()));
            }
            K::Raw => self.raw(n),
            K::Equation => {
                self.inline();
                self.out.push_str(&format!("<code>{}</code>", esc(n.full_text().trim_matches('$').trim())));
            }
            K::Markup => self.markup(n),
            _ => {}
        }
    }

    fn raw(&mut self, n: &typst_syntax::SyntaxNode) {
        use typst_syntax::SyntaxKind as K;
        let lang = n.children().find(|c| c.kind() == K::RawLang).map(|l| l.leaf_text().to_string());
        let body: String = n.children().filter(|c| matches!(c.kind(), K::Text | K::RawTrimmed)).map(|c| if c.kind() == K::RawTrimmed { if c.leaf_text().contains('\n') { "\n" } else { "" } } else { c.leaf_text().as_str() }).collect();
        if self.is_block_raw(n) {
            let body = body.trim_matches('\n');
            match lang.filter(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(c))) {
                Some(l) => self.out.push_str(&format!("<pre><code class=\"language-{l}\">{}</code></pre>", esc(body))),
                None => self.out.push_str(&format!("<pre>{}</pre>", esc(body))),
            }
        } else {
            self.inline();
            self.out.push_str(&format!("<code>{}</code>", esc(&body)));
        }
    }

    /// An embedded expression: what a reader would see of it.
    fn code(&mut self, n: &typst_syntax::SyntaxNode) {
        use typst_syntax::SyntaxKind as K;
        self.depth += 1;
        if self.depth > 200 {
            self.depth -= 1;
            return;
        }
        match n.kind() {
            K::ContentBlock => {
                for c in n.children().filter(|c| c.kind() == K::Markup) {
                    self.markup(c);
                }
            }
            K::Str => {
                self.inline();
                self.out.push_str(&esc(n.leaf_text().trim_matches('"')));
            }
            K::FuncCall => self.call(n),
            K::CodeBlock => {
                // `{ … }`: its content blocks only.
                for c in n.children() {
                    if c.kind() == K::Code {
                        for e in c.children() {
                            if matches!(e.kind(), K::FuncCall | K::ContentBlock) {
                                self.code(e);
                            }
                        }
                    }
                }
            }
            K::Conditional | K::ForLoop | K::WhileLoop | K::Contextual | K::Parenthesized => {
                for c in n.children() {
                    if matches!(c.kind(), K::ContentBlock | K::FuncCall | K::CodeBlock) {
                        self.code(c);
                    }
                }
            }
            // Let, set, show, import, include, identifiers and values: code, not text.
            _ => {}
        }
        self.depth -= 1;
    }

    /// A function call: the built-in elements a document is made of, by name; any
    /// other call's content arguments.
    fn call(&mut self, n: &typst_syntax::SyntaxNode) {
        use typst_syntax::SyntaxKind as K;
        let callee = n.children().next().map(|c| c.full_text().to_string()).unwrap_or_default();
        let name = callee.rsplit('.').next().unwrap_or("").to_string();
        let Some(args) = n.children().find(|c| c.kind() == K::Args) else { return };
        let positional: Vec<_> = args.children().filter(|c| !matches!(c.kind(), K::LeftParen | K::RightParen | K::Comma | K::Space | K::Named | K::Spread | K::LineComment | K::BlockComment)).collect();
        let named = |key: &str| args.children().filter(|c| c.kind() == K::Named).find(|c| c.children().next().is_some_and(|k| k.leaf_text() == key)).and_then(|c| c.children().rfind(|x| !matches!(x.kind(), K::Ident | K::Colon | K::Space)));
        let str_arg = |i: usize| positional.get(i).filter(|a| a.kind() == K::Str).map(|a| a.leaf_text().trim_matches('"').to_string());
        let wrap = |t: &mut Typst, tag: &str| {
            t.inline();
            t.out.push_str(&format!("<{tag}>"));
            positional.iter().for_each(|a| t.code(a));
            t.out.push_str(&format!("</{tag}>"));
        };
        match name.as_str() {
            "strong" => wrap(self, "strong"),
            "emph" => wrap(self, "em"),
            "underline" => wrap(self, "u"),
            "strike" => wrap(self, "s"),
            "sub" => wrap(self, "sub"),
            "super" => wrap(self, "sup"),
            "raw" => {
                if let Some(s) = str_arg(0) {
                    self.inline();
                    self.out.push_str(&format!("<code>{}</code>", esc(&s)));
                }
            }
            "link" => {
                let url = str_arg(0).unwrap_or_default();
                self.inline();
                self.out.push_str(&format!("<a href=\"{}\">", esc(&url)));
                if positional.len() > 1 {
                    positional[1..].iter().for_each(|a| self.code(a));
                } else {
                    self.out.push_str(&esc(&url));
                }
                self.out.push_str("</a>");
            }
            "image" => {
                if let Some(path) = str_arg(0) {
                    let alt = named("alt").map(|a| a.leaf_text().trim_matches('"').to_string()).unwrap_or_default();
                    self.out.push_str(&format!("<img src=\"{}\" alt=\"{}\">", esc(&path), esc(&alt)));
                }
            }
            "heading" | "title" => {
                self.end_para();
                self.close_lists(0);
                let level = named("level").and_then(|l| l.leaf_text().parse::<usize>().ok()).unwrap_or(1).clamp(1, 6);
                self.out.push_str(&format!("<h{level}>"));
                self.para = true;
                positional.iter().for_each(|a| self.code(a));
                self.para = false;
                self.out.push_str(&format!("</h{level}>"));
            }
            "quote" => {
                self.end_para();
                self.out.push_str("<blockquote>");
                positional.iter().for_each(|a| self.code(a));
                self.end_para();
                self.out.push_str("</blockquote>");
            }
            "footnote" => {
                self.inline();
                self.out.push_str("<sup>[");
                positional.iter().for_each(|a| self.code(a));
                self.out.push_str("]</sup>");
            }
            "table" | "grid" => {
                self.end_para();
                self.close_lists(0);
                let columns = named("columns").map_or(1, |c| match c.kind() {
                    K::Int => c.leaf_text().parse().unwrap_or(1),
                    K::Array => c.children().filter(|x| !matches!(x.kind(), K::LeftParen | K::RightParen | K::Comma | K::Space)).count().max(1),
                    _ => 1,
                });
                let cells: Vec<_> = positional.iter().filter(|a| matches!(a.kind(), K::ContentBlock | K::Str | K::FuncCall)).collect();
                self.out.push_str("<table>");
                for row in cells.chunks(columns.clamp(1, 1000)) {
                    self.out.push_str("<tr>");
                    for cell in row {
                        self.out.push_str("<td>");
                        self.para = true;
                        self.code(cell);
                        self.para = false;
                        self.out.push_str("</td>");
                    }
                    self.out.push_str("</tr>");
                }
                self.out.push_str("</table>");
            }
            "figure" => {
                positional.iter().for_each(|a| self.code(a));
                if let Some(c) = named("caption") {
                    self.end_para();
                    self.out.push_str("<p><em>");
                    self.para = true;
                    self.code(c);
                    self.para = false;
                    self.out.push_str("</em></p>");
                }
            }
            "pagebreak" | "colbreak" | "line" => {}
            _ => {
                for a in positional.iter().filter(|a| matches!(a.kind(), K::ContentBlock | K::FuncCall)) {
                    self.code(a);
                }
            }
        }
    }
}

/// `$ x $` (with spaces inside the dollars) is display math in Typst.
fn is_display_math(n: &typst_syntax::SyntaxNode) -> bool {
    let t = n.full_text();
    t.len() > 2 && t.starts_with("$ ") && t.ends_with(" $")
}


// ---------------------------------------------------------------- Textile

markup!(
    /// Textile (Redmine's and Textpattern's markup), read by `markup-lite`: headings,
    /// paragraphs, quotes, code, lists, tables, links, images and phrase marks.
    TextileReader, TEXTILE, "textile", "Textile", "Textile document", ["textile"], ["text/x-textile"], textile
);

fn textile(text: &str) -> Result<Rendered> {
    Ok(markup_lite::textile(text).into())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn rst_blocks(text: &str) -> String {
        let doc = RstReader.open(&FileSource::from_bytes("t.rst", text.as_bytes().to_vec()), &ParseContext::new()).unwrap();
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        format!("{:?}", f.blocks(0..100).unwrap())
    }

    #[test]
    fn an_rst_option_the_parser_refuses_is_left_out_not_the_document() {
        let doc = RstReader.open(&FileSource::from_bytes("t.rst", b"Images\n======\n\n.. image:: lalune.jpg\n   :height: 2343\n   :alt: Voyage dans la Lune\n\nAfter the image.\n".to_vec()), &ParseContext::new()).unwrap();
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..100).unwrap());
        assert!(s.contains("After the image.") && s.contains("Heading"), "{s}");
        assert!(doc.warnings().iter().any(|w| w.message.contains("directive option")), "{:?}", doc.warnings());
    }

    #[test]
    fn rst_grid_and_simple_tables() {
        let grid = rst_blocks("Before.\n\n+--------+---------+\n| Head   | Two     |\n+========+=========+\n| *a*    | b       |\n| more a |         |\n+--------+---------+\n| c      | d       |\n+--------+---------+\n\nAfter.\n");
        assert!(grid.contains("Table { header_rows: 1"), "{grid}");
        for want in ["\"Head\"", "Emphasis([Text(\"a\")])", "more a", "\"d\"", "After."] {
            assert!(grid.contains(want), "{want} in {grid}");
        }
        assert!(!grid.contains("+---"), "{grid}");
        let simple = rst_blocks("=====  =====\nA      B\n=====  =====\n1      one\n       still one\n2      two\n=====  =====\n\nAfter.\n");
        assert!(simple.contains("Table { header_rows: 1"), "{simple}");
        for want in ["\"A\"", "one still one", "\"two\"", "After."] {
            assert!(simple.contains(want), "{want} in {simple}");
        }
        // A table directive's table, its caption before it.
        let dir = rst_blocks("Intro.\n\n.. table:: The caption.\n\n   =====  =====\n   A      B\n   =====  =====\n   1      2\n   =====  =====\n\nAfter.\n");
        assert!(dir.contains("Table { header_rows: 1") && dir.contains("The caption.") && dir.contains("After."), "{dir}");
        let list = rst_blocks(".. list-table:: Frozen\n   :header-rows: 1\n   :widths: 1 2\n\n   * - Name\n     - Notes\n   * - *one*\n     - first line\n       still first\n   * - two\n     -\n\nAfter.\n");
        assert!(list.contains("Table { header_rows: 1") && list.contains("Emphasis([Text(\"one\")])") && list.contains("first line still first") && list.contains("Frozen"), "{list}");
        assert!(!list.contains("widths"), "{list}");
        // In a literal block, a table is code.
        let lit = rst_blocks("Example::\n\n    +---+---+\n    | a | b |\n    +---+---+\n\nDone.\n");
        assert!(!lit.contains("Table {") && lit.contains("Code { language: None, text: \"+---+---+\\n| a | b |\\n+---+---+\" }"), "{lit}");
        assert!(lit.contains("Text(\"Example:\")") && !lit.contains("Example::"), "{lit}");
        let lone = rst_blocks("Text ::\n\n  x = 1\n\n::\n\n  y = 2\n\nEnd.\n");
        assert!(lone.contains("Text(\"Text\")") && lone.contains("\"x = 1\"") && lone.contains("\"y = 2\"") && !lone.contains("::"), "{lone}");
    }
    use libfileparser_core::Reader;

    fn blocks(reader: &dyn Reader, name: &str, text: &str) -> (Document, String) {
        let doc = reader.open(&FileSource::from_bytes(name, text.as_bytes().to_vec()), &ParseContext::new()).unwrap();
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..1000).unwrap());
        (doc, s)
    }

    fn has(s: &str, wants: &[&str]) {
        for w in wants {
            assert!(s.contains(w), "{w} in {s}");
        }
    }

    #[test]
    fn rst() {
        let (doc, s) = blocks(&RstReader, "a.rst", "Title\n=====\n\nSome *em* and **strong** and ``code`` [#]_.\n\nSub\n---\n\n* one\n* two\n\n.. code:: python\n\n   print(1)\n\n`Link <https://x.org>`_\n\n.. note:: Careful\n\n.. [#] foot\n\n.. image:: a.png\n   :alt: Alt\n");
        has(&s, &["Heading { level: 1", "Heading { level: 2", "Emphasis", "Strong", "Code(\"code\")", "[1]", "List", "language: Some(\"python\")", "print(1)", "https://x.org", "Note", "Careful", "foot", "a.png"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Title"));
    }

    /// Found by fuzzing: one long line took rst_parser tens of seconds.
    #[test]
    fn rst_long_lines_are_kept_and_quick() {
        let long = "word ".repeat(2000);
        let text = format!("Title\n=====\n\nShort *em*.\n\n{long}\n\n::\n\n    {long}\n");
        let t = std::time::Instant::now();
        let (doc, s) = blocks(&RstReader, "l.rst", &text);
        assert!(t.elapsed().as_secs() < 3, "{:?}", t.elapsed());
        assert!(s.contains("Emphasis") && !s.contains("lfplongline"), "{}", &s[..300]);
        assert!(s.matches("word word").count() >= 2);
        assert!(s.contains("Code {"), "the literal block keeps its long line: {}", &s[..200]);
        assert!(doc.warnings()[0].message.contains("2 line(s)"));
    }

    #[test]
    fn djot() {
        let (_, s) = blocks(&DjotReader, "a.dj", "# Head\n\n_em_ and *strong* and `code` [link](https://x.org)\n\n- a\n- b\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n``` rust\nfn main() {}\n```\n");
        has(&s, &["Heading { level: 1", "Emphasis", "Strong", "Code(\"code\")", "https://x.org", "List", "Table", "language: Some(\"rust\")"]);
    }

    #[test]
    fn asciidoc() {
        let (doc, s) = blocks(&AsciidocReader, "a.adoc", "= The Title\n\n== Section\n\nSome _em_ and *strong* and `mono`.\n\n* one\n* two\n\n[source,rust]\n----\nfn main() {}\n----\n\n|===\n|a |b\n\n|1 |2\n|===\n\nNOTE: careful\n\nhttps://x.org[a link]\n");
        has(&s, &["Heading { level: 1", "The Title", "Heading { level: 2", "Emphasis", "Strong", "Code(\"mono\")", "List", "language: Some(\"rust\")", "Table", "careful", "https://x.org"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "The Title"));
    }

    #[test]
    fn asciidoc_shows_its_authors_and_revision() {
        let (_, s) = blocks(&AsciidocReader, "a.adoc", "= Document Title\nDoc Writer <doc.writer@asciidoc.org>\nv1.0, 2013-01-01\n\nBody content.\n");
        for want in ["Doc Writer", "mailto:doc.writer@asciidoc.org", "version 1.0, 2013-01-01", "Body content."] {
            assert!(s.contains(want), "{want} in {s}");
        }
    }

    #[test]
    fn asciidoc_includes_nothing() {
        let (_, s) = blocks(&AsciidocReader, "a.adoc", "include::/etc/passwd[]\n\nafter\n");
        assert!(!s.contains("root:"), "{s}");
    }

    #[test]
    fn typst() {
        let (doc, s) = blocks(&TypstReader, "a.typ", "#set page(width: 10cm)\n#let x = 1\n= Intro\nSome _em_ and *strong* and `code` -- see https://x.org.\n\n- one\n- two\n  - nested\n+ first\n\n```rust\nfn main() {}\n```\n\n$ x^2 $\n\n#strong[called] #link(\"https://y.org\")[there] #table(columns: 2, [a], [b], [c], [d])\n");
        has(&s, &["Heading { level: 1", "Intro", "Emphasis", "Strong", "Code(\"code\")", "–", "https://x.org", "List { ordered: false", "nested", "List { ordered: true", "language: Some(\"rust\")", "x^2", "called", "https://y.org", "Table"]);
        assert!(!s.contains("width") && !s.contains("let x"), "{s}");
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Intro"));
    }

    #[test]
    fn textile() {
        let (doc, s) = blocks(&TextileReader, "a.textile", "h1. Head\n\nSome _em_ and *strong* and @code@ and \"a link\":https://x.org.\n\n* one\n** nested\n* two\n# first\n\nbq. quoted\n\nbc. let x = 1;\n\n|_. a |_. b |\n| 1 | 2 |\n\n!pic.png(Alt)!\n");
        has(&s, &["Heading { level: 1", "Emphasis", "Strong", "Code(\"code\")", "https://x.org", "List { ordered: false", "nested", "List { ordered: true", "Quote", "let x = 1;", "Table { header_rows: 1", "pic.png"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Head"));
    }

}


