//! Wiki and lightweight markups with no maintained Rust parser, read by ours:
//! MediaWiki, Creole, DokuWiki, Jira, txt2tags, Muse, Haddock, XWiki, TikiWiki, TWiki,
//! Zim, Vimwiki and Gemtext. Each is a short line-by-line reader over a shared block
//! writer ([`Out`]) and inline engine ([`inline`]), writing HTML that becomes formatted
//! text as every markup does (`crate::markup`).
//!
//! They read what documents commonly use: headings, paragraphs, lists, tables, code,
//! quotes, links, images and the usual marks. Templates, macros and plugins (a
//! MediaWiki `{{template}}`, an XWiki `{{macro}}`, a TWiki `%VARIABLE%`) are skipped,
//! as nothing is run or fetched.

use crate::{Html, esc, strip_tags};

// ---------------------------------------------------------------- the block writer

/// Blocks as HTML, with paragraphs, lists and tables kept open across lines.
#[derive(Default)]
pub(crate) struct Out {
    html: String,
    para: Vec<String>,
    /// Open lists, outermost first: `true` for numbered.
    lists: Vec<bool>,
    rows: Vec<Vec<(bool, String)>>,
    /// Indents of open indented list levels (for markups that nest by indenting).
    indents: Vec<usize>,
    title: Option<String>,
    warnings: Vec<String>,
}

impl Out {
    fn end_para(&mut self) {
        if !self.para.is_empty() {
            self.html.push_str("<p>");
            self.html.push_str(&self.para.join("\n"));
            self.html.push_str("</p>");
            self.para.clear();
        }
    }

    fn end_lists(&mut self) {
        while let Some(ordered) = self.lists.pop() {
            self.html.push_str(if ordered { "</li></ol>" } else { "</li></ul>" });
        }
        self.indents.clear();
    }

    fn end_table(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.html.push_str("<table>");
        for (n, row) in std::mem::take(&mut self.rows).into_iter().enumerate() {
            let header = n == 0 && row.iter().all(|(h, _)| *h);
            if header {
                self.html.push_str("<thead>");
            }
            self.html.push_str("<tr>");
            for (h, cell) in row {
                let tag = if h { "th" } else { "td" };
                self.html.push_str(&format!("<{tag}>{cell}</{tag}>"));
            }
            self.html.push_str("</tr>");
            if header {
                self.html.push_str("</thead>");
            }
        }
        self.html.push_str("</table>");
    }

    /// Ends whatever is open: a paragraph, lists, a table.
    pub fn flush(&mut self) {
        self.end_para();
        self.end_lists();
        self.end_table();
    }

    pub fn heading(&mut self, level: usize, html: &str) {
        self.flush();
        let level = level.clamp(1, 6);
        if level == 1 && self.title.is_none() {
            self.title = Some(strip_tags(html).trim().to_string());
        }
        self.html.push_str(&format!("<h{level}>{html}</h{level}>"));
    }

    /// A line of a paragraph.
    pub fn line(&mut self, html: String) {
        if !self.lists.is_empty() || !self.rows.is_empty() {
            self.end_lists();
            self.end_table();
        }
        self.para.push(html);
    }

    /// A list item at `depth` (from 1).
    pub fn item(&mut self, depth: usize, ordered: bool, html: &str) {
        self.end_para();
        self.end_table();
        let depth = depth.clamp(1, 32);
        while self.lists.len() > depth {
            let o = self.lists.pop().unwrap_or(false);
            self.html.push_str(if o { "</li></ol>" } else { "</li></ul>" });
        }
        if self.lists.len() == depth && self.lists.last() != Some(&ordered) {
            let o = self.lists.pop().unwrap_or(false);
            self.html.push_str(if o { "</li></ol>" } else { "</li></ul>" });
        }
        if self.lists.len() == depth {
            self.html.push_str("</li>");
        }
        while self.lists.len() < depth {
            self.html.push_str(if ordered { "<ol>" } else { "<ul>" });
            self.lists.push(ordered);
        }
        self.html.push_str(&format!("<li>{html}"));
    }

    /// A list item nested by its indent, for markups where indenting nests.
    pub fn indented_item(&mut self, indent: usize, ordered: bool, html: &str) {
        let mut indents = std::mem::take(&mut self.indents);
        if self.lists.is_empty() {
            indents.clear();
        }
        while indents.last().is_some_and(|&top| top > indent) {
            indents.pop();
        }
        if indents.last() != Some(&indent) {
            indents.push(indent);
        }
        let depth = indents.len();
        self.item(depth, ordered, html);
        self.indents = indents;
    }

    /// More text for the open list item.
    pub fn continue_item(&mut self, html: &str) -> bool {
        if self.lists.is_empty() {
            return false;
        }
        self.html.push_str("<br>");
        self.html.push_str(html);
        true
    }

    /// A table row: each cell header or not, as HTML.
    pub fn row(&mut self, cells: Vec<(bool, String)>) {
        self.end_para();
        self.end_lists();
        self.rows.push(cells);
    }

    /// A separator row (`|---|---|`) makes the row above it the header.
    pub fn header_separator(&mut self) {
        if self.rows.len() == 1 {
            self.rows[0].iter_mut().for_each(|c| c.0 = true);
        }
    }

    pub fn code(&mut self, lang: Option<&str>, text: &str) {
        self.flush();
        match lang.map(str::trim).filter(|l| !l.is_empty() && l.len() <= 32 && l.chars().all(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(c))) {
            Some(l) => self.html.push_str(&format!("<pre><code class=\"language-{}\">{}</code></pre>", l.to_ascii_lowercase(), esc(text))),
            None => self.html.push_str(&format!("<pre>{}</pre>", esc(text))),
        }
    }

    pub fn quote(&mut self, html: &str) {
        self.flush();
        self.html.push_str(&format!("<blockquote><p>{html}</p></blockquote>"));
    }

    pub fn rule(&mut self) {
        self.flush();
        self.html.push_str("<hr>");
    }

    pub fn image(&mut self, src: &str, alt: &str) {
        self.end_para();
        self.html.push_str(&format!("<img src=\"{}\" alt=\"{}\">", esc(src), esc(alt)));
    }

    pub fn finish(mut self) -> Html {
        self.flush();
        Html { html: self.html, title: self.title, warnings: self.warnings }
    }
}

// ---------------------------------------------------------------- the inline engine

/// One inline rule of a markup.
#[derive(Clone, Copy)]
pub(crate) enum Rule {
    /// `open text close` as an HTML element, its text read for more marks.
    Mark(&'static str, &'static str, &'static str),
    /// `open text close` as code, shown as written.
    Literal(&'static str, &'static str),
    /// `open inside close` as a link or image, by `parse` (which returns HTML).
    Link(&'static str, &'static str, fn(&str, &[Rule]) -> Option<String>),
    /// HTML elements a markup lets through (`<b>`, `<sup>`…): shown, not escaped.
    Html(&'static [&'static str]),
    /// `\x` makes `x` literal.
    Backslash,
}

/// How far a mark's close is looked for: past it, it isn't a mark.
const REACH: usize = 4000;

/// How many candidate closes a mark tries (each one that isn't a mark's end, like a
/// `*` before a space): with [`REACH`], what keeps hostile lines linear.
const MAX_TRIES: usize = 64;

/// `text` with `rules` applied, as HTML. Bare `http(s)://` addresses become links.
pub(crate) fn inline(text: &str, rules: &[Rule]) -> String {
    inline_depth(text, rules, 0)
}

