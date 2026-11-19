//! Plain-text markup languages into formatted text. Each is read by the most mature
//! parser there is for it, written out as HTML, and that HTML becomes formatted text
//! through html5ever (`crate::html`), as Org does: one pipeline for every markup's
//! structure (headings, lists, tables, code, quotes, links, marks).
//!
//! | Format | Parser |
//! |---|---|
//! | reStructuredText | `rst_parser`; its tree is written out by us (its own writer is unfinished) |
//! | Djot | `jotdown`, John MacFarlane's language's reference port |
//! | AsciiDoc | `asciidoc-parser` (Asciidoctor's language, secure mode: nothing is included or fetched) |
//! | Typst | `typst-syntax`, Typst's own parser; markup is shown, code isn't run |
//! | Textile | ours (no maintained crate): the common block and phrase syntax |
//!
//! None of them runs anything: a Typst file's code, an AsciiDoc include or a reST
//! directive that would read another file are skipped.

use std::sync::Arc;

use libfileparser_core::{Category, Content, Document, FileSource, FormatInfo, OwnedFlow, ParseContext, ParseError, Result, Role, Warning, is_text};

use crate::build::Builder;

/// What a markup's parser made of a file.
#[derive(Default)]
pub(crate) struct Rendered {
    pub html: String,
    pub title: Option<String>,
    pub warnings: Vec<String>,
    /// Files the HTML's images name, by that name (a FictionBook's pictures).
    pub resources: Vec<(String, FileSource)>,
}

pub(crate) use markup_lite::{esc, strip_tags};

impl From<markup_lite::Html> for Rendered {
    fn from(h: markup_lite::Html) -> Self {
        Rendered { html: h.html, title: h.title, warnings: h.warnings, resources: Vec::new() }
    }
}

/// One markup reader: its description and how its text becomes HTML (`fn(&str) ->
/// Result<Rendered>`). Shared by every plain-text markup in the crate.
macro_rules! markup {
    ($(#[$doc:meta])* $reader:ident, $info:ident, $id:literal, $name:literal, $kind:literal, [$($ext:literal),*], [$($mime:literal),*], $render:path) => {
        static $info: libfileparser_core::ReaderInfo = libfileparser_core::ReaderInfo::new($id, $name, libfileparser_core::Category::Docs).extensions(&[$($ext),*]).mime_types(&[$($mime),*]);

        $(#[$doc])*
        pub struct $reader;

        impl libfileparser_core::Reader for $reader {
            fn info(&self) -> &libfileparser_core::ReaderInfo {
                &$info
            }

            /// By extension only: none of these markups has a signature.
            fn detect(&self, header: &[u8], name: &str) -> libfileparser_core::Detection {
                if $info.matches_extension(name) && libfileparser_core::is_text(header) {
                    libfileparser_core::Detection::new(libfileparser_core::Confidence::Extension).with_language($id)
                } else {
                    libfileparser_core::Detection::NONE
                }
            }

            fn open(&self, src: &libfileparser_core::FileSource, cx: &libfileparser_core::ParseContext) -> libfileparser_core::Result<libfileparser_core::Document> {
                $crate::markup::read(src, cx, $id, $kind, $render)
            }
        }
    };
}
pub(crate) use markup;

pub(crate) fn read(src: &FileSource, cx: &ParseContext, id: &'static str, kind: &'static str, render: fn(&str) -> Result<Rendered>) -> Result<Document> {
    cx.cancel.check()?;
    if !is_text(src.header()) {
        return Err(ParseError::NotThisFormat);
    }
    let bytes = crate::whole(src, cx)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let rendered = render(text)?;
    cx.cancel.check()?;
    let mut b = Builder::new(cx);
    crate::html::parse(&rendered.html, src.name(), &mut b)?;
    let flow = rendered.resources.into_iter().fold(OwnedFlow::new(b.finish()?), |f, (name, file)| f.with_resource(name, file));
    let mut doc = Document::new(FormatInfo::new(id, kind, Category::Docs)).with_facet(Role::Primary, "Document", Content::Flow(Arc::new(flow)));
    if let Some(t) = rendered.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        doc = doc.with_meta("Title", t);
    }
    for w in rendered.warnings {
        doc = doc.with_warning(Warning::new(w));
    }
    Ok(doc.with_file_facets(src, Some(id)))
}

// ---------------------------------------------------------------- reStructuredText

markup!(
    /// reStructuredText (Python's documentation markup), through `rst_parser`.
    RstReader, RST, "rst", "reStructuredText", "reStructuredText document", ["rst", "rest"], ["text/x-rst"], rst
);

fn rst(text: &str) -> Result<Rendered> {
    // rst_parser's grammar wants a final newline, and tabs as spaces (as docutils does).
    let mut text = text.replace('\t', "        ");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let (text, long) = tame_rst(&text);
    let doc = rst_parser::parse(&text).map_err(|e| ParseError::invalid(format!("not reStructuredText this parser reads: {}", first_line(&e.to_string()))))?;
    // The tree as JSON (document_tree derives Serialize), walked generically: rst_parser's
    // own HTML writer, rst_renderer, panics on 13 kinds of element.
    let tree = serde_json::to_value(&doc).map_err(ParseError::invalid)?;
    let mut w = RstHtml { out: String::with_capacity(text.len() * 2), sections: 0, title: None, depth: 0 };
    w.children(&tree);
    let mut warnings = Vec::new();
    if !long.is_empty() {
        // Each long line back where its placeholder is, as written.
        for (n, line) in long.iter().enumerate() {
            w.out = w.out.replacen(&placeholder(n), &esc(line.trim()), 1);
        }
        warnings.push(format!("{} line(s) over {LONG_LINE} characters shown without their inline formatting: reading them as reStructuredText takes too long.", long.len()));
    }
    let title = w.title.map(|t| long.iter().enumerate().fold(t, |t, (n, line)| t.replacen(&placeholder(n), line.trim(), 1)));
    Ok(Rendered { html: w.out, title, warnings, ..Rendered::default() })
}

/// Characters in one line past which rst_parser is too slow: its parse grows with the
/// square of a line's length (4,000 characters take seconds), and it can't be
/// cancelled. Found by fuzzing; many short lines stay linear.
const LONG_LINE: usize = 800;

/// A word standing in for long line `n` while rst_parser reads the rest.
fn placeholder(n: usize) -> String {
    format!("lfplongline{n}x")
}

/// `text` with every line over [`LONG_LINE`] characters swapped for a placeholder (its
/// indent kept, so it stays in the same block), and those lines.
fn tame_rst(text: &str) -> (String, Vec<String>) {
    if !text.lines().any(|l| l.chars().count() > LONG_LINE) {
        return (text.to_string(), Vec::new());
    }
    let mut long = Vec::new();
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        if body.chars().count() > LONG_LINE {
            let indent = &body[..body.len() - body.trim_start().len()];
            out.push_str(indent);
            out.push_str(&placeholder(long.len()));
            out.push('\n');
            long.push(body.to_string());
        } else {
            out.push_str(line);
        }
    }
    (out, long)
}

