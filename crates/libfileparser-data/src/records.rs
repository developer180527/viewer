//! Contacts, calendars and directories: vCard and iCalendar (content lines, RFC 6350
//! and 5545) as a tree of components with a table of contacts or events; LDIF as a
//! table of directory entries, read by range.

use std::sync::Arc;

use libfileparser_core::{Cell, Column, Document, FileSource, ParseContext, ParseError, Result, ValueKind};

use crate::lines::{self, Line, LineTable, text};
use crate::nodes::{self, NodeTable, SAMPLE, format, value};
use crate::tree::{Builder, DataTree};

/// Unfolded content lines (`NAME;PARAM=V:value`), each with the line it started on.
fn content_lines(src: &str) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = Vec::new();
    for (n, line) in src.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        // A line starting with a space or tab continues the one before.
        match (line.strip_prefix([' ', '\t']), out.last_mut()) {
            (Some(rest), Some(last)) => last.0.push_str(rest),
            _ if line.is_empty() => {}
            _ => out.push((line.to_string(), n as u32)),
        }
    }
    out
}

/// `NAME;P=V;P2="a,b":value` → (name, params, value). The name loses any `group.`.
/// A content line's name, parameters and value.
type ContentLine = (String, Vec<(String, String)>, String);

fn split_line(line: &str) -> Option<ContentLine> {
    // The value starts at the first `:` outside quotes.
    let mut quoted = false;
    let colon = line.char_indices().find(|&(_, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        c == ':' && !quoted
    })?.0;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?;
    let name = name.rsplit('.').next().unwrap_or(name).to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts.filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_ascii_uppercase(), v.trim_matches('"').to_string())).collect();
    Some((name, params, value.to_string()))
}

/// Text values' escapes: `\n`, `\,`, `\;`, `\\`.
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The tree of a vCard or iCalendar file: `BEGIN:X` … `END:X` as nodes, properties
/// as their children (parameters as `@NAME`), and the components of `kinds`.
fn components(text: &str, kinds: &[&str], cx: &ParseContext) -> Result<(DataTree, Vec<u32>)> {
    let mut b = Builder::new(cx);
    let mut stack: Vec<(u32, String)> = Vec::new();
    let mut rows = Vec::new();
    for (line, n) in content_lines(text) {
        let Some((name, params, value)) = split_line(&line) else { continue };
        match name.as_str() {
            "BEGIN" => {
                let kind = value.trim().to_ascii_uppercase();
                let id = b.add(stack.last().map(|s| s.0), stack.len() + 1, kind.clone(), "", ValueKind::Group, n..n + 1)?;
                if kinds.contains(&kind.as_str()) {
                    rows.push(id);
                }
                stack.push((id, kind));
            }
            "END" => {
                if let Some(at) = stack.iter().rposition(|s| s.1.eq_ignore_ascii_case(value.trim())) {
                    for (id, _) in stack.drain(at..) {
                        b.end(id, n);
                    }
                }
            }
            _ => {
                let parent = stack.last().map(|s| s.0);
                let kind = if name.starts_with("DT") || matches!(name.as_str(), "BDAY" | "ANNIVERSARY" | "REV" | "LAST-MODIFIED" | "CREATED" | "DUE") { ValueKind::Date } else { ValueKind::Text };
                let id = b.add(parent, stack.len() + 1, name, unescape(&value), kind, n..n + 1)?;
                for (k, v) in params {
                    b.add(Some(id), stack.len() + 2, format!("@{k}"), v, ValueKind::Text, n..n + 1)?;
                }
            }
        }
    }
    Ok((b.finish()?, rows))
}

fn prop(t: &DataTree, row: u32, name: &str) -> Cell {
    value(t, t.child(row, name))
}

