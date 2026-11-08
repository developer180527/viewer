//! Lenient tag markup: SGML, and OFX 1.x (SGML whose leaf tags aren't closed). An end
//! tag closes the nearest open element of its name and any left open inside it;
//! with `leaves`, an element holding text is closed by the next start tag. Attribute
//! values may be bare. The tree is shaped as XML's (`crate::xml`).

use libfileparser_core::{ParseContext, Result, ValueKind};

use crate::tree::{Builder, DataTree, LineCounter};

struct Open {
    id: u32,
    name: String,
    text: String,
    text_line: u32,
    elements: usize,
}

pub(crate) fn parse(bytes: &[u8], cx: &ParseContext, leaves: bool) -> Result<DataTree> {
    let s = bytes;
    let mut b = Builder::new(cx);
    let mut lines = LineCounter::new(s);
    let mut stack: Vec<Open> = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'<' {
            let end = memchr::memchr(b'<', &s[i..]).map_or(s.len(), |p| i + p);
            if let Some(open) = stack.last_mut() {
                let text = decode(&String::from_utf8_lossy(&s[i..end]));
                if open.text.trim().is_empty() {
                    let lead = s[i..end].iter().take_while(|b| b.is_ascii_whitespace()).count();
                    open.text_line = lines.line(i + lead);
                }
                open.text.push_str(&text);
            }
            i = end;
            continue;
        }
        let rest = &s[i..];
        // Comments, declarations, processing instructions: skipped.
        if rest.starts_with(b"<!--") {
            i = memchr::memmem::find(&rest[4..], b"-->").map_or(s.len(), |p| i + 4 + p + 3);
            continue;
        }
        if rest.starts_with(b"<!") || rest.starts_with(b"<?") {
            i = memchr::memchr(b'>', rest).map_or(s.len(), |p| i + p + 1);
            continue;
        }
        let end = memchr::memchr(b'>', rest).map_or(s.len(), |p| i + p);
        let tag = String::from_utf8_lossy(&s[i + 1..end]).into_owned();
        let start = i;
        i = (end + 1).min(s.len());
        if let Some(name) = tag.strip_prefix('/') {
            let name = name.trim();
            // Close the nearest open element of this name, and everything inside it.
            if let Some(at) = stack.iter().rposition(|o| o.name.eq_ignore_ascii_case(name)) {
                let line = lines.line(end.saturating_sub(1));
                while stack.len() > at {
                    let open = stack.pop().expect("an open element");
                    close(&mut b, open, stack.len() + 1, line)?;
                }
            }
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag = tag.trim_end_matches('/');
        let name_end = tag.find(|c: char| c.is_whitespace()).unwrap_or(tag.len());
        let name = tag[..name_end].to_string();
        if name.is_empty() {
            continue;
        }
        let line = lines.line(start);
        // OFX: `<CODE>0` then `<SEVERITY>` closes CODE.
        if leaves && stack.last().is_some_and(|o| o.elements == 0 && !o.text.trim().is_empty() && b.children(o.id) == 0) {
            let open = stack.pop().expect("an open element");
            close(&mut b, open, stack.len() + 1, line)?;
        }
        let depth = stack.len() + 1;
        if let Some(open) = stack.last_mut() {
            flush(&mut b, open, depth)?;
            open.elements += 1;
        }
        let parent = stack.last().map(|o| o.id);
        let id = b.add(parent, depth, name.clone(), "", ValueKind::Group, line..line + 1)?;
        for (k, v) in attributes(&tag[name_end..]) {
            b.add(Some(id), depth + 1, format!("@{k}"), v, ValueKind::Text, line..line + 1)?;
        }
        if !self_closing {
            stack.push(Open { id, name, text: String::new(), text_line: line, elements: 0 });
        }
    }
    let last = lines.line(s.len().saturating_sub(1));
    while let Some(open) = stack.pop() {
        close(&mut b, open, stack.len() + 1, last)?;
    }
    b.finish()
}

fn close(b: &mut Builder, mut open: Open, depth: usize, line: u32) -> Result<()> {
    if open.elements == 0 {
        let text = open.text.trim().to_string();
        if !text.is_empty() {
            b.set_kind(open.id, ValueKind::Text);
        }
        b.set_value(open.id, text);
    } else {
        flush(b, &mut open, depth + 1)?;
    }
    b.end(open.id, line);
    Ok(())
}

fn flush(b: &mut Builder, open: &mut Open, depth: usize) -> Result<()> {
    let text = std::mem::take(&mut open.text);
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        let n = trimmed.matches('\n').count() as u32;
        b.add(Some(open.id), depth, "#text", crate::xml::spaced(&text, trimmed), ValueKind::Text, open.text_line..open.text_line + n + 1)?;
    }
    Ok(())
}

/// `name="v" name='v' name=v name`.
fn attributes(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let name_end = rest.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(rest.len());
        let name = &rest[..name_end];
        rest = rest[name_end..].trim_start();
        let value = if let Some(r) = rest.strip_prefix('=') {
            let r = r.trim_start();
            match r.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let close = r[1..].find(q).map_or(r.len(), |p| p + 1);
                    let v = &r[1..close];
                    rest = r.get(close + 1..).unwrap_or("");
                    v.to_string()
                }
                _ => {
                    let end = r.find(char::is_whitespace).unwrap_or(r.len());
                    rest = &r[end..];
                    r[..end].to_string()
                }
            }
        } else {
            String::new()
        };
        if !name.is_empty() {
            out.push((name.to_string(), decode(&value)));
        }
        rest = rest.trim_start();
    }
    out
}

/// The five XML entities and numeric references.
pub(crate) fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            }
        });
        match (decoded, end) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::TreeSource;

    #[test]
    fn ofx_leaves_and_sgml_end_tags() {
        let cx = ParseContext::new();
        let t = parse(b"OFXHEADER:100\n<OFX><SIGNONMSGSRSV1><SONRS><STATUS><CODE>0<SEVERITY>INFO</STATUS><DTSERVER>20241108</SONRS></SIGNONMSGSRSV1></OFX>", &cx, true).unwrap();
        let status = t.find("/OFX/SIGNONMSGSRSV1/SONRS/STATUS").unwrap().unwrap();
        let kids: Vec<_> = t.children(Some(status), 0..9).unwrap().into_iter().map(|n| (n.label, n.value)).collect();
        assert_eq!(kids, [("CODE".to_string(), "0".to_string()), ("SEVERITY".to_string(), "INFO".to_string())]);
        assert!(t.find("/OFX/SIGNONMSGSRSV1/SONRS/DTSERVER").unwrap().is_some());
        // SGML: unclosed paragraphs closed by their parent's end tag; bare attributes.
        let t = parse(b"<!DOCTYPE doc><doc lang=en><p>one<p>two &amp; three</doc>", &cx, false).unwrap();
        let doc = t.find("/doc").unwrap().unwrap();
        let kids: Vec<_> = t.children(Some(doc), 0..9).unwrap().into_iter().map(|n| n.label).collect();
        assert_eq!(kids, ["@lang", "p"]);
        let p = t.find("/doc/p/p").unwrap().unwrap();
        assert_eq!(t.children(Some(t.find("/doc/p").unwrap().unwrap()), 0..9).unwrap()[0].value, "one");
        assert_eq!(TreeSource::value(&t, p).unwrap(), Some(libfileparser_core::Cell::Text("two & three".into())));
    }
}
