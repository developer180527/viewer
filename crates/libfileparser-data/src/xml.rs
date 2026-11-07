//! XML as a tree of elements: an element's attributes as `@name` children, its text as
//! its value when it holds only text, else as `#text` children among its elements.
//! Comments, processing instructions and the doctype aren't shown.

use libfileparser_core::{ParseContext, ParseError, Position, Result, ValueKind};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use crate::tree::{Builder, DataTree, LineCounter};

struct Open {
    id: u32,
    /// Text not yet placed: an element's value if no element follows.
    text: String,
    text_line: u32,
    elements: usize,
}

pub(crate) fn parse(bytes: &[u8], cx: &ParseContext) -> Result<DataTree> {
    let mut b = Builder::new(cx);
    let mut lines = LineCounter::new(bytes);
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Open> = Vec::new();
    let mut roots = 0usize;
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(|e| {
            let at = reader.error_position() as usize;
            ParseError::corrupt_at(format!("XML: {e}"), Position::in_text(bytes, at))
        })?;
        let end = reader.buffer_position() as usize;
        match event {
            ev @ (Event::Start(_) | Event::Empty(_)) => {
                let empty = matches!(ev, Event::Empty(_));
                let (Event::Start(e) | Event::Empty(e)) = ev else {
                    unreachable!()
                };
                let line = lines.line(start);
                let depth = stack.len() + 1;
                if let Some(open) = stack.last_mut() {
                    flush(&mut b, open, depth)?;
                }
                let parent = stack.last().map(|o| o.id);
                if let Some(open) = stack.last_mut() {
                    open.elements += 1;
                }
                if parent.is_none() {
                    roots += 1;
                }
                let id = element(
                    &mut b,
                    parent,
                    stack.len() + 1,
                    &e,
                    line,
                    lines.line(end.saturating_sub(1)),
                )?;
                if !empty {
                    stack.push(Open {
                        id,
                        text: String::new(),
                        text_line: line,
                        elements: 0,
                    });
                }
            }
            Event::End(_) => {
                let line = lines.line(end.saturating_sub(1));
                let Some(mut open) = stack.pop() else {
                    return Err(ParseError::corrupt(
                        "XML: a closing tag without an opening one",
                    ));
                };
                if open.elements == 0 && b.children(open.id) == 0 {
                    // Only text: the element is that text.
                    let text = open.text.trim();
                    if !text.is_empty() {
                        b.set_kind(open.id, ValueKind::Text);
                    }
                    b.set_value(open.id, text.to_string());
                } else if open.elements == 0 && !open.text.trim().is_empty() {
                    // Attributes and text: the text as the value.
                    b.set_value(open.id, open.text.trim().to_string());
                } else {
                    flush(&mut b, &mut open, stack.len() + 2)?;
                }
                b.end(open.id, line);
            }
            Event::Text(t) => {
                if let Some(open) = stack.last_mut() {
                    let text = t
                        .xml10_content()
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned());
                    if open.text.trim().is_empty() {
                        open.text_line =
                            lines.line(start + (t.len() - text.trim_start().len()).min(t.len()));
                    }
                    open.text.push_str(&text);
                }
            }
            Event::GeneralRef(r) => {
                if let Some(open) = stack.last_mut() {
                    let name = String::from_utf8_lossy(&r).into_owned();
                    let resolved = match r.resolve_char_ref() {
                        Ok(Some(c)) => c.to_string(),
                        _ => match quick_xml::escape::resolve_xml_entity(&name) {
                            Some(s) => s.to_string(),
                            None => format!("&{name};"),
                        },
                    };
                    open.text.push_str(&resolved);
                }
            }
            Event::CData(t) => {
                if let Some(open) = stack.last_mut() {
                    open.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(ParseError::corrupt("XML: an element that never closes"));
    }
    if roots == 0 {
        return Err(ParseError::corrupt("XML: no element"));
    }
    b.finish()
}

/// Places pending text as a `#text` child, before the element that follows it.
fn flush(b: &mut Builder, open: &mut Open, depth: usize) -> Result<()> {
    let text = std::mem::take(&mut open.text);
    let text = text.trim();
    if !text.is_empty() {
        let lines = text.matches('\n').count() as u32;
        b.add(
            Some(open.id),
            depth,
            "#text",
            text.to_string(),
            ValueKind::Text,
            open.text_line..open.text_line + lines + 1,
        )?;
    }
    Ok(())
}

fn element(
    b: &mut Builder,
    parent: Option<u32>,
    depth: usize,
    e: &BytesStart,
    first: u32,
    last: u32,
) -> Result<u32> {
    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let id = b.add(parent, depth, name, "", ValueKind::Group, first..last + 1)?;
    for attr in e.attributes().with_checks(false) {
        let attr = attr.map_err(|e| ParseError::corrupt(format!("XML: {e}")))?;
        let key = format!("@{}", String::from_utf8_lossy(attr.key.as_ref()));
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|v| v.into_owned())
            .unwrap_or_else(|_| String::from_utf8_lossy(&attr.value).into_owned());
        b.add(Some(id), depth + 1, key, value, ValueKind::Text, first..last + 1)?;
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::TreeSource;

    #[test]
    fn elements_attributes_and_text() {
        let cx = ParseContext::new();
        let t = parse(b"<?xml version=\"1.0\"?>\n<feed lang=\"en\">\n  <title>A &amp; B</title>\n  <entry id=\"1\"/>\n  <entry id=\"2\">x<b>y</b></entry>\n</feed>\n", &cx).unwrap();
        let kids = |p: &str| -> Vec<(String, String)> {
            let id = t.find(p).unwrap().unwrap();
            t.children(Some(id), 0..10)
                .unwrap()
                .into_iter()
                .map(|n| (n.label.clone(), crate::tree::shown(&n)))
                .collect()
        };
        assert_eq!(
            kids("/feed"),
            [
                ("@lang", "\"en\""),
                ("title", "\"A & B\""),
                ("entry", ""),
                ("entry", "")
            ]
            .map(|(a, b)| (a.to_string(), b.to_string()))
        );
        assert_eq!(
            kids("/feed/entry[2]"),
            [("@id", "\"2\""), ("#text", "\"x\""), ("b", "\"y\"")].map(|(a, b)| (a.to_string(), b.to_string()))
        );
        let title = t.find("/feed/title").unwrap().unwrap();
        assert_eq!(t.lines(title.0 as u32), 2..3);
        assert!(parse(b"<a><b></a>", &cx).is_err());
        assert!(parse(b"not xml", &cx).is_err());
    }
}
