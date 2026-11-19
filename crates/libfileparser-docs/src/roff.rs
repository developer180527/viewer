//! Manual pages: roff with the `man` macros (Linux, most Unix tools) or BSD's `mdoc`,
//! read by our own interpreter of the macros manuals use (no Rust troff exists). It
//! keeps a manual's structure: its sections, paragraphs, tagged lists of options,
//! examples and fonts. Typesetting requests (spacing, fonts sizes, traps, macro
//! definitions, conditionals) are skipped: the manual is shown as text, not typeset.

use libfileparser_core::{Confidence, Detection, Document, FileSource, ParseContext, Reader, ReaderInfo, Result, Category, is_text};

use crate::markup::{Rendered, esc};

static INFO: ReaderInfo = ReaderInfo::new("man", "Manual page", Category::Docs)
    .extensions(&["man", "mdoc", "roff", "1", "2", "3", "4", "5", "6", "7", "8", "9", "1m", "3p", "n", "l"])
    .mime_types(&["text/troff", "application/x-troff-man", "text/x-mdoc"]);

pub struct ManReader;

/// The first request of a manual, past comments: `.TH` (man) or `.Dd` (mdoc).
fn starts_like_a_manual(header: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&header[..header.len().min(4096)]);
    head.lines().map(str::trim_start).find(|l| !l.is_empty() && !l.starts_with(".\\\"") && !l.starts_with("'\\\"") && !l.starts_with("\\\"") && *l != "." && !l.starts_with(".ig") && !l.starts_with("$")).is_some_and(|l| {
        let l = l.trim_start_matches(['.', '\'']).trim_start();
        ["TH ", "Dd ", "Dd\t", "Dt ", "so ", "de ", "ds ", "nr ", "if ", "ie "].iter().any(|m| l.starts_with(m)) || l == "Dd"
    })
}

impl Reader for ManReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// A manual's first request (`.TH`, `.Dd`); a section number alone as extension
    /// (`ls.1`) counts only with one.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if !is_text(header) {
            return Detection::NONE;
        }
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        let named = matches!(ext.as_str(), "man" | "mdoc" | "roff");
        if starts_like_a_manual(header) {
            Detection::new(Confidence::Content).with_language("roff")
        } else if named {
            Detection::new(Confidence::Extension).with_language("roff")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        crate::markup::read(src, cx, "man", "Manual page", roff)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Font {
    Roman,
    Bold,
    Italic,
    BoldItalic,
    Mono,
}

impl Font {
    fn tags(self) -> &'static [&'static str] {
        match self {
            Font::Roman => &[],
            Font::Bold => &["strong"],
            Font::Italic => &["em"],
            Font::BoldItalic => &["strong", "em"],
            Font::Mono => &["code"],
        }
    }
}

/// An open list (mdoc's `.Bl`, or man's tagged paragraphs).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Each item a term, its body indented.
    Tag,
    Bullet,
    Dash,
    Enum,
    Column,
    Item,
}

struct Frame {
    kind: Kind,
    count: usize,
    /// An item's body is open (a blockquote, for tagged lists).
    body: bool,
    /// Started by man's `.TP`/`.IP` rather than `.Bl`: ended by the next paragraph.
    implicit: bool,
}

#[derive(Default)]
struct Roff {
    html: String,
    /// The paragraph being filled, as HTML.
    inl: String,
    font: Option<Font>,
    /// The font before the last change (`\fP`).
    previous: Option<Font>,
    frames: Vec<Frame>,
    /// `.RS` blockquotes open.
    rs: usize,
    title: Option<String>,
    /// mdoc's `.Nm`: the manual's name, for `.Nm` without arguments.
    name: Option<String>,
    rows: Vec<Vec<String>>,
}

impl Roff {
    fn set_font(&mut self, f: Font) {
        let now = self.font.unwrap_or(Font::Roman);
        if now == f {
            return;
        }
        for t in now.tags().iter().rev() {
            self.inl.push_str(&format!("</{t}>"));
        }
        for t in f.tags() {
            self.inl.push_str(&format!("<{t}>"));
        }
        self.previous = Some(now);
        self.font = Some(f);
    }

    fn end_para(&mut self) {
        if let Some(f) = self.font.take() {
            for t in f.tags().iter().rev() {
                self.inl.push_str(&format!("</{t}>"));
            }
        }
        self.previous = None;
        let p = std::mem::take(&mut self.inl);
        if !p.trim().is_empty() {
            self.html.push_str("<p>");
            self.html.push_str(p.trim());
            self.html.push_str("</p>");
        }
    }

    /// Ends the open item's body.
    fn end_body(&mut self) {
        self.end_para();
        if let Some(top) = self.frames.last_mut() {
            if top.body {
                top.body = false;
                if top.kind == Kind::Tag {
                    self.html.push_str("</blockquote>");
                }
            }
        }
    }

    /// Ends man's implicit tagged lists (at `.PP`, `.SH`).
    fn end_implicit(&mut self) {
        while self.frames.last().is_some_and(|f| f.implicit) {
            self.end_body();
            self.frames.pop();
        }
    }

    fn end_all(&mut self) {
        self.end_table();
        while !self.frames.is_empty() {
            self.end_body();
            self.frames.pop();
        }
        while self.rs > 0 {
            self.html.push_str("</blockquote>");
            self.rs -= 1;
        }
        self.end_para();
    }

    fn heading(&mut self, level: u8, text: &str) {
        self.end_all();
        let mut h = Roff::default();
        h.text(text);
        h.end_para_inline();
        let inner = h.inl;
        self.html.push_str(&format!("<h{level}>{inner}</h{level}>"));
    }

    /// Closes fonts without making a paragraph.
    fn end_para_inline(&mut self) {
        if let Some(f) = self.font.take() {
            for t in f.tags().iter().rev() {
                self.inl.push_str(&format!("</{t}>"));
            }
        }
    }

