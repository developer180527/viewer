//! Localization: gettext PO and POT (as text, read by range) and MO (compiled), XLIFF
//! 1.2 and 2.x, TMX, .NET RESX, Apple `.strings` and Flutter ARB, each a table of
//! messages: what is said, what it says translated, and what helps translate it.

use std::sync::Arc;

use libfileparser_core::{Cell, Content, Document, FileSource, Limit, OwnedTable, ParseContext, ParseError, Result, Role, ValueKind};

use crate::json::{self, Dialect};
use crate::lines::{self, Line, LineTable, RowLines, columns, text};
use crate::nodes::{self, NodeTable, SAMPLE, child, format, local, named, value};
use crate::tree::{DataTree, TOP};

// ---------------------------------------------------------------- PO

/// A PO string's lines, `"…"` each, joined and unescaped.
fn po_string(parts: &[&str]) -> String {
    let mut out = String::new();
    for p in parts {
        let p = p.trim();
        let inner = p.strip_prefix('"').and_then(|p| p.strip_suffix('"')).unwrap_or(p);
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(o) => out.push(o),
                None => {}
            }
        }
    }
    out
}

/// One PO entry: context, source, plural, translations, flags, references, comments.
fn po_entry(lines: &[String]) -> [Cell; 7] {
    let (mut ctx, mut id, mut plural) = (Vec::new(), Vec::new(), Vec::new());
    let mut strs: Vec<Vec<&str>> = Vec::new();
    let (mut flags, mut refs, mut notes) = (Vec::new(), Vec::new(), Vec::new());
    // Which string continuation lines belong to.
    let mut target = 0u8; // 1 ctx, 2 id, 3 plural, 4 msgstr
    for line in lines {
        let l = line.trim();
        // Obsolete entries keep their shape behind `#~`.
        let l = l.strip_prefix("#~").map_or(l, str::trim_start);
        if let Some(f) = l.strip_prefix("#,") {
            flags.extend(f.split(',').map(str::trim).filter(|f| !f.is_empty()));
        } else if let Some(r) = l.strip_prefix("#:") {
            refs.push(r.trim());
        } else if let Some(n) = l.strip_prefix("#.").or_else(|| l.strip_prefix("# ")) {
            notes.push(n.trim());
        } else if l.starts_with('#') {
        } else if let Some(s) = l.strip_prefix("msgctxt") {
            ctx.push(s);
            target = 1;
        } else if let Some(s) = l.strip_prefix("msgid_plural") {
            plural.push(s);
            target = 3;
        } else if let Some(s) = l.strip_prefix("msgid") {
            id.push(s);
            target = 2;
        } else if let Some(s) = l.strip_prefix("msgstr") {
            // `msgstr "…"` or `msgstr[n] "…"`.
            let s = s.trim_start();
            let s = if s.starts_with('[') { s.split_once(']').map_or(s, |(_, r)| r) } else { s };
            strs.push(vec![s]);
            target = 4;
        } else if l.starts_with('"') {
            match target {
                1 => ctx.push(l),
                2 => id.push(l),
                3 => plural.push(l),
                4 => {
                    if let Some(last) = strs.last_mut() {
                        last.push(l);
                    }
                }
                _ => {}
            }
        }
    }
    let translation = strs.iter().map(|s| po_string(s)).collect::<Vec<_>>();
    let opt = |v: &[&str]| if v.is_empty() { Cell::Null } else { text(&po_string(v)) };
    let joined = |v: &[&str], sep: &str| if v.is_empty() { Cell::Null } else { Cell::Text(v.join(sep)) };
    [
        opt(&ctx),
        Cell::Text(po_string(&id)),
        opt(&plural),
        if translation.iter().all(String::is_empty) { Cell::Null } else { Cell::Text(translation.join("\n")) },
        joined(&flags, ", "),
        joined(&refs, " "),
        joined(&notes, "\n"),
    ]
}

