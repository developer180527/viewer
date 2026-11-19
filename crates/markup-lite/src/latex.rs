//! LaTeX and Texinfo, read by our own interpreters of the commands documents are
//! written with (no Rust TeX exists that reads documents rather than math). Nothing is
//! typeset or run: macros a document defines aren't expanded, packages aren't loaded,
//! `\input` and `@include` aren't followed. What's kept is the document's structure
//! and text: sections, paragraphs, lists, tables, code, quotes, notes, links, images
//! and emphasis; math is shown as its source.

use crate::{Html, esc, strip_tags};
/// Commands whose arguments are settings, not text: skipped with them.
const SETTINGS: &[&str] = &[
    "documentclass", "usepackage", "RequirePackage", "PassOptionsToPackage", "newcommand", "renewcommand", "providecommand", "DeclareRobustCommand", "newenvironment", "renewenvironment", "setlength", "addtolength", "setcounter", "addtocounter", "stepcounter",
    "pagestyle", "thispagestyle", "pagenumbering", "label", "hypersetup", "geometry", "vspace", "hspace", "vskip", "hskip", "bibliographystyle", "newtheorem", "definecolor", "color", "colorbox", "setmainfont", "setsansfont", "setmonofont", "newfontfamily",
    "defaultfontfeatures", "babelfont", "selectlanguage", "setotherlanguage", "setmainlanguage", "graphicspath", "DeclareGraphicsExtensions", "lstset", "AtBeginDocument", "AtEndDocument", "newlength", "settowidth", "newif", "nocite", "index", "glossary",
    "markboth", "markright", "titleformat", "titlespacing", "fancyhead", "fancyfoot", "fancyhf", "captionsetup", "setkeys", "IfFileExists", "ifthenelse", "providecolor", "urlstyle", "linespread", "fontsize", "rule", "raisebox", "phantom", "hphantom", "vphantom",
    "newsavebox", "sbox", "usebox", "bibliography", "addbibresource", "include", "input", "includeonly", "tableofcontents", "listoffigures", "listoftables", "printbibliography", "makeindex", "printindex", "newpage", "clearpage", "cleardoublepage", "pagebreak",
    "linebreak", "nolinebreak", "nopagebreak", "noindent", "indent", "centering", "raggedright", "raggedleft", "small", "footnotesize", "scriptsize", "tiny", "large", "Large", "LARGE", "huge", "Huge", "normalsize", "normalfont", "selectfont", "fontfamily",
    "fontseries", "fontshape", "fontencoding", "maketitle", "makeatletter", "makeatother", "hline", "cline", "toprule", "midrule", "bottomrule", "cmidrule", "endhead", "endfirsthead", "endfoot", "endlastfoot", "tabularnewline", "arraybackslash", "strut",
    "tightlist", "protect", "relax", "par", "medskip", "bigskip", "smallskip", "hfill", "vfill", "null", "leavevmode", "appendix", "frontmatter", "mainmatter", "backmatter", "sloppy", "fussy", "frenchspacing", "nonfrenchspacing", "onecolumn", "twocolumn",
    "let", "def", "gdef", "edef", "xdef", "global", "long", "expandafter", "csname", "endcsname", "begingroup", "endgroup", "ifx", "fi", "else", "ifLuaTeX", "ifPDFTeX", "ifXeTeX", "iftutex", "ifnum", "ifdim", "ifcsname", "unless", "or", "item@", "noalign",
    "extracolsep", "addlinespace", "setstretch", "doublespacing", "onehalfspacing", "singlespacing", "allowdisplaybreaks", "numberwithin", "counterwithin", "theoremstyle", "DeclareMathOperator", "newacronym", "hyphenation", "pdfbookmark", "phantomsection",
    "addcontentsline", "textdir", "pardir", "bidi", "TextOrMath", "author", "title", "date", "thanks", "multicolumn",
];

/// Environments shown as written (code and math).
const VERBATIM: &[&str] = &["verbatim", "verbatim*", "Verbatim", "lstlisting", "minted", "alltt", "comment", "code", "Highlighting", "BVerbatim", "LVerbatim", "spverbatim", "filecontents", "filecontents*"];
const MATH: &[&str] = &["equation", "equation*", "align", "align*", "gather", "gather*", "multline", "multline*", "eqnarray", "eqnarray*", "displaymath", "math", "flalign", "flalign*", "alignat", "alignat*", "dmath"];
const SKIPPED: &[&str] = &["comment", "tikzpicture", "pgfpicture", "picture", "filecontents", "filecontents*", "thebibliography-skip"];

struct Tex {
    chars: Vec<char>,
    i: usize,
    out: String,
    depth: usize,
    /// Tags opened by a font switch (`{\bf …}`) in each open group.
    groups: Vec<Vec<&'static str>>,
}

/// LaTeX's body, with the preamble's title, author and date.
pub fn latex(text: &str) -> Html {
    let (body, preamble) = match text.find("\\begin{document}") {
        Some(at) => {
            let rest = &text[at + "\\begin{document}".len()..];
            (rest.find("\\end{document}").map_or(rest, |e| &rest[..e]), &text[..at])
        }
        None => (text, ""),
    };
    let grab = |src: &str, cmd: &str| -> Option<String> {
        let at = find_command(src, cmd)?;
        let mut t = Tex::new(&src[at + cmd.len() + 1..], 0);
        let _ = t.opt();
        t.arg().map(|raw| strip_tags(&Tex::render(&raw, 1)).trim().to_string()).filter(|s| !s.is_empty())
    };
    let title = grab(preamble, "title").or_else(|| grab(body, "title"));
    let author = grab(preamble, "author").or_else(|| grab(body, "author"));
    let date = grab(preamble, "date").or_else(|| grab(body, "date")).filter(|d| d != "\\today" && !d.contains("today"));
    let mut out = String::new();
    if let Some(t) = &title {
        out.push_str(&format!("<h1>{}</h1>", esc(t)));
        let byline: Vec<&str> = [author.as_deref(), date.as_deref()].into_iter().flatten().collect();
        if !byline.is_empty() {
            out.push_str(&format!("<p><em>{}</em></p>", esc(&byline.join(" · "))));
        }
    }
    let mut t = Tex::new(body, 0);
    t.run();
    out.push_str("<p>");
    out.push_str(&t.out);
    Html { html: out, title, warnings: Vec::new() }
}