struct RstHtml {
    out: String,
    /// Sections open: a title's heading level.
    sections: usize,
    title: Option<String>,
    depth: usize,
}

impl RstHtml {
    fn children(&mut self, v: &serde_json::Value) {
        if let Some(kids) = v.get("children").and_then(|c| c.as_array()) {
            for k in kids {
                self.node(k, None);
            }
        }
    }

    /// One child: a string, an element tagged with its kind (`{"Paragraph": {…}}`,
    /// inside transparent category wrappers), or an untagged element (a list item).
    fn node(&mut self, v: &serde_json::Value, tag: Option<&str>) {
        use serde_json::Value;
        self.depth += 1;
        if self.depth > 256 {
            self.depth -= 1;
            return;
        }
        match v {
            Value::String(t) => self.out.push_str(&esc(t)),
            Value::Object(m) if m.len() == 1 && tag.is_none() && m.keys().all(|k| k.starts_with(|c: char| c.is_ascii_uppercase())) => {
                let (k, inner) = m.iter().next().expect("one entry");
                match k.as_str() {
                    "SubStructure" | "StructuralSubElement" | "BodyElement" | "TextOrInlineElement" | "SubSection" | "SubTopic" | "SubSidebar" | "SubLineBlock" | "SubBlockQuote" | "SubFootnote" | "SubFigure" | "SubTable" | "SubTableGroup" | "SubDLItem" | "SubField" | "SubOptionListItem" | "SubOption" => self.node(inner, None),
                    "String" => self.node(inner, Some("String")),
                    kind => self.element(kind, inner),
                }
            }
            Value::Object(_) => self.element(tag.unwrap_or(""), v),
            Value::Array(a) => a.iter().for_each(|x| self.node(x, None)),
            _ => {}
        }
        self.depth -= 1;
    }

    fn wrap(&mut self, tag: &str, v: &serde_json::Value) {
        self.out.push_str(&format!("<{tag}>"));
        self.children(v);
        self.out.push_str(&format!("</{tag}>"));
    }

