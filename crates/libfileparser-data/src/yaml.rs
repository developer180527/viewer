//! YAML 1.2, every document in the file. Aliases are shown as aliases, never expanded,
//! so a file of nested aliases ("billion laughs") costs no more than its text.

use std::collections::HashMap;

use libfileparser_core::{ParseContext, ParseError, Position, Result, ValueKind};
use saphyr_parser::{Event, Parser, ScalarStyle};

use crate::tree::{Builder, DataTree};

enum Frame {
    Map {
        id: Option<u32>,
        key: Option<String>,
    },
    Seq {
        id: Option<u32>,
        n: usize,
    },
}

/// Where a value goes: its parent and label; `None` for a scalar that is a key.
fn place(
    stack: &mut [Frame],
    docs: usize,
    container: bool,
    scalar: Option<&str>,
) -> Option<(Option<u32>, String)> {
    match stack.last_mut() {
        Some(Frame::Map { id, key }) => match key.take() {
            Some(k) => Some((*id, k)),
            None => match (scalar, container) {
                (Some(s), false) => {
                    *key = Some(s.to_string());
                    None
                }
                // A key that is itself a mapping or sequence (or an alias).
                _ => {
                    *key = Some("(value)".into());
                    Some((*id, "(key)".into()))
                }
            },
        },
        Some(Frame::Seq { id, n }) => {
            *n += 1;
            Some((*id, (*n - 1).to_string()))
        }
        None => Some((None, docs.to_string())),
    }
}

/// The kind of a plain scalar, by the YAML 1.2 core schema.
fn plain_kind(v: &str) -> ValueKind {
    match v {
        "" | "~" | "null" | "Null" | "NULL" => ValueKind::Null,
        "true" | "True" | "TRUE" | "false" | "False" | "FALSE" => ValueKind::Bool,
        ".inf" | "-.inf" | "+.inf" | ".Inf" | ".INF" | "-.Inf" | "-.INF" | ".nan" | ".NaN" | ".NAN" => ValueKind::Number,
        _ if is_core_number(v) => ValueKind::Number,
        _ => ValueKind::Text,
    }
}

/// `[-+]?[0-9]+`, `0o[0-7]+`, `0x[0-9a-fA-F]+`, or a decimal float.
fn is_core_number(v: &str) -> bool {
    if let Some(h) = v.strip_prefix("0x") {
        return !h.is_empty() && h.bytes().all(|c| c.is_ascii_hexdigit());
    }
    if let Some(o) = v.strip_prefix("0o") {
        return !o.is_empty() && o.bytes().all(|c| (b'0'..=b'7').contains(&c));
    }
    let d = v.strip_prefix(['-', '+']).unwrap_or(v);
    d.starts_with(|c: char| c.is_ascii_digit() || c == '.')
        && d.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'-' | b'+'))
        && v.parse::<f64>().is_ok()
}

fn core_tag(suffix: &str) -> ValueKind {
    match suffix {
        "null" => ValueKind::Null,
        "bool" => ValueKind::Bool,
        "int" | "float" => ValueKind::Number,
        "timestamp" => ValueKind::Date,
        "binary" => ValueKind::Bytes,
        _ => ValueKind::Text,
    }
}