fn inline_depth(text: &str, rules: &[Rule], depth: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    let starts = |at: usize, s: &str| s.chars().enumerate().all(|(k, c)| chars.get(at + k) == Some(&c));
    // For each closing string, where it next occurs from every position: made once,
    // in one pass, the first time it's looked for. Looking a close up is then constant,
    // so a line of thousands of unclosed marks stays linear.
    let nexts: std::cell::RefCell<Vec<(&str, Vec<usize>)>> = std::cell::RefCell::new(Vec::new());
    let next_at = |s: &'static str, from: usize| -> Option<usize> {
        let mut nexts = nexts.borrow_mut();
        let k = match nexts.iter().position(|(c, _)| *c == s) {
            Some(k) => k,
            None => {
                let mut next = vec![usize::MAX; chars.len() + 1];
                for j in (0..chars.len()).rev() {
                    next[j] = if starts(j, s) { j } else { next[j + 1] };
                }
                nexts.push((s, next));
                nexts.len() - 1
            }
        };
        nexts[k].1.get(from).copied().filter(|&p| p != usize::MAX)
    };
    let find = |from: usize, s: &'static str| -> Option<usize> { next_at(s, from).filter(|&p| p < from + REACH) };
    'outer: while i < chars.len() {
        let c = chars[i];
        if depth < 16 {
            for rule in rules {
                match *rule {
                    Rule::Backslash if c == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() => {
                        out.push_str(&esc(&chars[i + 1].to_string()));
                        i += 2;
                        continue 'outer;
                    }
                    Rule::Link(open, close, parse) if starts(i, open) => {
                        let from = i + open.chars().count();
                        if let Some(end) = find(from, close) {
                            let inside: String = chars[from..end].iter().collect();
                            if let Some(html) = parse(&inside, rules) {
                                out.push_str(&html);
                                i = end + close.chars().count();
                                continue 'outer;
                            }
                        }
                    }
                    Rule::Mark(open, close, _) | Rule::Literal(open, close) if starts(i, open) => {
                        let n = open.chars().count();
                        let single = n == 1;
                        let from = i + n;
                        // Opens before a non-space; a one-character mark only at a word's start.
                        let opens = chars.get(from).is_some_and(|x| !x.is_whitespace()) && (!single || i == 0 || !chars[i - 1].is_alphanumeric());
                        if opens {
                            let m = close.chars().count();
                            let mut j = from + 1;
                            // At most this many candidate closes, each found in one step.
                            let mut tries = 0;
                            while let Some(at) = next_at(close, j).filter(|&at| at < from + REACH && tries < MAX_TRIES) {
                                tries += 1;
                                j = at;
                                if !chars[j - 1].is_whitespace() && (!single || chars.get(j + m).is_none_or(|x| !x.is_alphanumeric())) {
                                    let inside: String = chars[from..j].iter().collect();
                                    match *rule {
                                        Rule::Mark(_, _, tag) => {
                                            // `strong><em`: elements nested, closed in reverse.
                                            let tags: Vec<&str> = tag.split("><").collect();
                                            tags.iter().for_each(|t| out.push_str(&format!("<{t}>")));
                                            out.push_str(&inline_depth(&inside, rules, depth + 1));
                                            tags.iter().rev().for_each(|t| out.push_str(&format!("</{t}>")));
                                        }
                                        _ => out.push_str(&format!("<code>{}</code>", esc(&inside))),
                                    }
                                    i = j + m;
                                    continue 'outer;
                                }
                                j += 1;
                            }
                        }
                    }
                    Rule::Html(allowed) if c == '<' => {
                        if let Some(end) = find(i, ">") {
                            let tag: String = chars[i + 1..end].iter().collect();
                            let name = tag.trim_start_matches('/').split(|x: char| x.is_whitespace() || x == '/').next().unwrap_or("").to_ascii_lowercase();
                            if allowed.contains(&name.as_str()) {
                                out.push_str(&format!("<{}>", if tag.starts_with('/') { format!("/{name}") } else { name }));
                                i = end + 1;
                                continue 'outer;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // A bare web address.
        if (starts(i, "https://") || starts(i, "http://")) && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let end = (i..chars.len()).find(|&j| chars[j].is_whitespace() || "<>\"]|".contains(chars[j])).unwrap_or(chars.len());
            let url: String = chars[i..end].iter().collect();
            let url = url.trim_end_matches(['.', ',', ';', ':', ')', '!', '?', '\'']);
            out.push_str(&format!("<a href=\"{0}\">{0}</a>", esc(url)));
            i += url.chars().count();
            continue;
        }
        out.push_str(&esc(&c.to_string()));
        i += 1;
    }
    out
}

/// A link's HTML: `label` read for marks, or the target when there's none.
fn a(target: &str, label: &str, rules: &[Rule]) -> String {
    let target = target.trim();
    let label = label.trim();
    let text = if label.is_empty() { esc(target) } else { inline(label, rules) };
    format!("<a href=\"{}\">{text}</a>", esc(target))
}

fn img(src: &str, alt: &str) -> String {
    format!("<img src=\"{}\" alt=\"{}\">", esc(src.trim()), esc(alt.trim()))
}

fn looks_like_image(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".bmp"].iter().any(|e| l.split(['?', '|', ' ']).next().unwrap_or("").ends_with(e))
}

/// `[[target|label]]` (MediaWiki, Creole, DokuWiki, Zim, Vimwiki).
fn pipe_link(inside: &str, rules: &[Rule]) -> Option<String> {
    if inside.contains('\n') {
        return None;
    }
    let (target, label) = inside.split_once('|').unwrap_or((inside, ""));
    let lower = target.trim().to_ascii_lowercase();
    // MediaWiki's files and images, and its categories (not shown).
    for prefix in ["file:", "image:", "datei:", "fichier:", "archivo:"] {
        if let Some(name) = lower.strip_prefix(prefix) {
            let caption = inside.rsplit('|').next().filter(|_| inside.contains('|')).unwrap_or("");
            let _ = name;
            return Some(img(&target.trim()[prefix.len()..], caption));
        }
    }
    if lower.starts_with("category:") || lower.starts_with("kategorie:") {
        return Some(String::new());
    }
    Some(a(target, label, rules))
}

/// `{{image|alt}}` (Creole, DokuWiki, Zim, Vimwiki).
fn brace_image(inside: &str, _: &[Rule]) -> Option<String> {
    if inside.contains('\n') || inside.trim().is_empty() {
        return None;
    }
    let (src, alt) = inside.split_once('|').unwrap_or((inside, ""));
    let src = src.trim().split('?').next().unwrap_or("");
    looks_like_image(src).then(|| img(src, alt)).or_else(|| Some(a(src, alt, &[])))
}

const HTML_INLINE: &[&str] = &["b", "i", "u", "s", "em", "strong", "code", "tt", "kbd", "var", "samp", "sup", "sub", "del", "ins", "strike", "small", "big", "br", "cite", "q", "mark", "abbr", "span"];

// ---------------------------------------------------------------- small helpers

/// `= text =` → (level, text): a run of `c` before and (optionally) after.
fn wrapped_heading(line: &str, c: char, need_close: bool) -> Option<(usize, &str)> {
    let t = line.trim_end();
    let open = t.chars().take_while(|&x| x == c).count();
    if open == 0 || open > 6 {
        return None;
    }
    let rest = &t[open..];
    let close = rest.chars().rev().take_while(|&x| x == c).count();
    if need_close && close != open {
        return None;
    }
    let inner = rest[..rest.len() - close].trim();
    if inner.is_empty() || (need_close && !rest.starts_with(char::is_whitespace) && !rest.is_empty() && close == 0) {
        return None;
    }
    Some((open, inner))
}

/// A list marker run (`**`, `#*#`) at the line's start followed by a space: (depth,
/// numbered, text).
fn repeated_marker<'a>(line: &'a str, bullets: &str, numbers: &str) -> Option<(usize, bool, &'a str)> {
    let marks: String = line.chars().take_while(|c| bullets.contains(*c) || numbers.contains(*c)).collect();
    if marks.is_empty() {
        return None;
    }
    let rest = &line[marks.len()..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let last = marks.chars().last().unwrap_or(' ');
    Some((marks.chars().count(), numbers.contains(last), rest.trim()))
}

/// An indented bullet (`  * x`, `   1. x`): (indent, numbered, text).
fn indented_marker(line: &str, tab: usize) -> Option<(usize, bool, &str)> {
    let indent: usize = line.chars().take_while(|c| c.is_whitespace()).map(|c| if c == '\t' { tab } else { 1 }).sum();
    let t = line.trim_start();
    for b in ["* ", "- ", "+ ", "• "] {
        if let Some(rest) = t.strip_prefix(b) {
            return Some((indent, false, rest.trim()));
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    let alpha = usize::from(digits == 0 && t.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && t[1..].starts_with(['.', ')']));
    let n = digits.max(alpha);
    if n > 0 && n < 10 {
        let rest = &t[n..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((indent, true, r.trim()));
        }
    }
    None
}

/// Cells of a `|`-separated row, each with the marker that made it a header.
fn pipe_cells(line: &str, sep: &str, header: &[&str], rules: &[Rule]) -> Vec<(bool, String)> {
    let t = line.trim();
    let t = t.strip_prefix(sep).unwrap_or(t);
    let t = t.strip_suffix(sep).unwrap_or(t);
    t.split(sep)
        .map(|c| {
            let c = c.trim();
            match header.iter().find_map(|h| c.strip_prefix(h)) {
                Some(rest) => (true, inline(rest.trim(), rules)),
                None => (false, inline(c, rules)),
            }
        })
        .collect()
}

fn is_separator_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | '+' | ' '))
}

/// Lines from `start` up to (not including) the first that `end` accepts; the index
/// after it.
fn until<'a>(lines: &[&'a str], start: usize, end: impl Fn(&str) -> bool) -> (Vec<&'a str>, usize) {
    match lines[start..].iter().position(|l| end(l)) {
        Some(n) => (lines[start..start + n].to_vec(), start + n + 1),
        None => (lines[start..].to_vec(), lines.len()),
    }
}