    fn element(&mut self, kind: &str, v: &serde_json::Value) {
        let field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        let classes: Vec<String> = v.get("classes").and_then(|c| c.as_array()).map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect()).unwrap_or_default();
        match kind {
            "Section" => {
                self.sections += 1;
                self.children(v);
                self.sections -= 1;
            }
            "Title" => {
                let level = self.sections.clamp(1, 6);
                let start = self.out.len();
                self.wrap(&format!("h{level}"), v);
                if self.title.is_none() && level == 1 {
                    self.title = Some(strip_tags(&self.out[start..]));
                }
            }
            "Subtitle" => self.wrap("h2", v),
            "Paragraph" | "Line" | "Rubric" | "Attribution" => self.wrap("p", v),
            "Emphasis" | "TitleReference" => self.wrap("em", v),
            "Strong" => self.wrap("strong", v),
            "Subscript" => self.wrap("sub", v),
            "Superscript" => self.wrap("sup", v),
            "Literal" | "Math" => {
                let mut text = String::new();
                collect_text(v, &mut text);
                self.out.push_str(&format!("<code>{}</code>", esc(&text)));
            }
            "Reference" => match field("refuri") {
                Some(uri) => {
                    self.out.push_str(&format!("<a href=\"{}\">", esc(&uri)));
                    self.children(v);
                    self.out.push_str("</a>");
                }
                None => self.children(v),
            },
            "FootnoteReference" | "CitationReference" => {
                let mut label = String::new();
                collect_text(v, &mut label);
                self.out.push_str(&format!("<sup>[{}]</sup>", esc(&label)));
            }
            "BulletList" | "EnumeratedList" => {
                let tag = if kind == "BulletList" { "ul" } else { "ol" };
                self.out.push_str(&format!("<{tag}>"));
                for item in v.get("children").and_then(|c| c.as_array()).into_iter().flatten() {
                    self.out.push_str("<li>");
                    self.children(item);
                    self.out.push_str("</li>");
                }
                self.out.push_str(&format!("</{tag}>"));
            }
            "LiteralBlock" | "DoctestBlock" | "MathBlock" => {
                let mut text = String::new();
                collect_text(v, &mut text);
                let lang = match classes.as_slice() {
                    [c, l, ..] if c == "code" => Some(l.as_str()),
                    _ if kind == "DoctestBlock" => Some("pycon"),
                    _ if kind == "MathBlock" => Some("math"),
                    _ => None,
                };
                match lang.filter(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(c))) {
                    Some(l) => self.out.push_str(&format!("<pre><code class=\"language-{l}\">{}</code></pre>", esc(text.trim_end_matches('\n')))),
                    None => self.out.push_str(&format!("<pre>{}</pre>", esc(text.trim_end_matches('\n')))),
                }
            }
            "BlockQuote" | "Topic" | "Sidebar" | "Compound" | "Container" => self.wrap("blockquote", v),
            "Attention" | "Caution" | "Danger" | "Error" | "Hint" | "Important" | "Note" | "Tip" | "Warning" | "AdmonitionElement" => {
                self.out.push_str(&format!("<blockquote><p><strong>{}</strong></p>", if kind == "AdmonitionElement" { "Admonition" } else { kind }));
                self.children(v);
                self.out.push_str("</blockquote>");
            }
            "Footnote" | "Citation" => {
                self.out.push_str("<div>");
                self.children(v);
                self.out.push_str("</div>");
            }
            "Label" => {
                let mut label = String::new();
                collect_text(v, &mut label);
                self.out.push_str(&format!("<p><sup>[{}]</sup></p>", esc(&label)));
            }
            "Image" => {
                let uri = field("uri").unwrap_or_default();
                let alt = field("alt").unwrap_or_default();
                self.out.push_str(&format!("<img src=\"{}\" alt=\"{}\">", esc(&uri), esc(alt.trim())));
            }
            "Caption" | "Legend" => {
                self.out.push_str("<p><em>");
                self.children(v);
                self.out.push_str("</em></p>");
            }
            "Transition" => self.out.push_str("<hr>"),
            "LineBlock" => self.wrap("div", v),
            "DefinitionList" | "FieldList" | "OptionList" => self.wrap("div", v),
            "Term" | "FieldName" | "OptionGroup" => {
                self.out.push_str("<p><strong>");
                self.children(v);
                self.out.push_str("</strong></p>");
            }
            "Definition" | "FieldBody" | "Description" => self.wrap("blockquote", v),
            "Table" => self.wrap("table", v),
            "TableHead" => self.wrap("thead", v),
            "TableRow" => self.wrap("tr", v),
            "TableEntry" => self.wrap("td", v),
            "Comment" | "Target" | "SubstitutionDefinition" | "Raw" | "Pending" | "SystemMessage" | "Decoration" | "Header" | "Footer" | "ColSpec" | "Generated" => {}
            _ => self.children(v),
        }
    }
}

/// Every string under `v`.
fn collect_text(v: &serde_json::Value, out: &mut String) {
    match v {
        serde_json::Value::String(s) => out.push_str(s),
        serde_json::Value::Object(m) => {
            if let Some(c) = m.get("children") {
                collect_text(c, out);
            } else if m.len() == 1 {
                m.values().for_each(|x| collect_text(x, out));
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_text(x, out)),
        _ => {}
    }
}

fn first_line(s: &str) -> &str {
    let s = s.trim();
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s);
    &line[..line.floor_char_boundary(200)]
}

// ---------------------------------------------------------------- Djot

markup!(
    /// Djot (John MacFarlane's successor to CommonMark), through `jotdown`.
    DjotReader, DJOT, "djot", "Djot", "Djot document", ["dj", "djot"], ["text/x-djot"], djot
);

fn djot(text: &str) -> Result<Rendered> {
    Ok(Rendered { html: jotdown::html::render_to_string(jotdown::Parser::new(text)), ..Rendered::default() })
}

// ---------------------------------------------------------------- AsciiDoc

markup!(
    /// AsciiDoc (Asciidoctor's language), through `asciidoc-parser` in secure mode.
    AsciidocReader, ASCIIDOC, "asciidoc", "AsciiDoc", "AsciiDoc document", ["adoc", "asciidoc", "asc.txt"], ["text/asciidoc"], asciidoc
);