    /// A list item with a term (`term_html`, already formatted) and a body to come.
    fn tag_item(&mut self, term_html: &str, implicit: bool) {
        self.end_table();
        if implicit {
            if !self.frames.last().is_some_and(|f| f.implicit) {
                self.end_para();
                self.frames.push(Frame { kind: Kind::Tag, count: 0, body: false, implicit: true });
            }
        }
        self.end_body();
        let top = self.frames.last().map(|f| f.kind).unwrap_or(Kind::Tag);
        match top {
            Kind::Tag => {
                if !term_html.trim().is_empty() {
                    self.html.push_str(&format!("<p>{}</p>", term_html.trim()));
                }
                self.html.push_str("<blockquote>");
            }
            Kind::Bullet => self.inl.push_str("• "),
            Kind::Dash => self.inl.push_str("– "),
            Kind::Enum => {
                let n = self.frames.last().map_or(1, |f| f.count + 1);
                self.inl.push_str(&format!("{n}. "));
            }
            Kind::Item | Kind::Column => {}
        }
        if let Some(f) = self.frames.last_mut() {
            f.body = true;
            f.count += 1;
        }
    }

    fn end_table(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.end_para();
        self.html.push_str("<table>");
        for row in std::mem::take(&mut self.rows) {
            self.html.push_str("<tr>");
            for c in row {
                self.html.push_str(&format!("<td>{c}</td>"));
            }
            self.html.push_str("</tr>");
        }
        self.html.push_str("</table>");
    }

    fn pre(&mut self, lines: &[String]) {
        self.end_para();
        let body: Vec<String> = lines.iter().map(|l| plain(l)).collect();
        self.html.push_str(&format!("<pre>{}</pre>", esc(body.join("\n").trim_end_matches('\n'))));
    }

    fn space(&mut self) {
        if !self.inl.is_empty() && !self.inl.ends_with(' ') && !self.inl.ends_with('\u{a0}') {
            self.inl.push(' ');
        }
    }

    /// Filled text, with its escapes and font changes.
    fn text(&mut self, s: &str) {
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c != '\\' {
                self.inl.push_str(&esc(&c.to_string()));
                i += 1;
                continue;
            }
            i += 1;
            let Some(&e) = chars.get(i) else { break };
            i += 1;
            match e {
                'f' => {
                    let (name, n) = escape_name(&chars[i..]);
                    i += n;
                    let f = match name.as_str() {
                        "B" | "3" => Some(Font::Bold),
                        "I" | "2" => Some(Font::Italic),
                        "BI" | "4" => Some(Font::BoldItalic),
                        "CW" | "C" | "CR" | "CB" | "CI" | "V" => Some(Font::Mono),
                        "P" | "" => Some(self.previous.unwrap_or(Font::Roman)),
                        _ => Some(Font::Roman),
                    };
                    if let Some(f) = f {
                        self.set_font(f);
                    }
                }
                '(' | '[' | 'C' => {
                    // `\(xx`, `\[name]`, `\C'name'`: from the escape's own bracket.
                    let from = if e == 'C' { i } else { i - 1 };
                    let (name, n) = escape_name(&chars[from..]);
                    i = from + n;
                    self.inl.push_str(&esc(&special(&name)));
                }
                '*' => {
                    let (name, n) = escape_name(&chars[i..]);
                    i += n;
                    self.inl.push_str(&esc(match name.as_str() {
                        "R" | "Rg" => "®",
                        "Tm" => "™",
                        "lq" | "Lq" => "“",
                        "rq" | "Rq" => "”",
                        "q" => "\"",
                        // mdoc's strings.
                        "Gt" => ">",
                        "Lt" => "<",
                        "Ge" => "≥",
                        "Le" => "≤",
                        "Ne" => "≠",
                        "Am" => "&",
                        "Ba" => "|",
                        "Pi" => "π",
                        "If" => "∞",
                        "Na" => "NaN",
                        "Pm" => "±",
                        "Br" => "[",
                        "Ket" => "]",
                        _ => "",
                    }));
                }
                'e' | '\\' => self.inl.push('\\'),
                '-' => self.inl.push('-'),
                '.' => self.inl.push('.'),
                '\'' => self.inl.push('´'),
                '`' => self.inl.push('`'),
                ' ' | '~' | '0' => self.inl.push('\u{a0}'),
                '&' | '|' | '^' | ')' | '%' | ':' | 'c' | '/' | ',' | 'd' | 'u' | 'r' | 'p' | 'a' | 't' | '{' | '}' | 'z' => {}
                '"' | '#' => break,
                'n' | 'g' | 'k' | 'F' | 'm' | 'M' | 'Y' | 'V' | 'O' => {
                    let (_, n) = escape_name(&chars[i..]);
                    i += n;
                }
                's' => {
                    // A size: `\s+2`, `\s-1`, `\s0`, `\s(12`, `\s[12]`.
                    if matches!(chars.get(i), Some('+' | '-')) {
                        i += 1;
                    }
                    if chars.get(i) == Some(&'(') {
                        i += 3;
                    } else if chars.get(i) == Some(&'[') {
                        i += chars[i..].iter().position(|&x| x == ']').map_or(0, |p| p + 1);
                    } else {
                        i += chars[i..].iter().take(2).take_while(|x| x.is_ascii_digit()).count().min(1);
                    }
                }
                'h' | 'v' | 'w' | 'o' | 'b' | 'l' | 'L' | 'x' | 'D' | 'X' | 'S' | 'H' | 'R' | 'Z' | 'A' | 'B' => {
                    // A quoted argument (`\h'1n'`): skipped; `\o` and `\Z` show it.
                    if let Some(&q) = chars.get(i) {
                        let end = chars[i + 1..].iter().position(|&x| x == q).map_or(chars.len(), |p| i + 1 + p);
                        if matches!(e, 'o' | 'Z') {
                            let inner: String = chars[(i + 1).min(end)..end].iter().collect();
                            self.inl.push_str(&esc(&inner));
                        }
                        i = (end + 1).min(chars.len());
                    }
                }
                'N' => {
                    if chars.get(i) == Some(&'\'') {
                        let end = chars[i + 1..].iter().position(|&x| x == '\'').map_or(chars.len(), |p| i + 1 + p);
                        let n: String = chars[i + 1..end].iter().collect();
                        if let Some(c) = n.parse::<u32>().ok().and_then(char::from_u32) {
                            self.inl.push_str(&esc(&c.to_string()));
                        }
                        i = (end + 1).min(chars.len());
                    }
                }
                other => self.inl.push_str(&esc(&other.to_string())),
            }
        }
    }

    fn word(&mut self, w: &str, f: Font) {
        self.set_font(f);
        self.text(w);
    }
}