/// Where `\cmd` (not `\cmdx`) is used, past comments.
fn find_command(src: &str, cmd: &str) -> Option<usize> {
    let needle = format!("\\{cmd}");
    let mut from = 0;
    while let Some(at) = src[from..].find(&needle).map(|a| a + from) {
        let after = src[at + needle.len()..].chars().next();
        let line_start = src[..at].rfind('\n').map_or(0, |n| n + 1);
        let commented = src[line_start..at].contains('%');
        if !commented && !after.is_some_and(|c| c.is_ascii_alphabetic()) {
            return Some(at);
        }
        from = at + needle.len();
    }
    None
}

impl Tex {
    fn new(s: &str, depth: usize) -> Self {
        Tex { chars: s.chars().collect(), i: 0, out: String::with_capacity(s.len()), depth, groups: Vec::new() }
    }

    /// `raw`'s HTML, interpreted on its own (an argument or an environment's body).
    fn render(raw: &str, depth: usize) -> String {
        if depth > 48 {
            return esc(raw);
        }
        let mut t = Tex::new(raw, depth);
        t.run();
        t.out
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn skip_spaces(&mut self) {
        while self.peek().is_some_and(|c| c == ' ' || c == '\t' || c == '\r') {
            self.i += 1;
        }
        // One newline, not a blank line.
        if self.peek() == Some('\n') {
            let mut j = self.i + 1;
            while self.chars.get(j).is_some_and(|c| *c == ' ' || *c == '\t' || *c == '\r') {
                j += 1;
            }
            if self.chars.get(j) != Some(&'\n') {
                self.i = j;
            }
        }
    }

    /// A balanced `{…}` from `self.i` (at the `{`): its inside.
    fn group(&mut self, open: char, close: char) -> String {
        let start = self.i + 1;
        let mut depth = 0usize;
        while self.i < self.chars.len() {
            let c = self.chars[self.i];
            if c == '\\' {
                self.i += 2;
                continue;
            }
            if c == '%' && open == '{' {
                while self.i < self.chars.len() && self.chars[self.i] != '\n' {
                    self.i += 1;
                }
                continue;
            }
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    let inner: String = self.chars[start..self.i].iter().collect();
                    self.i += 1;
                    return inner;
                }
            }
            self.i += 1;
        }
        self.chars[start.min(self.chars.len())..].iter().collect()
    }

    /// A command's argument: `{…}`, or a single token.
    fn arg(&mut self) -> Option<String> {
        self.skip_spaces();
        match self.peek()? {
            '{' => Some(self.group('{', '}')),
            '\\' => {
                let start = self.i;
                self.i += 1;
                if self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                    while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                        self.i += 1;
                    }
                } else {
                    self.i += 1;
                }
                Some(self.chars[start..self.i.min(self.chars.len())].iter().collect())
            }
            '}' | ']' => None,
            c => {
                self.i += 1;
                Some(c.to_string())
            }
        }
    }

    /// An optional `[…]` argument.
    fn opt(&mut self) -> Option<String> {
        let save = self.i;
        self.skip_spaces();
        if self.peek() == Some('[') {
            Some(self.group('[', ']'))
        } else {
            self.i = save;
            None
        }
    }

    fn star(&mut self) -> bool {
        if self.peek() == Some('*') {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn wrap(&mut self, tag: &str) {
        let a = self.arg().unwrap_or_default();
        let inner = Tex::render(&a, self.depth + 1);
        self.out.push_str(&format!("<{tag}>{inner}</{tag}>"));
    }

    /// The text up to `\end{name}`, with `\begin{name}` inside nesting.
    fn environment_body(&mut self, name: &str) -> String {
        let begin: Vec<char> = format!("\\begin{{{name}}}").chars().collect();
        let end: Vec<char> = format!("\\end{{{name}}}").chars().collect();
        // A trailing backslash can step past the end.
        let start = self.i.min(self.chars.len());
        let mut depth = 1usize;
        while self.i < self.chars.len() {
            if self.chars[self.i..].starts_with(&end) {
                depth -= 1;
                if depth == 0 {
                    let body: String = self.chars[start..self.i].iter().collect();
                    self.i += end.len();
                    return body;
                }
                self.i += end.len();
            } else if self.chars[self.i..].starts_with(&begin) {
                depth += 1;
                self.i += begin.len();
            } else {
                self.i += 1;
            }
        }
        self.chars[start..].iter().collect()
    }

    fn run(&mut self) {
        let mut steps = 0usize;
        while self.i < self.chars.len() {
            steps += 1;
            if steps > 50_000_000 {
                break;
            }
            let c = self.chars[self.i];
            match c {
                '%' => {
                    while self.i < self.chars.len() && self.chars[self.i] != '\n' {
                        self.i += 1;
                    }
                    self.i += 1;
                    self.skip_spaces();
                }
                '\\' => self.command(),
                '{' => {
                    self.i += 1;
                    self.groups.push(Vec::new());
                }
                '}' => {
                    self.i += 1;
                    if let Some(tags) = self.groups.pop() {
                        for t in tags.iter().rev() {
                            self.out.push_str(&format!("</{t}>"));
                        }
                    }
                }
                '$' => {
                    let display = self.chars.get(self.i + 1) == Some(&'$');
                    let open = if display { 2 } else { 1 };
                    let start = self.i + open;
                    let mut j = start;
                    while j < self.chars.len() && !(self.chars[j] == '$' && self.chars[j - 1] != '\\') {
                        j += 1;
                    }
                    let math: String = self.chars[start..j.min(self.chars.len())].iter().collect();
                    self.i = (j + open).min(self.chars.len());
                    if display {
                        self.out.push_str(&format!("<pre><code class=\"language-latex\">{}</code></pre><p>", esc(math.trim())));
                    } else {
                        self.out.push_str(&format!("<code>{}</code>", esc(math.trim())));
                    }
                }
                '~' => {
                    self.out.push('\u{a0}');
                    self.i += 1;
                }
                '\n' => {
                    let mut j = self.i + 1;
                    while self.chars.get(j).is_some_and(|c| *c == ' ' || *c == '\t' || *c == '\r') {
                        j += 1;
                    }
                    if self.chars.get(j) == Some(&'\n') {
                        // A blank line: a new paragraph.
                        self.close_groups();
                        self.out.push_str("<p>");
                        while self.chars.get(j).is_some_and(|c| c.is_whitespace()) {
                            j += 1;
                        }
                    } else {
                        self.out.push(' ');
                    }
                    self.i = j;
                }
                '`' if self.chars.get(self.i + 1) == Some(&'`') => {
                    self.out.push('“');
                    self.i += 2;
                }
                '\'' if self.chars.get(self.i + 1) == Some(&'\'') => {
                    self.out.push('”');
                    self.i += 2;
                }
                '-' if self.chars.get(self.i + 1) == Some(&'-') => {
                    if self.chars.get(self.i + 2) == Some(&'-') {
                        self.out.push('—');
                        self.i += 3;
                    } else {
                        self.out.push('–');
                        self.i += 2;
                    }
                }
                '&' => {
                    self.out.push(' ');
                    self.i += 1;
                }
                '#' => self.i += 1,
                _ => {
                    self.out.push_str(&esc(&c.to_string()));
                    self.i += 1;
                }
            }
        }
        self.close_groups();
    }

    fn close_groups(&mut self) {
        while let Some(tags) = self.groups.pop() {
            for t in tags.iter().rev() {
                self.out.push_str(&format!("</{t}>"));
            }
        }
    }

    /// A font switch (`\bf`, `\itshape`): its tag until the group ends.
    fn switch(&mut self, tag: &'static str) {
        self.out.push_str(&format!("<{tag}>"));
        match self.groups.last_mut() {
            Some(g) => g.push(tag),
            None => self.groups.push(vec![tag]),
        }
    }

    fn command(&mut self) {
        self.i += 1;
        let Some(first) = self.peek() else { return };
        if !first.is_ascii_alphabetic() {
            self.i += 1;
            match first {
                '\\' => {
                    let _ = self.star();
                    let _ = self.opt();
                    self.out.push_str("<br>");
                }
                '(' | '[' => {
                    let close = if first == '(' { "\\)" } else { "\\]" };
                    let rest: String = self.chars[self.i..].iter().collect();
                    let end = rest.find(close).unwrap_or(rest.len());
                    let math = rest[..end].trim().to_string();
                    self.i += rest[..end].chars().count() + 2;
                    if first == '[' {
                        self.out.push_str(&format!("<pre><code class=\"language-latex\">{}</code></pre><p>", esc(&math)));
                    } else {
                        self.out.push_str(&format!("<code>{}</code>", esc(&math)));
                    }
                }
                ' ' | '\n' | ',' | ';' | ':' | '!' | '>' => self.out.push(' '),
                '-' | '/' | '@' => {}
                '\'' | '`' | '^' | '"' | '~' | '=' | '.' => {
                    // An accent: `\'e` → é.
                    let base = self.arg().unwrap_or_default();
                    self.out.push_str(&esc(&accent(first, &base)));
                }
                c => self.out.push_str(&esc(&c.to_string())),
            }
            return;
        }
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic() || c == '@') {
            self.i += 1;
        }
        let name: String = self.chars[start..self.i].iter().collect();
        let _ = self.star();
        // As TeX does: spaces after a control word are its end, not text.
        while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
            self.i += 1;
        }
        match name.as_str() {
            "begin" => {
                let env = self.arg().unwrap_or_default();
                self.environment(&env);
            }
            "end" => {
                let _ = self.arg();
            }
            "part" | "chapter" | "section" | "subsection" | "subsubsection" | "paragraph" | "subparagraph" | "frametitle" => {
                let _ = self.opt();
                let text = self.arg().unwrap_or_default();
                let level = match name.as_str() {
                    "part" | "chapter" => 1,
                    "section" | "frametitle" => 2,
                    "subsection" => 3,
                    "subsubsection" => 4,
                    "paragraph" => 5,
                    _ => 6,
                };
                self.close_groups();
                self.out.push_str(&format!("<h{level}>{}</h{level}><p>", Tex::render(&text, self.depth + 1)));
            }
            "textbf" | "mathbf" | "bm" | "boldsymbol" => self.wrap("strong"),
            "emph" | "textit" | "textsl" | "mathit" | "textsc" => self.wrap("em"),
            "texttt" | "mathtt" | "code" | "path" | "lstinline" | "mintinline" | "cmd" | "file" if name != "mintinline" => self.literal_arg(),
            "mintinline" => {
                let _ = self.arg();
                self.literal_arg();
            }
            "underline" | "uline" | "uuline" => self.wrap("u"),
            "sout" | "st" | "xout" => self.wrap("s"),
            "textsuperscript" => self.wrap("sup"),
            "textsubscript" => self.wrap("sub"),
            "bf" | "bfseries" => self.switch("strong"),
            "it" | "itshape" | "em" | "sl" | "slshape" | "sc" | "scshape" => self.switch("em"),
            "tt" | "ttfamily" => self.switch("code"),
            "verb" => {
                // `\verb|x|`: any delimiter.
                let d = self.peek().unwrap_or('|');
                self.i += 1;
                let start = self.i;
                while self.peek().is_some_and(|c| c != d && c != '\n') {
                    self.i += 1;
                }
                let v: String = self.chars[start..self.i].iter().collect();
                self.i += 1;
                self.out.push_str(&format!("<code>{}</code>", esc(&v)));
            }
            "href" => {
                let url = self.arg().unwrap_or_default();
                let text = self.arg().unwrap_or_default();
                self.out.push_str(&format!("<a href=\"{}\">{}</a>", esc(url.trim()), Tex::render(&text, self.depth + 1)));
            }
            "url" | "nolinkurl" => {
                let url = self.arg().unwrap_or_default();
                self.out.push_str(&format!("<a href=\"{0}\">{0}</a>", esc(url.trim())));
            }
            "footnote" | "footnotetext" | "marginpar" | "sidenote" => {
                let _ = self.opt();
                let text = self.arg().unwrap_or_default();
                self.out.push_str(&format!("<sup>[{}]</sup>", Tex::render(&text, self.depth + 1)));
            }
            "cite" | "citep" | "citet" | "parencite" | "textcite" | "autocite" | "footcite" | "citeauthor" | "citeyear" => {
                let _ = self.opt();
                let _ = self.opt();
                let keys = self.arg().unwrap_or_default();
                self.out.push_str(&format!("[{}]", esc(keys.trim())));
            }
            "ref" | "eqref" | "pageref" | "autoref" | "cref" | "Cref" | "nameref" => {
                let key = self.arg().unwrap_or_default();
                self.out.push_str(&format!("[{}]", esc(key.trim())));
            }
            "includegraphics" | "includesvg" => {
                let _ = self.opt();
                let file = self.arg().unwrap_or_default();
                self.out.push_str(&format!("<img src=\"{}\" alt=\"\">", esc(file.trim())));
            }
            "caption" => {
                let _ = self.opt();
                let text = self.arg().unwrap_or_default();
                self.out.push_str(&format!("<p><em>{}</em></p><p>", Tex::render(&text, self.depth + 1)));
            }
            "item" => {
                // Outside a list environment (where items are split first): a bullet.
                let label = self.opt();
                self.out.push_str(&format!("<p>{} ", label.map_or("•".to_string(), |l| Tex::render(&l, self.depth + 1))));
            }
            "texorpdfstring" => {
                // The typeset text; the second is for PDF bookmarks.
                self.wrap("span");
                let _ = self.arg();
            }
            "LaTeX" => self.out.push_str("LaTeX"),
            "TeX" => self.out.push_str("TeX"),
            "LaTeXe" => self.out.push_str("LaTeX2ε"),
            "ldots" | "dots" | "textellipsis" => self.out.push('…'),
            "today" => self.out.push_str("today"),
            "textbackslash" => self.out.push('\\'),
            "textgreater" => self.out.push_str("&gt;"),
            "textless" => self.out.push_str("&lt;"),
            "textbar" => self.out.push('|'),
            "textasciitilde" => self.out.push('~'),
            "textasciicircum" => self.out.push('^'),
            "textunderscore" => self.out.push('_'),
            "textendash" => self.out.push('–'),
            "textemdash" => self.out.push('—'),
            "textquoteleft" => self.out.push('‘'),
            "textquoteright" => self.out.push('’'),
            "textquotedblleft" => self.out.push('“'),
            "textquotedblright" => self.out.push('”'),
            "textbullet" => self.out.push('•'),
            "textdegree" => self.out.push('°'),
            "textregistered" => self.out.push('®'),
            "texttrademark" => self.out.push('™'),
            "textcopyright" | "copyright" => self.out.push('©'),
            "S" => self.out.push('§'),
            "P" => self.out.push('¶'),
            "ss" => self.out.push('ß'),
            "ae" => self.out.push('æ'),
            "oe" => self.out.push('œ'),
            "o" => self.out.push('ø'),
            "aa" => self.out.push('å'),
            "euro" => self.out.push('€'),
            "pounds" => self.out.push('£'),
            "quad" | "qquad" | "enspace" | "thinspace" | "space" => self.out.push(' '),
            "newline" | "break" => self.out.push_str("<br>"),
            "hrule" | "hrulefill" => self.out.push_str("<hr><p>"),
            "abstractname" => self.out.push_str("Abstract"),
            "def" | "gdef" | "edef" | "xdef" => {
                // `\def\name#1{…}`: up to its body, skipped.
                while self.peek().is_some_and(|c| c != '{') {
                    self.i += 1;
                }
                let _ = self.arg();
            }
            n if SETTINGS.contains(&n) => {
                // Settings: their optional and braced arguments go with them.
                loop {
                    let save = self.i;
                    self.skip_spaces();
                    match self.peek() {
                        Some('[') => {
                            let _ = self.group('[', ']');
                        }
                        Some('{') => {
                            let _ = self.group('{', '}');
                        }
                        Some('*') => self.i += 1,
                        _ => {
                            self.i = save;
                            break;
                        }
                    }
                    if matches!(n, "label" | "vspace" | "hspace" | "color" | "pagestyle" | "thispagestyle" | "index" | "include" | "input" | "selectlanguage" | "fontsize") && n != "newcommand" {
                        // One argument (two for `\fontsize`).
                        if n != "fontsize" || self.peek() != Some('{') {
                            break;
                        }
                    }
                }
                self.skip_spaces();
            }
            _ => {
                // Unknown: the command isn't shown, its text arguments are.
                let _ = self.opt();
                self.skip_spaces();
                if self.peek() == Some('{') {
                    let a = self.group('{', '}');
                    self.out.push_str(&Tex::render(&a, self.depth + 1));
                }
            }
        }
    }

    fn literal_arg(&mut self) {
        let _ = self.opt();
        self.skip_spaces();
        let raw = match self.peek() {
            Some('{') => self.group('{', '}'),
            Some(d) => {
                // `\lstinline|x|`.
                self.i += 1;
                let start = self.i;
                while self.peek().is_some_and(|c| c != d) {
                    self.i += 1;
                }
                let v: String = self.chars[start..self.i].iter().collect();
                self.i += 1;
                v
            }
            None => String::new(),
        };
        let text = strip_tags(&Tex::render(&raw, self.depth + 1));
        self.out.push_str(&format!("<code>{}</code>", esc(&text)));
    }

    fn environment(&mut self, env: &str) {
        let env = env.trim();
        if VERBATIM.contains(&env) {
            let lang = if env == "minted" {
                let _ = self.opt();
                self.arg()
            } else if env == "lstlisting" {
                self.opt().and_then(|o| o.split(',').find_map(|kv| kv.trim().strip_prefix("language=").map(|l| l.trim().trim_matches(['{', '}']).to_string())))
            } else {
                if env == "Highlighting" || env == "Verbatim" {
                    let _ = self.opt();
                }
                None
            };
            let body = self.environment_body(env);
            if env == "comment" || env.starts_with("filecontents") {
                return;
            }
            let body = body.strip_prefix('\n').unwrap_or(&body).trim_end();
            // Pandoc's highlighted code: its token macros' text.
            let body = if env == "Highlighting" { strip_tags(&Tex::render(body, self.depth + 1)).replace("<br>", "\n") } else { body.to_string() };
            match lang.filter(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(c)) && !l.is_empty()) {
                Some(l) => self.out.push_str(&format!("<pre><code class=\"language-{}\">{}</code></pre><p>", l.to_ascii_lowercase(), esc(&body))),
                None => self.out.push_str(&format!("<pre>{}</pre><p>", esc(&body))),
            }
            return;
        }
        if MATH.contains(&env) {
            let body = self.environment_body(env);
            self.out.push_str(&format!("<pre><code class=\"language-latex\">{}</code></pre><p>", esc(body.trim())));
            return;
        }
        if SKIPPED.contains(&env) {
            let _ = self.environment_body(env);
            return;
        }
        match env {
            "itemize" | "enumerate" | "description" | "compactitem" | "compactenum" | "list" | "inparaenum" | "asparaenum" | "tasks" => {
                let _ = self.opt();
                if env == "list" {
                    let _ = self.arg();
                    let _ = self.arg();
                }
                let body = self.environment_body(env);
                let ordered = env.contains("enum");
                let tag = if ordered { "ol" } else { "ul" };
                self.close_groups();
                self.out.push_str(&format!("<{tag}>"));
                for item in split_items(&body) {
                    let mut t = Tex::new(&item, self.depth + 1);
                    let label = t.opt();
                    t.run();
                    let label = label.map(|l| format!("<strong>{}</strong> ", Tex::render(&l, self.depth + 1))).unwrap_or_default();
                    self.out.push_str(&format!("<li><p>{label}{}</li>", t.out));
                }
                self.out.push_str(&format!("</{tag}><p>"));
            }
            "tabular" | "tabular*" | "tabularx" | "longtable" | "array" | "tabu" | "supertabular" | "xtabular" | "tblr" => {
                if env == "tabular*" || env == "tabularx" || env == "tabu" {
                    let _ = self.arg();
                }
                let _ = self.opt();
                let _ = self.arg();
                let body = self.environment_body(env);
                self.close_groups();
                self.out.push_str("<table>");
                for row in split_top(&body, "\\\\") {
                    let row = row.trim();
                    let cells = split_top(row, "&");
                    let rendered: Vec<String> = cells.iter().map(|c| Tex::render(c.trim(), self.depth + 1)).collect();
                    if rendered.iter().all(|c| strip_tags(c).trim().is_empty()) {
                        continue;
                    }
                    self.out.push_str("<tr>");
                    for c in rendered {
                        self.out.push_str(&format!("<td>{}</td>", c.trim()));
                    }
                    self.out.push_str("</tr>");
                }
                self.out.push_str("</table><p>");
            }
            "quote" | "quotation" | "verse" | "abstract" | "displayquote" | "theorem" | "lemma" | "proof" | "definition" | "remark" | "example" | "note" | "corollary" | "proposition" => {
                let body = self.environment_body(env);
                self.close_groups();
                let label = match env {
                    "quote" | "quotation" | "verse" | "displayquote" => String::new(),
                    other => {
                        let mut l = other.to_string();
                        if let Some(f) = l.get_mut(0..1) {
                            f.make_ascii_uppercase();
                        }
                        format!("<p><strong>{l}.</strong></p>")
                    }
                };
                self.out.push_str(&format!("<blockquote>{label}<p>{}</blockquote><p>", Tex::render(&body, self.depth + 1)));
            }
            "thebibliography" => {
                let _ = self.arg();
                let body = self.environment_body(env);
                self.out.push_str("<h2>References</h2><ul>");
                for item in split_top(&body, "\\bibitem").into_iter().skip(1) {
                    let mut t = Tex::new(&item, self.depth + 1);
                    let _ = t.opt();
                    let key = t.arg().unwrap_or_default();
                    t.run();
                    self.out.push_str(&format!("<li><p>[{}] {}</li>", esc(key.trim()), t.out));
                }
                self.out.push_str("</ul><p>");
            }
            "figure" | "figure*" | "table" | "table*" | "wrapfigure" | "subfigure" | "minipage" | "center" | "flushleft" | "flushright" | "otherlanguage" | "otherlanguage*" | "document" | "frame" | "column" | "columns" | "small" | "footnotesize" | "landscape" | "multicols" | "samepage" | "sloppypar" | "obeylines" | "titlepage" | "spacing" | "adjustbox" | "framed" | "mdframed" | "tcolorbox" => {
                // Containers: their content, without their placement arguments.
                let _ = self.opt();
                if matches!(env, "minipage" | "otherlanguage" | "otherlanguage*" | "wrapfigure" | "subfigure" | "multicols" | "spacing" | "adjustbox") {
                    let _ = self.arg();
                }
                let body = self.environment_body(env);
                self.out.push_str(&format!("<p>{}<p>", Tex::render(&body, self.depth + 1)));
            }
            _ => {
                // An unknown environment: its content.
                let body = self.environment_body(env);
                self.out.push_str(&format!("<p>{}<p>", Tex::render(&body, self.depth + 1)));
            }
        }
    }
}