fn asciidoc(text: &str) -> Result<Rendered> {
    use asciidoc_parser::blocks::FindBlocks;
    let doc = asciidoc_parser::Parser::new().parse(text);
    let mut out = String::with_capacity(text.len() * 2);
    let title = doc.doctitle().map(str::to_string);
    if let Some(t) = &title {
        out.push_str(&format!("<h1>{t}</h1>"));
    }
    let mut warnings = Vec::new();
    adoc_blocks(doc.child_blocks(), &mut out, 0, &mut warnings);
    let title = doc.doctitle_sanitized().or(title);
    Ok(Rendered { html: out, title, warnings, ..Rendered::default() })
}

fn adoc_blocks<'a>(blocks: impl Iterator<Item = &'a asciidoc_parser::blocks::Block<'a>>, out: &mut String, depth: usize, warnings: &mut Vec<String>) {
    for b in blocks {
        adoc_block(b, out, depth, warnings);
    }
}

fn adoc_block<'a>(block: &'a asciidoc_parser::blocks::Block<'a>, out: &mut String, depth: usize, warnings: &mut Vec<String>) {
    use asciidoc_parser::blocks::{Block, BreakType, ContentModel, FindBlocks, IsBlock, ListType, MediaType, TableCellContent};
    if depth > 64 {
        return;
    }
    // A block's own title (".Title" above it), except a section's, which is its heading.
    if !matches!(block, Block::Section(_))
        && let Some(t) = block.title() {
            out.push_str(&format!("<p><strong>{t}</strong></p>"));
        }
    let context = block.resolved_context();
    match block {
        Block::Section(s) => {
            let level = (s.level() + 1).min(6);
            out.push_str(&format!("<h{level}>{}</h{level}>", s.section_title()));
            adoc_blocks(s.child_blocks(), out, depth + 1, warnings);
        }
        Block::List(l) => {
            let tag = if matches!(l.type_(), ListType::Ordered) { "ol" } else { "ul" };
            out.push_str(&format!("<{tag}>"));
            adoc_blocks(l.child_blocks(), out, depth + 1, warnings);
            out.push_str(&format!("</{tag}>"));
        }
        Block::ListItem(item) => {
            out.push_str("<li>");
            if let Some(c) = item.checkbox() {
                out.push_str(if c { "☑ " } else { "☐ " });
            }
            if let Some(html) = item.rendered_html_content() {
                out.push_str(&format!("<p>{html}</p>"));
            }
            adoc_blocks(item.child_blocks(), out, depth + 1, warnings);
            out.push_str("</li>");
        }
        Block::Media(m) => match m.type_() {
            MediaType::Image => out.push_str(&format!("<img src=\"{}\" alt=\"{}\">", esc(m.resolved_target()), esc(block.title().unwrap_or("")))),
            _ => out.push_str(&format!("<p>[{}: {}]</p>", context, esc(m.resolved_target()))),
        },
        Block::Break(b) => {
            if matches!(b.type_(), BreakType::Thematic) {
                out.push_str("<hr>");
            }
        }
        Block::Table(t) => {
            out.push_str("<table>");
            let cells = |row: &'a asciidoc_parser::blocks::TableRow<'a>, tag: &str, out: &mut String, warnings: &mut Vec<String>| {
                out.push_str("<tr>");
                for cell in row.cells() {
                    out.push_str(&format!("<{tag}>"));
                    match cell.content() {
                        TableCellContent::Simple(c) => out.push_str(c.rendered_html()),
                        TableCellContent::AsciiDoc(d) => {
                            // A cell's blocks become its text: our tables hold inlines.
                            let mut inner = String::new();
                            adoc_blocks(d.blocks().iter(), &mut inner, depth + 1, warnings);
                            out.push_str(&inner.replace("<p>", "").replace("</p>", " "));
                        }
                    }
                    out.push_str(&format!("</{tag}>"));
                }
                out.push_str("</tr>");
            };
            if let Some(h) = t.header_row() {
                out.push_str("<thead>");
                cells(h, "th", out, warnings);
                out.push_str("</thead>");
            }
            for r in t.body_rows() {
                cells(r, "td", out, warnings);
            }
            if let Some(f) = t.footer_row() {
                cells(f, "td", out, warnings);
            }
            out.push_str("</table>");
        }
        Block::Admonition(a) => {
            out.push_str(&format!("<blockquote><p><strong>{}</strong></p>", esc(a.label())));
            if let Some(c) = a.content() {
                out.push_str(&format!("<p>{}</p>", c.rendered_html()));
            }
            adoc_blocks(a.child_blocks(), out, depth + 1, warnings);
            out.push_str("</blockquote>");
        }
        Block::Toc(_) | Block::DocumentAttribute(_) => {}
        _ => match block.content_model() {
            ContentModel::Compound => {
                let quote = matches!(&*context, "quote" | "verse" | "example" | "sidebar");
                out.push_str(if quote { "<blockquote>" } else { "<div>" });
                if let Some(html) = block.rendered_html_content() {
                    out.push_str(&format!("<p>{html}</p>"));
                }
                adoc_blocks(block.child_blocks(), out, depth + 1, warnings);
                out.push_str(if quote { "</blockquote>" } else { "</div>" });
            }
            ContentModel::Verbatim => {
                let html = block.rendered_html_content().unwrap_or("");
                let lang = block.attrlist().and_then(|a| a.nth_attribute(2)).map(|a| a.value().to_string());
                match lang.filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(c))) {
                    Some(l) => out.push_str(&format!("<pre><code class=\"language-{l}\">{html}</code></pre>")),
                    None => out.push_str(&format!("<pre>{html}</pre>")),
                }
            }
            ContentModel::Raw => {
                // A passthrough's HTML, as Asciidoctor would emit it; ours only reads it.
                if let Some(html) = block.rendered_html_content() {
                    out.push_str(html);
                }
            }
            ContentModel::Simple => {
                let html = block.rendered_html_content().unwrap_or("");
                match &*context {
                    "verse" | "quote" => out.push_str(&format!("<blockquote><p>{}</p></blockquote>", html.replace('\n', "<br>"))),
                    "literal" | "listing" => out.push_str(&format!("<pre>{html}</pre>")),
                    _ => out.push_str(&format!("<p>{html}</p>")),
                }
            }
            ContentModel::Empty | ContentModel::Table => {}
        },
    }
}