pub(crate) fn vcard(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let body = String::from_utf8_lossy(src.bytes());
    let (tree, cards) = components(&body, &["VCARD"], cx)?;
    if cards.is_empty() {
        return Err(ParseError::corrupt("vCard: no BEGIN:VCARD"));
    }
    let tree = Arc::new(tree);
    let table = NodeTable::new(tree, cards.into())
        .column("Name", ValueKind::Text, |t, row| match prop(t, row, "FN") {
            Cell::Null => match prop(t, row, "N") {
                // `Family;Given;Middle;Prefix;Suffix`.
                Cell::Text(n) => {
                    let p: Vec<&str> = n.split(';').collect();
                    text(&[p.get(3), p.get(1), p.get(2), p.first(), p.get(4)].into_iter().flatten().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" "))
                }
                c => c,
            },
            c => c,
        })
        .column("Email", ValueKind::Text, |t, row| prop(t, row, "EMAIL"))
        .column("Phone", ValueKind::Text, |t, row| prop(t, row, "TEL"))
        .column("Organization", ValueKind::Text, |t, row| match prop(t, row, "ORG") {
            Cell::Text(o) => text(o.trim_end_matches(';').replace(';', ", ").as_str()),
            c => c,
        })
        .column("Title", ValueKind::Text, |t, row| prop(t, row, "TITLE"))
        .column("Address", ValueKind::Text, |t, row| match prop(t, row, "ADR") {
            Cell::Text(a) => text(&a.split(';').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ")),
            c => c,
        })
        .column("Birthday", ValueKind::Date, |t, row| prop(t, row, "BDAY"))
        .column("Note", ValueKind::Text, |t, row| prop(t, row, "NOTE"));
    Ok(nodes::document(format("vcard", "vCard"), "Contacts", table, src, Some("vcard"), true))
}

pub(crate) fn icalendar(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let body = String::from_utf8_lossy(src.bytes());
    let (tree, items) = components(&body, &["VEVENT", "VTODO", "VJOURNAL", "VFREEBUSY"], cx)?;
    let calendar = tree.roots().first().copied();
    let name = calendar.map(|c| (value(&tree, tree.child(c, "X-WR-CALNAME")), value(&tree, tree.child(c, "PRODID"))));
    let tree = Arc::new(tree);
    let table = NodeTable::new(tree, items.into())
        .column("Kind", ValueKind::Text, |t, row| {
            Cell::Text(match t.label(row) {
                "VEVENT" => "Event",
                "VTODO" => "To-do",
                "VJOURNAL" => "Journal",
                _ => "Free/busy",
            }
            .into())
        })
        .column("Summary", ValueKind::Text, |t, row| prop(t, row, "SUMMARY"))
        .column("Start", ValueKind::Date, |t, row| prop(t, row, "DTSTART"))
        .column("End", ValueKind::Date, |t, row| match prop(t, row, "DTEND") {
            Cell::Null => prop(t, row, "DUE"),
            c => c,
        })
        .column("Location", ValueKind::Text, |t, row| prop(t, row, "LOCATION"))
        .column("Status", ValueKind::Text, |t, row| prop(t, row, "STATUS"))
        .column("Organizer", ValueKind::Text, |t, row| match prop(t, row, "ORGANIZER") {
            Cell::Text(o) => text(o.strip_prefix("mailto:").or_else(|| o.strip_prefix("MAILTO:")).unwrap_or(&o)),
            c => c,
        })
        .column("Repeats", ValueKind::Text, |t, row| prop(t, row, "RRULE"))
        .column("Description", ValueKind::Text, |t, row| prop(t, row, "DESCRIPTION"));
    let mut doc = nodes::document(format("icalendar", "iCalendar"), "Events", table, src, Some("ics"), true);
    if let Some((cal, prodid)) = name {
        if let Cell::Text(c) = cal {
            doc = doc.with_meta("Calendar", c);
        }
        if let Cell::Text(p) = prodid {
            doc = doc.with_meta("Made by", p);
        }
    }
    Ok(doc)
}

// ---------------------------------------------------------------- LDIF