/// An escape's name: one character, `(xx`, or `[name]`; and the characters used.
fn escape_name(chars: &[char]) -> (String, usize) {
    match chars.first() {
        Some('(') => (chars.iter().skip(1).take(2).collect(), 3.min(chars.len())),
        Some('[') => {
            let end = chars.iter().position(|&c| c == ']').unwrap_or(chars.len());
            (chars[1..end].iter().collect(), (end + 1).min(chars.len()))
        }
        Some('\'') => {
            let end = chars[1..].iter().position(|&c| c == '\'').map_or(chars.len(), |p| p + 1);
            (chars[1..end].iter().collect(), (end + 1).min(chars.len()))
        }
        Some(&c) => (c.to_string(), 1),
        None => (String::new(), 0),
    }
}

/// A special character by its troff name (`\(em`, `\[bu]`).
fn special(name: &str) -> String {
    let s = match name {
        "em" => "—",
        "en" => "–",
        "hy" | "-" | "mi" => "-",
        "bu" => "•",
        "co" => "©",
        "rg" => "®",
        "tm" => "™",
        "lq" | "Lq" => "“",
        "rq" | "Rq" => "”",
        "oq" => "‘",
        "cq" => "’",
        "aq" => "'",
        "dq" => "\"",
        "ga" => "`",
        "aa" => "´",
        "ti" | "a~" => "~",
        "ha" | "a^" => "^",
        "rs" | "e" => "\\",
        "sl" => "/",
        "ba" | "or" => "|",
        "->" | "ra" => "→",
        "<-" | "la" => "←",
        "<=" => "≤",
        ">=" => "≥",
        "!=" => "≠",
        "==" => "≡",
        "**" => "∗",
        "mu" => "×",
        "di" => "÷",
        "+-" => "±",
        "de" => "°",
        "dg" => "†",
        "sc" => "§",
        "ps" => "¶",
        "ct" => "¢",
        "Po" => "£",
        "Eu" | "eu" => "€",
        "Ye" => "¥",
        "lB" => "[",
        "rB" => "]",
        "lC" => "{",
        "rC" => "}",
        "fo" => "‹",
        "fc" => "›",
        "Fo" => "«",
        "Fc" => "»",
        "sp" => " ",
        "nbsp" => "\u{a0}",
        "ss" => "ß",
        "'e" => "é",
        "'a" => "á",
        "`e" => "è",
        ":u" => "ü",
        ":o" => "ö",
        ":a" => "ä",
        "ae" => "æ",
        "o/" => "ø",
        "ff" => "ff",
        "fi" => "fi",
        "fl" => "fl",
        "ul" | "_" => "_",
        "ci" => "○",
        "sq" => "□",
        "OK" => "✓",
        "*a" => "α",
        "*b" => "β",
        "*l" => "λ",
        "*m" => "μ",
        "*p" => "π",
        n if n.starts_with("u") && n.len() >= 5 => return u32::from_str_radix(&n[1..], 16).ok().and_then(char::from_u32).map_or_else(String::new, String::from),
        n if n.starts_with("char") => return n[4..].parse::<u32>().ok().and_then(char::from_u32).map_or_else(String::new, String::from),
        _ => "",
    };
    s.to_string()
}

/// A line's text without fonts, for examples.
fn plain(line: &str) -> String {
    let mut r = Roff::default();
    r.text(line);
    r.end_para_inline();
    crate::markup::strip_tags(&r.inl).replace('\u{a0}', " ")
}

/// A request's arguments: words, or "quoted words" (with `""` a quote).
fn args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let mut w = String::new();
        if chars[i] == '"' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '"' {
                    if chars.get(i + 1) == Some(&'"') {
                        w.push('"');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                w.push(chars[i]);
                i += 1;
            }
        } else {
            while i < chars.len() && chars[i] != ' ' && chars[i] != '\t' {
                if chars[i] == '\\' && chars.get(i + 1) == Some(&'"') {
                    // A comment: the rest of the line.
                    return out;
                }
                if chars[i] == '\\' && i + 1 < chars.len() {
                    w.push(chars[i]);
                    i += 1;
                }
                w.push(chars[i]);
                i += 1;
            }
        }
        out.push(w);
    }
    out
}

