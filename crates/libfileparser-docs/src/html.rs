//! HTML and XHTML into formatted text, parsed by html5ever (Servo's HTML parser,
//! through `scraper`): what EPUB's chapters are written in, and later HTML's prose
//! reading. Only the structure is kept (headings, paragraphs, lists, tables, quotes,
//! code, images, links, emphasis); styles, scripts and forms aren't, and nothing is
//! run or fetched.

use ego_tree::iter::Edge;
use libfileparser_core::{Block, Result};
use scraper::{ElementRef, Html, Node};

use crate::build::{Builder, Open};
use crate::package::resolve;

/// Parses `html` (a part named `base`, against which images resolve) into `b`.
pub(crate) fn parse(html: &str, base: &str, b: &mut Builder) -> Result<()> {
    let doc = Html::parse_document(html);
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