/// Lines from `start` while `keep` accepts them, and the index of the first it doesn't.
fn run<'a>(lines: &[&'a str], start: usize, keep: impl Fn(&str) -> bool) -> (Vec<&'a str>, usize) {
    let end = lines[start..].iter().position(|l| !keep(l)).map_or(lines.len(), |n| start + n);
    (lines[start..end].to_vec(), end)
}

fn lines_of(text: &str) -> Vec<&str> {
    text.lines().collect()
}

/// A line that only says the reader of a pandoc template something (`$body$`).
fn template_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('$') && t.ends_with('$') && t.len() > 1 && !t.contains(' ')
}

// ---------------------------------------------------------------- MediaWiki


fn mw_external(inside: &str, rules: &[Rule]) -> Option<String> {
    let inside = inside.trim();
    let url_like = ["http://", "https://", "ftp://", "mailto:", "//", "irc://", "news:"].iter().any(|p| inside.starts_with(p));
    if !url_like || inside.contains('\n') {
        return None;
    }
    let (url, label) = inside.split_once(' ').unwrap_or((inside, ""));
    Some(a(url, label, rules))
}

fn mw_ref(inside: &str, rules: &[Rule]) -> Option<String> {
    let body = inside.split_once('>').map_or("", |(_, b)| b);
    Some(format!("<sup>[{}]</sup>", inline(body, rules)))
}

const MW: &[Rule] = &[
    Rule::Link("<ref", "</ref>", mw_ref),
    Rule::Literal("<code>", "</code>"),
    Rule::Literal("<nowiki>", "</nowiki>"),
    Rule::Link("[[", "]]", pipe_link),
    Rule::Link("[", "]", mw_external),
    Rule::Mark("'''''", "'''''", "strong><em"),
    Rule::Mark("'''", "'''", "strong"),
    Rule::Mark("''", "''", "em"),
    Rule::Html(HTML_INLINE),
];

/// Wikitext without `{{templates}}` (nested), `<!-- comments -->` and `__MAGIC__`
/// words.
fn mw_strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.split_once("-->").map_or("", |(_, after)| after);
        } else if rest.starts_with("<ref") && rest.find('>').is_some_and(|at| rest[..at].ends_with('/')) {
            // `<ref name="a" />`: a reference to a note already given.
            rest = &rest[rest.find('>').map_or(rest.len(), |at| at + 1)..];
        } else if let Some(r) = rest.strip_prefix("{{") {
            depth += 1;
            rest = r;
        } else if depth > 0 && rest.starts_with("}}") {
            depth -= 1;
            rest = &rest[2..];
        } else {
            let c = rest.chars().next().unwrap_or(' ');
            if depth == 0 {
                out.push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    for magic in ["__TOC__", "__NOTOC__", "__FORCETOC__", "__NOEDITSECTION__"] {
        out = out.replace(magic, "");
    }
    out
}

pub fn mediawiki(text: &str) -> Html {
    let text = mw_strip(text);
    let lines = lines_of(&text);
    let mut o = Out::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        i += 1;
        if template_line(line) {
            continue;
        }
        if t.is_empty() {
            o.flush();
            continue;
        }
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("<pre") || lower.starts_with("<syntaxhighlight") || lower.starts_with("<source") {
            let tag = if lower.starts_with("<pre") { "pre" } else if lower.starts_with("<source") { "source" } else { "syntaxhighlight" };
            let lang = attr(t, "lang");
            let first = t.split_once('>').map_or("", |(_, r)| r);
            let close = format!("</{tag}>");
            if let Some(body) = first.to_ascii_lowercase().find(&close).map(|at| &first[..at]) {
                o.code(lang.as_deref(), body);
                continue;
            }
            let (mut body, next) = until(&lines, i, |l| l.to_ascii_lowercase().contains(&close));
            if !first.is_empty() {
                body.insert(0, first);
            }
            if let Some(last) = lines.get(next.wrapping_sub(1)).filter(|l| next > i && l.to_ascii_lowercase().contains(&close)) {
                let before = last.to_ascii_lowercase().find(&close).map_or("", |at| &last[..at]);
                if !before.trim().is_empty() {
                    body.push(before);
                }
            }
            o.code(lang.as_deref(), &body.join("\n"));
            i = next;
            continue;
        }
        if t.starts_with("{|") {
            let (body, next) = until(&lines, i, |l| l.trim_start().starts_with("|}"));
            mw_table(&body, &mut o);
            i = next;
            continue;
        }
        if let Some((level, inner)) = wrapped_heading(t, '=', true) {
            o.heading(level, &inline(inner, MW));
            continue;
        }
        if t.len() >= 4 && t.chars().all(|c| c == '-') {
            o.rule();
            continue;
        }
        if let Some(marks) = Some(t.chars().take_while(|c| "*#:;".contains(*c)).collect::<String>()).filter(|m| !m.is_empty()) {
            let rest = t[marks.len()..].trim();
            let last = marks.chars().last().unwrap_or('*');
            if marks.chars().all(|c| c == ':') {
                o.quote(&inline(rest, MW));
            } else if last == ';' {
                let (term, def) = rest.split_once(" : ").unwrap_or((rest, ""));
                o.item(marks.len(), false, &format!("<strong>{}</strong> {}", inline(term, MW), inline(def, MW)));
            } else {
                o.item(marks.len(), last == '#', &inline(rest, MW));
            }
            continue;
        }
        if line.starts_with(' ') {
            // Leading space: preformatted, as a run of such lines.
            let (body, next) = run(&lines, i - 1, |l| l.starts_with(' ') && !l.trim().is_empty());
            o.code(None, &body.iter().map(|l| &l[1..]).collect::<Vec<_>>().join("\n"));
            i = next;
            continue;
        }
        o.line(inline(t, MW));
    }
    o.finish()
}

