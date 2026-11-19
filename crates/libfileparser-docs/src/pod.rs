//! POD (Perl's Plain Old Documentation) and Fountain (screenplays), read by our own
//! parsers of their specifications (perlpodspec, fountain.io): neither has a Rust
//! crate that's maintained.

use libfileparser_core::Result;

use crate::markup::{Rendered, esc, markup, strip_tags};
use crate::wiki::{Rule, inline};

// ---------------------------------------------------------------- POD

markup!(
    /// POD, in a `.pod` file of its own.
    PodReader, POD, "pod", "POD", "Perl documentation", ["pod"], ["text/x-pod"], pod
);

/// POD's formatting codes (`B<…>`, `I<< … >>`, `L<…>`, `E<…>`) as HTML.
fn pod_inline(s: &str, depth: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let code = c.is_ascii_uppercase() && "BICLEFSXZ".contains(c) && chars.get(i + 1) == Some(&'<') && depth < 32;
        if !code {
            out.push_str(&esc(&c.to_string()));
            i += 1;
            continue;
        }
        // `<` alone, or `<<` … `>>` (any count) with space inside.
        let n = chars[i + 1..].iter().take_while(|&&x| x == '<').count();
        let (open, inner_start) = if n > 1 && chars.get(i + 1 + n).is_some_and(|x| x.is_whitespace()) { (n, i + 1 + n + 1) } else { (1, i + 2) };
        let (inner, end) = if open == 1 {
            // Nested codes balance their own brackets.
            let mut depth_b = 1;
            let mut j = inner_start;
            while j < chars.len() {
                match chars[j] {
                    '<' if j > 0 && chars[j - 1].is_ascii_uppercase() => depth_b += 1,
                    '>' => {
                        depth_b -= 1;
                        if depth_b == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            (chars[inner_start.min(chars.len())..j.min(chars.len())].iter().collect::<String>(), j + 1)
        } else {
            let close: Vec<char> = std::iter::once(' ').chain(std::iter::repeat_n('>', open)).collect();
            let mut j = inner_start;
            while j + close.len() <= chars.len() && !chars[j..].starts_with(&close) {
                j += 1;
            }
            let found = j + close.len() <= chars.len();
            (chars[inner_start.min(chars.len())..j.min(chars.len())].iter().collect::<String>(), if found { j + close.len() } else { chars.len() })
        };
        let inner_html = || pod_inline(inner.trim(), depth + 1);
        match c {
            'B' => out.push_str(&format!("<strong>{}</strong>", inner_html())),
            'I' | 'F' => out.push_str(&format!("<em>{}</em>", inner_html())),
            'C' => out.push_str(&format!("<code>{}</code>", esc(&strip_tags(&inner_html())))),
            'S' => out.push_str(&inner_html().replace(' ', "\u{a0}")),
            'E' => out.push_str(&esc(&pod_entity(inner.trim()))),
            'X' | 'Z' => {}
            'L' => {
                let (text, target) = match inner.split_once('|') {
                    Some((t, target)) => (Some(t.to_string()), target.to_string()),
                    None => (None, inner.clone()),
                };
                let target = target.trim();
                let url = if target.contains("://") || target.starts_with("mailto:") {
                    target.to_string()
                } else if let Some((page, sec)) = target.split_once('/') {
                    format!("perldoc:{}#{}", page.trim(), sec.trim().trim_matches('"'))
                } else if target.starts_with('"') {
                    format!("#{}", target.trim_matches('"'))
                } else {
                    format!("perldoc:{target}")
                };
                let shown = match &text {
                    Some(t) => pod_inline(t, depth + 1),
                    None => esc(&target.replace('/', " ").trim_matches('"').replace('"', "")),
                };
                out.push_str(&format!("<a href=\"{}\">{shown}</a>", esc(&url)));
            }
            _ => {}
        }
        i = end.min(chars.len()).max(i + 2);
    }
    out
}

fn pod_entity(name: &str) -> String {
    match name {
        "lt" => "<".into(),
        "gt" => ">".into(),
        "verbar" => "|".into(),
        "sol" => "/".into(),
        "quot" => "\"".into(),
        "amp" => "&".into(),
        "apos" => "'".into(),
        "nbsp" => "\u{a0}".into(),
        "copy" => "©".into(),
        "reg" => "®".into(),
        "eacute" => "é".into(),
        "egrave" => "è".into(),
        "uuml" => "ü".into(),
        "ouml" => "ö".into(),
        "auml" => "ä".into(),
        "szlig" => "ß".into(),
        "laquo" => "«".into(),
        "raquo" => "»".into(),
        n => {
            let code = n.strip_prefix("0x").map(|h| u32::from_str_radix(h, 16)).or_else(|| n.strip_prefix('0').filter(|o| !o.is_empty()).map(|o| u32::from_str_radix(o, 8))).unwrap_or_else(|| n.parse());
            code.ok().and_then(char::from_u32).map_or_else(|| format!("E<{n}>"), String::from)
        }
    }
}

fn pod(text: &str) -> Result<Rendered> {
    let text = text.replace("\r\n", "\n");
    // Paragraphs: runs of lines between blank (or white-space-only) lines.
    let mut paras: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                paras.push(cur.join("\n"));
                cur.clear();
            }
        } else {
            cur.push(line);
        }
    }
    if !cur.is_empty() {
        paras.push(cur.join("\n"));
    }
    let mut out = String::new();
    let mut title = None;
    let mut in_pod = false;
    // Open `=over` lists: their kind, decided by the first item.
    let mut lists: Vec<Option<&'static str>> = Vec::new();
    let mut skip_until: Option<String> = None;
    let mut verbatim: Vec<String> = Vec::new();
    let flush = |verbatim: &mut Vec<String>, out: &mut String| {
        if !verbatim.is_empty() {
            out.push_str(&format!("<pre>{}</pre>", esc(&verbatim.join("\n\n"))));
            verbatim.clear();
        }
    };
    let mut after_name = false;
    for p in &paras {
        if p.starts_with('=') && p.chars().nth(1).is_some_and(|c| c.is_ascii_alphabetic()) {
            in_pod = true;
        }
        if !in_pod {
            continue;
        }
        if let Some(end) = &skip_until {
            if p.starts_with(end.as_str()) {
                skip_until = None;
            }
            continue;
        }
        if !p.starts_with('=') || !p.chars().nth(1).is_some_and(|c| c.is_ascii_alphabetic()) {
            if p.starts_with([' ', '\t']) {
                verbatim.push(p.to_string());
                continue;
            }
            flush(&mut verbatim, &mut out);
            let html = pod_inline(&p.replace('\n', " "), 0);
            if after_name && title.is_none() {
                // `=head1 NAME` then `Foo::Bar - what it does`: the title.
                title = Some(strip_tags(&html).split(" - ").next().unwrap_or("").trim().to_string()).filter(|t| !t.is_empty());
            }
            after_name = false;
            out.push_str(&format!("<p>{html}</p>"));
            continue;
        }
        flush(&mut verbatim, &mut out);
        let (cmd, rest) = p[1..].split_once(char::is_whitespace).unwrap_or((&p[1..], ""));
        let rest = rest.trim();
        match cmd {
            "head1" | "head2" | "head3" | "head4" | "head5" | "head6" => {
                let level = cmd.as_bytes()[4] - b'0';
                after_name = level == 1 && rest == "NAME";
                out.push_str(&format!("<h{level}>{}</h{level}>", pod_inline(&rest.replace('\n', " "), 0)));
            }
            "over" => lists.push(None),
            "item" => {
                let kind = if rest == "*" || rest.starts_with("* ") || rest == "o" || rest == "-" {
                    "ul"
                } else if rest.trim_end_matches(['.', ')']).chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
                    "ol"
                } else {
                    "dl"
                };
                match lists.last_mut() {
                    Some(slot @ None) => {
                        *slot = Some(kind);
                        out.push_str(if kind == "ol" { "<ol>" } else { "<ul>" });
                    }
                    Some(Some(_)) => out.push_str("</li>"),
                    None => {
                        // An item outside `=over`: a list of its own.
                        lists.push(Some(kind));
                        out.push_str(if kind == "ol" { "<ol>" } else { "<ul>" });
                    }
                }
                out.push_str("<li>");
                let text = match kind {
                    "ul" => rest.trim_start_matches(['*', 'o', '-']).trim(),
                    "ol" => "",
                    _ => rest,
                };
                if !text.is_empty() {
                    let html = pod_inline(&text.replace('\n', " "), 0);
                    out.push_str(&if kind == "dl" { format!("<p><strong>{html}</strong></p>") } else { format!("<p>{html}</p>") });
                }
            }
            "back" => {
                if let Some(kind) = lists.pop() {
                    match kind {
                        Some("ol") => out.push_str("</li></ol>"),
                        Some(_) => out.push_str("</li></ul>"),
                        None => {}
                    }
                }
            }
            "begin" => {
                let format = rest.split_whitespace().next().unwrap_or("");
                if !matches!(format, "text" | ":text" | "pod") {
                    skip_until = Some(format!("=end {format}"));
                }
            }
            "cut" => in_pod = false,
            // `=for`, `=encoding`, `=pod`, `=end`: nothing to show.
            _ => {}
        }
    }
    flush(&mut verbatim, &mut out);
    while let Some(kind) = lists.pop() {
        match kind {
            Some("ol") => out.push_str("</li></ol>"),
            Some(_) => out.push_str("</li></ul>"),
            None => {}
        }
    }
    Ok(Rendered { html: out, title, warnings: Vec::new() })
}

