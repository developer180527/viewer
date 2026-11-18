//! HTML and XHTML into formatted text, parsed by html5ever (Servo's HTML parser,
//! through `scraper`): web pages ([`HtmlReader`], docs/design/libfileparser.md §2.1:
//! a web page is a document) and EPUB's chapters. Only the structure is kept
//! (headings, paragraphs, lists, tables, quotes, code, images, links, emphasis);
//! styles, scripts and forms aren't, and nothing is run or fetched.
//!
//! A page's other readings come from the same parse: its elements as a tree (the data
//! view), and, from `-core`, its source (the text view) and bytes.

use std::sync::Arc;

use ego_tree::iter::Edge;
use libfileparser_core::{
    Block, Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, OwnedFlow, OwnedTree, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, Warning,
};
use scraper::{ElementRef, Html, Node};

use crate::build::{Builder, Open};
use crate::package::resolve;

/// Parses `html` (a part named `base`, against which images resolve) into `b`.
pub(crate) fn parse(html: &str, base: &str, b: &mut Builder) -> Result<()> {
    flow(&Html::parse_document(html), base, b)
}

/// A parsed page's prose into `b`.
fn flow(doc: &Html, base: &str, b: &mut Builder) -> Result<()> {
    // Inside <script>, <style>, <head>…: skipped, by depth.
    let mut skip = 0usize;
    let mut pre = 0usize;
    // Inline <code> outside <pre>: its text, gathered.
    let mut code: Option<String> = None;
    let mut thead = false;
    for (n, edge) in doc.root_element().traverse().enumerate() {
        if n % 4096 == 0 {
            b.cancel_check()?;
        }
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Element(e) => {
                    let name = e.name();
                    if skip > 0 || matches!(name, "script" | "style" | "head" | "template" | "noscript" | "svg" | "math" | "iframe" | "object" | "select" | "button") {
                        skip += 1;
                        continue;
                    }
                    let el = ElementRef::wrap(node).expect("an element node");
                    open(name, el, base, b, &mut pre, &mut code, thead)?;
                    if name == "thead" {
                        thead = true;
                    }
                }
                Node::Text(t) if skip == 0 => text(t, b, pre > 0, &mut code)?,
                _ => {}
            },
            Edge::Close(node) => {
                if let Node::Element(e) = node.value() {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    if e.name() == "thead" {
                        thead = false;
                    }
                    close(e.name(), b, &mut pre, &mut code)?;
                }
            }
        }
    }
    Ok(())
}

fn text(t: &str, b: &mut Builder, pre: bool, code: &mut Option<String>) -> Result<()> {
    if let Some(c) = code {
        c.push_str(t);
        return Ok(());
    }
    if pre {
        return b.text(t);
    }
    // Whitespace collapses, as a browser would.
    let mut s = String::with_capacity(t.len());
    let mut space = false;
    for ch in t.chars() {
        if ch.is_whitespace() && ch != '\u{a0}' {
            space = true;
        } else {
            if space {
                s.push(' ');
                space = false;
            }
            s.push(ch);
        }
    }
    if space {
        s.push(' ');
    }
    b.text(&s)
}