pub(crate) fn po(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    // An entry starts on the first non-blank line after a blank one.
    let mut after_blank = true;
    let mut msgids = 0usize;
    let index = lines::index(src, "po", cx, |_, line| {
        let blank = line.trim().is_empty();
        if line.trim_start().trim_start_matches("#~").trim_start().starts_with("msgid ") {
            msgids += 1;
        }
        let start = !blank && after_blank;
        after_blank = blank;
        if start { Line::Start } else { Line::Within }
    })?;
    if msgids == 0 {
        return Err(ParseError::corrupt("PO: no msgid"));
    }
    let table = LineTable::new(
        index,
        columns(&[
            ("Context", ValueKind::Text),
            ("Source", ValueKind::Text),
            ("Plural", ValueKind::Text),
            ("Translation", ValueKind::Text),
            ("Flags", ValueKind::Text),
            ("References", ValueKind::Text),
            ("Comments", ValueKind::Text),
        ]),
        |_, lines| po_entry(lines).to_vec(),
    );
    // The header entry (an empty msgid) says the language and the plural rule.
    let header = table.records(0..table.len().min(1))?.into_iter().next().map(|r| po_entry(&r));
    let template = src.ext().as_deref() == Some("pot");
    let mut doc = lines::document("po", if template { "gettext template (POT)" } else { "gettext catalog (PO)" }, "Messages", table, src, "po");
    if let Some([_, Cell::Text(id), _, Cell::Text(h), ..]) = header
        && id.is_empty()
    {
        for line in h.lines() {
            if let Some((k, v)) = line.split_once(':')
                && matches!(k.trim(), "Language" | "Plural-Forms" | "Project-Id-Version" | "PO-Revision-Date")
            {
                doc = doc.with_meta(k.trim(), v.trim());
            }
        }
    }
    Ok(doc)
}

// ---------------------------------------------------------------- MO

pub(crate) fn looks_like_mo(header: &[u8]) -> bool {
    header.starts_with(&[0xde, 0x12, 0x04, 0x95]) || header.starts_with(&[0x95, 0x04, 0x12, 0xde])
}

pub(crate) fn mo(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let b = src.bytes();
    let le = b.starts_with(&[0xde, 0x12, 0x04, 0x95]);
    let u32_at = |at: usize| -> Option<u32> {
        let w: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(w) } else { u32::from_be_bytes(w) })
    };
    let bad = || ParseError::corrupt("MO: damaged");
    let n = u32_at(8).ok_or_else(bad)? as usize;
    let (originals, translations) = (u32_at(12).ok_or_else(bad)? as usize, u32_at(16).ok_or_else(bad)? as usize);
    cx.limits.check(Limit::Rows, n as u64)?;
    let string = |table: usize, i: usize| -> Option<&[u8]> {
        let len = u32_at(table + i * 8)? as usize;
        let off = u32_at(table + i * 8 + 4)? as usize;
        b.get(off..off.checked_add(len)?)
    };
    let mut rows = Vec::with_capacity(n);
    let mut meta = Vec::new();
    for i in 0..n {
        let (orig, tr) = (string(originals, i).ok_or_else(bad)?, string(translations, i).ok_or_else(bad)?);
        let orig = String::from_utf8_lossy(orig);
        let tr = String::from_utf8_lossy(tr);
        // `context\x04msgid`, and `msgid\0plural`; translations `form0\0form1`.
        let (ctx, rest) = orig.split_once('\u{4}').map_or((None, orig.as_ref()), |(c, r)| (Some(c), r));
        let (id, plural) = rest.split_once('\0').map_or((rest, None), |(a, p)| (a, Some(p)));
        if id.is_empty() {
            meta = tr.lines().filter_map(|l| l.split_once(':')).filter(|(k, _)| matches!(k.trim(), "Language" | "Plural-Forms" | "Project-Id-Version")).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
        }
        rows.push(vec![ctx.map_or(Cell::Null, text), Cell::Text(id.to_string()), plural.map_or(Cell::Null, text), Cell::Text(tr.replace('\0', "\n"))]);
    }
    let table = OwnedTable::new(columns(&[("Context", ValueKind::Text), ("Source", ValueKind::Text), ("Plural", ValueKind::Text), ("Translation", ValueKind::Text)]), rows);
    let mut doc = Document::new(format("mo", "gettext compiled catalog (MO)")).with_facet(Role::Primary, "Messages", Content::Table(Arc::new(table))).with_meta("Messages", n);
    for (k, v) in meta {
        doc = doc.with_meta(k, v);
    }
    Ok(doc.with_file_facets(src, None))
}

// ---------------------------------------------------------------- XML formats