/// `name="value"` in a tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag[at..];
    let v = match rest.chars().next()? {
        q @ ('"' | '\'') => rest[1..].split(q).next()?,
        _ => rest.split(|c: char| c.is_whitespace() || c == '>').next()?,
    };
    Some(v.to_string())
}

fn mw_table(lines: &[&str], o: &mut Out) {
    let mut row: Vec<(bool, String)> = Vec::new();
    // A cell's attributes (`style="…" | text`) aren't shown.
    let content = |c: &str| -> String {
        let c = c.trim();
        match c.split_once('|') {
            Some((attrs, text)) if attrs.contains('=') && !attrs.contains("[[") => inline(text.trim(), MW),
            _ => inline(c, MW),
        }
    };
    for line in lines {
        let t = line.trim();
        if t.starts_with("|-") {
            if !row.is_empty() {
                o.row(std::mem::take(&mut row));
            }
        } else if t.starts_with("|+") {
            continue;
        } else if let Some(r) = t.strip_prefix('!') {
            row.extend(r.split("!!").flat_map(|c| c.split("||")).map(|c| (true, content(c))));
        } else if let Some(r) = t.strip_prefix('|') {
            row.extend(r.split("||").map(|c| (false, content(c))));
        } else if let Some(last) = row.last_mut() {
            // A cell's text continues on the next line.
            last.1.push(' ');
            last.1.push_str(&inline(t, MW));
        }
    }
    if !row.is_empty() {
        o.row(row);
    }
    o.end_table();
}

// ---------------------------------------------------------------- Creole


const CREOLE_RULES: &[Rule] = &[Rule::Literal("{{{", "}}}"), Rule::Link("[[", "]]", pipe_link), Rule::Link("{{", "}}", brace_image), Rule::Mark("**", "**", "strong"), Rule::Mark("//", "//", "em"), Rule::Link("\\\\", "", |_, _| Some("<br>".into())), Rule::Backslash];

pub fn creole(text: &str) -> Html {
    simple_wiki(text, &WikiSyntax { heading: Heading::Wrapped('=', false), bullets: "*", numbers: "#", rules: CREOLE_RULES, code: &[("{{{", "}}}")], table: Some(("|", &["="])), quote: None, indent_lists: None })
}

// ---------------------------------------------------------------- a shared shape

/// How a wiki writes headings.
#[derive(Clone, Copy)]
enum Heading {
    /// `== text ==`, the run's length the level; the close optional unless `true`.
    Wrapped(char, bool),
    /// `====== text ======` with six the top level (DokuWiki, Zim).
    Inverted,
}