/// `body` split at `\item`s outside groups and nested environments; the text before
/// the first is dropped.
fn split_items(body: &str) -> Vec<String> {
    let parts = split_top(body, "\\item");
    parts.into_iter().skip(1).collect()
}

/// `s` split at `sep` where it's outside braces and nested environments.
fn split_top(s: &str, sep: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let sep: Vec<char> = sep.chars().collect();
    let (begin, end): (Vec<char>, Vec<char>) = ("\\begin{".chars().collect(), "\\end{".chars().collect());
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut braces, mut envs) = (0i32, 0i32);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '%' && (i == 0 || chars[i - 1] != '\\') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if chars[i..].starts_with(&begin) {
            envs += 1;
        } else if chars[i..].starts_with(&end) {
            envs -= 1;
        }
        if braces == 0 && envs <= 0 && chars[i..].starts_with(&sep) {
            // `\item` but not `\itemize`.
            let next = chars.get(i + sep.len());
            if !(sep[0] == '\\' && sep.len() > 2 && next.is_some_and(|c| c.is_ascii_alphabetic())) {
                out.push(std::mem::take(&mut cur));
                i += sep.len();
                continue;
            }
        }
        if c == '\\' && i + 1 < chars.len() {
            cur.push(c);
            cur.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '{' {
            braces += 1;
        } else if c == '}' {
            braces -= 1;
        }
        cur.push(c);
        i += 1;
    }
    out.push(cur);
    out
}