// ---------------------------------------------------------------- Fountain

markup!(
    /// Fountain, the plain-text screenplay format: scenes, characters and their
    /// dialogue, parentheticals, transitions, sections, synopses and notes.
    FountainReader, FOUNTAIN, "fountain", "Fountain", "Screenplay", ["fountain", "spmd"], ["text/x-fountain"], fountain
);

const FOUNTAIN_MARKS: &[Rule] = &[Rule::Backslash, Rule::Mark("***", "***", "strong><em"), Rule::Mark("**", "**", "strong"), Rule::Mark("*", "*", "em"), Rule::Mark("_", "_", "u")];

/// Text without boneyard (`/* … */`) and notes (`[[ … ]]`).
fn fountain_strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, a)| a);
        } else if let Some(r) = rest.strip_prefix("[[") {
            rest = r.split_once("]]").map_or("", |(_, a)| a);
        } else {
            let c = rest.chars().next().unwrap_or(' ');
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

fn is_scene_heading(t: &str) -> bool {
    if t.starts_with('.') && !t.starts_with("..") && t.len() > 1 {
        return true;
    }
    let u = t.to_ascii_uppercase();
    ["INT.", "EXT.", "EST.", "INT/EXT", "INT./EXT", "EXT/INT", "I/E", "INT ", "EXT ", "EST "].iter().any(|p| u.starts_with(p))
}

/// A character's cue: in capitals (an extension like `(V.O.)` aside), or forced with `@`.
fn is_character(t: &str) -> bool {
    if t.starts_with('@') {
        return true;
    }
    let name = t.split('(').next().unwrap_or("").trim_end_matches('^').trim();
    !name.is_empty() && name.chars().any(|c| c.is_alphabetic()) && name == name.to_uppercase() && !name.ends_with(':')
}

fn fountain(text: &str) -> Result<Rendered> {
    let text = fountain_strip(&text.replace("\r\n", "\n").replace('\t', "    "));
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut title = None;
    // The title page: `Key: value` lines (with indented continuations) at the start.
    if lines.first().is_some_and(|l| l.contains(':') && !l.starts_with(' ') && l.split(':').next().is_some_and(|k| !k.trim().is_empty() && !k.contains('.'))) {
        let mut fields: Vec<(String, String)> = Vec::new();
        while i < lines.len() && !lines[i].trim().is_empty() {
            let l = lines[i];
            if let Some((k, v)) = l.split_once(':').filter(|_| !l.starts_with([' ', '\t'])) {
                fields.push((k.trim().to_string(), v.trim().to_string()));
            } else if let Some(last) = fields.last_mut() {
                if !last.1.is_empty() {
                    last.1.push('\n');
                }
                last.1.push_str(l.trim());
            }
            i += 1;
        }
        for (k, v) in &fields {
            if k.eq_ignore_ascii_case("title") {
                title = Some(strip_tags(&inline(&v.replace('\n', " "), FOUNTAIN_MARKS)).trim().to_string());
                out.push_str(&format!("<h1>{}</h1>", inline(&v.replace('\n', " "), FOUNTAIN_MARKS)));
            }
        }
        for (k, v) in &fields {
            if !k.eq_ignore_ascii_case("title") && !v.is_empty() {
                out.push_str(&format!("<p><em>{}:</em> {}</p>", esc(k), inline(v, FOUNTAIN_MARKS).replace('\n', "<br>")));
            }
        }
        out.push_str("<hr>");
    }
    let has_sections = lines.iter().any(|l| l.starts_with('#'));
    let scene_level = if has_sections { 3 } else { 2 };
    let blank_before = |i: usize| i == 0 || lines[i - 1].trim().is_empty();
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.is_empty() {
            i += 1;
            continue;
        }
        let next_blank = lines.get(i + 1).is_none_or(|l| l.trim().is_empty());
        if t.starts_with('#') {
            let level = t.chars().take_while(|&c| c == '#').count();
            out.push_str(&format!("<h{0}>{1}</h{0}>", (level + 1).min(6), inline(t[level..].trim(), FOUNTAIN_MARKS)));
        } else if let Some(s) = t.strip_prefix('=').filter(|s| !s.starts_with("==")) {
            out.push_str(&format!("<p><em>{}</em></p>", inline(s.trim(), FOUNTAIN_MARKS)));
        } else if t.chars().all(|c| c == '=') && t.len() >= 3 {
            out.push_str("<hr>");
        } else if blank_before(i) && is_scene_heading(t) {
            let h = t.strip_prefix('.').unwrap_or(t);
            // A scene number (`#12#`) at the end.
            let (h, number) = match h.trim_end().strip_suffix('#').and_then(|x| x.rsplit_once('#')) {
                Some((head, n)) => (head.trim(), Some(n)),
                None => (h, None),
            };
            let shown = number.map_or_else(|| inline(h, FOUNTAIN_MARKS), |n| format!("{} {}", esc(n), inline(h, FOUNTAIN_MARKS)));
            out.push_str(&format!("<h{scene_level}>{shown}</h{scene_level}>"));
        } else if let Some(c) = t.strip_prefix('>').and_then(|r| r.strip_suffix('<')) {
            out.push_str(&format!("<p><em>{}</em></p>", inline(c.trim(), FOUNTAIN_MARKS)));
        } else if t.starts_with('>') || (blank_before(i) && next_blank && t.ends_with("TO:") && t == t.to_uppercase()) {
            out.push_str(&format!("<p><strong>{}</strong></p>", inline(t.trim_start_matches('>').trim(), FOUNTAIN_MARKS)));
        } else if blank_before(i) && !next_blank && !t.starts_with('!') && is_character(t) {
            // A cue, then its dialogue and parentheticals up to a blank line.
            let cue = t.trim_start_matches('@').trim_end_matches('^').trim();
            out.push_str(&format!("<p><strong>{}</strong></p><blockquote>", inline(cue, FOUNTAIN_MARKS)));
            i += 1;
            let mut speech: Vec<String> = Vec::new();
            while i < lines.len() && !lines[i].trim().is_empty() {
                let d = lines[i].trim();
                if d.starts_with('(') && d.ends_with(')') {
                    if !speech.is_empty() {
                        out.push_str(&format!("<p>{}</p>", speech.join("<br>")));
                        speech.clear();
                    }
                    out.push_str(&format!("<p><em>{}</em></p>", inline(d, FOUNTAIN_MARKS)));
                } else {
                    speech.push(inline(d, FOUNTAIN_MARKS));
                }
                i += 1;
            }
            if !speech.is_empty() {
                out.push_str(&format!("<p>{}</p>", speech.join("<br>")));
            }
            out.push_str("</blockquote>");
            continue;
        } else if let Some(l) = t.strip_prefix('~') {
            out.push_str(&format!("<p><em>{}</em></p>", inline(l.trim(), FOUNTAIN_MARKS)));
        } else {
            // Action: up to a blank line, its line breaks kept.
            let mut action = vec![inline(line.trim_start_matches('!').trim_end(), FOUNTAIN_MARKS)];
            while i + 1 < lines.len() && !lines[i + 1].trim().is_empty() {
                i += 1;
                action.push(inline(lines[i].trim_end(), FOUNTAIN_MARKS));
            }
            out.push_str(&format!("<p>{}</p>", action.join("<br>")));
        }
        i += 1;
    }
    Ok(Rendered { html: out, title, warnings: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::{Content, Document, FileSource, ParseContext, Reader};

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
    fn pod_document() {
        let (doc, s) = blocks(&PodReader, "a.pod", "ignored before pod\n\n=head1 NAME\n\nFoo::Bar - does things\n\n=head1 SYNOPSIS\n\n  use Foo::Bar;\n  my $x = Foo::Bar->new;\n\n=head2 Methods\n\nCall B<new> with I<< args >> and C<< $x->go >>, see L<perlpod> and L<the site|https://x.org>. E<lt>tagE<gt>\n\n=over 4\n\n=item * first\n\n=item * second\n\n=back\n\n=over\n\n=item new\n\nMakes one.\n\n=back\n\n=begin html\n\n<b>raw</b>\n\n=end html\n\n=cut\n\nsub code {}\n");
        has(&s, &["Heading { level: 1, text: [Text(\"NAME\")]", "Foo::Bar - does things", "use Foo::Bar;\\n  my $x", "Strong([Text(\"new\")])", "Emphasis([Text(\"args\")])", "Code(\"$x->go\")", "perldoc:perlpod", "https://x.org", "<tag>", "List { ordered: false", "second", "Strong([Text(\"new\")])", "Makes one."]);
        for not in ["ignored before", "raw", "sub code"] {
            assert!(!s.contains(not), "{not} in {s}");
        }
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Foo::Bar"));
    }

    #[test]
    fn fountain_screenplay() {
        let (doc, s) = blocks(&FountainReader, "a.fountain", "Title: **BRICK & STEEL**\nCredit: Written by\nAuthor: Stu Maschwitz\n\n# Act One\n\nINT. GARAGE - DAY #1#\n\nBRICK and STEEL sit in the *dark*.\n\nSTEEL (V.O.)\n(quietly)\nBeer's ready!\nAre they cold?\n\n[[A note to cut]]\n\nCUT TO:\n\n.FLASHBACK\n\n> THE END <\n\n= A synopsis.\n");
        has(&s, &["Heading { level: 1, text: [Strong([Text(\"BRICK & STEEL\")])]", "Stu Maschwitz", "Heading { level: 2, text: [Text(\"Act One\")]", "Heading { level: 3, text: [Text(\"1 INT. GARAGE - DAY\")]", "Emphasis([Text(\"dark\")])", "Strong([Text(\"STEEL (V.O.)\")])", "Quote([Paragraph([Emphasis([Text(\"(quietly)\")])]), Paragraph([Text(\"Beer's ready!\"), LineBreak, Text(\"Are they cold?\")])", "Strong([Text(\"CUT TO:\")])", "FLASHBACK", "THE END", "A synopsis."]);
        assert!(!s.contains("note to cut"), "{s}");
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "BRICK & STEEL"));
    }
}