/// The common shape of a line-based wiki.
struct WikiSyntax {
    heading: Heading,
    bullets: &'static str,
    numbers: &'static str,
    rules: &'static [Rule],
    /// Code blocks: a line starting with the open, up to a line with the close.
    code: &'static [(&'static str, &'static str)],
    /// Tables: the cell separator, and prefixes that make a cell a header.
    table: Option<(&'static str, &'static [&'static str])>,
    quote: Option<&'static str>,
    /// Lists nest by indenting (with a tab this wide), not by repeating the marker.
    indent_lists: Option<usize>,
}

fn simple_wiki(text: &str, s: &WikiSyntax) -> Html {
    let lines = lines_of(text);
    let mut o = Out::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        i += 1;
        if template_line(line) || t == "__TOC__" {
            continue;
        }
        if t.is_empty() {
            o.flush();
            continue;
        }
        if let Some((open, close)) = s.code.iter().find(|(open, _)| t.starts_with(open)) {
            let after = t[open.len()..].trim();
            // `{{{code}}}` on one line, or a language after the open (`<code rust>`).
            if let Some(inner) = after.strip_suffix(close) {
                o.code(None, inner);
                continue;
            }
            // Code that closes on its own line, then text: inline code, not a block.
            if !after.contains(close) {
            let lang = after.trim_end_matches(['>', '}', ':']).trim_start_matches([':', '=']).split_whitespace().next().map(|l| l.trim_matches(['"', '\'']).trim_start_matches("lang=").trim_start_matches("language=").trim_matches(['"', '\'']).to_string());
            let (body, next) = until(&lines, i, |l| l.trim_start().starts_with(close) || l.trim_end().ends_with(close));
            let mut body: Vec<String> = body.iter().map(|l| l.to_string()).collect();
            if let Some(last) = lines.get(next.wrapping_sub(1)).filter(|l| next > i && l.contains(close)) {
                let before = last.trim_end().strip_suffix(close).unwrap_or("");
                if !before.trim().is_empty() {
                    body.push(before.to_string());
                }
            }
            o.code(lang.as_deref(), &body.join("\n"));
            i = next;
            continue;
            }
        }
        let heading = match s.heading {
            Heading::Wrapped(c, need) => wrapped_heading(t, c, need).filter(|_| line.starts_with(c)),
            Heading::Inverted => wrapped_heading(t, '=', false).filter(|(n, _)| *n >= 2 && line.starts_with('=')).map(|(n, inner)| (7 - n.min(6), inner)),
        };
        if let Some((level, inner)) = heading {
            o.heading(level, &inline(inner, s.rules));
            continue;
        }
        if t.len() >= 4 && t.chars().all(|c| c == '-') {
            o.rule();
            continue;
        }
        if let Some((sep, header)) = s.table
            && (t.starts_with(sep) || (header == ["^"] && t.starts_with('^'))) {
                if is_separator_row(t) {
                    o.header_separator();
                } else {
                    let row = if header == ["^"] {
                        // DokuWiki: `^` separates header cells, `|` others.
                        doku_row(t, s.rules)
                    } else {
                        pipe_cells(t, sep, header, s.rules)
                    };
                    o.row(row);
                }
                continue;
            }
        if let Some(q) = s.quote
            && let Some(rest) = t.strip_prefix(q) {
                o.quote(&inline(rest.trim(), s.rules));
                continue;
            }
        if let Some(tab) = s.indent_lists {
            if let Some((indent, ordered, rest)) = indented_marker(line, tab) {
                o.indented_item(indent, ordered, &inline(rest, s.rules));
                continue;
            }
        } else if let Some((depth, ordered, rest)) = repeated_marker(t, s.bullets, s.numbers) {
            o.item(depth, ordered, &inline(rest, s.rules));
            continue;
        }
        if line.starts_with([' ', '\t']) && o.continue_item(&inline(t, s.rules)) {
            continue;
        }
        o.line(inline(t, s.rules));
    }
    o.finish()
}

// ---------------------------------------------------------------- DokuWiki


const DOKU: &[Rule] = &[
    Rule::Literal("''", "''"),
    Rule::Literal("%%", "%%"),
    Rule::Link("[[", "]]", pipe_link),
    Rule::Link("{{", "}}", brace_image),
    Rule::Mark("**", "**", "strong"),
    Rule::Mark("//", "//", "em"),
    Rule::Mark("__", "__", "u"),
    Rule::Link("\\\\ ", "", |_, _| Some("<br>".into())),
    Rule::Html(&["del", "sup", "sub"]),
];

fn doku_row(line: &str, rules: &[Rule]) -> Vec<(bool, String)> {
    let mut cells = Vec::new();
    let t = line.trim();
    let mut header = t.starts_with('^');
    let mut cell = String::new();
    for c in t.chars().skip(1) {
        if c == '|' || c == '^' {
            cells.push((header, inline(cell.trim(), rules)));
            cell.clear();
            header = c == '^';
        } else {
            cell.push(c);
        }
    }
    if !cell.trim().is_empty() {
        cells.push((header, inline(cell.trim(), rules)));
    }
    cells
}

pub fn dokuwiki(text: &str) -> Html {
    // Code: `<code lang>…</code>` or `<file>`; lines indented two spaces that aren't
    // list items are code too.
    let mut joined = String::with_capacity(text.len());
    let mut run: Vec<String> = Vec::new();
    let flush = |run: &mut Vec<String>, joined: &mut String| {
        if !run.is_empty() {
            joined.push_str("<code>\n");
            for l in run.drain(..) {
                joined.push_str(&l[2..]);
                joined.push('\n');
            }
            joined.push_str("</code>\n");
        }
    };
    for line in text.lines() {
        // DokuWiki's `-` item is numbered.
        let line: std::borrow::Cow<str> = match line.trim_start().strip_prefix("- ") {
            Some(rest) if line.starts_with("  ") => format!("{}1. {rest}", &line[..line.len() - line.trim_start().len()]).into(),
            _ => line.into(),
        };
        let line = &*line;
        if line.starts_with("  ") && indented_marker(line, 2).is_none() && !line.trim().is_empty() {
            run.push(line.to_string());
        } else {
            flush(&mut run, &mut joined);
            joined.push_str(line);
            joined.push('\n');
        }
    }
    flush(&mut run, &mut joined);
    simple_wiki(&joined, &WikiSyntax { heading: Heading::Inverted, bullets: "", numbers: "", rules: DOKU, code: &[("<code", "</code>"), ("<file", "</file>"), ("<nowiki>", "</nowiki>")], table: Some(("|", &["^"])), quote: Some(">"), indent_lists: Some(2) })
}

// ---------------------------------------------------------------- Jira


fn jira_link(inside: &str, rules: &[Rule]) -> Option<String> {
    if inside.contains('\n') || inside.is_empty() {
        return None;
    }
    let (label, url) = inside.split_once('|').unwrap_or(("", inside));
    if url.starts_with('~') || url.starts_with('^') {
        return Some(esc(inside));
    }
    Some(a(url, label, rules))
}

fn jira_image(inside: &str, _: &[Rule]) -> Option<String> {
    let src = inside.split('|').next().unwrap_or("");
    (!src.contains(' ') && !src.is_empty()).then(|| img(src, ""))
}

const JIRA_RULES: &[Rule] = &[
    Rule::Literal("{{", "}}"),
    Rule::Link("[", "]", jira_link),
    Rule::Link("!", "!", jira_image),
    Rule::Mark("*", "*", "strong"),
    Rule::Mark("_", "_", "em"),
    Rule::Mark("??", "??", "cite"),
    Rule::Mark("-", "-", "del"),
    Rule::Mark("+", "+", "u"),
    Rule::Mark("^", "^", "sup"),
    Rule::Mark("~", "~", "sub"),
    Rule::Backslash,
];

pub fn jira(text: &str) -> Html {
    let text = text.replace("{quote}", "\n{quote}\n");
    let lines = lines_of(&text);
    let mut o = Out::default();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        i += 1;
        if t.starts_with("{code") || t.starts_with("{noformat") {
            let lang = t.strip_prefix("{code:").and_then(|r| r.split(['}', '|']).next()).filter(|l| !l.contains('='));
            let close = if t.starts_with("{code") { "{code}" } else { "{noformat}" };
            let after = t.split_once('}').map_or("", |(_, r)| r);
            if let Some(inner) = after.strip_suffix(close) {
                o.code(lang, inner);
                continue;
            }
            let (body, next) = until(&lines, i, |l| l.contains(close));
            o.code(lang, &body.join("\n"));
            i = next;
            continue;
        }
        if t == "{quote}" {
            let (body, next) = until(&lines, i, |l| l.trim() == "{quote}");
            o.quote(&body.iter().map(|l| inline(l.trim(), JIRA_RULES)).collect::<Vec<_>>().join("<br>"));
            i = next;
            continue;
        }
        if template_line(t) {
            continue;
        }
        if t.is_empty() {
            o.flush();
            continue;
        }
        if let Some(r) = t.strip_prefix("bq. ") {
            o.quote(&inline(r, JIRA_RULES));
        } else if t.starts_with("||") {
            o.row(pipe_cells(&t.replace("||", "|\u{1}"), "|", &["\u{1}"], JIRA_RULES).into_iter().filter(|(_, c)| !c.is_empty()).collect());
        } else if t.starts_with('|') {
            o.row(pipe_cells(t, "|", &[], JIRA_RULES));
        } else if let Some((level, inner)) = t.strip_prefix('h').and_then(|r| r.chars().next().filter(|c| ('1'..='6').contains(c)).map(|c| (c as usize - '0' as usize, &r[1..]))).and_then(|(n, r)| r.strip_prefix(". ").map(|r| (n, r))) {
            o.heading(level, &inline(inner, JIRA_RULES));
        } else if t == "----" {
            o.rule();
        } else if let Some((depth, ordered, rest)) = repeated_marker(t, "*-", "#") {
            o.item(depth, ordered, &inline(rest, JIRA_RULES));
        } else if t.starts_with('{') && t.ends_with('}') && !t.contains(' ') {
            // A macro of its own (`{panel}`, `{color}`): not shown.
        } else {
            o.line(inline(t, JIRA_RULES));
        }
    }
    o.finish()
}

// ---------------------------------------------------------------- txt2tags


fn t2t_link(inside: &str, rules: &[Rule]) -> Option<String> {
    if inside.contains('\n') {
        return None;
    }
    let inside = inside.trim();
    if looks_like_image(inside) && !inside.contains(' ') {
        return Some(img(inside, ""));
    }
    let (label, url) = inside.rsplit_once(' ')?;
    if !(url.contains("://") || url.contains('.') || url.starts_with('#') || url.starts_with("mailto:")) {
        return None;
    }
    Some(a(url, label, rules))
}

const T2T: &[Rule] = &[
    Rule::Literal("``", "``"),
    Rule::Literal("\"\"", "\"\""),
    Rule::Literal("''", "''"),
    Rule::Link("[", "]", t2t_link),
    Rule::Mark("**", "**", "strong"),
    Rule::Mark("//", "//", "em"),
    Rule::Mark("__", "__", "u"),
    Rule::Mark("--", "--", "del"),
];

pub fn txt2tags(text: &str) -> Html {
    let lines = lines_of(text);
    let mut o = Out::default();
    // The header: up to three lines (title, author, date) unless the first is blank.
    let mut i = 0;
    if lines.first().is_some_and(|l| !l.trim().is_empty() && !l.starts_with('%')) {
        let title = lines[0].trim();
        if !template_line(title) {
            o.heading(1, &inline(title, T2T));
        }
        i = 1;
        while i < lines.len().min(3) && !lines[i].trim().is_empty() {
            if !template_line(lines[i]) && !lines[i].contains("$for") {
                o.line(inline(lines[i].trim(), T2T));
            }
            i += 1;
        }
        o.flush();
    }
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        i += 1;
        if line.starts_with('%') || template_line(line) || t == "__TOC__" {
            continue;
        }
        if t.is_empty() {
            o.flush();
            continue;
        }
        if t == "```" || t == "\"\"\"" || t == "'''" {
            let close = t.to_string();
            let (body, next) = until(&lines, i, |l| l.trim() == close);
            if close == "```" {
                o.code(None, &body.join("\n"));
            }
            i = next;
            continue;
        }
        if let Some(r) = line.strip_prefix("``` ") {
            o.code(None, r);
            continue;
        }
        if let Some((level, inner)) = wrapped_heading(t, '=', true).or_else(|| wrapped_heading(t, '+', true)) {
            o.heading(level, &inline(inner.split("[").next().unwrap_or(inner).trim(), T2T));
            continue;
        }
        if t.len() >= 20 && t.chars().all(|c| matches!(c, '-' | '=' | '_')) {
            o.rule();
            continue;
        }
        if t.starts_with('|') {
            let header = t.starts_with("||");
            let row = pipe_cells(t.trim_start_matches('|'), "|", &[], T2T).into_iter().map(|(_, c)| (header, c)).collect();
            o.row(row);
            continue;
        }
        if line.starts_with('\t') {
            o.quote(&inline(t, T2T));
            continue;
        }
        if let Some(rest) = t.strip_prefix("+ ") {
            let indent = line.len() - line.trim_start().len();
            o.indented_item(indent, true, &inline(rest, T2T));
            continue;
        }
        if let Some((indent, ordered, rest)) = indented_marker(line, 8).filter(|(_, _, r)| !r.is_empty()) {
            o.indented_item(indent, ordered, &inline(rest, T2T));
            continue;
        }
        if let Some(rest) = t.strip_prefix(": ") {
            o.line(format!("<strong>{}</strong>", inline(rest, T2T)));
            continue;
        }
        o.line(inline(t, T2T));
    }
    o.finish()
}