/// Parses `text` into a tree: one document's mapping or sequence members at the top,
/// or, for several documents, one top-level node per document.
pub(crate) fn parse(text: &str, cx: &ParseContext) -> Result<DataTree> {
    let mut b = Builder::new(cx);
    let mut stack: Vec<Frame> = Vec::new();
    // Each document's top-level node.
    let mut docs: Vec<u32> = Vec::new();
    let mut anchors: HashMap<usize, String> = HashMap::new();
    let last_line = text.lines().count().saturating_sub(1) as u32;
    for event in Parser::new_from_str(text) {
        let (event, span) = event.map_err(|e| {
            let m = e.marker();
            ParseError::corrupt_at(format!("YAML: {}", e.info()), Position::line_column(m.line() as u64, m.col() as u64 + 1))
        })?;
        // An end marker at the end of the file is on the line after the last.
        let line = (span.start.line().saturating_sub(1) as u32).min(last_line);
        match event {
            Event::Scalar(value, style, anchor, tag) => {
                let Some((parent, label)) = place(&mut stack, docs.len(), false, Some(&value))
                else {
                    continue;
                };
                let (shown, kind) = match (&tag, style) {
                    // The core schema's tags say the kind.
                    (Some(t), _) if t.handle == "tag:yaml.org,2002:" => (value.to_string(), core_tag(&t.suffix)),
                    // Others are the application's: kept with the value.
                    (Some(t), _) => (format!("{}{} {value}", t.handle, t.suffix), ValueKind::Unknown),
                    (None, ScalarStyle::Plain) => (value.to_string(), plain_kind(&value)),
                    (None, _) => (value.to_string(), ValueKind::Text),
                };
                let last = span
                    .end
                    .line()
                    .saturating_sub(1)
                    .max(span.start.line().saturating_sub(1)).min(last_line as usize) as u32;
                let id = b.add(
                    parent,
                    stack.len() + 1,
                    label.clone(),
                    shown,
                    kind,
                    line..last + 1,
                )?;
                if anchor > 0 {
                    anchors.insert(anchor, label);
                }
                if stack.is_empty() {
                    docs.push(id);
                }
            }
            Event::Alias(anchor) => {
                let Some((parent, label)) = place(&mut stack, docs.len(), false, None) else {
                    continue;
                };
                let target = anchors
                    .get(&anchor)
                    .map_or_else(|| "an anchor".to_string(), |l| format!("`{l}`"));
                let id = b.add(
                    parent,
                    stack.len() + 1,
                    label,
                    format!("alias of {target}"),
                    ValueKind::Unknown,
                    line..line + 1,
                )?;
                if stack.is_empty() {
                    docs.push(id);
                }
            }
            Event::MappingStart(anchor, _) | Event::SequenceStart(anchor, _) => {
                let map = matches!(event, Event::MappingStart(..));
                let (parent, label) = place(&mut stack, docs.len(), true, None)
                    .expect("a container always has a place");
                let kind = if map { ValueKind::Map } else { ValueKind::List };
                let id = b.add(parent, stack.len() + 1, label.clone(), "", kind, line..line + 1)?;
                if anchor > 0 {
                    anchors.insert(anchor, label);
                }
                if stack.is_empty() {
                    docs.push(id);
                }
                stack.push(if map {
                    Frame::Map {
                        id: Some(id),
                        key: None,
                    }
                } else {
                    Frame::Seq { id: Some(id), n: 0 }
                });
            }
            Event::MappingEnd | Event::SequenceEnd => {
                let (id, map) = match stack.pop() {
                    Some(Frame::Map { id, .. }) => (id, true),
                    Some(Frame::Seq { id, .. }) => (id, false),
                    None => return Err(ParseError::corrupt("YAML: an end without a start")),
                };
                if let Some(id) = id {
                    let n = b.children(id);
                    b.set_value(
                        id,
                        if map {
                            format!("{{{n}}}")
                        } else {
                            format!("[{n}]")
                        },
                    );
                    b.end(id, line);
                }
            }
            _ => {}
        }
    }
    // One document: its members at the top, as for JSON.
    if let [only] = docs[..] {
        if b.children(only) > 0 {
            b.hoist(only);
        } else {
            b.set_label(only, "value");
        }
    }
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::TreeSource;

    fn top(t: &DataTree) -> Vec<(String, String)> {
        t.children(None, 0..100)
            .unwrap()
            .into_iter()
            .map(|n| (n.label.clone(), crate::tree::shown(&n)))
            .collect()
    }

    #[test]
    fn documents_scalars_and_aliases() {
        let cx = ParseContext::new();
        let t = parse(
            "base: &b\n  x: 1\nuse: *b\nlist:\n  - 'q'\n  - !!str 2\ntext: |\n  a\n  b\n",
            &cx,
        )
        .unwrap();
        assert_eq!(
            top(&t),
            [
                ("base", "{1}"),
                ("use", "alias of `base`"),
                ("list", "[2]"),
                ("text", "\"a\\nb\\n\"")
            ]
            .map(|(a, b)| (a.to_string(), b.to_string()))
        );
        let list = t.find("/list/1").unwrap().unwrap();
        // `!!str 2` is the text "2", not a number; a plain `2` is a number.
        let two = &t.children(t.find("/list").ok().flatten(), 1..2).unwrap()[0];
        assert_eq!((two.value.as_str(), two.kind), ("2", ValueKind::Text));
        let n = parse("a: 2\nb: '2'\nc: ~\nd: yes\n", &cx).unwrap();
        let kinds: Vec<_> = n.children(None, 0..4).unwrap().into_iter().map(|n| n.kind).collect();
        assert_eq!(kinds, [ValueKind::Number, ValueKind::Text, ValueKind::Null, ValueKind::Text]);
        let err = parse("a: [1,\n", &cx).err().unwrap();
        assert_eq!(err.position().and_then(|p| p.line), Some(2));
        assert_eq!(t.lines(list.0 as u32), 5..6);
        let two = parse("a: 1\n---\n- x\n", &cx).unwrap();
        assert_eq!(
            top(&two),
            [("0", "{1}"), ("1", "[1]")].map(|(a, b)| (a.to_string(), b.to_string()))
        );
        assert_eq!(
            top(&parse("just text", &cx).unwrap())[0],
            ("value".into(), "\"just text\"".into())
        );
        assert!(parse("a: [1, 2\n", &cx).is_err());
    }
}
