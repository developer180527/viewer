//! Textile (Redmine's and Textpattern's markup): headings, paragraphs, quotes, code,
//! lists, tables, links, images and phrase marks. No maintained crate reads it.

use crate::{Html, esc, strip_tags};

pub fn textile(text: &str) -> Html {
    let mut out = String::with_capacity(text.len() * 2);
    let mut title = None;
    let blocks = text.replace("\r\n", "\n");
    let mut lines = blocks.lines().peekable();
    // An extended block (`bc..`, `bq..`, `pre..`) runs until the next block signature.
    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        let mut para = vec![line];
        // Lists and tables are line by line; everything else is up to a blank line.
        let lead = line.trim_start();
        let listy = lead.starts_with(['*', '#']) && lead.trim_start_matches(['*', '#']).starts_with(' ');
        let tabley = lead.starts_with('|') || lead.starts_with("table");
        while let Some(next) = lines.peek() {
            if next.trim().is_empty() {
                break;
            }
            para.push(next);
            lines.next();
        }
        if listy {
            textile_list(&para, &mut out);
            continue;
        }
        if tabley {
            textile_table(&para, &mut out);
            continue;
        }
        let (sig, rest) = textile_signature(para[0]);
        let body_lines: Vec<&str> = std::iter::once(rest).chain(para[1..].iter().copied()).collect();
        let body = body_lines.join("\n");
        match sig.as_deref() {
            Some(h @ ("h1" | "h2" | "h3" | "h4" | "h5" | "h6")) => {
                let inner = textile_inline(&body);
                if h == "h1" && title.is_none() {
                    title = Some(strip_tags(&inner));
                }
                out.push_str(&format!("<{h}>{inner}</{h}>"));
            }
            Some("bq") => out.push_str(&format!("<blockquote><p>{}</p></blockquote>", textile_inline(&body).replace('\n', "<br>"))),
            Some("bc") => out.push_str(&format!("<pre><code>{}</code></pre>", esc(&body))),
            Some("pre") => out.push_str(&format!("<pre>{}</pre>", esc(&body))),
            Some("notextile") => out.push_str(&format!("<p>{}</p>", esc(&body))),
            Some(f) if f.starts_with("fn") => out.push_str(&format!("<p><sup>{}</sup> {}</p>", esc(&f[2..]), textile_inline(&body))),
            _ => out.push_str(&format!("<p>{}</p>", textile_inline(&body).replace('\n', "<br>"))),
        }
    }
    Html { html: out, title, warnings: Vec::new() }
}

/// A block's signature (`h1`, `bq`, `bc`, `p`, `fn1`, with any attributes and a
/// trailing `..`) and the text after it.
pub(crate) fn textile_signature(line: &str) -> (Option<String>, &str) {
    let Some(dot) = line.find(". ") else { return (None, line) };
    let head = &line[..dot];
    let name: String = head.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    let attrs = &head[name.len()..];
    let known = matches!(name.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "bq" | "bc" | "pre" | "notextile") || (name.starts_with("fn") && name[2..].chars().all(|c| c.is_ascii_digit()) && name.len() > 2);
    let attrs_ok = attrs.trim_end_matches('.').chars().all(|c| "(){}[]<>=-#. :;%_".contains(c) || c.is_alphanumeric());
    if known && attrs_ok { (Some(name), &line[dot + 2..]) } else { (None, line) }
}

fn textile_list(lines: &[&str], out: &mut String) {
    let mut open: Vec<char> = Vec::new();
    for line in lines {
        let l = line.trim_start();
        let marks: String = l.chars().take_while(|c| *c == '*' || *c == '#').collect();
        let text = l[marks.len()..].trim();
        if marks.is_empty() {
            // A continuation of the previous item.
            out.push_str(&format!("<br>{}", textile_inline(text)));
            continue;
        }
        let depth = marks.len().min(16);
        let kind = marks.chars().last().unwrap_or('*');
        while open.len() > depth {
            out.push_str(if open.pop() == Some('#') { "</li></ol>" } else { "</li></ul>" });
        }
        if open.len() == depth && open.last() != Some(&kind) {
            out.push_str(if open.pop() == Some('#') { "</li></ol>" } else { "</li></ul>" });
        }
        if open.len() == depth {
            out.push_str("</li>");
        }
        while open.len() < depth {
            out.push_str(if kind == '#' { "<ol>" } else { "<ul>" });
            open.push(kind);
        }
        out.push_str(&format!("<li>{}", textile_inline(text)));
    }
    while let Some(k) = open.pop() {
        out.push_str(if k == '#' { "</li></ol>" } else { "</li></ul>" });
    }
}