fn open(name: &str, e: ElementRef, base: &str, b: &mut Builder, pre: &mut usize, code: &mut Option<String>, thead: bool) -> Result<()> {
    let attr = |k: &str| e.value().attr(k).map(str::to_string);
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => b.open(Open::Heading(name.as_bytes()[1] - b'0')),
        "p" | "dt" | "dd" | "figcaption" | "caption" | "summary" => b.open(Open::Paragraph),
        "div" | "section" | "article" | "aside" | "header" | "footer" | "main" | "figure" | "body" | "dl" | "details" | "nav" => b.close(&Open::Paragraph),
        "blockquote" => b.open(Open::Quote),
        "ul" | "menu" => b.open(Open::List { ordered: false, start: 1 }),
        "ol" => b.open(Open::List { ordered: true, start: attr("start").and_then(|s| s.parse().ok()).unwrap_or(1) }),
        "li" => b.open(Open::Item),
        "pre" => {
            *pre += 1;
            b.open(Open::Code(None))
        }
        "code" | "kbd" | "samp" | "tt" if *pre == 0 => {
            *code = Some(String::new());
            Ok(())
        }
        "table" => b.open(Open::Table),
        "tr" => b.open(Open::Row { header: thead }),
        "td" | "th" => b.open(Open::Cell),
        "em" | "i" | "cite" | "dfn" | "var" => b.open(Open::Emphasis),
        "strong" | "b" => b.open(Open::Strong),
        "u" | "ins" => b.open(Open::Underline),
        "s" | "strike" | "del" => b.open(Open::Strikethrough),
        "sup" => b.open(Open::Superscript),
        "sub" => b.open(Open::Subscript),
        "a" => match attr("href") {
            Some(href) if !href.is_empty() => b.open(Open::Link(href)),
            _ => Ok(()),
        },
        "br" => b.line_break(),
        "hr" => b.block(Block::Rule),
        "img" | "image" => {
            let src = attr("src").or_else(|| attr("xlink:href")).or_else(|| attr("href")).unwrap_or_default();
            if src.is_empty() || src.starts_with("data:") {
                return Ok(());
            }
            let reference = if src.contains("://") { src } else { resolve(base, &src) };
            b.image(&reference, &attr("alt").unwrap_or_default())
        }
        _ => Ok(()),
    }
}

fn close(name: &str, b: &mut Builder, pre: &mut usize, code: &mut Option<String>) -> Result<()> {
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => b.close(&Open::Heading(0)),
        "p" | "dt" | "dd" | "figcaption" | "caption" | "summary" | "div" | "section" | "article" | "aside" | "header" | "footer" | "main" | "figure" | "details" | "nav" => {
            b.close(&Open::Paragraph)
        }
        "blockquote" => b.close(&Open::Quote),
        "ul" | "ol" | "menu" => b.close(&Open::List { ordered: false, start: 0 }),
        "li" => b.close(&Open::Item),
        "pre" => {
            *pre = pre.saturating_sub(1);
            b.close(&Open::Code(None))
        }
        "code" | "kbd" | "samp" | "tt" if code.is_some() => {
            let c = code.take().unwrap_or_default();
            b.code(&c)
        }
        "table" => b.close(&Open::Table),
        "tr" => b.close(&Open::Row { header: false }),
        "td" | "th" => b.close(&Open::Cell),
        "em" | "i" | "cite" | "dfn" | "var" => b.close(&Open::Emphasis),
        "strong" | "b" => b.close(&Open::Strong),
        "u" | "ins" => b.close(&Open::Underline),
        "s" | "strike" | "del" => b.close(&Open::Strikethrough),
        "sup" => b.close(&Open::Superscript),
        "sub" => b.close(&Open::Subscript),
        "a" => b.close(&Open::Link(String::new())),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::{Inline, ParseContext};

    #[test]
    fn html_into_blocks() {
        let cx = ParseContext::new();
        let mut b = Builder::new(&cx);
        parse(
            "<html><head><title>T</title><style>p{}</style></head><body><h1>Ch&eacute;pter</h1><p>Some <em>em</em>,  <b>bold</b>\n and <code>x()</code> <a href=\"c2.xhtml\">next</a>.<p>Unclosed<img src=\"../img/a.png\" alt=\"A\"><ul><li>one<li>two</ul><table><thead><tr><th>h</th></tr></thead><tr><td>c</td></tr></table><pre>  keep\n  this</pre><script>bad()</script></body></html>",
            "OEBPS/text/c1.xhtml",
            &mut b,
        )
        .unwrap();
        let blocks = b.finish().unwrap();
        assert_eq!(blocks[0], Block::Heading { level: 1, text: vec![Inline::Text("Chépter".into())] });
        let Block::Paragraph(p) = &blocks[1] else { panic!("{:?}", blocks[1]) };
        assert!(p.contains(&Inline::Emphasis(vec![Inline::Text("em".into())])));
        assert!(p.contains(&Inline::Code("x()".into())));
        // HTML that isn't XML (unclosed <p>, <li>, <img>) parses as a browser would.
        assert!(format!("{blocks:?}").contains("OEBPS/img/a.png"), "{blocks:?}");
        assert!(blocks.iter().any(|b| matches!(b, Block::List { items, .. } if items.len() == 2)), "{blocks:?}");
        assert!(blocks.iter().any(|b| matches!(b, Block::Table { header_rows: 1, rows } if rows.len() == 2)), "{blocks:?}");
        assert!(blocks.contains(&Block::Code { language: None, text: "  keep\n  this".into() }), "{blocks:?}");
        assert!(!format!("{blocks:?}").contains("bad()"));
    }

    #[test]
    fn deep_nesting_doesnt_overflow() {
        let cx = ParseContext::new();
        let mut b = Builder::new(&cx);
        let html = "<div>".repeat(50_000) + "deep" + &"</div>".repeat(50_000);
        parse(&html, "x.html", &mut b).unwrap();
        assert_eq!(b.finish().unwrap().len(), 1);
    }
}

