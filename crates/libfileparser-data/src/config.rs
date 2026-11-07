//! Line-based config: INI (sections of `key = value`), Java properties and `.env`
//! files. Every key is a node on its line; INI sections are nodes holding theirs.

use libfileparser_core::{ParseContext, ParseError, Position, Result, ValueKind};

use crate::tree::{Builder, DataTree};

/// INI and its relatives (`.cfg`, `.conf`, `.gitconfig`, `.editorconfig`, `.desktop`):
/// `[section]` lines, `key = value` or `key: value`, `;` and `#` comments, and indented
/// lines continuing the value above.
pub(crate) fn parse_ini(text: &str, cx: &ParseContext) -> Result<DataTree> {
    let mut b = Builder::new(cx);
    let mut section: Option<u32> = None;
    // The last key, for continuation lines.
    let mut last: Option<(u32, String)> = None;
    for (n, raw) in text.lines().enumerate() {
        let n = n as u32;
        let line = raw.trim();
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if raw.starts_with([' ', '\t'])
            && let Some((id, value)) = &mut last
        {
            value.push('\n');
            value.push_str(line);
            b.set_value(*id, value.clone());
            b.end(*id, n);
            if let Some(s) = section {
                b.end(s, n);
            }
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let id = b.add(None, 1, name.trim(), "", ValueKind::Map, n..n + 1)?;
            section = Some(id);
            last = None;
            continue;
        }
        let (key, value) = match line.find(['=', ':']) {
            Some(at) => (line[..at].trim(), line[at + 1..].trim()),
            None => (line, ""),
        };
        let id = b.add(
            section,
            if section.is_some() { 2 } else { 1 },
            key,
            value,
            ValueKind::Text,
            n..n + 1,
        )?;
        if let Some(s) = section {
            b.end(s, n);
        }
        last = Some((id, value.to_string()));
    }
    summarize(&mut b);
    b.finish()
}

/// Whether text looks like INI: most of its first lines are sections, `key = value`
/// pairs or comments. For `.conf` and `.cfg`, which are often a program's own syntax.
pub(crate) fn looks_like_ini(header: &[u8]) -> bool {
    let text = String::from_utf8_lossy(header);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with([';', '#']))
        .take(40)
        .collect();
    if lines.is_empty() {
        return false;
    }
    let ini = lines
        .iter()
        .filter(|l| {
            (l.starts_with('[') && l.ends_with(']'))
                || l.find(['=', ':']).is_some_and(|at| {
                    let key = l[..at].trim();
                    !key.is_empty() && !key.contains(['{', '}', '(', ')', ';', '"'])
                })
        })
        .count();
    ini * 10 >= lines.len() * 9
}

/// Java `.properties`: `key=value`, `key: value` or `key value`; `#` and `!` comments;
/// a trailing `\` continues a line; `\t`, `\n`, `\uXXXX` and `\` before any other
/// character are escapes.
pub(crate) fn parse_properties(text: &str, cx: &ParseContext) -> Result<DataTree> {
    let mut b = Builder::new(cx);
    let mut lines = text.lines().enumerate();
    while let Some((n, raw)) = lines.next() {
        let first = n as u32;
        let mut logical = raw.trim_start().to_string();
        if logical.is_empty() || logical.starts_with(['#', '!']) {
            continue;
        }
        let mut last = first;
        while ends_in_odd_backslashes(&logical) {
            logical.pop();
            match lines.next() {
                Some((n, next)) => {
                    logical.push_str(next.trim_start());
                    last = n as u32;
                }
                None => break,
            }
        }
        let (key, value) = split_property(&logical);
        let at = |e: ParseError| match e {
            ParseError::Corrupt { why, .. } => ParseError::corrupt_at(why, Position::line(u64::from(first) + 1)),
            other => other,
        };
        b.add(None, 1, unescape(key).map_err(at)?, unescape(value).map_err(at)?, ValueKind::Text, first..last + 1)?;
    }
    b.finish()
}

fn ends_in_odd_backslashes(s: &str) -> bool {
    s.bytes().rev().take_while(|&c| c == b'\\').count() % 2 == 1
}

fn split_property(line: &str) -> (&str, &str) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'=' | b':' | b' ' | b'\t' | b'\x0c' => break,
            _ => i += 1,
        }
    }
    let key = &line[..i.min(line.len())];
    let mut rest = line[i.min(line.len())..].trim_start_matches([' ', '\t', '\x0c']);
    if let Some(r) = rest.strip_prefix(['=', ':']) {
        rest = r.trim_start_matches([' ', '\t', '\x0c']);
    }
    (key, rest)
}