fn textile_table(lines: &[&str], out: &mut String) {
    out.push_str("<table>");
    let mut head = true;
    for line in lines {
        let l = line.trim();
        if !l.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = l.trim_matches('|').split('|').collect();
        let header = cells.iter().all(|c| c.starts_with("_.")) && head;
        if header {
            out.push_str("<thead>");
        }
        out.push_str("<tr>");
        for c in cells {
            let c = c.strip_prefix("_.").unwrap_or(c);
            // Cell attributes (`\2.`, `/3.`, `>.`, `{…}.`) end in ". ".
            let c = match c.find(". ") {
                Some(at) if c[..at].chars().all(|x| "\\/<>=^~-{}(): ;#%0123456789".contains(x) || x.is_alphabetic()) && at < 24 && c[..at].chars().any(|x| !x.is_alphabetic()) => &c[at + 2..],
                _ => c,
            };
            let tag = if header { "th" } else { "td" };
            out.push_str(&format!("<{tag}>{}</{tag}>", textile_inline(c.trim())));
        }
        out.push_str("</tr>");
        if header {
            out.push_str("</thead>");
        }
        head = false;
    }
    out.push_str("</table>");
}

/// How far a mark's close or a link's quote is looked for, and how many candidate
/// closes a mark tries.
const REACH: usize = 4000;
const MAX_TRIES: usize = 64;

/// Textile's phrase marks, links and images, as HTML.
pub(crate) fn textile_inline(text: &str) -> String {
    inline_depth(text, 0)
}

fn inline_depth(text: &str, depth: usize) -> String {
    if depth > 16 {
        return esc(text);
    }

    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    let boundary = |i: usize| i == 0 || !chars[i - 1].is_alphanumeric();
    while i < chars.len() {
        let c = chars[i];
        // `"text":url`
        if c == '"'
            && let Some(close) = chars[i + 1..].iter().take(REACH).position(|&x| x == '"').map(|p| i + 1 + p)
                && chars.get(close + 1) == Some(&':') {
                    let url: String = chars[close + 2..].iter().take_while(|x| !x.is_whitespace()).collect();
                    let url = url.trim_end_matches(['.', ',', ';', ')', '!', '?']).to_string();
                    if !url.is_empty() {
                        let label: String = chars[i + 1..close].iter().collect();
                        out.push_str(&format!("<a href=\"{}\">{}</a>", esc(&url), inline_depth(&label, depth + 1)));
                        i = close + 2 + url.chars().count();
                        continue;
                    }
                }
        // `!image.png(alt)!` and `!image.png!:link`
        if c == '!' && chars.get(i + 1).is_some_and(|x| !x.is_whitespace())
            && let Some(close) = chars[i + 1..].iter().take(REACH).position(|&x| x == '!').map(|p| i + 1 + p) {
                let inner: String = chars[i + 1..close].iter().collect();
                if !inner.contains(' ') || inner.contains('(') {
                    let (src, alt) = match inner.split_once('(') {
                        Some((s, a)) => (s.to_string(), a.trim_end_matches(')').to_string()),
                        None => (inner.clone(), String::new()),
                    };
                    let src = src.trim_start_matches(['<', '>', '=']).to_string();
                    if !src.is_empty() {
                        out.push_str(&format!("<img src=\"{}\" alt=\"{}\">", esc(&src), esc(&alt)));
                        i = close + 1;
                        if chars.get(i) == Some(&':') {
                            i += chars[i..].iter().take_while(|x| !x.is_whitespace()).count();
                        }
                        continue;
                    }
                }
            }
        let marks: &[(&str, &str)] = &[("**", "b"), ("__", "i"), ("??", "cite"), ("*", "strong"), ("_", "em"), ("@", "code"), ("+", "ins"), ("-", "del"), ("^", "sup"), ("~", "sub"), ("%", "span")];
        let mut matched = false;
        if boundary(i) {
            for (m, tag) in marks {
                let mc: Vec<char> = m.chars().collect();
                if chars[i..].starts_with(&mc) && chars.get(i + mc.len()).is_some_and(|x| !x.is_whitespace()) {
                    // The closing mark, followed by a non-word character.
                    let mut j = i + mc.len() + 1;
                    let mut found = None;
                    // Within reach, and at most so many candidate closes: linear on a
                    // line of thousands of unclosed marks.
                    let mut tries = 0;
                    while j + mc.len() <= chars.len() && j < i + REACH && tries < MAX_TRIES {
                        if chars[j..].starts_with(&mc) {
                            tries += 1;
                            if !chars[j - 1].is_whitespace() && chars.get(j + mc.len()).is_none_or(|x| !x.is_alphanumeric()) {
                                found = Some(j);
                                break;
                            }
                        }
                        j += 1;
                    }
                    if let Some(end) = found {
                        let inner: String = chars[i + mc.len()..end].iter().collect();
                        if *tag == "code" {
                            out.push_str(&format!("<code>{}</code>", esc(&inner)));
                        } else {
                            out.push_str(&format!("<{tag}>{}</{tag}>", inline_depth(&inner, depth + 1)));
                        }
                        i = end + mc.len();
                        matched = true;
                        break;
                    }
                }
            }
        }
        if matched {
            continue;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textile_plain_text_survives() {
        assert_eq!(textile_inline("a-b c*d 2*3 x_y"), "a-b c*d 2*3 x_y");
        assert_eq!(textile_inline("<b>&"), "&lt;b&gt;&amp;");
        assert_eq!(textile_signature("p. text").0.as_deref(), Some("p"));
        assert_eq!(textile_signature("Hello. World").0, None);
    }
}