/// A line without its comment (`\"` to the end).
fn uncomment(line: &str) -> &str {
    let b = line.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'\\' {
            if b[i + 1] == b'"' || b[i + 1] == b'#' {
                return &line[..i];
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    line
}

const MDOC: &[&str] = &[
    "Ad", "An", "Ap", "Ar", "At", "Bsx", "Bx", "Cd", "Cm", "Dv", "Dx", "Em", "Er", "Ev", "Fa", "Fl", "Fn", "Ft", "Fx", "Ic", "Li", "Lk", "Ms", "Mt", "Nm", "No", "Ns", "Nx", "Op", "Ox", "Pa", "Pf", "Sx", "Sy", "Tn", "Ux", "Va", "Vt", "Xr", "Aq", "Bq",
    "Brq", "Dq", "Pq", "Qq", "Ql", "Sq", "Oo", "Oc", "Ao", "Ac", "Bo", "Bc", "Bro", "Brc", "Do", "Dc", "Po", "Pc", "Qo", "Qc", "So", "Sc", "Eo", "Ec", "Xo", "Xc", "Ta", "St", "Fo", "Fc", "In", "Sm", "Ot", "Lb", "Rv", "Ex", "Mt", "Eq", "Hf", "%A", "%T", "%B", "%J", "%D", "%N", "%V", "%P", "%I", "%O", "%Q", "%R", "%U", "%C",
];

fn roff(text: &str) -> Result<Rendered> {
    let mut r = Roff::default();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    // man's `.B`/`.I` with no arguments set the next text line's font.
    let mut next_font: Option<Font> = None;
    // man's `.TP`: the next text line is the term.
    let mut tp = false;
    let mut nofill = false;
    let mut pre_lines: Vec<String> = Vec::new();
    // `.de`/`.ig`: lines skipped until `..` (or the named end).
    let mut skip_until: Option<String> = None;
    let mut url: Option<String> = None;
    // A table (`.TS`): its format lines are skipped up to the one ending in `.`.
    let mut table: Option<(bool, char)> = None;
    let mut is_mdoc = false;
    while i < lines.len() {
        let raw = lines[i];
        i += 1;
        if let Some(end) = &skip_until {
            if raw.trim_start_matches(['.', '\'']).trim() == end.as_str() || raw.trim() == ".." {
                skip_until = None;
            }
            continue;
        }
        let line = raw.trim_end();
        let control = line.starts_with('.') || line.starts_with('\'');
        if let Some((formatting, tab)) = table.as_mut() {
            if line.starts_with(".TE") {
                table = None;
                r.end_table();
                continue;
            }
            if control {
                continue;
            }
            if *formatting {
                // Options (`tab(:);`) then format lines, the last ending in `.`.
                if let Some(at) = line.find("tab(") {
                    *tab = line[at + 4..].chars().next().unwrap_or('\t');
                }
                if line.trim_end().ends_with('.') {
                    *formatting = false;
                }
                continue;
            }
            if line.starts_with("_") || line.starts_with("=") {
                continue;
            }
            let t = *tab;
            let cells: Vec<String> = line
                .split(t)
                .map(|c| {
                    let mut cell = Roff::default();
                    cell.text(c.trim_start_matches("T{").trim_end_matches("T}"));
                    cell.end_para_inline();
                    cell.inl
                })
                .collect();
            r.end_para();
            r.rows.push(cells);
            continue;
        }
        if nofill {
            if control {
                let req = line[1..].trim_start();
                let name = req.split_whitespace().next().unwrap_or("");
                if matches!(name, "fi" | "EE" | "Ed" | "ft" | "in" | "sp" | "br" | "PP" | "SH" | "Sh" | "Ss" | "SS") {
                    if matches!(name, "fi" | "EE" | "Ed" | "PP" | "SH" | "Sh" | "SS" | "Ss") {
                        nofill = false;
                        r.pre(&std::mem::take(&mut pre_lines));
                        if !matches!(name, "PP" | "SH" | "Sh" | "SS" | "Ss") {
                            continue;
                        }
                    } else {
                        if name == "sp" {
                            pre_lines.push(String::new());
                        }
                        continue;
                    }
                } else if matches!(name, "B" | "I" | "R" | "BR" | "IR" | "RB" | "BI" | "IB" | "RI" | "SM") {
                    pre_lines.push(args(&req[name.len()..]).join(if name.len() == 2 { "" } else { " " }));
                    continue;
                } else if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && is_mdoc {
                    // mdoc macros inside a display: their words.
                    let words = args(&req[name.len()..]);
                    pre_lines.push(words.into_iter().filter(|w| !MDOC.contains(&w.as_str())).collect::<Vec<_>>().join(" "));
                    continue;
                } else {
                    continue;
                }
            } else {
                pre_lines.push(uncomment(line).to_string());
                continue;
            }
        }
        if !control {
            let body = uncomment(line);
            if body.trim().is_empty() {
                if line.trim().is_empty() {
                    // A blank line: a paragraph break.
                    r.end_para();
                }
                continue;
            }
            if tp {
                tp = false;
                let mut term = Roff::default();
                term.text(body);
                term.end_para_inline();
                r.tag_item(&term.inl, true);
                continue;
            }
            if body.starts_with([' ', '\t']) && !r.inl.is_empty() {
                r.inl.push_str("<br>");
            }
            r.space();
            if let Some(f) = next_font.take() {
                r.word(body, f);
                r.set_font(Font::Roman);
            } else {
                r.text(body);
            }
            continue;
        }
        let req = uncomment(&line[1..]).trim_start();
        let name: String = req.chars().take_while(|c| !c.is_whitespace()).collect();
        let rest = &req[name.len()..];
        let a = args(rest);
        match name.as_str() {
            "" => {}
            // ---- man
            "TH" => {
                let title = match (a.first(), a.get(1)) {
                    (Some(t), Some(s)) => format!("{}({})", plain(t), plain(s)),
                    (Some(t), None) => plain(t),
                    _ => String::new(),
                };
                if !title.is_empty() {
                    r.heading(1, &title);
                    r.title = Some(title);
                }
            }
            "SH" | "SS" | "Sh" | "Ss" => {
                is_mdoc |= name.starts_with('S') && name.ends_with('h') || name == "Ss";
                let level = if name == "SH" || name == "Sh" { 2 } else { 3 };
                let text = if a.is_empty() && i < lines.len() && !lines[i].starts_with('.') {
                    i += 1;
                    lines[i - 1].to_string()
                } else {
                    a.iter().filter(|w| !MDOC.contains(&w.as_str())).cloned().collect::<Vec<_>>().join(" ")
                };
                r.heading(level, &text);
                tp = false;
            }
            "PP" | "LP" | "P" | "Pp" | "Lp" => {
                tp = false;
                if name.starts_with('P') && name.len() == 2 && name != "PP" {
                    // mdoc's .Pp inside a list item stays in the item.
                    r.end_para();
                } else {
                    r.end_implicit();
                    r.end_para();
                }
            }
            "TP" | "TQ" => {
                tp = true;
            }
            "IP" | "HP" => {
                let tag = a.first().cloned().unwrap_or_default();
                let bulletish = matches!(plain(&tag).trim(), "•" | "*" | "-" | "o" | "·" | "–");
                if bulletish {
                    r.tag_item("", true);
                    r.inl.push_str("• ");
                } else {
                    let mut term = Roff::default();
                    term.text(&tag);
                    term.end_para_inline();
                    r.tag_item(&term.inl, true);
                }
            }
            "RS" => {
                r.end_para();
                r.html.push_str("<blockquote>");
                r.rs += 1;
            }
            "RE" => {
                r.end_implicit();
                r.end_para();
                if r.rs > 0 {
                    r.html.push_str("</blockquote>");
                    r.rs -= 1;
                }
            }
            "nf" | "EX" => {
                r.end_para();
                nofill = true;
            }
            "fi" | "EE" => {}
            "br" => r.inl.push_str("<br>"),
            "sp" | "ne" => r.end_para(),
            "B" | "I" | "SB" | "SM" | "CW" => {
                let f = match name.as_str() {
                    "B" | "SB" => Font::Bold,
                    "I" => Font::Italic,
                    "CW" => Font::Mono,
                    _ => Font::Roman,
                };
                if a.is_empty() {
                    next_font = Some(f);
                } else if tp {
                    tp = false;
                    let mut term = Roff::default();
                    term.word(&a.join(" "), f);
                    term.end_para_inline();
                    r.tag_item(&term.inl, true);
                } else {
                    r.space();
                    r.word(&a.join(" "), f);
                    r.set_font(Font::Roman);
                }
            }
            "BR" | "BI" | "IB" | "IR" | "RB" | "RI" => {
                // Alternating fonts, word by word, with no space between.
                let fonts: Vec<Font> = name
                    .chars()
                    .map(|c| match c {
                        'B' => Font::Bold,
                        'I' => Font::Italic,
                        _ => Font::Roman,
                    })
                    .collect();
                let mut out = Roff::default();
                for (k, w) in a.iter().enumerate() {
                    out.word(w, fonts[k % 2]);
                }
                out.end_para_inline();
                if tp {
                    tp = false;
                    r.tag_item(&out.inl, true);
                } else {
                    r.space();
                    r.end_para_inline();
                    r.inl.push_str(&out.inl);
                }
            }
            "UR" | "MT" => {
                url = a.first().map(|u| if name == "MT" { format!("mailto:{u}") } else { u.clone() });
                r.space();
                r.end_para_inline();
                if let Some(u) = &url {
                    r.inl.push_str(&format!("<a href=\"{}\">", esc(u)));
                }
            }
            "UE" | "ME" => {
                if url.take().is_some() {
                    r.end_para_inline();
                    r.inl.push_str("</a>");
                }
                if let Some(p) = a.first() {
                    r.text(p);
                }
            }
            "TS" => {
                r.end_para();
                table = Some((true, '\t'));
            }
            "de" | "de1" | "am" | "ig" | "dei" | "ami" => {
                skip_until = Some(if name == "ig" { a.first().cloned().unwrap_or_else(|| ".".into()) } else { a.get(1).cloned().unwrap_or_else(|| ".".into()) });
                if skip_until.as_deref() == Some(".") {
                    skip_until = Some(".".into());
                }
            }
            "so" => {
                r.space();
                r.text(&format!("(See {}.)", a.first().map(String::as_str).unwrap_or("another page")));
            }
            // ---- mdoc, blocks
            "Dd" => {
                is_mdoc = true;
            }
            "Dt" => {
                is_mdoc = true;
                let title = match (a.first(), a.get(1)) {
                    (Some(t), Some(s)) => format!("{}({})", t.to_lowercase(), s),
                    (Some(t), None) => t.to_lowercase(),
                    _ => String::new(),
                };
                if !title.is_empty() {
                    r.heading(1, &title);
                    r.title = Some(title);
                }
            }
            "Os" | "Bk" | "Ek" | "Sm" | "Rs" | "Re" | "Ud" | "Lb" | "Fd" => {
                if name == "Rs" || name == "Re" {
                    r.end_para();
                }
            }
            "Nd" => {
                r.space();
                r.inl.push_str("— ");
                r.text(&a.join(" "));
            }
            "Bl" => {
                r.end_para();
                let kind = if a.iter().any(|x| x == "-bullet") {
                    Kind::Bullet
                } else if a.iter().any(|x| x == "-dash" || x == "-hyphen") {
                    Kind::Dash
                } else if a.iter().any(|x| x == "-enum") {
                    Kind::Enum
                } else if a.iter().any(|x| x == "-column") {
                    Kind::Column
                } else if a.iter().any(|x| x == "-item") {
                    Kind::Item
                } else {
                    Kind::Tag
                };
                // Inside man's implicit lists, an explicit one nests.
                r.frames.push(Frame { kind, count: 0, body: false, implicit: false });
            }
            "El" => {
                r.end_table();
                while r.frames.last().is_some_and(|f| f.implicit) {
                    r.end_body();
                    r.frames.pop();
                }
                r.end_body();
                r.frames.pop();
            }
            "It" => {
                let mut a = a;
                // `.It Xo` … `.Xc`: the item's head continues over the lines between.
                if a.iter().any(|w| w == "Xo") {
                    while i < lines.len() {
                        let l = uncomment(lines[i]).trim_start_matches(['.', '\'']).trim().to_string();
                        i += 1;
                        let words = args(&l);
                        let done = words.iter().any(|w| w == "Xc");
                        a.extend(words);
                        if done {
                            break;
                        }
                    }
                }
                let kind = r.frames.last().map(|f| f.kind);
                let mut term = Roff::default();
                term.name = r.name.clone();
                mdoc_line(&mut term, &a);
                term.end_para_inline();
                match kind {
                    Some(Kind::Column) => {
                        r.end_para();
                        // Cells by `Ta` or tab.
                        let mut cells = vec![Vec::new()];
                        for w in &a {
                            if w == "Ta" {
                                cells.push(Vec::new());
                            } else {
                                cells.last_mut().map(|c| c.push(w.clone()));
                            }
                        }
                        let row = cells
                            .iter()
                            .flat_map(|c| {
                                let joined = c.join(" ");
                                joined.split('\t').map(|x| x.to_string()).collect::<Vec<_>>()
                            })
                            .map(|c| {
                                let mut cell = Roff::default();
                                mdoc_line(&mut cell, &args(&c));
                                cell.end_para_inline();
                                cell.inl
                            })
                            .collect();
                        r.rows.push(row);
                    }
                    Some(Kind::Tag) | None => {
                        if kind.is_none() {
                            r.frames.push(Frame { kind: Kind::Tag, count: 0, body: false, implicit: false });
                        }
                        r.tag_item(&term.inl, false);
                    }
                    _ => {
                        r.tag_item("", false);
                        r.end_para_inline();
                        r.inl.push_str(&term.inl);
                    }
                }
            }
            "Bd" => {
                r.end_para();
                if a.iter().any(|x| x == "-literal" || x == "-unfilled") {
                    nofill = true;
                } else {
                    r.html.push_str("<blockquote>");
                    r.rs += 1;
                }
            }
            "Ed" => {
                r.end_para();
                if r.rs > 0 {
                    r.html.push_str("</blockquote>");
                    r.rs -= 1;
                }
            }
            "D1" | "Dl" => {
                r.end_para();
                if name == "Dl" {
                    let words: Vec<String> = a.iter().filter(|w| !MDOC.contains(&w.as_str())).cloned().collect();
                    r.pre(&[words.join(" ")]);
                } else {
                    let mut line = Roff::default();
                    line.name = r.name.clone();
                    mdoc_line(&mut line, &a);
                    line.end_para_inline();
                    r.html.push_str(&format!("<blockquote><p>{}</p></blockquote>", line.inl));
                }
            }
            "Ex" => {
                r.space();
                let n = a.iter().find(|x| !x.starts_with('-')).cloned().or_else(|| r.name.clone()).unwrap_or_else(|| "The utility".into());
                r.text(&format!("The {n} utility exits 0 on success, and >0 if an error occurs."));
            }
            "Rv" => {
                r.space();
                r.text("The function returns the value 0 if successful; otherwise the value -1 is returned and the global variable errno is set to indicate the error.");
            }
            n if n.len() <= 3 && (MDOC.contains(&n) || n == "Nm") => {
                is_mdoc = true;
                if n == "Nm" && r.name.is_none() {
                    r.name = a.first().cloned();
                }
                let mut words = vec![name.clone()];
                words.extend(a.iter().cloned());
                r.space();
                let mut line = Roff::default();
                line.name = r.name.clone();
                mdoc_line(&mut line, &words);
                line.end_para_inline();
                let html = line.inl;
                // Closing punctuation attaches to the word before it.
                if html.starts_with(['.', ',', ';', ':', ')', ']', '?', '!']) && r.inl.ends_with(' ') {
                    r.inl.pop();
                }
                r.end_para_inline();
                r.inl.push_str(&html);
            }
            // Typesetting requests and anything unknown: not shown.
            _ => {}
        }
    }
    if nofill && !pre_lines.is_empty() {
        r.pre(&pre_lines);
    }
    r.end_all();
    Ok(Rendered { html: r.html, title: r.title, warnings: Vec::new() })
}

/// One mdoc line's words, with its callable macros (`Fl`, `Ar`, `Op`…), into `r`.
fn mdoc_line(r: &mut Roff, words: &[String]) {
    let mut font = Font::Roman;
    // Closers for enclosures opened on this line (`Op` → `]`), at the line's end.
    let mut closers: Vec<&'static str> = Vec::new();
    let mut nospace = false;
    let mut prefix: Option<&str> = None;
    let mut k = 0;
    let push = |r: &mut Roff, w: &str, font: Font, nospace: &mut bool| {
        let punct = w.len() <= 2 && w.chars().all(|c| ".,;:)]?!".contains(c));
        let opening = w.len() == 1 && "([".contains(w);
        if !*nospace && !punct && !r.inl.is_empty() && !r.inl.ends_with(['(', '[', ' ', '\u{a0}']) {
            // The space between words is in neither's font.
            r.set_font(Font::Roman);
            r.inl.push(' ');
        }
        *nospace = opening;
        if punct {
            r.set_font(Font::Roman);
        } else {
            r.set_font(font);
        }
        r.text(w);
    };
    let is_punct = |w: &str| w.len() <= 2 && w.chars().all(|c| ".,;:)]?!".contains(c));
    while k < words.len() {
        let w = words[k].as_str();
        k += 1;
        if w != "Fl" && MDOC.contains(&w) {
            prefix = None;
        }
        // Enclosures close before the line's trailing punctuation.
        if is_punct(w) && !closers.is_empty() && words[k..].iter().all(|x| is_punct(x)) {
            r.set_font(Font::Roman);
            for c in closers.drain(..).rev() {
                r.inl.push_str(&esc(c));
            }
        }
        match w {
            "Fl" => {
                font = Font::Bold;
                prefix = Some("-");
                // `Fl` alone, or before another macro: a lone dash.
                if k >= words.len() || MDOC.contains(&words[k].as_str()) {
                    push(r, "-", font, &mut nospace);
                    prefix = None;
                }
            }
            "Ar" => {
                font = Font::Italic;
                if k >= words.len() || MDOC.contains(&words[k].as_str()) || words[k].chars().all(|c| ".,;:)]".contains(c)) {
                    push(r, "file ...", font, &mut nospace);
                }
            }
            "Nm" => {
                font = Font::Bold;
                if (k >= words.len() || MDOC.contains(&words[k].as_str()) || words[k].chars().all(|c| ".,;:)]".contains(c))) && r.name.is_some() {
                    let n = r.name.clone().unwrap_or_default();
                    push(r, &n, font, &mut nospace);
                }
            }
            "Cm" | "Ic" | "Sy" | "Fn" | "Fo" | "Ft" | "Fd" | "In" | "Cd" | "Ms" => font = Font::Bold,
            "Em" | "Va" | "Fa" | "Pa" | "Sx" | "Vt" | "Ad" | "%T" | "%B" | "%J" => font = Font::Italic,
            "Li" | "Dv" | "Ev" | "Er" | "Lk" | "Mt" => font = Font::Mono,
            "No" | "An" | "Tn" | "%A" | "%D" | "%N" | "%V" | "%P" | "%I" | "%O" | "%Q" | "%R" | "%U" | "%C" => font = Font::Roman,
            "Ql" => {
                font = Font::Mono;
                push(r, "‘", Font::Roman, &mut nospace);
                nospace = true;
                closers.push("’");
            }
            "Xr" => {
                let page = words.get(k).cloned().unwrap_or_default();
                let sec = words.get(k + 1).filter(|s| s.chars().next().is_some_and(|c| c.is_ascii_digit())).cloned();
                k += 1 + usize::from(sec.is_some());
                push(r, &page, Font::Bold, &mut nospace);
                if let Some(s) = sec {
                    r.set_font(Font::Roman);
                    r.text(&format!("({s})"));
                }
                font = Font::Roman;
            }
            "Op" | "Oo" | "Bq" | "Bo" => {
                push(r, "[", Font::Roman, &mut nospace);
                if w == "Op" || w == "Bq" {
                    closers.push("]");
                }
            }
            "Oc" | "Bc" => {
                nospace = true;
                push(r, "]", Font::Roman, &mut nospace);
            }
            "Dq" | "Do" => {
                push(r, "“", Font::Roman, &mut nospace);
                nospace = true;
                if w == "Dq" {
                    closers.push("”");
                }
            }
            "Dc" => {
                nospace = true;
                push(r, "”", Font::Roman, &mut nospace);
            }
            "Sq" | "So" => {
                push(r, "‘", Font::Roman, &mut nospace);
                nospace = true;
                if w == "Sq" {
                    closers.push("’");
                }
            }
            "Sc" => {
                nospace = true;
                push(r, "’", Font::Roman, &mut nospace);
            }
            "Qq" | "Qo" => {
                push(r, "\"", Font::Roman, &mut nospace);
                nospace = true;
                if w == "Qq" {
                    closers.push("\"");
                }
            }
            "Qc" => {
                nospace = true;
                push(r, "\"", Font::Roman, &mut nospace);
            }
            "Pq" | "Po" => {
                push(r, "(", Font::Roman, &mut nospace);
                if w == "Pq" {
                    closers.push(")");
                }
            }
            "Pc" => {
                nospace = true;
                push(r, ")", Font::Roman, &mut nospace);
            }
            "Aq" | "Ao" => {
                push(r, "⟨", Font::Roman, &mut nospace);
                nospace = true;
                if w == "Aq" {
                    closers.push("⟩");
                }
            }
            "Ac" => {
                nospace = true;
                push(r, "⟩", Font::Roman, &mut nospace);
            }
            "Brq" | "Bro" => {
                push(r, "{", Font::Roman, &mut nospace);
                nospace = true;
                if w == "Brq" {
                    closers.push("}");
                }
            }
            "Brc" => {
                nospace = true;
                push(r, "}", Font::Roman, &mut nospace);
            }
            "Ns" | "Ap" => {
                nospace = true;
                if w == "Ap" {
                    r.inl.push('\'');
                }
            }
            "Pf" => {
                if let Some(p) = words.get(k) {
                    push(r, p, Font::Roman, &mut nospace);
                    nospace = true;
                    k += 1;
                }
            }
            "Ta" => push(r, "\t", Font::Roman, &mut nospace),
            "St" => {
                let s = words.get(k).cloned().unwrap_or_default();
                k += 1;
                let name = match s.as_str() {
                    "-p1003.1" | "-p1003.1-2008" | "-p1003.1-2001" | "-p1003.1-2004" | "-p1003.1-2024" => "IEEE Std 1003.1 (“POSIX.1”)",
                    "-p1003.2" | "-p1003.2-92" => "IEEE Std 1003.2 (“POSIX.2”)",
                    "-ansiC" | "-isoC" | "-isoC-99" | "-isoC-2011" => "ISO/IEC 9899 (“ISO C”)",
                    "-xpg4.2" | "-susv2" | "-susv3" | "-susv4" => "X/Open Portability Guide (“XPG”)",
                    other => other.trim_start_matches('-'),
                };
                push(r, name, Font::Roman, &mut nospace);
            }
            "At" => {
                let v = words.get(k).filter(|v| !MDOC.contains(&v.as_str()) && !v.chars().all(|c| ".,;:".contains(c))).cloned();
                k += usize::from(v.is_some());
                push(r, &format!("{}AT&T UNIX", v.map(|v| format!("Version {} ", v.trim_start_matches('v'))).unwrap_or_default()).replace("Version  ", ""), Font::Roman, &mut nospace);
            }
            "Bx" | "Bsx" | "Ox" | "Nx" | "Fx" | "Dx" | "Ux" => {
                let os = match w {
                    "Bx" => "BSD",
                    "Bsx" => "BSD/OS",
                    "Ox" => "OpenBSD",
                    "Nx" => "NetBSD",
                    "Fx" => "FreeBSD",
                    "Dx" => "DragonFly",
                    _ => "UNIX",
                };
                let v = words.get(k).filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit())).cloned();
                k += usize::from(v.is_some());
                push(r, &v.map_or(os.to_string(), |v| format!("{os} {v}")), Font::Roman, &mut nospace);
            }
            "Xo" | "Xc" | "Eo" | "Ec" | "Fc" | "Sm" | "Ot" | "Hf" | "Eq" | "Lb" | "Rv" | "Ex" => {}
            word => {
                let shown = match prefix.take() {
                    Some(p) => format!("{p}{word}"),
                    None => word.to_string(),
                };
                // `Fl` applies a dash to each of its words.
                if w != "|" && font == Font::Bold && k >= 2 && words[..k - 1].iter().rev().find(|x| MDOC.contains(&x.as_str())).is_some_and(|m| m == "Fl") {
                    prefix = Some("-");
                }
                if w == "|" {
                    push(r, "|", Font::Roman, &mut nospace);
                } else {
                    push(r, &shown, font, &mut nospace);
                }
            }
        }
    }
    r.set_font(Font::Roman);
    for c in closers.iter().rev() {
        r.inl.push_str(&esc(c));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::Content;

    fn blocks(text: &str) -> (Document, String) {
        let src = FileSource::from_bytes("ls.1", text.as_bytes().to_vec());
        assert!(ManReader.detect(src.header(), "ls.1").confidence >= Confidence::Content);
        let doc = ManReader.open(&src, &ParseContext::new()).unwrap();
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
    fn man_macros() {
        let (doc, s) = blocks(".\\\" a comment\n.TH LS 1 \"2026\" \"GNU\"\n.SH NAME\nls \\- list directory contents\n.SH SYNOPSIS\n.B ls\n[\\fIOPTION\\fR]... [\\fIFILE\\fR]...\n.SH OPTIONS\n.TP\n.BR \\-a \", \" \\-\\-all\ndo not ignore entries starting with .\n.TP\n\\fB\\-l\\fR\nuse a long listing format\n.PP\nAfter the list \\(em done.\n.SH EXAMPLES\n.EX\nls \\-la /tmp\n.EE\n.SH SEE ALSO\n.UR https://www.gnu.org/software/coreutils/\nthe manual\n.UE\n");
        has(&s, &["Heading { level: 1, text: [Text(\"LS(1)\")]", "Heading { level: 2, text: [Text(\"NAME\")]", "ls - list directory contents", "Strong([Text(\"ls\")])", "Emphasis([Text(\"OPTION\")])", "Strong([Text(\"-a\")])", "--all", "Quote([Paragraph([Text(\"do not ignore", "Strong([Text(\"-l\")])", "—", "Code { language: None, text: \"ls -la /tmp\" }", "https://www.gnu.org/software/coreutils/"]);
        assert!(!s.contains("comment"));
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "LS(1)"));
    }

    #[test]
    fn mdoc_macros() {
        let (doc, s) = blocks(".Dd $Mdocdate: May 1 2026 $\n.Dt CAT 1\n.Os\n.Sh NAME\n.Nm cat\n.Nd concatenate files\n.Sh SYNOPSIS\n.Nm cat\n.Op Fl benstuv\n.Op Ar\n.Sh DESCRIPTION\nThe\n.Nm\nutility reads files.\n.Bl -tag -width Ds\n.It Fl b\nNumber the lines.\n.It Fl n\nNumber all lines.\n.El\n.Bd -literal -offset indent\n$ cat file1 file2 > file3\n.Ed\n.Sh SEE ALSO\n.Xr head 1 ,\n.Xr tail 1\n.Sh STANDARDS\nThe utility follows\n.St -p1003.1-2008 .\n");
        has(&s, &["Heading { level: 1, text: [Text(\"cat(1)\")]", "Strong([Text(\"cat\")])", "— concatenate files", "[", "Strong([Text(\"-benstuv\")])", "Emphasis([Text(\"file ...\")])", "utility reads files", "Strong([Text(\"-b\")])", "Quote([Paragraph([Text(\"Number the lines.\")", "Code { language: None, text: \"$ cat file1 file2 > file3\" }", "Strong([Text(\"head\")]), Text(\"(1),", "POSIX.1"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "cat(1)"));
    }

    #[test]
    fn tables_and_definitions_are_kept_or_skipped() {
        let (_, s) = blocks(".TH T 7\n.de XX\nhidden macro body\n..\n.TS\ntab(:);\nl l.\na:b\n1:2\n.TE\n.ig\nignored too\n..\nshown\n");
        has(&s, &["Table", "Text(\"a\")", "Text(\"2\")", "shown"]);
        assert!(!s.contains("hidden") && !s.contains("ignored"), "{s}");
    }

    #[test]
    fn a_section_number_alone_isnt_enough() {
        assert_eq!(ManReader.detect(b"just some text\n", "notes.1"), Detection::NONE);
        assert!(ManReader.detect(b".TH X 1\n", "x").confidence >= Confidence::Content);
    }
}