// ---------------------------------------------------------------- Muse


fn muse_link(inside: &str, rules: &[Rule]) -> Option<String> {
    if inside.contains('\n') {
        return None;
    }
    let (url, label) = inside.split_once("][").unwrap_or((inside, ""));
    if looks_like_image(url) && label.is_empty() {
        return Some(img(url, ""));
    }
    Some(a(url, label, rules))
}

const MUSE_RULES: &[Rule] = &[
    Rule::Literal("<code>", "</code>"),
    Rule::Literal("=", "="),
    Rule::Link("[[", "]]", muse_link),
    Rule::Mark("***", "***", "strong><em"),
    Rule::Mark("**", "**", "strong"),
    Rule::Mark("*", "*", "em"),
    Rule::Mark("_", "_", "u"),
    Rule::Html(&["em", "strong", "sup", "sub", "del", "br", "verbatim"]),
];

pub fn muse(text: &str) -> Html {
    let lines = lines_of(text);
    let mut o = Out::default();
    let mut i = 0;
    let mut title = None;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        i += 1;
        if template_line(line) || t.starts_with("$if") || t.starts_with("$endif") {
            continue;
        }
        if let Some(r) = line.strip_prefix("#title ") {
            title = Some(r.trim().to_string());
            continue;
        }
        if line.starts_with('#') && line.chars().nth(1).is_some_and(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        if line.starts_with("; ") || t == ";" {
            continue;
        }
        if t.is_empty() {
            o.flush();
            continue;
        }
        let lower = t.to_ascii_lowercase();
        for (open, close, code) in [("<example>", "</example>", true), ("<src", "</src>", true), ("<verse>", "</verse>", false), ("<quote>", "</quote>", false), ("<literal", "</literal>", false), ("<comment>", "</comment>", false), ("<center>", "</center>", false), ("<right>", "</right>", false), ("{{{", "}}}", true)] {
            if lower.starts_with(open) {
                let lang = attr(t, "lang");
                let (body, next) = until(&lines, i, |l| l.trim().to_ascii_lowercase().starts_with(close));
                match open {
                    "<verse>" | "<quote>" | "<center>" | "<right>" => o.quote(&body.iter().map(|l| inline(l.trim(), MUSE_RULES)).collect::<Vec<_>>().join("<br>")),
                    _ if code => o.code(lang.as_deref(), &body.join("\n")),
                    _ => {}
                }
                i = next;
                break;
            }
        }
        if lower.starts_with('<') && ["<example>", "<src", "<verse>", "<quote>", "<literal", "<comment>", "<center>", "<right>"].iter().any(|o| lower.starts_with(o)) || lower.starts_with("{{{") {
            continue;
        }
        let stars = line.chars().take_while(|&c| c == '*').count();
        if stars > 0 && stars <= 5 && line[stars..].starts_with(' ') {
            o.heading(stars, &inline(line[stars..].trim(), MUSE_RULES));
            continue;
        }
        if t.len() >= 4 && t.chars().all(|c| c == '-') {
            o.rule();
            continue;
        }
        if t.contains(" || ") || t.contains(" | ") && !line.starts_with(' ') {
            let header = t.contains(" || ");
            let cells = t.split(if header { " || " } else { " | " }).map(|c| (header, inline(c.trim().trim_matches('|').trim(), MUSE_RULES))).collect();
            o.row(cells);
            continue;
        }
        if line.starts_with(' ') {
            if let Some((indent, ordered, rest)) = indented_marker(line, 8) {
                o.indented_item(indent, ordered, &inline(rest, MUSE_RULES));
                continue;
            }
            if o.continue_item(&inline(t, MUSE_RULES)) {
                continue;
            }
            o.quote(&inline(t, MUSE_RULES));
            continue;
        }
        o.line(inline(t, MUSE_RULES));
    }
    let mut r = o.finish();
    if title.is_some() {
        r.title = title;
    }
    r
}

// ---------------------------------------------------------------- Haddock


fn haddock_link(inside: &str, rules: &[Rule]) -> Option<String> {
    if inside.contains('\n') || inside.is_empty() {
        return None;
    }
    if let Some(src) = inside.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        let (src, alt) = src.split_once(' ').unwrap_or((src, ""));
        return Some(img(src, alt));
    }
    let (url, label) = inside.split_once(' ').unwrap_or((inside, ""));
    (url.contains(':') || url.contains('.') || url.contains('/')).then(|| a(url, label, rules))
}

const HADDOCK_RULES: &[Rule] = &[
    Rule::Literal("@", "@"),
    Rule::Link("<", ">", haddock_link),
    Rule::Mark("__", "__", "strong"),
    Rule::Mark("/", "/", "em"),
    Rule::Backslash,
];

pub fn haddock(text: &str) -> Html {
    let lines = lines_of(text);
    let mut o = Out::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        i += 1;
        if template_line(line) {
            continue;
        }
        if t.is_empty() {
            o.flush();
            continue;
        }
        if t == "@" {
            let (body, next) = until(&lines, i, |l| l.trim() == "@");
            o.code(Some("haskell"), &body.join("\n"));
            i = next;
            continue;
        }
        if t.starts_with('>') {
            // Bird tracks (`> code`) and examples (`>>> expr`), as a run.
            let (body, next) = run(&lines, i - 1, |l| l.trim_start().starts_with('>'));
            o.code(Some("haskell"), &body.iter().map(|l| l.trim_start().strip_prefix("> ").unwrap_or(l.trim_start())).collect::<Vec<_>>().join("\n"));
            i = next;
            continue;
        }
        if let Some((level, inner)) = wrapped_heading(t, '=', false).filter(|_| line.starts_with('=')) {
            o.heading(level, &inline(inner, HADDOCK_RULES));
            continue;
        }
        if let Some(rest) = t.strip_prefix('[').and_then(|r| r.split_once(']')) {
            o.item(1, false, &format!("<strong>{}</strong> {}", inline(rest.0, HADDOCK_RULES), inline(rest.1.trim_start_matches(':').trim(), HADDOCK_RULES)));
            continue;
        }
        if let Some(rest) = t.strip_prefix('(').and_then(|r| r.split_once(") ")).filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit())) {
            o.indented_item(line.len() - line.trim_start().len(), true, &inline(rest.1, HADDOCK_RULES));
            continue;
        }
        if let Some((indent, ordered, rest)) = indented_marker(line, 8) {
            o.indented_item(indent, ordered, &inline(rest, HADDOCK_RULES));
            continue;
        }
        o.line(inline(t, HADDOCK_RULES));
    }
    o.finish()
}

