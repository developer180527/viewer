//! Documents as outlines, for checking them against a reference renderer
//! (corpus/reference.py): every document under `LFP_OUTLINE_IN` (usually
//! `corpus/real/docs` or `corpus/files/docs`) opened through the front door, its first
//! formatted-text facet written as `LFP_OUTLINE_OUT/<path>.json`: what it opened as,
//! and its blocks reduced to what any renderer agrees on, which is headings and their
//! levels, paragraphs, list items, code, tables' cells and images.
//!
//! ```text
//! LFP_OUTLINE_IN=$PWD/corpus/real/docs LFP_OUTLINE_OUT=$PWD/target/reference/ours/real \
//!     cargo test --release -p libfileparser --features docs-mupdf --test outline -- --ignored --nocapture
//! ```
//!
//! A document that opened as pages (PDF, XPS, DjVu) is written with no blocks: its
//! engine is the reference.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use libfileparser::{Block, Content, FileSource, FolderFiles, Inline, Model, ParseContext, Role};
use serde_json::{Value, json};

const LIMIT: Duration = Duration::from_secs(60);

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if name.starts_with('.') || name.ends_with("-invalid") {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Text with its whitespace collapsed: renderers break lines differently.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn inline(i: &[Inline]) -> String {
    squash(&Inline::plain(i))
}

/// Blocks flattened: a list's items and a quote's blocks are blocks of their own, an
/// item's first paragraph marked as the item.
fn flatten(blocks: &[Block], out: &mut Vec<Value>) {
    for b in blocks {
        match b {
            Block::Heading { level, text } => out.push(json!(["h", level, inline(text)])),
            Block::Paragraph(t) => {
                let t = inline(t);
                if !t.is_empty() {
                    out.push(json!(["p", t]));
                }
            }
            Block::List { items, .. } => {
                for item in items {
                    let mut inner = Vec::new();
                    flatten(item, &mut inner);
                    // The item's own text, then whatever it holds after it.
                    match inner.first().and_then(|v| v.as_array()).filter(|a| a[0] == "p") {
                        Some(first) => {
                            out.push(json!(["li", first[1]]));
                            out.extend(inner.into_iter().skip(1));
                        }
                        None => {
                            out.push(json!(["li", ""]));
                            out.extend(inner);
                        }
                    }
                }
            }
            Block::Quote(inner) => flatten(inner, out),
            Block::Code { text, .. } => out.push(json!(["code", squash(text)])),
            Block::Table { rows, .. } => {
                let rows: Vec<Vec<String>> = rows.iter().map(|r| r.iter().map(|c| inline(c)).collect()).collect();
                out.push(json!(["table", rows]));
            }
            Block::Image { alt, .. } => out.push(json!(["img", squash(alt)])),
            Block::Raw { text, .. } => out.push(json!(["raw", squash(text)])),
            Block::Rule | Block::Break => {}
            _ => {}
        }
    }
}

fn outline(path: &Path) -> Value {
    // Its folder readable, as when a user grants it: includes and linked files resolve.
    let cx = ParseContext::new().with_files(FolderFiles::within(path.parent().unwrap_or(Path::new("."))));
    let src = match FileSource::open(path) {
        Ok(s) => s,
        Err(e) => return json!({ "error": e.to_string() }),
    };
    let doc = match libfileparser::open(&src, &cx) {
        Ok(d) => d,
        Err(e) => return json!({ "error": e.to_string() }),
    };
    let reader = doc.format().reader.to_string();
    let facets: Vec<_> = doc.facets().cloned().collect();
    // The first formatted text among what the document is for; else its pages; else
    // its first text.
    let pick = |model: Model| facets.iter().position(|f| f.model == model && matches!(f.role, Role::Primary));
    let Some(i) = pick(Model::Flow).or_else(|| pick(Model::Pages)).or_else(|| pick(Model::Text)) else {
        return json!({ "reader": reader, "model": "other", "blocks": [] });
    };
    let mut blocks = Vec::new();
    let model = match doc.facet(i, &cx) {
        Ok(Content::Flow(f)) => {
            let mut at = 0;
            loop {
                match f.blocks(at..at + 512) {
                    Ok(b) if !b.is_empty() => {
                        at += b.len() as u64;
                        flatten(&b, &mut blocks);
                    }
                    Ok(_) => break,
                    Err(e) => return json!({ "reader": reader, "error": e.to_string() }),
                }
            }
            "flow"
        }
        Ok(Content::Pages(_)) => "pages",
        Ok(Content::Text(t)) => {
            // Plain text as paragraphs: runs of lines between blank ones.
            let lines = t.lines(0..t.info().lines.known()).unwrap_or_default();
            let mut para = String::new();
            for l in lines.iter().map(|l| String::from_utf8_lossy(l).into_owned()).chain([String::new()]) {
                if l.trim().is_empty() {
                    if !para.is_empty() {
                        blocks.push(json!(["p", squash(&para)]));
                        para.clear();
                    }
                } else {
                    para.push(' ');
                    para.push_str(&l);
                }
            }
            "text"
        }
        Ok(_) => "other",
        Err(e) => return json!({ "reader": reader, "error": e.to_string() }),
    };
    json!({ "reader": reader, "model": model, "facet": facets[i].name, "blocks": blocks })
}

#[test]
#[ignore = "needs LFP_OUTLINE_IN and LFP_OUTLINE_OUT"]
fn outlines() {
    let (Some(input), Some(output)) = (std::env::var_os("LFP_OUTLINE_IN").map(PathBuf::from), std::env::var_os("LFP_OUTLINE_OUT").map(PathBuf::from)) else {
        eprintln!("LFP_OUTLINE_IN or LFP_OUTLINE_OUT isn't set; nothing to write");
        return;
    };
    let mut files = Vec::new();
    walk(&input, &mut files);
    let mut written = 0;
    for path in files {
        let rel = path.strip_prefix(&input).unwrap().to_path_buf();
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        std::thread::Builder::new().stack_size(8 << 20).spawn(move || {
            let v = std::panic::catch_unwind(|| outline(&p)).unwrap_or_else(|_| json!({ "error": "panicked" }));
            let _ = tx.send(v);
        }).unwrap();
        let v = rx.recv_timeout(LIMIT).unwrap_or_else(|_| json!({ "error": "took too long" }));
        let out = output.join(format!("{}.json", rel.display()));
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::write(&out, serde_json::to_string(&v).unwrap()).unwrap();
        written += 1;
    }
    println!("wrote {written} outlines to {}", output.display());
}
