//! Prints a document's formatted text as plain text, roughly as it reads, for
//! checking a reader on real files: `cargo run -p libfileparser-docs --example flow -- FILE`.

use libfileparser_core::{Block, Content, FileSource, Inline, ParseContext};

fn inlines(v: &[Inline]) -> String {
    v.iter()
        .map(|i| match i {
            Inline::Text(t) => t.clone(),
            Inline::Emphasis(v) => format!("_{}_", inlines(v)),
            Inline::Strong(v) => format!("*{}*", inlines(v)),
            Inline::Code(c) => format!("`{c}`"),
            Inline::Link { target, text } => format!("[{}]({target})", inlines(text)),
            Inline::Image { reference, .. } => format!("![]({reference})"),
            Inline::LineBreak => "\n".into(),
            Inline::Note(n) => format!("[^{n}]"),
            Inline::Underline(v) | Inline::Strikethrough(v) | Inline::Superscript(v) | Inline::Subscript(v) => inlines(v),
            _ => String::new(),
        })
        .collect()
}

fn blocks(v: &[Block], pad: &str) {
    for b in v {
        match b {
            Block::Heading { level, text } => println!("\n{pad}{} {}", "#".repeat(*level as usize), inlines(text)),
            Block::Paragraph(t) => println!("{pad}{}", inlines(t)),
            Block::List { ordered, items, .. } => {
                for (n, item) in items.iter().enumerate() {
                    println!("{pad}{}", if *ordered { format!("{}.", n + 1) } else { "-".into() });
                    blocks(item, &format!("{pad}   "));
                }
            }
            Block::Quote(v) => blocks(v, &format!("{pad}> ")),
            Block::Code { language, text } => println!("{pad}```{}\n{pad}{}\n{pad}```", language.as_deref().unwrap_or(""), text.replace('\n', &format!("\n{pad}"))),
            Block::Table { rows, .. } => rows.iter().for_each(|r| println!("{pad}| {} |", r.iter().map(|c| inlines(c)).collect::<Vec<_>>().join(" | "))),
            Block::Image { reference, .. } => println!("{pad}![]({reference})"),
            Block::Rule => println!("{pad}---"),
            _ => {}
        }
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("a file");
    let bytes = std::fs::read(&path).expect("readable");
    let src = FileSource::from_bytes(&path, bytes);
    let cx = ParseContext::new();
    let doc = libfileparser_docs::open(&src, &cx).expect("opens");
    for (k, v) in doc.metadata() {
        println!("{k}: {v}");
    }
    for w in doc.warnings() {
        println!("warning: {}", w.message);
    }
    match doc.facet(0, &cx) {
        Ok(Content::Flow(f)) => blocks(&f.blocks(0..u64::MAX).unwrap_or_default(), ""),
        // Pages: the first, written as a PPM to the path given second.
        Ok(Content::Pages(p)) => {
            let Some(out) = std::env::args().nth(2) else { return println!("{} pages", p.page_count()) };
            let size = p.page_size(0).expect("a first page");
            let scale = 800.0 / size.width.max(1.0);
            let mut rgba = Vec::new();
            let (w, h) = p.render(0, scale, libfileparser_core::PageRect::whole(size), &mut rgba, &cx.cancel).expect("renders");
            let mut ppm = format!("P6 {w} {h} 255\n").into_bytes();
            ppm.extend(rgba.chunks_exact(4).flat_map(|c| [c[0], c[1], c[2]]));
            std::fs::write(&out, ppm).expect("writable");
            println!("{} pages; page 1 is {w} × {h} px in {out}", p.page_count());
        }
        _ => {}
    }
}