// ---------------------------------------------------------------- XWiki


fn xwiki_link(inside: &str, rules: &[Rule]) -> Option<String> {
    if inside.contains('\n') {
        return None;
    }
    let (label, target) = inside.split_once(">>").unwrap_or(("", inside));
    let target = target.split("||").next().unwrap_or(target);
    if let Some(src) = target.strip_prefix("image:") {
        return Some(img(src, label));
    }
    let target = target.trim_start_matches("url:").trim_start_matches("doc:").trim_start_matches("mailto:");
    Some(a(target, label, rules))
}

const XWIKI_RULES: &[Rule] = &[
    Rule::Literal("##", "##"),
    Rule::Literal("{{{", "}}}"),
    Rule::Link("[[", "]]", xwiki_link),
    Rule::Mark("**", "**", "strong"),
    Rule::Mark("//", "//", "em"),
    Rule::Mark("__", "__", "u"),
    Rule::Mark("--", "--", "del"),
    Rule::Mark("^^", "^^", "sup"),
    Rule::Mark(",,", ",,", "sub"),
    Rule::Link("\\\\", "", |_, _| Some("<br>".into())),
    Rule::Link("{{", "}}", |inside, _| (!inside.contains('\n')).then(String::new)),
];

pub fn xwiki(text: &str) -> Html {
    let lines = lines_of(text);
    let mut o = Out::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        i += 1;
        if template_line(line) {
            continue;
        }
        if t.is_empty() {
            o.flush();
            continue;
        }
        if t.starts_with("{{code") || t.starts_with("{{{") {
            let close = if t.starts_with("{{code") { "{{/code}}" } else { "}}}" };
            let lang = attr(t, "language").or_else(|| attr(t, "lang"));
            let after = t.split_once(if close == "}}}" { "{{{" } else { "}}" }).map_or("", |(_, r)| r);
            if let Some(inner) = after.strip_suffix(close) {
                o.code(lang.as_deref(), inner);
                continue;
            }
            let (body, next) = until(&lines, i, |l| l.contains(close));
            o.code(lang.as_deref(), &body.join("\n"));
            i = next;
            continue;
        }
        if t.starts_with("{{") && (t.ends_with("/}}") || t.ends_with("}}") && !t[2..].contains("{{")) {
            // A macro on its own line (`{{toc/}}`, `{{box}}`): not shown.
            continue;
        }
        if let Some((level, inner)) = wrapped_heading(t, '=', false).filter(|_| t.starts_with('=')) {
            o.heading(level, &inline(inner, XWIKI_RULES));
            continue;
        }
        if t == "----" {
            o.rule();
            continue;
        }
        if t.starts_with('|') {
            o.row(pipe_cells(t, "|", &["="], XWIKI_RULES));
            continue;
        }
        if let Some(r) = t.strip_prefix('>') {
            o.quote(&inline(r.trim_start_matches('>').trim(), XWIKI_RULES));
            continue;
        }
        let marks: String = t.chars().take_while(|c| *c == '*' || *c == '1' || *c == '.').collect();
        if !marks.is_empty() && t[marks.len()..].starts_with(' ') && (marks.chars().all(|c| c == '*') || marks.ends_with('.')) {
            let depth = marks.trim_end_matches('.').chars().count();
            o.item(depth, marks.ends_with('.'), &inline(t[marks.len()..].trim(), XWIKI_RULES));
            continue;
        }
        o.line(inline(t, XWIKI_RULES));
    }
    o.finish()
}

// ---------------------------------------------------------------- TikiWiki


fn tiki_link(inside: &str, rules: &[Rule]) -> Option<String> {
    if inside.contains('\n') || inside.is_empty() {
        return None;
    }
    let (url, label) = inside.split_once('|').unwrap_or((inside, ""));
    Some(a(url, label, rules))
}

const TIKI: &[Rule] = &[
    Rule::Literal("-+", "+-"),
    Rule::Link("((", "))", tiki_link),
    Rule::Link("[", "]", tiki_link),
    Rule::Mark("__", "__", "strong"),
    Rule::Mark("''", "''", "em"),
    Rule::Mark("===", "===", "u"),
    Rule::Mark("--", "--", "del"),
    Rule::Link("%%%", "", |_, _| Some("<br>".into())),
    Rule::Link("{img", "}", |inside, _| attr(inside, "src").map(|s| img(&s, &attr(inside, "alt").unwrap_or_default()))),
];

pub fn tikiwiki(text: &str) -> Html {
    let lines = lines_of(text);
    let mut o = Out::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_end();
        i += 1;
        if t.trim().is_empty() {
            o.flush();
            continue;
        }
        if t.starts_with("{CODE") || t.starts_with("~pp~") || t.starts_with("{code") {
            let close = if t.starts_with("~pp~") { "~/pp~" } else if t.starts_with("{CODE") { "{CODE}" } else { "{code}" };
            let lang = attr(t, "colors").or_else(|| attr(t, "lang"));
            let after = t.split_once(if close == "~/pp~" { "~pp~" } else { ")}" }).map_or("", |(_, r)| r);
            if let Some(inner) = after.strip_suffix(close) {
                o.code(lang.as_deref(), inner);
                continue;
            }
            let (body, next) = until(&lines, i, |l| l.contains(close));
            o.code(lang.as_deref(), &body.join("\n"));
            i = next;
            continue;
        }
        if let Some(first) = t.strip_prefix("||") {
            // `||a|b\nc|d||`: rows by line, cells by `|`.
            let mut rows = vec![t.trim_start_matches("||").to_string()];
            if !first.contains("||") {
                let (body, next) = until(&lines, i, |l| l.contains("||"));
                rows.extend(body.iter().map(|l| l.to_string()));
                if let Some(last) = lines.get(next.wrapping_sub(1)).filter(|l| next > i && l.contains("||")) {
                    rows.push(last.to_string());
                }
                i = next;
            }
            for r in &rows {
                let r = r.trim().trim_end_matches("||");
                if r.is_empty() {
                    continue;
                }
                o.row(r.split('|').map(|c| (false, inline(c.trim(), TIKI))).collect());
            }
            o.end_table();
            continue;
        }
        let bangs = t.chars().take_while(|&c| c == '!').count();
        if (1..=6).contains(&bangs) {
            let inner = t[bangs..].trim_start_matches(['+', '-']).trim();
            o.heading(bangs, &inline(inner, TIKI));
            continue;
        }
        if t.trim() == "---" || t.trim() == "----" {
            o.rule();
            continue;
        }
        if let Some((depth, ordered, rest)) = repeated_marker(t, "*", "#") {
            o.item(depth, ordered, &inline(rest, TIKI));
            continue;
        }
        if let Some(rest) = t.strip_prefix('^').and_then(|r| r.strip_suffix('^')) {
            o.quote(&inline(rest, TIKI));
            continue;
        }
        o.line(inline(t.trim(), TIKI));
    }
    o.finish()
}

// ---------------------------------------------------------------- TWiki


fn twiki_link(inside: &str, rules: &[Rule]) -> Option<String> {
    if inside.contains('\n') {
        return None;
    }
    let (url, label) = inside.split_once("][").unwrap_or((inside, ""));
    Some(a(url, label, rules))
}

const TWIKI_RULES: &[Rule] = &[
    Rule::Link("[[", "]]", twiki_link),
    Rule::Literal("==", "=="),
    Rule::Literal("=", "="),
    Rule::Mark("__", "__", "strong><em"),
    Rule::Mark("*", "*", "strong"),
    Rule::Mark("_", "_", "em"),
    Rule::Link("%", "%", |inside, _| inside.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c == '{' || c == '}').then(String::new)),
    Rule::Html(HTML_INLINE),
];