fn unescape(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\x0c'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                let c = u32::from_str_radix(&hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| {
                        ParseError::corrupt(format!("properties: a bad \\u{hex} escape"))
                    })?;
                out.push(c);
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    Ok(out)
}

/// `.env`: `KEY=value` lines, optionally after `export`; values bare (to a ` #`
/// comment), in single quotes (as written) or double quotes (escapes, and line breaks
/// within).
pub(crate) fn parse_env(text: &str, cx: &ParseContext) -> Result<DataTree> {
    let mut b = Builder::new(cx);
    let lines: Vec<&str> = text.lines().collect();
    let mut n = 0;
    while n < lines.len() {
        let first = n as u32;
        let line = lines[n].trim();
        n += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let Some((key, value)) = line.split_once('=') else {
            return Err(ParseError::corrupt_at(".env: expected KEY=value", Position::line(u64::from(first) + 1)));
        };
        let value = value.trim_start();
        let shown = if let Some(rest) = value.strip_prefix('"') {
            // To the closing quote, maybe lines later.
            let mut text = rest.to_string();
            while !closes(&text) && n < lines.len() {
                text.push('\n');
                text.push_str(lines[n]);
                n += 1;
            }
            let body = text.rfind('"').map_or(text.as_str(), |at| &text[..at]);
            body.replace("\\n", "\n")
                .replace("\\t", "\t")
                .replace("\\\"", "\"")
                .replace("\\\\", "\\")
        } else if let Some(rest) = value.strip_prefix('\'') {
            rest.rfind('\'').map_or(rest, |at| &rest[..at]).to_string()
        } else {
            value
                .find(" #")
                .map_or(value, |at| &value[..at])
                .trim_end()
                .to_string()
        };
        b.add(None, 1, key.trim(), shown, ValueKind::Text, first..n as u32)?;
    }
    b.finish()
}

/// Whether a double-quoted value's text holds its closing quote.
fn closes(text: &str) -> bool {
    let mut escaped = false;
    for c in text.chars() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return true,
            _ => escaped = false,
        }
    }
    false
}

/// Sections show how many keys they hold.
fn summarize(b: &mut Builder) {
    for id in 0..b.len() as u32 {
        let n = b.children(id);
        if n > 0 {
            b.set_value(id, format!("{{{n}}}"));
        }
    }
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

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn ini_sections_and_continuations() {
        let cx = ParseContext::new();
        let t = parse_ini(
            "root = 1\n; c\n[core]\neditor = vim\nmulti: a\n  b\n[core]\nx=\n",
            &cx,
        )
        .unwrap();
        assert_eq!(
            top(&t),
            pairs(&[("root", "\"1\""), ("core", "{2}"), ("core", "{1}")])
        );
        let multi = t.find("/core/multi").unwrap().unwrap();
        assert_eq!(
            t.children(Some(t.find("/core").unwrap().unwrap()), 1..2)
                .unwrap()[0]
                .value,
            "a\nb"
        );
        assert_eq!(t.lines(multi.0 as u32), 4..6);
        assert!(t.find("/core[2]/x").unwrap().is_some());
        assert!(looks_like_ini(b"[a]\nk = v\n# c\nz: 1\n"));
        assert!(!looks_like_ini(b"server {\n  listen 80;\n}\n"));
    }

    #[test]
    fn properties_and_env() {
        let cx = ParseContext::new();
        let t = parse_properties(
            "# c\na=1\nb : two \\\n   lines\nc\\:d value\nu=\\u00e9\n",
            &cx,
        )
        .unwrap();
        assert_eq!(
            top(&t),
            pairs(&[("a", "\"1\""), ("b", "\"two lines\""), ("c:d", "\"value\""), ("u", "\"é\"")])
        );
        assert_eq!(t.lines(t.find("/b").unwrap().unwrap().0 as u32), 2..4);
        let e = parse_env(
            "# c\nexport A=1 # note\nB='$x y'\nC=\"line\\n2\nmore\"\n",
            &cx,
        )
        .unwrap();
        assert_eq!(
            top(&e),
            pairs(&[("A", "\"1\""), ("B", "\"$x y\""), ("C", "\"line\\n2\\nmore\"")])
        );
        assert!(parse_env("nonsense\n", &cx).is_err());
    }
}