// ---------------------------------------------------------------- web pages

static INFO: ReaderInfo = ReaderInfo::new("html", "Web page", Category::Docs).extensions(&["html", "htm", "xhtml", "xht", "shtml"]).mime_types(&["text/html", "application/xhtml+xml"]);

pub struct HtmlReader;

/// A page's first bytes, lower case, past a byte-order mark, white space and comments.
fn lead(header: &[u8]) -> String {
    let head = String::from_utf8_lossy(&header[..header.len().min(2048)]).to_ascii_lowercase();
    let mut t = head.trim_start_matches('\u{feff}').trim_start();
    while let Some(rest) = t.strip_prefix("<!--") {
        t = rest.split_once("-->").map_or("", |(_, after)| after).trim_start();
    }
    if let Some(rest) = t.strip_prefix("<?xml") {
        t = rest.split_once("?>").map_or("", |(_, after)| after).trim_start();
    }
    t.to_string()
}

/// The page's text, in the encoding it declares: a byte-order mark, `<meta charset>`,
/// or `<meta http-equiv content="…; charset=…">`; UTF-8 when it declares none.
fn decode(bytes: &[u8]) -> String {
    if let Some((encoding, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding.decode_without_bom_handling(&bytes[bom..]).0.into_owned();
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_lowercase();
    let declared = head.find("charset=").and_then(|at| {
        let rest = head[at + 8..].trim_start_matches(['"', '\'']);
        let label: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')).collect();
        encoding_rs::Encoding::for_label(label.as_bytes())
    });
    match declared {
        Some(e) if e != encoding_rs::UTF_8 => e.decode_without_bom_handling(bytes).0.into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// What a page says about itself in its `<head>`, and what it holds.
fn about(doc: &Html) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let first = |css: &str| scraper::Selector::parse(css).ok().and_then(|s| doc.select(&s).next());
    if let Some(t) = first("title").map(|t| t.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")).filter(|t| !t.is_empty()) {
        out.push(("Title", t));
    }
    if let Some(lang) = doc.root_element().value().attr("lang").or_else(|| doc.root_element().value().attr("xml:lang")).filter(|l| !l.is_empty()) {
        out.push(("Language", lang.to_string()));
    }
    for (name, label) in [("description", "Description"), ("author", "Author"), ("generator", "Generator"), ("keywords", "Keywords")] {
        let meta = first(&format!("meta[name=\"{name}\" i]")).and_then(|m| m.value().attr("content")).map(str::trim).filter(|c| !c.is_empty());
        if let Some(c) = meta {
            out.push((label, c.to_string()));
        }
    }
    let count = |css: &str| scraper::Selector::parse(css).map_or(0, |s| doc.select(&s).count());
    for (css, label) in [("a[href]", "Links"), ("img", "Images"), ("script", "Scripts"), ("form", "Forms")] {
        let n = count(css);
        if n > 0 {
            out.push((label, n.to_string()));
        }
    }
    out
}

/// The page's elements as a tree: each element with its attributes, its text, and
/// comments, at most `max` nodes. Whether the page held more.
fn elements(doc: &Html, max: u64) -> (OwnedTree, bool) {
    let mut tree = OwnedTree::new();
    let mut parents: Vec<Option<libfileparser_core::NodeId>> = vec![None];
    let mut count = 0u64;
    let mut cut = false;
    for edge in doc.tree.root().traverse() {
        match edge {
            Edge::Open(node) => {
                let parent = *parents.last().unwrap_or(&None);
                let added = match node.value() {
                    Node::Element(e) => {
                        let attrs: Vec<String> = e.attrs().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
                        Some((format!("<{}>", e.name()), attrs.join(" ")))
                    }
                    Node::Text(t) => {
                        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                        (!t.is_empty()).then(|| ("text".to_string(), t))
                    }
                    Node::Comment(c) => Some(("comment".to_string(), c.trim().to_string())),
                    Node::Doctype(d) => Some(("doctype".to_string(), d.name().to_string())),
                    _ => None,
                };
                let id = match added {
                    Some((label, value)) if count < max => {
                        count += 1;
                        Some(tree.add(parent, label, value))
                    }
                    Some(_) => {
                        cut = true;
                        None
                    }
                    None => None,
                };
                // Children go under this node; under the parent when it wasn't added.
                parents.push(id.or(parent));
            }
            Edge::Close(_) => {
                parents.pop();
            }
        }
    }
    (tree, cut)
}

impl Reader for HtmlReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// By its doctype or `<html>`, whatever its name; by name alone, no more than an
    /// extension says.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        let lead = lead(header);
        if lead.starts_with("<!doctype html") || lead.starts_with("<html") {
            Detection::new(Confidence::Content).with_language("html")
        } else if INFO.matches_extension(name) {
            Detection::new(Confidence::Extension).with_language("html")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let bytes = crate::whole(src, cx)?;
        if !libfileparser_core::is_text(src.header()) && !lead(src.header()).starts_with('<') {
            return Err(ParseError::NotThisFormat);
        }
        let text = decode(&bytes);
        let doc = Html::parse_document(&text);
        cx.cancel.check()?;
        let mut b = Builder::new(cx);
        flow(&doc, "", &mut b)?;
        let blocks = b.finish()?;
        let about = about(&doc);
        let (tree, cut) = elements(&doc, cx.limits.nodes);
        let xhtml = INFO.matches_extension(src.name()) && src.name().to_ascii_lowercase().contains(".xht");
        let mut out = Document::new(FormatInfo::new("html", if xhtml { "XHTML page" } else { "Web page" }, Category::Docs))
            .with_facet(Role::Primary, "Document", Content::Flow(Arc::new(OwnedFlow::new(blocks))))
            .with_facet(Role::Structure, "Elements", Content::Tree(Arc::new(tree)));
        for (k, v) in about {
            out = out.with_meta(k, v);
        }
        if cut {
            out = out.with_warning(Warning::new(format!("The page has more than {} elements and texts; the Elements tree shows the first.", cx.limits.nodes)));
        }
        Ok(out.with_file_facets(src, Some("html")))
    }
}

#[cfg(test)]
mod pages {
    use super::*;

    const PAGE: &str = "<!-- a comment first -->\n<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>  My   page </title><meta name=\"Description\" content=\"About it\"><script>var x = 1;</script></head>\n<body><h1 id=\"top\">Hello</h1><p>Some <em>text</em> and <a href=\"https://x.org\">a link</a>.</p><img src=\"a.png\" alt=\"pic\"></body></html>";

    fn open(name: &str, bytes: &[u8]) -> Document {
        HtmlReader.open(&FileSource::from_bytes(name, bytes.to_vec()), &ParseContext::new()).unwrap()
    }

    #[test]
    fn a_page_as_a_document_with_its_elements() {
        assert_eq!(HtmlReader.detect(PAGE.as_bytes(), "page.txt").confidence, Confidence::Content);
        assert_eq!(HtmlReader.detect(b"plain", "page.htm").confidence, Confidence::Extension);
        assert_eq!(HtmlReader.detect(b"plain", "page.txt").confidence, Confidence::None);
        let doc = open("page.html", PAGE.as_bytes());
        let names: Vec<String> = doc.facets().map(|f| f.name.clone()).collect();
        assert_eq!(names, ["Document", "Elements", "Source", "Raw"]);
        let meta = |k: &str| doc.metadata().iter().find(|(m, _)| m == k).map(|(_, v)| v.clone());
        assert_eq!((meta("Title"), meta("Language"), meta("Description"), meta("Links"), meta("Images"), meta("Scripts")), (Some("My page".into()), Some("en".into()), Some("About it".into()), Some("1".into()), Some("1".into()), Some("1".into())));
        let cx = ParseContext::new();
        let Content::Flow(f) = doc.facet(0, &cx).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..10).unwrap());
        assert!(s.contains("Heading { level: 1") && s.contains("Emphasis") && s.contains("https://x.org") && !s.contains("var x"), "{s}");
        let Content::Tree(t) = doc.facet(1, &cx).unwrap() else { panic!() };
        // Every node, depth first, as `label value`.
        fn walk(t: &dyn libfileparser_core::TreeSource, parent: Option<libfileparser_core::NodeId>, out: &mut Vec<String>) {
            for n in t.children(parent, 0..1000).unwrap() {
                out.push(format!("{} {}", n.label, n.value));
                walk(t, Some(n.id), out);
            }
        }
        let mut all = Vec::new();
        walk(t.as_ref(), None, &mut all);
        for want in ["comment a comment first", "doctype html", "<h1> id=\"top\"", "text Hello", "<a> href=\"https://x.org\""] {
            assert!(all.iter().any(|n| n.starts_with(want)), "{want} in {all:?}");
        }
    }

    #[test]
    fn encodings_it_declares() {
        let latin = [&b"<html><head><meta charset=\"iso-8859-1\"></head><body><p>caf"[..], &[0xe9], b"</p></body></html>"].concat();
        assert!(decode(&latin).contains("café"));
        let old = [&b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1251\"></head><body>"[..], &[0xcf, 0xf0, 0xe8], b"</body></html>"].concat();
        assert!(decode(&old).contains("При"));
        let utf16: Vec<u8> = [0xff, 0xfe].into_iter().chain("<p>hi</p>".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(decode(&utf16), "<p>hi</p>");
        assert_eq!(decode("<p>ünï</p>".as_bytes()), "<p>ünï</p>");
    }

    #[test]
    fn a_big_page_cut_and_said() {
        // Small as prose, big as elements: comments are the tree's, not the document's.
        let big = format!("<html><body><p>x</p>{}</body></html>", "<!-- c -->".repeat(50));
        let mut limits = libfileparser_core::Limits::desktop();
        limits.nodes = 20;
        let doc = HtmlReader.open(&FileSource::from_bytes("big.html", big.into_bytes()), &ParseContext::new().with_limits(limits)).unwrap();
        assert!(doc.warnings().iter().any(|w| w.message.contains("more than 20")));
        assert_eq!(open("x.xhtml", b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><body/></html>").format().name, "XHTML page");
        assert!(matches!(HtmlReader.open(&FileSource::from_bytes("b.html", vec![0, 1, 2, 0xff]), &ParseContext::new()), Err(ParseError::NotThisFormat)));
    }
}