/// Base64, as LDIF writes binary and non-ASCII values (`attr:: …`).
pub(crate) fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// An LDIF entry's attributes, folded lines joined, values decoded; repeated
/// attributes (`objectClass`) in order.
fn ldif_entry(lines: &[String]) -> Vec<(String, String)> {
    let mut joined: Vec<String> = Vec::new();
    for l in lines {
        let l = l.trim_end_matches('\r');
        match (l.strip_prefix(' '), joined.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ if l.is_empty() || l.starts_with('#') => {}
            _ => joined.push(l.to_string()),
        }
    }
    joined
        .iter()
        .filter_map(|l| {
            let (attr, rest) = l.split_once(':')?;
            let value = if let Some(b) = rest.strip_prefix(':') {
                match base64(b.trim()) {
                    Some(bytes) => String::from_utf8(bytes.clone()).unwrap_or_else(|_| format!("{} bytes", bytes.len())),
                    None => b.trim().to_string(),
                }
            } else if let Some(url) = rest.strip_prefix('<') {
                url.trim().to_string()
            } else {
                rest.trim_start().to_string()
            };
            Some((attr.trim().to_string(), value))
        })
        .collect()
}

pub(crate) fn looks_like_ldif(header: &[u8]) -> bool {
    let text = String::from_utf8_lossy(header);
    text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).is_some_and(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("version:") || l.starts_with("dn:")
    })
}

pub(crate) fn ldif(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let index = lines::index(src, "ldif", cx, |_, line| if line.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("dn:")) { Line::Start } else { Line::Within })?;
    if index.starts.is_empty() {
        return Err(ParseError::corrupt("LDIF: no dn:"));
    }
    let table = LineTable::new(index, Vec::new(), |_, _| Vec::new());
    // Columns: the attributes of the first entries, dn first.
    let mut attrs: Vec<String> = Vec::new();
    for entry in table.records(0..table.len().min(SAMPLE))? {
        for (a, _) in ldif_entry(&entry) {
            if !a.eq_ignore_ascii_case("dn") && !attrs.iter().any(|x| x.eq_ignore_ascii_case(&a)) && attrs.len() < nodes::MAX_PROPERTIES {
                attrs.push(a);
            }
        }
    }
    let mut columns = vec![Column::new("DN").with_kind(ValueKind::Text)];
    columns.extend(attrs.iter().map(|a| Column::new(a.as_str()).with_kind(ValueKind::Text)));
    let table = table.with(columns, move |_, lines| {
        let entry = ldif_entry(lines);
        let get = |name: &str| {
            let values: Vec<&str> = entry.iter().filter(|(a, _)| a.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect();
            if values.is_empty() { Cell::Null } else { Cell::Text(values.join("; ")) }
        };
        std::iter::once(get("dn")).chain(attrs.iter().map(|a| get(a))).collect()
    });
    Ok(lines::document("ldif", "LDIF", "Entries", table, src, "ldif"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_lines_unfold_and_split() {
        let l = content_lines("BEGIN:VCARD\r\nNOTE:one\r\n two\r\nEND:VCARD\r\n");
        assert_eq!(l[1], ("NOTE:onetwo".to_string(), 1));
        let (name, params, value) = split_line(r#"item1.EMAIL;TYPE="work,pref":ann@example.com"#).unwrap();
        assert_eq!((name.as_str(), params[0].1.as_str(), value.as_str()), ("EMAIL", "work,pref", "ann@example.com"));
        assert_eq!(unescape(r"a\, b\nc"), "a, b\nc");
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        let e = ldif_entry(&["dn: cn=a,dc=x".into(), "objectClass: top".into(), "objectClass: person".into(), "description:: aGVsbG8=".into(), "cn: Ann".into(), " e".into()]);
        assert_eq!(e[3], ("description".to_string(), "hello".to_string()));
        assert_eq!(e[4], ("cn".to_string(), "Anne".to_string()));
    }
}