// ---------------------------------------------------------------- Typst

markup!(
    /// Typst's markup, through `typst-syntax` (Typst's own parser). Text, headings,
    /// lists, emphasis, raw text, links and math are shown; code (`#let`, `#set`,
    /// `#show`, imports) isn't run, and of a function call only its content is shown.
    TypstReader, TYPST, "typst", "Typst", "Typst document", ["typ"], ["text/vnd.typst"], typst
);

fn typst(text: &str) -> Result<Rendered> {
    let root = typst_syntax::parse(text);
    let mut t = Typst { out: String::with_capacity(text.len() * 2), para: false, lists: Vec::new(), depth: 0, title: None };
    t.markup(&root);
    t.close_lists(0);
    t.end_para();
    let (errors, _) = root.errors_and_warnings();
    let warnings = if errors.is_empty() { Vec::new() } else { vec![format!("{} syntax error(s); the text around them is shown as Typst read it.", errors.len())] };
    Ok(Rendered { html: t.out, title: t.title, warnings, ..Rendered::default() })
}

struct Typst {
    out: String,
    /// A paragraph is open.
    para: bool,
    /// Open lists, outermost first: `true` for numbered.
    lists: Vec<bool>,
    depth: usize,
    title: Option<String>,
}

impl Typst {
    fn inline(&mut self) {
        if !self.para {
            self.close_lists(0);
            self.out.push_str("<p>");
            self.para = true;
        }
    }

    fn end_para(&mut self) {
        if self.para {
            self.out.push_str("</p>");
            self.para = false;
        }
    }

    fn close_lists(&mut self, keep: usize) {
        while self.lists.len() > keep {
            let ordered = self.lists.pop().unwrap_or(false);
            self.out.push_str(if ordered { "</ol>" } else { "</ul>" });
        }
    }

    fn markup(&mut self, node: &typst_syntax::SyntaxNode) {
        use typst_syntax::SyntaxKind as K;
        self.depth += 1;
        if self.depth > 200 {
            self.depth -= 1;
            return;
        }
        let kids: Vec<_> = node.children().collect();
        let mut i = 0;
        while i < kids.len() {
            let n = kids[i];
            match n.kind() {
                K::Raw if self.is_block_raw(n) => {
                    self.end_para();
                    self.close_lists(0);
                    self.raw(n);
                }
                K::Equation if n.children().any(|c| c.kind() == K::Space && c.leaf_text().contains('\n')) || is_display_math(n) => {
                    self.end_para();
                    self.close_lists(0);
                    self.out.push_str(&format!("<pre><code class=\"language-math\">{}</code></pre>", esc(n.full_text().trim_matches('$').trim())));
                }
                K::Space => {
                    if self.para {
                        self.out.push(' ');
                    }
                }
                K::Parbreak => {
                    self.end_para();
                }
                K::Heading => {
                    self.end_para();
                    self.close_lists(0);
                    let level = n.children().find(|c| c.kind() == K::HeadingMarker).map_or(1, |m| m.leaf_text().len()).clamp(1, 6);
                    self.out.push_str(&format!("<h{level}>"));
                    let start = self.out.len();
                    self.para = true;
                    for c in n.children().filter(|c| c.kind() == K::Markup) {
                        self.markup(c);
                    }
                    self.para = false;
                    if level == 1 && self.title.is_none() {
                        self.title = Some(strip_tags(&self.out[start..]));
                    }
                    self.out.push_str(&format!("</h{level}>"));
                }
                K::ListItem | K::EnumItem | K::TermItem => {
                    self.end_para();
                    let ordered = n.kind() == K::EnumItem;
                    // Items at this markup level share one list; a nested list is
                    // inside the item's own markup.
                    if self.lists.last() != Some(&ordered) || self.lists.is_empty() {
                        self.close_lists(self.lists.len().saturating_sub(usize::from(!self.lists.is_empty())));
                        self.lists.push(ordered);
                        self.out.push_str(if ordered { "<ol>" } else { "<ul>" });
                    }
                    let saved = std::mem::take(&mut self.lists);
                    self.out.push_str("<li>");
                    let markups: Vec<_> = n.children().filter(|c| c.kind() == K::Markup).collect();
                    for (k, m) in markups.iter().enumerate() {
                        if k == 1 {
                            self.end_para();
                            self.out.push_str("<p>");
                            self.para = true;
                        }
                        if k == 0 && n.kind() == K::TermItem {
                            self.inline();
                            self.out.push_str("<strong>");
                            self.markup(m);
                            self.out.push_str("</strong>");
                        } else {
                            self.markup(m);
                        }
                    }
                    self.end_para();
                    self.close_lists(0);
                    self.lists = saved;
                    self.out.push_str("</li>");
                }
                K::Hash => {
                    // Embedded code: the next node is the expression.
                    if let Some(expr) = kids.get(i + 1) {
                        self.code(expr);
                        i += 1;
                    }
                }
                K::FuncCall | K::ContentBlock => self.code(n),
                K::LineComment | K::BlockComment | K::Label | K::Error => {}
                _ => self.leafish(n),
            }
            i += 1;
        }
        self.depth -= 1;
    }