/// An accented letter: `\'e` → é.
fn accent(mark: char, base: &str) -> String {
    let b = base.trim().chars().next().unwrap_or(' ');
    let combining = match mark {
        '\'' => '\u{301}',
        '`' => '\u{300}',
        '^' => '\u{302}',
        '"' => '\u{308}',
        '~' => '\u{303}',
        '=' => '\u{304}',
        '.' => '\u{307}',
        _ => return base.to_string(),
    };
    let s: String = [b, combining].iter().collect();
    // Composed where Unicode has the letter.
    compose(&s).unwrap_or(s)
}

fn compose(s: &str) -> Option<String> {
    let table: &[(&str, &str)] = &[
        ("e\u{301}", "é"), ("a\u{301}", "á"), ("i\u{301}", "í"), ("o\u{301}", "ó"), ("u\u{301}", "ú"), ("E\u{301}", "É"), ("e\u{300}", "è"), ("a\u{300}", "à"), ("u\u{300}", "ù"), ("o\u{300}", "ò"), ("e\u{302}", "ê"), ("a\u{302}", "â"), ("o\u{302}", "ô"), ("i\u{302}", "î"),
        ("u\u{302}", "û"), ("a\u{308}", "ä"), ("o\u{308}", "ö"), ("u\u{308}", "ü"), ("e\u{308}", "ë"), ("i\u{308}", "ï"), ("A\u{308}", "Ä"), ("O\u{308}", "Ö"), ("U\u{308}", "Ü"), ("n\u{303}", "ñ"), ("a\u{303}", "ã"), ("o\u{303}", "õ"), ("N\u{303}", "Ñ"),
    ];
    table.iter().find(|(k, _)| *k == s).map(|(_, v)| v.to_string())
}

