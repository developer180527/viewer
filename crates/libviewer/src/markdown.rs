//! Formatted text (`Content::Flow`), drawn with libgui's Markdown widget: the blocks
//! are written out as Markdown, which the widget lays out (headings, lists, tables,
//! code, quotes, emphasis, links). Images show their alt text for now.

use libfileparser_core::{Block, FlowSource, Inline};
use libgui::*;

/// A document's formatted text, prepared once for its view.
pub struct FlowView {
    md: libgui_markdown::Markdown,
}

/// More blocks than this show as the first part, with a note: the widget lays out the
/// whole document every frame.
const MAX_BLOCKS: u64 = 20_000;

impl FlowView {
    pub fn new(src: &dyn FlowSource) -> Self {
        let total = src.block_count().known();
        let (blocks, error) = match src.blocks(0..total.min(MAX_BLOCKS)) {
            Ok(b) => (b, None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
        let mut text = String::new();
        for b in &blocks {
            block(b, "", &mut text);
        }
        if let Some(e) = error {
            text.push_str(&format!("*This document couldn't be read: {e}*\n"));
        } else if total > MAX_BLOCKS {
            text.push_str(&format!("\n---\n\n*Shown up to block {MAX_BLOCKS} of {total}.*\n"));
        }
        Self { md: libgui_markdown::Markdown::parse(&text) }
    }
}

/// Draws formatted text, in a scroll.
pub fn flow(ui: &mut Ui, v: &mut FlowView) {
    ui.scroll_area("flow", |ui| {
        ui.container(Layout::column().height(Size::Fit).padding(Insets::xy(24.0, 16.0)), Frame::none(), |ui| {
            v.md.show(ui, "flow");
        });
    });
}

/// `b` as Markdown, each line after the first starting with `indent`.
fn block(b: &Block, indent: &str, out: &mut String) {
    let line = |out: &mut String, s: &str| {
        out.push_str(indent);
        out.push_str(s);
        out.push('\n');
    };
    match b {
        Block::Heading { level, text } => line(out, &format!("{} {}", "#".repeat(*level as usize), inlines(text))),
        Block::Paragraph(text) => line(out, &inlines(text)),
        Block::List { ordered, start, items } => {
            for (n, item) in items.iter().enumerate() {
                let marker = if *ordered { format!("{}. ", start + n as u64) } else { "- ".to_string() };
                let inner = format!("{indent}{}", " ".repeat(marker.len()));
                let mut body = String::new();
                for (k, ib) in item.iter().enumerate() {
                    block(ib, if k == 0 { "" } else { &inner }, &mut body);
                }
                out.push_str(indent);
                out.push_str(&marker);
                out.push_str(body.trim());
                out.push('\n');
            }
        }
        Block::Quote(bs) => {
            let mut inner = String::new();
            for q in bs {
                block(q, "", &mut inner);
            }
            for l in inner.lines() {
                line(out, &format!("> {l}"));
            }
        }
        Block::Code { language, text } => {
            line(out, &format!("```{}", language.as_deref().unwrap_or("")));
            for l in text.lines() {
                line(out, l);
            }
            line(out, "```");
        }
        Block::Table { header_rows, rows } => {
            let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
            let row = |cells: &[Vec<Inline>]| {
                let mut r = String::from("|");
                for c in 0..cols {
                    r.push(' ');
                    r.push_str(&cells.get(c).map(|x| cell(&inlines(x))).unwrap_or_default());
                    r.push_str(" |");
                }
                r
            };
            // Markdown tables need a header row: the first row, or an empty one.
            let body = if *header_rows > 0 {
                line(out, &row(&rows[0]));
                &rows[1..]
            } else {
                line(out, &row(&[]));
                &rows[..]
            };
            line(out, &format!("|{}", " --- |".repeat(cols)));
            for r in body {
                line(out, &row(r));
            }
        }
        Block::Image { alt, .. } => line(out, &format!("*[image: {}]*", if alt.is_empty() { "untitled" } else { alt })),
        Block::Rule | Block::Break => line(out, "---"),
        Block::Raw { text, .. } => {
            line(out, "```");
            for l in text.lines() {
                line(out, l);
            }
            line(out, "```");
        }
        _ => {}
    }
    out.push('\n');
}

/// Inline Markdown made safe for a table cell: on one line, every `|` escaped once
/// (text's are already; code's and links' aren't).
fn cell(md: &str) -> String {
    let mut s = String::with_capacity(md.len());
    let mut escaped = false;
    for c in md.chars() {
        match c {
            '\n' => s.push(' '),
            '|' if !escaped => s.push_str("\\|"),
            _ => s.push(c),
        }
        escaped = c == '\\' && !escaped;
    }
    s
}

fn inlines(i: &[Inline]) -> String {
    let mut s = String::new();
    for x in i {
        match x {
            Inline::Text(t) => s.push_str(&escape(t)),
            Inline::Emphasis(c) => s.push_str(&format!("*{}*", inlines(c))),
            Inline::Strong(c) => s.push_str(&format!("**{}**", inlines(c))),
            Inline::Strikethrough(c) => s.push_str(&format!("~~{}~~", inlines(c))),
            // No Markdown for these: the text alone.
            Inline::Underline(c) | Inline::Superscript(c) | Inline::Subscript(c) => s.push_str(&inlines(c)),
            Inline::Code(c) => s.push_str(&format!("`{}`", c.replace('`', "'"))),
            Inline::Link { target, text } => s.push_str(&format!("[{}]({})", inlines(text), target.replace(' ', "%20").replace(')', "%29"))),
            Inline::Image { alt, .. } => s.push_str(&format!("*[image: {}]*", escape(alt))),
            Inline::LineBreak => s.push_str("  \n"),
            Inline::Note(l) => s.push_str(&format!("[{}]", escape(l))),
            _ => {}
        }
    }
    s
}

/// Text with Markdown's own characters escaped, so a document's `*` stays a `*`.
fn escape(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    for c in t.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '#' | '<' | '>' | '|' | '~') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_become_markdown() {
        let t = |s: &str| vec![Inline::Text(s.into())];
        let mut out = String::new();
        for b in [
            Block::Heading { level: 2, text: t("Hi *there*") },
            Block::List { ordered: true, start: 3, items: vec![vec![Block::Paragraph(t("a"))], vec![Block::Paragraph(t("b"))]] },
            Block::Table { header_rows: 1, rows: vec![vec![t("x"), t("y")], vec![t("1"), t("2|3")]] },
        ] {
            block(&b, "", &mut out);
        }
        assert!(out.contains("## Hi \\*there\\*"), "{out}");
        assert!(out.contains("3. a\n4. b"), "{out}");
        assert!(out.contains("| x | y |\n| --- | --- |\n| 1 | 2\\|3 |"), "{out}");
    }
}