pub fn twiki(text: &str) -> Html {
    let lines = lines_of(text);
    let mut o = Out::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_end();
        i += 1;
        if t.trim().is_empty() {
            o.flush();
            continue;
        }
        let lower = t.trim().to_ascii_lowercase();
        if lower.starts_with("<verbatim>") || lower.starts_with("<pre>") {
            let close = if lower.starts_with("<pre>") { "</pre>" } else { "</verbatim>" };
            let (body, next) = until(&lines, i, |l| l.to_ascii_lowercase().contains(close));
            o.code(None, &body.join("\n"));
            i = next;
            continue;
        }
        if let Some(rest) = t.strip_prefix("---").filter(|r| r.starts_with('+') || r.starts_with('#')) {
            let level = rest.chars().take_while(|&c| c == '+' || c == '#').count();
            if level <= 6 {
                o.heading(level, &inline(rest[level..].trim_start_matches("!!").trim(), TWIKI_RULES));
                continue;
            }
        }
        if t.len() >= 3 && t.chars().all(|c| c == '-') {
            o.rule();
            continue;
        }
        if t.trim_start().starts_with('|') {
            let cells = t.trim().trim_matches('|').split('|').map(|c| {
                let c = c.trim();
                match c.strip_prefix('*').and_then(|x| x.strip_suffix('*')) {
                    Some(h) if !h.contains('*') => (true, inline(h.trim(), TWIKI_RULES)),
                    _ => (false, inline(c, TWIKI_RULES)),
                }
            });
            o.row(cells.collect());
            continue;
        }
        if line.starts_with("   ") || line.starts_with('\t') {
            let indent: usize = line.chars().take_while(|c| c.is_whitespace()).map(|c| if c == '\t' { 3 } else { 1 }).sum();
            let s = line.trim_start();
            let numbered = s.split_once(' ').filter(|(m, _)| m.chars().all(|c| c.is_ascii_digit()) || matches!(*m, "1." | "A." | "a." | "i." | "I.")).map(|(_, r)| r);
            if let Some(r) = s.strip_prefix("* ") {
                o.indented_item(indent, false, &inline(r, TWIKI_RULES));
                continue;
            }
            if let Some(r) = numbered {
                o.indented_item(indent, true, &inline(r, TWIKI_RULES));
                continue;
            }
            if let Some((term, def)) = s.strip_prefix('$').and_then(|r| r.split_once(':')) {
                o.item(1, false, &format!("<strong>{}</strong> {}", inline(term.trim(), TWIKI_RULES), inline(def.trim(), TWIKI_RULES)));
                continue;
            }
            if o.continue_item(&inline(s, TWIKI_RULES)) {
                continue;
            }
        }
        o.line(inline(t.trim(), TWIKI_RULES));
    }
    o.finish()
}

// ---------------------------------------------------------------- Zim


const ZIM: &[Rule] = &[
    Rule::Literal("''", "''"),
    Rule::Link("[[", "]]", pipe_link),
    Rule::Link("{{", "}}", brace_image),
    Rule::Mark("**", "**", "strong"),
    Rule::Mark("//", "//", "em"),
    Rule::Mark("__", "__", "mark"),
    Rule::Mark("~~", "~~", "del"),
    Rule::Mark("^{", "}", "sup"),
    Rule::Mark("_{", "}", "sub"),
];

pub fn zimwiki(text: &str) -> Html {
    // The page's header lines (`Content-Type: text/x-zim-wiki`) aren't shown.
    let mut rest = text;
    while let Some((line, after)) = rest.split_once('\n') {
        let key = line.split_once(": ").map(|(k, _)| k);
        if key.is_some_and(|k| matches!(k, "Content-Type" | "Wiki-Format" | "Creation-Date" | "Modification-Date")) {
            rest = after;
        } else {
            break;
        }
    }
    let rest = rest.replace("[ ] ", "☐ ").replace("[*] ", "☑ ").replace("[x] ", "☒ ");
    simple_wiki(&rest, &WikiSyntax { heading: Heading::Inverted, bullets: "", numbers: "", rules: ZIM, code: &[("'''", "'''"), ("{{{code:", "}}}")], table: Some(("|", &[])), quote: None, indent_lists: Some(1) })
}

// ---------------------------------------------------------------- Vimwiki


const VIM: &[Rule] = &[
    Rule::Literal("`", "`"),
    Rule::Link("[[", "]]", pipe_link),
    Rule::Link("{{", "}}", brace_image),
    Rule::Mark("*", "*", "strong"),
    Rule::Mark("_", "_", "em"),
    Rule::Mark("~~", "~~", "del"),
    Rule::Mark("^", "^", "sup"),
    Rule::Mark(",,", ",,", "sub"),
];

pub fn vimwiki(text: &str) -> Html {
    let mut title = None;
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            if let Some(t) = l.strip_prefix("%title ") {
                title = Some(t.trim().to_string());
            }
            !l.starts_with("%%") && !l.starts_with("%title") && !l.starts_with("%nohtml") && !l.starts_with("%template") && !l.starts_with("%date")
        })
        .collect();
    let mut r = simple_wiki(&kept.join("\n"), &WikiSyntax { heading: Heading::Wrapped('=', true), bullets: "", numbers: "", rules: VIM, code: &[("{{{", "}}}")], table: Some(("|", &[])), quote: None, indent_lists: Some(4) });
    if title.is_some() {
        r.title = title;
    }
    r
}

// ---------------------------------------------------------------- Gemtext


pub fn gemtext(text: &str) -> Html {
    let lines = lines_of(text);
    let mut o = Out::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if let Some(alt) = line.strip_prefix("```") {
            let (body, next) = until(&lines, i, |l| l.starts_with("```"));
            o.code(alt.split_whitespace().next(), &body.join("\n"));
            i = next;
            continue;
        }
        if let Some(r) = line.strip_prefix("=>") {
            let r = r.trim();
            let (url, label) = r.split_once(char::is_whitespace).unwrap_or((r, ""));
            o.flush();
            if looks_like_image(url) {
                o.image(url, label.trim());
            } else {
                o.line(a(url, label, &[]));
                o.end_para();
            }
            continue;
        }
        let hashes = line.chars().take_while(|&c| c == '#').count();
        if (1..=3).contains(&hashes) {
            o.heading(hashes, &esc(line[hashes..].trim()));
        } else if let Some(r) = line.strip_prefix("* ") {
            o.item(1, false, &esc(r));
        } else if let Some(r) = line.strip_prefix('>') {
            o.quote(&esc(r.trim()));
        } else if line.trim().is_empty() {
            o.flush();
        } else {
            // Every line is a paragraph of its own.
            o.line(esc(line));
            o.end_para();
        }
    }
    o.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_engine() {
        assert_eq!(inline("a*b*c 2*3", &[Rule::Mark("*", "*", "strong")]), "a*b*c 2*3");
        assert_eq!(inline("*bold* x", &[Rule::Mark("*", "*", "strong")]), "<strong>bold</strong> x");
        assert_eq!(inline("see https://x.org/a.", &[]), "see <a href=\"https://x.org/a\">https://x.org/a</a>.");
        assert_eq!(inline("<b>x</b><script>", &[Rule::Html(HTML_INLINE)]), "<b>x</b>&lt;script&gt;");
        // Thousands of unmatched marks stay linear.
        let many = "*a ".repeat(20_000);
        let t = std::time::Instant::now();
        let _ = inline(&many, &[Rule::Mark("*", "*", "strong"), Rule::Mark("**", "**", "b")]);
        assert!(t.elapsed().as_secs() < 3, "{:?}", t.elapsed());
    }
}