/// All the text below `id`, inline elements included: XLIFF's `<g>`, `<ph>`.
fn inner_text(t: &DataTree, id: u32) -> String {
    let mut out = String::new();
    for n in t.subtree(id) {
        let label = t.label(n);
        if label.starts_with('@') {
            continue;
        }
        if t.child_ids(n).iter().all(|&c| t.label(c).starts_with('@')) || label == "#text" {
            out.push_str(t.value(n));
        }
    }
    out
}

fn inner(t: &DataTree, id: Option<u32>) -> Cell {
    id.map_or(Cell::Null, |id| text(&inner_text(t, id)))
}

pub(crate) fn looks_like_xliff(header: &[u8]) -> bool {
    memchr::memmem::find(header, b"<xliff").is_some()
}

pub(crate) fn xliff(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().iter().copied().find(|&r| local(tree.label(r)) == "xliff").ok_or_else(|| ParseError::corrupt("XLIFF: no <xliff>"))?;
    let version = child(&tree, root, "@version").map(|v| tree.value(v).to_string()).unwrap_or_default();
    // 1.2: trans-unit; 2.x: unit (its segments' sources and targets).
    let mut units = named(&tree, root, "trans-unit");
    units.extend(named(&tree, root, "unit"));
    let langs = named(&tree, root, "file")
        .first()
        .map(|&f| (child(&tree, f, "@source-language").or_else(|| child(&tree, root, "@srcLang")), child(&tree, f, "@target-language").or_else(|| child(&tree, root, "@trgLang"))))
        .map(|(s, t)| (s.map(|s| tree.value(s).to_string()), t.map(|t| tree.value(t).to_string())))
        .unwrap_or_default();
    let tree = Arc::new(tree);
    let part = |t: &DataTree, row: u32, name: &'static str| -> Option<u32> { child(t, row, name).or_else(|| child(t, row, "segment").and_then(|s| child(t, s, name))) };
    let table = NodeTable::new(tree, units.into())
        .column("ID", ValueKind::Text, |t, row| value(t, child(t, row, "@id").or_else(|| child(t, row, "@resname"))))
        .column("Source", ValueKind::Text, move |t, row| inner(t, part(t, row, "source")))
        .column("Target", ValueKind::Text, move |t, row| inner(t, part(t, row, "target")))
        .column("State", ValueKind::Text, move |t, row| {
            let target = part(t, row, "target");
            value(t, target.and_then(|g| child(t, g, "@state")).or_else(|| child(t, row, "segment").and_then(|s| child(t, s, "@state"))))
        })
        .column("Note", ValueKind::Text, |t, row| inner(t, child(t, row, "note").or_else(|| child(t, row, "notes").and_then(|n| child(t, n, "note")))))
        .column("File", ValueKind::Text, |t, row| {
            let mut at = t.parent(row);
            while let Some(p) = at {
                if local(t.label(p)) == "file" {
                    return value(t, child(t, p, "@original").or_else(|| child(t, p, "@id")));
                }
                at = t.parent(p);
            }
            Cell::Null
        });
    let mut doc = nodes::document(format("xliff", format!("XLIFF {version}").trim()), "Units", table, src, Some("xml"), true);
    if let Some(s) = langs.0 {
        doc = doc.with_meta("Source language", s);
    }
    if let Some(t) = langs.1 {
        doc = doc.with_meta("Target language", t);
    }
    Ok(doc)
}

pub(crate) fn tmx(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().iter().copied().find(|&r| local(tree.label(r)) == "tmx").ok_or_else(|| ParseError::corrupt("TMX: no <tmx>"))?;
    let units = named(&tree, root, "tu");
    let lang = |t: &DataTree, tuv: u32| t.child(tuv, "@xml:lang").or_else(|| t.child(tuv, "@lang")).map(|l| t.value(l).to_string());
    // A column per language of the first units.
    let mut langs: Vec<String> = Vec::new();
    for &tu in units.iter().take(SAMPLE) {
        for &tuv in tree.child_ids(tu).iter().filter(|&&c| local(tree.label(c)) == "tuv") {
            if let Some(l) = lang(&tree, tuv)
                && !langs.contains(&l)
            {
                langs.push(l);
            }
        }
    }
    let tree = Arc::new(tree);
    let mut table = NodeTable::new(tree, units.into()).column("ID", ValueKind::Text, |t, row| value(t, child(t, row, "@tuid")));
    for l in langs {
        let key = l.clone();
        table = table.column(&l, ValueKind::Text, move |t, row| {
            let tuv = t.child_ids(row).iter().copied().find(|&c| local(t.label(c)) == "tuv" && lang(t, c).as_deref() == Some(&key));
            inner(t, tuv.and_then(|v| child(t, v, "seg")))
        });
    }
    let table = table.column("Note", ValueKind::Text, |t, row| inner(t, child(t, row, "note")));
    Ok(nodes::document(format("tmx", "Translation memory (TMX)"), "Units", table, src, Some("xml"), true))
}

