//! VCD (Value Change Dump, IEEE 1364): what a digital circuit's signals did over a
//! simulation. A header of `$keyword … $end` sections declares the signals (`$var
//! wire 1 ! clk $end`) in nested scopes; then `#time` marks and changes: `1!` a bit,
//! `b1010 "` a vector, `r2.5 %` a real, `sHello &` a string. It reads as a table of
//! every change, in time order, and one of the signals with how often each changed.

use std::collections::HashMap;
use std::sync::Arc;

use libfileparser_core::{Category, Cell, Content, Document, FileSource, FormatInfo, Limit, OwnedTable, ParseContext, ParseError, Result, Role, ValueKind};

use crate::lines::columns;

/// A header that starts with one of the sections a VCD begins with.
pub(crate) fn looks_like(header: &[u8]) -> bool {
    let t = String::from_utf8_lossy(&header[..header.len().min(1024)]);
    let t = t.trim_start();
    ["$date", "$version", "$timescale", "$scope", "$comment"].iter().any(|k| t.starts_with(k)) && t.contains("$end")
}

/// A declared signal.
struct Var {
    scope: String,
    name: String,
    kind: String,
    width: String,
    id: String,
}

pub(crate) fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    cx.cancel.check()?;
    let text = String::from_utf8_lossy(src.bytes());
    let mut words = text.split_ascii_whitespace();
    let (mut scopes, mut vars): (Vec<String>, Vec<Var>) = (Vec::new(), Vec::new());
    let mut header: Vec<(&str, String)> = Vec::new();
    let mut definitions_ended = false;
    // The header: sections to `$enddefinitions`.
    while let Some(w) = words.next() {
        let Some(keyword) = w.strip_prefix('$') else { continue };
        let body: Vec<&str> = words.by_ref().take_while(|x| *x != "$end").collect();
        match keyword {
            "date" | "version" | "timescale" | "comment" => header.push((match keyword {
                "date" => "Date",
                "version" => "Version",
                "timescale" => "Timescale",
                _ => "Comment",
            }, body.join(" "))),
            "scope" => scopes.push(body.get(1).copied().unwrap_or("").to_string()),
            "upscope" => {
                scopes.pop();
            }
            "var" if body.len() >= 4 => vars.push(Var {
                scope: scopes.join("."),
                // A vector's range after its name: `data [7:0]`.
                name: body[3..].join(""),
                kind: body[0].to_string(),
                width: body[1].to_string(),
                id: body[2].to_string(),
            }),
            "enddefinitions" => {
                definitions_ended = true;
                break;
            }
            _ => {}
        }
    }
    if !definitions_ended && vars.is_empty() {
        return Err(ParseError::NotThisFormat);
    }
    if vars.is_empty() {
        return Err(ParseError::corrupt("VCD: no signals declared"));
    }
    // Signals sharing an id code are one signal under several names.
    let mut by_id: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, v) in vars.iter().enumerate() {
        by_id.entry(v.id.as_str()).or_default().push(i);
    }
    let names: HashMap<&str, String> = by_id.iter().map(|(id, is)| (*id, is.iter().map(|i| if vars[*i].scope.is_empty() { vars[*i].name.clone() } else { format!("{}.{}", vars[*i].scope, vars[*i].name) }).collect::<Vec<_>>().join(", "))).collect();
    let mut counts: HashMap<&str, u64> = HashMap::new();
    let mut rows = Vec::new();
    let (mut time, mut end) = (0u64, 0u64);
    let mut unknown = 0u64;
    let mut pending_vector: Option<String> = None;
    for w in words {
        if rows.len() % 4096 == 0 {
            cx.cancel.check()?;
        }
        // A vector or real's value comes first, its id as the next word.
        if let Some(value) = pending_vector.take() {
            let id = w;
            match names.get(id) {
                Some(n) => {
                    rows.push(vec![Cell::Int(time as i64), Cell::Text(n.clone()), Cell::Text(value)]);
                    *counts.entry(id).or_default() += 1;
                }
                None => unknown += 1,
            }
            continue;
        }
        if let Some(t) = w.strip_prefix('#') {
            time = t.parse().map_err(|_| ParseError::corrupt(format!("VCD: `#{t}` isn't a time")))?;
            end = end.max(time);
            continue;
        }
        if w.starts_with('$') {
            // `$dumpvars`, `$dumpoff`, `$end` and the like frame changes; they aren't any.
            continue;
        }
        let first = w.as_bytes()[0];
        match first {
            b'b' | b'B' | b'r' | b'R' | b's' | b'S' => pending_vector = Some(w[1..].to_string()),
            b'0' | b'1' | b'x' | b'X' | b'z' | b'Z' | b'u' | b'U' | b'w' | b'W' | b'l' | b'L' | b'h' | b'H' | b'-' => {
                let id = &w[1..];
                match names.get(id) {
                    Some(n) => {
                        rows.push(vec![Cell::Int(time as i64), Cell::Text(n.clone()), Cell::Text((first as char).to_ascii_lowercase().to_string())]);
                        *counts.entry(id).or_default() += 1;
                    }
                    None => unknown += 1,
                }
            }
            _ => unknown += 1,
        }
        cx.limits.check(Limit::Rows, rows.len() as u64)?;
    }
    let change_cols = [("Time", ValueKind::Number), ("Signal", ValueKind::Text), ("Value", ValueKind::Text)];
    let signal_cols = [("Scope", ValueKind::Text), ("Name", ValueKind::Text), ("Type", ValueKind::Text), ("Width", ValueKind::Number), ("Id", ValueKind::Text), ("Changes", ValueKind::Number)];
    let signals: Vec<Vec<Cell>> = vars
        .iter()
        .map(|v| vec![Cell::Text(v.scope.clone()), Cell::Text(v.name.clone()), Cell::Text(v.kind.clone()), crate::lines::num(&v.width), Cell::Text(v.id.clone()), Cell::Int(counts.get(v.id.as_str()).copied().unwrap_or(0) as i64)])
        .collect();
    let changes = rows.len();
    let mut doc = Document::new(FormatInfo::new("vcd", "Value Change Dump", Category::Data))
        .with_facet(Role::Primary, "Changes", Content::Table(Arc::new(OwnedTable::new(columns(&change_cols), rows))))
        .with_facet(Role::Structure, "Signals", Content::Table(Arc::new(OwnedTable::new(columns(&signal_cols), signals))));
    for (k, v) in header {
        if !v.is_empty() {
            doc = doc.with_meta(k, v);
        }
    }
    doc = doc.with_meta("Signals", vars.len()).with_meta("Changes", changes).with_meta("Last time", end);
    if unknown > 0 {
        doc = doc.with_warning(libfileparser_core::Warning::new(format!("{unknown} change(s) name a signal that isn't declared, or aren't understood; they're left out.")));
    }
    Ok(doc.with_file_facets(src, Some("vcd")))
}