    fn is_block_raw(&self, n: &typst_syntax::SyntaxNode) -> bool {
        n.kind() == typst_syntax::SyntaxKind::Raw && n.children().next().is_some_and(|d| d.leaf_text().len() >= 3) && n.full_text().contains('\n')
    }

    /// Inline content: text, marks, links, inline raw and math.
    fn leafish(&mut self, n: &typst_syntax::SyntaxNode) {
        use typst_syntax::SyntaxKind as K;
        match n.kind() {
            K::Text => {
                self.inline();
                self.out.push_str(&esc(n.leaf_text()));
            }
            K::Escape => {
                self.inline();
                let t = n.leaf_text().trim_start_matches('\\');
                let c = t.strip_prefix("u{").and_then(|h| h.strip_suffix('}')).and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32);
                self.out.push_str(&esc(&c.map_or_else(|| t.to_string(), String::from)));
            }
            K::Shorthand => {
                self.inline();
                self.out.push_str(match n.leaf_text().as_str() {
                    "~" => "\u{a0}",
                    "---" => "—",
                    "--" => "–",
                    "..." => "…",
                    "-?" => "\u{ad}",
                    "-" => "−",
                    other => other,
                });
            }
            K::SmartQuote => {
                self.inline();
                self.out.push_str(if n.leaf_text() == "'" { "’" } else { "\u{201d}" });
            }
            K::Linebreak => {
                self.inline();
                self.out.push_str("<br>");
            }
            K::Strong | K::Emph => {
                self.inline();
                let tag = if n.kind() == K::Strong { "strong" } else { "em" };
                self.out.push_str(&format!("<{tag}>"));
                for c in n.children().filter(|c| c.kind() == K::Markup) {
                    self.markup(c);
                }
                self.out.push_str(&format!("</{tag}>"));
            }
            K::Link => {
                self.inline();
                let url = n.leaf_text();
                self.out.push_str(&format!("<a href=\"{0}\">{0}</a>", esc(url)));
            }
            K::Ref => {
                self.inline();
                self.out.push_str(&esc(&n.full_text()));
            }
            K::Raw => self.raw(n),
            K::Equation => {
                self.inline();
                self.out.push_str(&format!("<code>{}</code>", esc(n.full_text().trim_matches('$').trim())));
            }
            K::Markup => self.markup(n),
            _ => {}
        }
    }

    fn raw(&mut self, n: &typst_syntax::SyntaxNode) {
        use typst_syntax::SyntaxKind as K;
        let lang = n.children().find(|c| c.kind() == K::RawLang).map(|l| l.leaf_text().to_string());
        let body: String = n.children().filter(|c| matches!(c.kind(), K::Text | K::RawTrimmed)).map(|c| if c.kind() == K::RawTrimmed { if c.leaf_text().contains('\n') { "\n" } else { "" } } else { c.leaf_text().as_str() }).collect();
        if self.is_block_raw(n) {
            let body = body.trim_matches('\n');
            match lang.filter(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(c))) {
                Some(l) => self.out.push_str(&format!("<pre><code class=\"language-{l}\">{}</code></pre>", esc(body))),
                None => self.out.push_str(&format!("<pre>{}</pre>", esc(body))),
            }
        } else {
            self.inline();
            self.out.push_str(&format!("<code>{}</code>", esc(&body)));
        }
    }

    /// An embedded expression: what a reader would see of it.
    fn code(&mut self, n: &typst_syntax::SyntaxNode) {
        use typst_syntax::SyntaxKind as K;
        self.depth += 1;
        if self.depth > 200 {
            self.depth -= 1;
            return;
        }
        match n.kind() {
            K::ContentBlock => {
                for c in n.children().filter(|c| c.kind() == K::Markup) {
                    self.markup(c);
                }
            }
            K::Str => {
                self.inline();
                self.out.push_str(&esc(n.leaf_text().trim_matches('"')));
            }
            K::FuncCall => self.call(n),
            K::CodeBlock => {
                // `{ … }`: its content blocks only.
                for c in n.children() {
                    if c.kind() == K::Code {
                        for e in c.children() {
                            if matches!(e.kind(), K::FuncCall | K::ContentBlock) {
                                self.code(e);
                            }
                        }
                    }
                }
            }
            K::Conditional | K::ForLoop | K::WhileLoop | K::Contextual | K::Parenthesized => {
                for c in n.children() {
                    if matches!(c.kind(), K::ContentBlock | K::FuncCall | K::CodeBlock) {
                        self.code(c);
                    }
                }
            }
            // Let, set, show, import, include, identifiers and values: code, not text.
            _ => {}
        }
        self.depth -= 1;
    }

    /// A function call: the built-in elements a document is made of, by name; any
    /// other call's content arguments.
    fn call(&mut self, n: &typst_syntax::SyntaxNode) {
        use typst_syntax::SyntaxKind as K;
        let callee = n.children().next().map(|c| c.full_text().to_string()).unwrap_or_default();
        let name = callee.rsplit('.').next().unwrap_or("").to_string();
        let Some(args) = n.children().find(|c| c.kind() == K::Args) else { return };
        let positional: Vec<_> = args.children().filter(|c| !matches!(c.kind(), K::LeftParen | K::RightParen | K::Comma | K::Space | K::Named | K::Spread | K::LineComment | K::BlockComment)).collect();
        let named = |key: &str| args.children().filter(|c| c.kind() == K::Named).find(|c| c.children().next().is_some_and(|k| k.leaf_text() == key)).and_then(|c| c.children().rfind(|x| !matches!(x.kind(), K::Ident | K::Colon | K::Space)));
        let str_arg = |i: usize| positional.get(i).filter(|a| a.kind() == K::Str).map(|a| a.leaf_text().trim_matches('"').to_string());
        let wrap = |t: &mut Typst, tag: &str| {
            t.inline();
            t.out.push_str(&format!("<{tag}>"));
            positional.iter().for_each(|a| t.code(a));
            t.out.push_str(&format!("</{tag}>"));
        };
        match name.as_str() {
            "strong" => wrap(self, "strong"),
            "emph" => wrap(self, "em"),
            "underline" => wrap(self, "u"),
            "strike" => wrap(self, "s"),
            "sub" => wrap(self, "sub"),
            "super" => wrap(self, "sup"),
            "raw" => {
                if let Some(s) = str_arg(0) {
                    self.inline();
                    self.out.push_str(&format!("<code>{}</code>", esc(&s)));
                }
            }
            "link" => {
                let url = str_arg(0).unwrap_or_default();
                self.inline();
                self.out.push_str(&format!("<a href=\"{}\">", esc(&url)));
                if positional.len() > 1 {
                    positional[1..].iter().for_each(|a| self.code(a));
                } else {
                    self.out.push_str(&esc(&url));
                }
                self.out.push_str("</a>");
            }
            "image" => {
                if let Some(path) = str_arg(0) {
                    let alt = named("alt").map(|a| a.leaf_text().trim_matches('"').to_string()).unwrap_or_default();
                    self.out.push_str(&format!("<img src=\"{}\" alt=\"{}\">", esc(&path), esc(&alt)));
                }
            }
            "heading" | "title" => {
                self.end_para();
                self.close_lists(0);
                let level = named("level").and_then(|l| l.leaf_text().parse::<usize>().ok()).unwrap_or(1).clamp(1, 6);
                self.out.push_str(&format!("<h{level}>"));
                self.para = true;
                positional.iter().for_each(|a| self.code(a));
                self.para = false;
                self.out.push_str(&format!("</h{level}>"));
            }
            "quote" => {
                self.end_para();
                self.out.push_str("<blockquote>");
                positional.iter().for_each(|a| self.code(a));
                self.end_para();
                self.out.push_str("</blockquote>");
            }
            "footnote" => {
                self.inline();
                self.out.push_str("<sup>[");
                positional.iter().for_each(|a| self.code(a));
                self.out.push_str("]</sup>");
            }
            "table" | "grid" => {
                self.end_para();
                self.close_lists(0);
                let columns = named("columns").map_or(1, |c| match c.kind() {
                    K::Int => c.leaf_text().parse().unwrap_or(1),
                    K::Array => c.children().filter(|x| !matches!(x.kind(), K::LeftParen | K::RightParen | K::Comma | K::Space)).count().max(1),
                    _ => 1,
                });
                let cells: Vec<_> = positional.iter().filter(|a| matches!(a.kind(), K::ContentBlock | K::Str | K::FuncCall)).collect();
                self.out.push_str("<table>");
                for row in cells.chunks(columns.clamp(1, 1000)) {
                    self.out.push_str("<tr>");
                    for cell in row {
                        self.out.push_str("<td>");
                        self.para = true;
                        self.code(cell);
                        self.para = false;
                        self.out.push_str("</td>");
                    }
                    self.out.push_str("</tr>");
                }
                self.out.push_str("</table>");
            }
            "figure" => {
                positional.iter().for_each(|a| self.code(a));
                if let Some(c) = named("caption") {
                    self.end_para();
                    self.out.push_str("<p><em>");
                    self.para = true;
                    self.code(c);
                    self.para = false;
                    self.out.push_str("</em></p>");
                }
            }
            "pagebreak" | "colbreak" | "line" => {}
            _ => {
                for a in positional.iter().filter(|a| matches!(a.kind(), K::ContentBlock | K::FuncCall)) {
                    self.code(a);
                }
            }
        }
    }
}