pub(crate) fn resx(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().first().copied().ok_or_else(|| ParseError::corrupt("RESX: empty"))?;
    let data: Vec<u32> = tree.child_ids(root).iter().copied().filter(|&c| tree.label(c) == "data").collect();
    let tree = Arc::new(tree);
    let table = NodeTable::new(tree, data.into())
        .column("Name", ValueKind::Text, |t, row| value(t, child(t, row, "@name")))
        .column("Value", ValueKind::Text, |t, row| value(t, child(t, row, "value")))
        .column("Comment", ValueKind::Text, |t, row| value(t, child(t, row, "comment")))
        .column("Type", ValueKind::Text, |t, row| value(t, child(t, row, "@type")));
    Ok(nodes::document(format("resx", ".NET resources (RESX)"), "Resources", table, src, Some("xml"), true))
}

// ---------------------------------------------------------------- Apple .strings

/// The file as text: UTF-16 with a byte-order mark (as Xcode writes them), else UTF-8.
fn utf16_or_8(b: &[u8]) -> String {
    let utf16 = |le: bool| {
        let units: Vec<u16> = b[2..].as_chunks::<2>().0.iter().map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    match b {
        [0xff, 0xfe, ..] => utf16(true),
        [0xfe, 0xff, ..] => utf16(false),
        _ => String::from_utf8_lossy(b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b)).into_owned(),
    }
}

/// `"key" = "value";` pairs, each with the comment before it and its lines.
/// Key, value, the comment before it, its lines.
type StringsEntry = (String, String, Option<String>, std::ops::Range<u64>);

fn strings_entries(text: &str) -> Result<Vec<StringsEntry>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut comment: Option<String> = None;
    let line = |at: usize| memchr::memchr_iter(b'\n', &b[..at]).count() as u64;
    let token = |i: &mut usize| -> Option<String> {
        if b.get(*i) == Some(&b'"') {
            let mut out = String::new();
            let mut j = *i + 1;
            while j < b.len() && b[j] != b'"' {
                if b[j] == b'\\' && j + 1 < b.len() {
                    j += 1;
                    out.push(match b[j] {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        c => c as char,
                    });
                    j += 1;
                } else {
                    let ch = text[j..].chars().next()?;
                    out.push(ch);
                    j += ch.len_utf8();
                }
            }
            *i = j + 1;
            Some(out)
        } else {
            let start = *i;
            while *i < b.len() && (b[*i].is_ascii_alphanumeric() || matches!(b[*i], b'_' | b'.' | b'-')) {
                *i += 1;
            }
            (*i > start).then(|| text[start..*i].to_string())
        }
    };
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_whitespace() => i += 1,
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = memchr::memmem::find(&b[i + 2..], b"*/").map_or(b.len(), |p| i + 2 + p);
                comment = Some(text[i + 2..end].trim().to_string());
                i = (end + 2).min(b.len());
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = memchr::memchr(b'\n', &b[i..]).map_or(b.len(), |p| i + p);
                comment = Some(text[i + 2..end].trim().to_string());
                i = end;
            }
            _ => {
                let start = i;
                let key = token(&mut i).ok_or_else(|| ParseError::corrupt_at("strings: expected a key", libfileparser_core::Position::in_text(b, start)))?;
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                // `"key";` alone means the value is the key.
                let value = if b.get(i) == Some(&b'=') {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    token(&mut i).ok_or_else(|| ParseError::corrupt_at("strings: expected a value", libfileparser_core::Position::in_text(b, i)))?
                } else {
                    key.clone()
                };
                while i < b.len() && b[i] != b';' && b[i] != b'\n' {
                    i += 1;
                }
                i += 1;
                out.push((key, value, comment.take(), line(start)..line(i.min(b.len()).saturating_sub(1)) + 1));
            }
        }
    }
    Ok(out)
}