// ---------------------------------------------------------------- Texinfo


/// Texinfo's `@cmd{…}` marks in a line, as HTML.
fn texi_inline(s: &str, depth: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '@' {
            out.push_str(&esc(&c.to_string()));
            i += 1;
            continue;
        }
        i += 1;
        let Some(&n) = chars.get(i) else { break };
        if !n.is_ascii_alphabetic() {
            i += 1;
            match n {
                '@' | '{' | '}' | '.' | '?' | '!' | ':' | '\'' | '"' | ',' | '=' | '^' | '`' | '~' => {
                    if !matches!(n, ':' | '\'' | '"' | ',' | '=' | '^' | '`' | '~') {
                        out.push_str(&esc(&n.to_string()));
                    }
                }
                '*' => out.push_str("<br>"),
                '-' | '/' => {}
                ' ' | '\t' | '\n' => out.push(' '),
                _ => {}
            }
            continue;
        }
        let start = i;
        while chars.get(i).is_some_and(|c| c.is_ascii_alphanumeric()) {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();
        let arg = if chars.get(i) == Some(&'{') {
            let mut depth_b = 0;
            let s = i + 1;
            while i < chars.len() {
                match chars[i] {
                    '@' => i += 1,
                    '{' => depth_b += 1,
                    '}' => {
                        depth_b -= 1;
                        if depth_b == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let a: String = chars[s.min(chars.len())..i.min(chars.len())].iter().collect();
            i += 1;
            Some(a)
        } else {
            None
        };
        let inner = |a: &str| if depth > 32 { esc(a) } else { texi_inline(a, depth + 1) };
        let a = arg.unwrap_or_default();
        match name.as_str() {
            "code" | "samp" | "kbd" | "command" | "env" | "file" | "option" | "verb" | "t" | "key" | "indicateurl" => out.push_str(&format!("<code>{}</code>", esc(&strip_tags(&inner(&a))))),
            "emph" | "var" | "dfn" | "cite" | "i" | "slanted" => out.push_str(&format!("<em>{}</em>", inner(&a))),
            "strong" | "b" => out.push_str(&format!("<strong>{}</strong>", inner(&a))),
            "sc" | "r" | "w" | "asis" | "acronym" | "abbr" | "titlefont" | "dmn" | "math" | "sansserif" => out.push_str(&inner(a.split(',').next().unwrap_or(""))),
            "uref" | "url" => {
                let mut parts = a.splitn(3, ',');
                let url = parts.next().unwrap_or("").trim().to_string();
                let label = parts.next().map(str::trim).filter(|l| !l.is_empty()).map_or_else(|| esc(&url), inner);
                out.push_str(&format!("<a href=\"{}\">{label}</a>", esc(&url)));
            }
            "email" => {
                let (addr, label) = a.split_once(',').unwrap_or((&a, ""));
                out.push_str(&format!("<a href=\"mailto:{0}\">{1}</a>", esc(addr.trim()), if label.trim().is_empty() { esc(addr.trim()) } else { inner(label.trim()) }));
            }
            "xref" | "pxref" | "ref" => {
                let node = a.split(',').next().unwrap_or("").trim();
                let lead = match name.as_str() {
                    "xref" => "See ",
                    "pxref" => "see ",
                    _ => "",
                };
                out.push_str(&format!("{lead}<em>{}</em>", inner(node)));
            }
            "footnote" => out.push_str(&format!("<sup>[{}]</sup>", inner(&a))),
            "image" => {
                let file = a.split(',').next().unwrap_or("").trim();
                out.push_str(&format!("<img src=\"{}\" alt=\"\">", esc(file)));
            }
            "dots" | "enddots" => out.push('…'),
            "copyright" => out.push('©'),
            "registeredsymbol" => out.push('®'),
            "minus" => out.push('−'),
            "result" => out.push('⇒'),
            "expansion" => out.push('↦'),
            "print" => out.push('⊣'),
            "error" => out.push_str("error→"),
            "equiv" => out.push('≡'),
            "bullet" => out.push('•'),
            "TeX" => out.push_str("TeX"),
            "LaTeX" => out.push_str("LaTeX"),
            "tie" => out.push('\u{a0}'),
            "today" => out.push_str("today"),
            "comma" => out.push(','),
            "atchar" => out.push('@'),
            "lbracechar" => out.push('{'),
            "rbracechar" => out.push('}'),
            "backslashchar" => out.push('\\'),
            "hashchar" => out.push('#'),
            "arrow" => out.push('→'),
            "euro" => out.push('€'),
            "pounds" => out.push('£'),
            "questiondown" => out.push('¿'),
            "exclamdown" => out.push('¡'),
            "ss" => out.push('ß'),
            "anchor" | "index" | "cindex" | "findex" | "vindex" | "kindex" | "pindex" | "tindex" => {}
            _ => out.push_str(&inner(&a)),
        }
    }
    out
}

pub fn texinfo(text: &str) -> Html {
    let mut o = String::new();
    let mut title = None;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    // Open lists: `@itemize`, `@enumerate`, `@table`.
    let mut lists: Vec<&'static str> = Vec::new();
    // `@table @code`: how each open table formats its terms.
    let mut formats: Vec<String> = Vec::new();
    let mut para = false;
    let start_para = |o: &mut String, para: &mut bool| {
        if !*para {
            o.push_str("<p>");
            *para = true;
        }
    };
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let t = line.trim_end();
        if t.trim().is_empty() {
            para = false;
            continue;
        }
        if i == 1 && t.starts_with("\\input") {
            continue;
        }
        if !t.starts_with('@') || !t[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            start_para(&mut o, &mut para);
            o.push_str(&texi_inline(t, 0));
            o.push(' ');
            continue;
        }
        let cmd: String = t[1..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        let rest = t[1 + cmd.len()..].trim();
        let was_para = para;
        para = false;
        match cmd.as_str() {
            "settitle" => {
                if title.is_none() {
                    title = Some(strip_tags(&texi_inline(rest, 0)));
                    o.push_str(&format!("<h1>{}</h1>", texi_inline(rest, 0)));
                }
            }
            "top" if title.as_deref() == Some(strip_tags(&texi_inline(rest, 0)).as_str()) => {}
            "top" | "chapter" | "unnumbered" | "appendix" | "majorheading" | "chapheading" => o.push_str(&format!("<h1>{}</h1>", texi_inline(rest, 0))),
            "section" | "unnumberedsec" | "appendixsec" | "heading" => o.push_str(&format!("<h2>{}</h2>", texi_inline(rest, 0))),
            "subsection" | "unnumberedsubsec" | "appendixsubsec" | "subheading" => o.push_str(&format!("<h3>{}</h3>", texi_inline(rest, 0))),
            "subsubsection" | "unnumberedsubsubsec" | "appendixsubsubsec" | "subsubheading" => o.push_str(&format!("<h4>{}</h4>", texi_inline(rest, 0))),
            "example" | "smallexample" | "lisp" | "smalllisp" | "verbatim" | "display" | "smalldisplay" | "format" | "smallformat" | "group" if cmd != "group" => {
                let end = format!("@end {cmd}");
                let body: Vec<&str> = lines[i..].iter().take_while(|l| l.trim() != end).copied().collect();
                i += body.len() + 1;
                let text: Vec<String> = body.iter().map(|l| if cmd == "verbatim" { l.to_string() } else { strip_tags(&texi_inline(l, 0)) }).collect();
                let lang = if cmd.contains("lisp") { "<code class=\"language-lisp\">" } else { "<code>" };
                o.push_str(&format!("<pre>{lang}{}</code></pre>", esc(&text.join("\n").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\""))));
            }
            "quotation" | "smallquotation" | "cartouche" => o.push_str("<blockquote>"),
            "itemize" => {
                o.push_str("<ul>");
                lists.push("ul");
            }
            "enumerate" => {
                o.push_str("<ol>");
                lists.push("ol");
            }
            "table" | "ftable" | "vtable" | "multitable" => {
                if cmd == "multitable" {
                    o.push_str("<table>");
                    lists.push("table");
                } else {
                    o.push_str("<ul>");
                    lists.push("dl");
                    formats.push(rest.split_whitespace().next().unwrap_or("").trim_start_matches('@').to_string());
                }
            }
            "item" | "itemx" => match lists.last() {
                Some(&"dl") => {
                    let f = formats.last().map(String::as_str).unwrap_or("");
                    let term = if f.is_empty() || f == "asis" { rest.to_string() } else { format!("@{f}{{{rest}}}") };
                    o.push_str(&format!("<li><p><strong>{}</strong></p><p>", texi_inline(&term, 0)));
                }
                Some(&"table") => {
                    o.push_str("<tr>");
                    for cell in rest.split("@tab") {
                        o.push_str(&format!("<td>{}</td>", texi_inline(cell.trim(), 0)));
                    }
                    o.push_str("</tr>");
                }
                _ => {
                    o.push_str("<li><p>");
                    if !rest.is_empty() {
                        o.push_str(&texi_inline(rest, 0));
                        o.push(' ');
                    }
                    para = true;
                }
            },
            "headitem" => {
                o.push_str("<thead><tr>");
                for cell in rest.split("@tab") {
                    o.push_str(&format!("<th>{}</th>", texi_inline(cell.trim(), 0)));
                }
                o.push_str("</tr></thead>");
            }
            "end" => match rest.split_whitespace().next().unwrap_or("") {
                "itemize" | "enumerate" | "table" | "ftable" | "vtable" | "multitable" => {
                    match lists.pop() {
                        Some("ul") => o.push_str("</ul>"),
                        Some("dl") => {
                            formats.pop();
                            o.push_str("</ul>");
                        }
                        Some("ol") => o.push_str("</ol>"),
                        Some("table") => o.push_str("</table>"),
                        _ => {}
                    }
                }
                "quotation" | "smallquotation" | "cartouche" => o.push_str("</blockquote>"),
                _ => {}
            },
            "ignore" | "iftex" | "ifhtml" | "ifdocbook" | "ifxml" | "iflatex" | "tex" | "html" | "titlepage" | "macro" | "rmacro" | "copying" | "direntry" | "documentdescription" | "ifnotinfo" | "docbook" | "xml" | "latex" | "displaymath" => {
                let end = format!("@end {cmd}");
                let n = lines[i..].iter().take_while(|l| l.trim() != end).count();
                i += n + 1;
            }
            "deffn" | "defun" | "defvar" | "defmac" | "defopt" | "deftypefn" | "deftypefun" | "deftypevar" | "defspec" | "defcmd" | "deftp" | "defmethod" | "deftypemethod" | "defvr" | "deftypevr" => {
                o.push_str(&format!("<p><strong><code>{}</code></strong></p><blockquote>", esc(&strip_tags(&texi_inline(rest, 0)))));
                lists.push("def");
            }
            "page" | "node" | "c" | "comment" | "setfilename" | "include" | "set" | "clear" | "printindex" | "contents" | "shortcontents" | "summarycontents" | "bye" | "dircategory" | "insertcopying" | "finalout" | "setchapternewpage" | "paragraphindent" | "firstparagraphindent" | "documentencoding" | "documentlanguage" | "frenchspacing" | "headings" | "footnotestyle" | "exampleindent" | "synindex" | "syncodeindex" | "defindex" | "defcodeindex" | "sp" | "need" | "vskip" | "noindent" | "cindex" | "findex" | "vindex" | "kindex" | "pindex" | "tindex" | "anchor" | "author" | "subtitle" | "title" | "menu" | "detailmenu" | "ifinfo" | "ifnottex" | "ifplaintext" | "ifnothtml" | "ifset" | "ifclear" | "center" | "exdent" | "indent" | "raggedright" | "flushleft" | "flushright" | "smallbook" | "afourpaper" | "allowcodebreaks" | "codequoteundirected" | "codequotebacktick" | "deffnx" | "defunx" | "dots" => {
                if cmd == "menu" || cmd == "detailmenu" {
                    let n = lines[i..].iter().take_while(|l| l.trim() != format!("@end {cmd}")).count();
                    i += n + 1;
                }
                if cmd == "center" || cmd == "exdent" {
                    o.push_str(&format!("<p>{}</p>", texi_inline(rest, 0)));
                }
                if cmd == "bye" {
                    break;
                }
            }
            _ => {
                // Inline marks starting a line of text.
                para = was_para;
                start_para(&mut o, &mut para);
                o.push_str(&texi_inline(t, 0));
                o.push(' ');
            }
        }
        // `@end deffn` and friends close a definition's body.
        if cmd == "end" && rest.starts_with("def") && lists.last() == Some(&"def") {
            lists.pop();
            o.push_str("</blockquote>");
        }
    }
    Html { html: o, title, warnings: Vec::new() }
}