/// `$ x $` (with spaces inside the dollars) is display math in Typst.
fn is_display_math(n: &typst_syntax::SyntaxNode) -> bool {
    let t = n.full_text();
    t.len() > 2 && t.starts_with("$ ") && t.ends_with(" $")
}


// ---------------------------------------------------------------- Textile

markup!(
    /// Textile (Redmine's and Textpattern's markup), read by `markup-lite`: headings,
    /// paragraphs, quotes, code, lists, tables, links, images and phrase marks.
    TextileReader, TEXTILE, "textile", "Textile", "Textile document", ["textile"], ["text/x-textile"], textile
);

fn textile(text: &str) -> Result<Rendered> {
    Ok(markup_lite::textile(text).into())
}


#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::Reader;

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
    fn rst() {
        let (doc, s) = blocks(&RstReader, "a.rst", "Title\n=====\n\nSome *em* and **strong** and ``code`` [#]_.\n\nSub\n---\n\n* one\n* two\n\n.. code:: python\n\n   print(1)\n\n`Link <https://x.org>`_\n\n.. note:: Careful\n\n.. [#] foot\n\n.. image:: a.png\n   :alt: Alt\n");
        has(&s, &["Heading { level: 1", "Heading { level: 2", "Emphasis", "Strong", "Code(\"code\")", "[1]", "List", "language: Some(\"python\")", "print(1)", "https://x.org", "Note", "Careful", "foot", "a.png"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Title"));
    }

    /// Found by fuzzing: one long line took rst_parser tens of seconds.
    #[test]
    fn rst_long_lines_are_kept_and_quick() {
        let long = "word ".repeat(2000);
        let text = format!("Title\n=====\n\nShort *em*.\n\n{long}\n\n::\n\n    {long}\n");
        let t = std::time::Instant::now();
        let (doc, s) = blocks(&RstReader, "l.rst", &text);
        assert!(t.elapsed().as_secs() < 3, "{:?}", t.elapsed());
        assert!(s.contains("Emphasis") && !s.contains("lfplongline"), "{}", &s[..300]);
        assert!(s.matches("word word").count() >= 2);
        assert!(s.contains("Code {"), "the literal block keeps its long line: {}", &s[..200]);
        assert!(doc.warnings()[0].message.contains("2 line(s)"));
    }

    #[test]
    fn djot() {
        let (_, s) = blocks(&DjotReader, "a.dj", "# Head\n\n_em_ and *strong* and `code` [link](https://x.org)\n\n- a\n- b\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n``` rust\nfn main() {}\n```\n");
        has(&s, &["Heading { level: 1", "Emphasis", "Strong", "Code(\"code\")", "https://x.org", "List", "Table", "language: Some(\"rust\")"]);
    }

    #[test]
    fn asciidoc() {
        let (doc, s) = blocks(&AsciidocReader, "a.adoc", "= The Title\n\n== Section\n\nSome _em_ and *strong* and `mono`.\n\n* one\n* two\n\n[source,rust]\n----\nfn main() {}\n----\n\n|===\n|a |b\n\n|1 |2\n|===\n\nNOTE: careful\n\nhttps://x.org[a link]\n");
        has(&s, &["Heading { level: 1", "The Title", "Heading { level: 2", "Emphasis", "Strong", "Code(\"mono\")", "List", "language: Some(\"rust\")", "Table", "careful", "https://x.org"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "The Title"));
    }

    #[test]
    fn asciidoc_includes_nothing() {
        let (_, s) = blocks(&AsciidocReader, "a.adoc", "include::/etc/passwd[]\n\nafter\n");
        assert!(!s.contains("root:"), "{s}");
    }

    #[test]
    fn typst() {
        let (doc, s) = blocks(&TypstReader, "a.typ", "#set page(width: 10cm)\n#let x = 1\n= Intro\nSome _em_ and *strong* and `code` -- see https://x.org.\n\n- one\n- two\n  - nested\n+ first\n\n```rust\nfn main() {}\n```\n\n$ x^2 $\n\n#strong[called] #link(\"https://y.org\")[there] #table(columns: 2, [a], [b], [c], [d])\n");
        has(&s, &["Heading { level: 1", "Intro", "Emphasis", "Strong", "Code(\"code\")", "–", "https://x.org", "List { ordered: false", "nested", "List { ordered: true", "language: Some(\"rust\")", "x^2", "called", "https://y.org", "Table"]);
        assert!(!s.contains("width") && !s.contains("let x"), "{s}");
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Intro"));
    }

    #[test]
    fn textile() {
        let (doc, s) = blocks(&TextileReader, "a.textile", "h1. Head\n\nSome _em_ and *strong* and @code@ and \"a link\":https://x.org.\n\n* one\n** nested\n* two\n# first\n\nbq. quoted\n\nbc. let x = 1;\n\n|_. a |_. b |\n| 1 | 2 |\n\n!pic.png(Alt)!\n");
        has(&s, &["Heading { level: 1", "Emphasis", "Strong", "Code(\"code\")", "https://x.org", "List { ordered: false", "nested", "List { ordered: true", "Quote", "let x = 1;", "Table { header_rows: 1", "pic.png"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Head"));
    }

}