pub(crate) fn strings(src: &FileSource) -> Result<Document> {
    let text = utf16_or_8(src.bytes());
    let entries = strings_entries(&text)?;
    let utf16 = matches!(src.bytes(), [0xff, 0xfe, ..] | [0xfe, 0xff, ..]);
    let lines: Vec<_> = entries.iter().map(|e| if utf16 { 0..0 } else { e.3.clone() }).collect();
    let n = entries.len();
    let rows = entries.into_iter().map(|(k, v, c, _)| vec![Cell::Text(k), Cell::Text(v), c.map_or(Cell::Null, Cell::Text)]).collect();
    let table = OwnedTable::new(columns(&[("Key", ValueKind::Text), ("Value", ValueKind::Text), ("Comment", ValueKind::Text)]), rows);
    let doc = Document::new(format("strings", "Strings file"))
        .with_facet(Role::Primary, "Strings", Content::Table(Arc::new(table)))
        .with_meta("Strings", n)
        .with_meta("Encoding", if utf16 { "UTF-16" } else { "UTF-8" });
    // A UTF-16 file has no Source to link lines to.
    Ok(doc.with_links(RowLines(lines)).with_file_facets(src, Some("strings")))
}

// ---------------------------------------------------------------- ARB

pub(crate) fn arb(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = json::parse(src.bytes(), Dialect::Json, cx)?;
    let locale = tree.child(TOP, "@@locale").map(|l| tree.value(l).to_string());
    let messages: Vec<u32> = tree.roots().iter().copied().filter(|&r| !tree.label(r).starts_with('@')).collect();
    let tree = Arc::new(tree);
    let meta = |t: &DataTree, row: u32| t.child(TOP, &format!("@{}", t.label(row)));
    let table = NodeTable::new(tree, messages.into())
        .column("Key", ValueKind::Text, |t, row| Cell::Text(t.label(row).to_string()))
        .column("Value", ValueKind::Text, |t, row| value(t, Some(row)))
        .column("Description", ValueKind::Text, move |t, row| value(t, meta(t, row).and_then(|m| t.child(m, "description"))))
        .column("Placeholders", ValueKind::Text, move |t, row| {
            let names: Vec<&str> = meta(t, row).and_then(|m| t.child(m, "placeholders")).map(|p| t.child_ids(p).iter().map(|&c| t.label(c)).collect()).unwrap_or_default();
            if names.is_empty() { Cell::Null } else { Cell::Text(names.join(", ")) }
        });
    let mut doc = nodes::document(format("arb", "Application Resource Bundle (ARB)"), "Messages", table, src, Some("json"), true);
    if let Some(l) = locale {
        doc = doc.with_meta("Locale", l);
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn po_entries() {
        let lines: Vec<String> = ["#. greeting on the home page", "#: src/home.rs:12", "#, fuzzy, c-format", "msgctxt \"menu\"", "msgid \"Hello %s\"", "msgid_plural \"Hello all\"", "msgstr[0] \"Hallo %s\"", "msgstr[1] \"Hallo \"", "\"alle\""]
            .map(String::from)
            .to_vec();
        let e = po_entry(&lines);
        assert_eq!(e[0], Cell::Text("menu".into()));
        assert_eq!(e[1], Cell::Text("Hello %s".into()));
        assert_eq!(e[3], Cell::Text("Hallo %s\nHallo alle".into()));
        assert_eq!(e[4], Cell::Text("fuzzy, c-format".into()));
        assert_eq!(e[5], Cell::Text("src/home.rs:12".into()));
    }

    #[test]
    fn apple_strings() {
        let e = strings_entries("/* Title */\n\"title\" = \"Hello\\n\\\"you\\\"\";\nplain = \"x\";\n\"same\";\n").unwrap();
        assert_eq!((e[0].0.as_str(), e[0].1.as_str(), e[0].2.as_deref()), ("title", "Hello\n\"you\"", Some("Title")));
        assert_eq!((e[1].0.as_str(), e[2].1.as_str()), ("plain", "same"));
        let utf16: Vec<u8> = [0xff, 0xfe].into_iter().chain("\"a\" = \"b\";".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(utf16_or_8(&utf16), "\"a\" = \"b\";");
    }
}
