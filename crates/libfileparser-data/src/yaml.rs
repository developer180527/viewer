//! YAML 1.2, every document in the file. Aliases are shown as aliases, never expanded,
//! so a file of nested aliases ("billion laughs") costs no more than its text.

use std::collections::HashMap;

use libfileparser_core::{ParseContext, ParseError, Result};
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

/// Parses `text` into a tree: one document's mapping or sequence members at the top,
/// or, for several documents, one top-level node per document.
pub(crate) fn parse(text: &str, cx: &ParseContext) -> Result<DataTree> {
    let mut b = Builder::new(cx);
    let mut stack: Vec<Frame> = Vec::new();
    // Each document's top-level node.
    let mut docs: Vec<u32> = Vec::new();
    let mut anchors: HashMap<usize, String> = HashMap::new();
    for event in Parser::new_from_str(text) {
        let (event, span) = event.map_err(|e| ParseError::corrupt(format!("YAML: {e}")))?;
        let line = span.start.line().saturating_sub(1) as u32;
        match event {
            Event::Scalar(value, style, anchor, tag) => {
                let Some((parent, label)) = place(&mut stack, docs.len(), false, Some(&value))
                else {
                    continue;
                };
                let shown = match style {
                    ScalarStyle::Plain => value.to_string(),
                    _ => format!("{value:?}"),
                };
                let shown = match &tag {
                    // As written: `!!str`, not the URI it stands for.
                    Some(t) if t.handle == "tag:yaml.org,2002:" => {
                        format!("!!{} {shown}", t.suffix)
                    }
                    Some(t) => format!("{}{} {shown}", t.handle, t.suffix),
                    None => shown,
                };
                let last = span
                    .end
                    .line()
                    .saturating_sub(1)
                    .max(span.start.line().saturating_sub(1)) as u32;
                let id = b.add(
                    parent,
                    stack.len() + 1,
                    label.clone(),
                    shown,
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
                let id = b.add(parent, stack.len() + 1, label.clone(), "", line..line + 1)?;
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
            .map(|n| (n.label, n.value))
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
        assert_eq!(
            t.children(t.find("/list").ok().flatten(), 1..2).unwrap()[0].value,
            "!!str 2"
        );
        assert_eq!(t.lines(list.0 as u32), 5..6);
        let two = parse("a: 1\n---\n- x\n", &cx).unwrap();
        assert_eq!(
            top(&two),
            [("0", "{1}"), ("1", "[1]")].map(|(a, b)| (a.to_string(), b.to_string()))
        );
        assert_eq!(
            top(&parse("just text", &cx).unwrap())[0],
            ("value".into(), "just text".into())
        );
        assert!(parse("a: [1, 2\n", &cx).is_err());
    }
}
