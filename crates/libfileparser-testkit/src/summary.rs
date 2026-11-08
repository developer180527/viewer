//! A document written out as plain text: its format, every facet's content in brief,
//! metadata and dependencies. Stable from run to run, so tests can compare two opens,
//! and readable in a failure message.

use std::fmt::Write;

use libfileparser_core::{Content, Document, NodeId, ParseContext, TreeSource};

/// Most tree nodes, table rows, text lines or entries written per facet.
const SHOWN: usize = 40;

/// Makes every facet of `doc` and writes them out.
pub fn summarize(doc: &Document, cx: &ParseContext) -> String {
    let mut out = String::new();
    let f = doc.format();
    let _ = writeln!(out, "format: {} ({}, {:?})", f.name, f.reader, f.category);
    let facets: Vec<_> = doc.facets().cloned().collect();
    for (i, info) in facets.iter().enumerate() {
        let _ = writeln!(out, "facet {i}: {:?} \"{}\" {:?}", info.role, info.name, info.model);
        match doc.facet(i, cx) {
            Ok(content) => content_summary(&content, &mut out),
            Err(e) => {
                let _ = writeln!(out, "  error: {e}");
            }
        }
    }
    for (k, v) in doc.metadata() {
        let _ = writeln!(out, "meta: {k} = {v}");
    }
    for d in doc.dependencies().list() {
        let _ = writeln!(out, "dependency: {} {:?}", d.reference, d.status);
    }
    out
}

/// One content, in brief.
pub fn content_summary(content: &Content, out: &mut String) {
    match content {
        Content::Raster(r) => {
            let _ = match r.decode() {
                Ok(rgba) => writeln!(out, "  raster {}x{} fnv {:016x}", r.width(), r.height(), fnv(&rgba)),
                Err(e) => writeln!(out, "  raster {}x{} not decoded: {e}", r.width(), r.height()),
            };
        }
        Content::Text(t) => {
            let info = t.info();
            let _ = writeln!(out, "  text: {} lines, language {:?}", info.lines, info.language);
            match t.lines(0..SHOWN as u64) {
                Ok(lines) => lines.iter().for_each(|l| {
                    let _ = writeln!(out, "  | {}", String::from_utf8_lossy(l));
                }),
                Err(e) => {
                    let _ = writeln!(out, "  lines failed: {e}");
                }
            }
        }
        Content::Tree(t) => {
            let _ = writeln!(out, "  tree:");
            let mut shown = 0;
            tree(t.as_ref(), None, 1, &mut shown, out);
        }
        Content::Table(t) => {
            let info = t.info();
            let _ = writeln!(out, "  table: {} rows; {}", info.rows, info.names().join(" | "));
            match t.rows(0..SHOWN as u64) {
                Ok(rows) => rows.iter().for_each(|row| {
                    let _ = writeln!(out, "  | {}", row.iter().map(ToString::to_string).collect::<Vec<_>>().join(" | "));
                }),
                Err(e) => {
                    let _ = writeln!(out, "  rows failed: {e}");
                }
            }
        }
        Content::Flow(f) => {
            let _ = writeln!(out, "  flow: {} blocks", f.block_count());
            match f.blocks(0..SHOWN as u64) {
                Ok(blocks) => blocks.iter().for_each(|b| {
                    let s = format!("{b:?}");
                    let _ = writeln!(out, "  | {}", s.chars().take(160).collect::<String>());
                }),
                Err(e) => {
                    let _ = writeln!(out, "  blocks failed: {e}");
                }
            }
        }
        Content::Pages(p) => {
            let _ = writeln!(out, "  pages: {} by {}", p.page_count(), p.engine());
            for i in 0..p.page_count().min(SHOWN as u64) {
                match p.page_size(i) {
                    Ok(s) => {
                        let _ = writeln!(out, "  - page {} {}x{}", i + 1, s.width, s.height);
                    }
                    Err(e) => {
                        let _ = writeln!(out, "  - page {} failed: {e}", i + 1);
                    }
                }
            }
        }
        Content::Container(c) => {
            let _ = writeln!(out, "  container: {} entries", c.entry_count());
            match c.entries(0..SHOWN as u64) {
                Ok(entries) => entries.iter().for_each(|e| {
                    let _ = writeln!(out, "  - {} {:?} {} bytes", e.path, e.kind, e.size);
                }),
                Err(e) => {
                    let _ = writeln!(out, "  entries failed: {e}");
                }
            }
        }
        Content::Scene(s) => {
            let read = s.scene().read();
            let meshes = read.meshes();
            let vertices: usize = meshes.iter().map(|m| m.info.vertices).sum();
            let elements: usize = meshes.iter().map(|m| m.info.elements).sum();
            let decoded = read.textures().iter().filter(|t| t.info.is_some()).count();
            let _ = writeln!(
                out,
                "  scene: {} meshes, {vertices} vertices, {elements} elements, {} materials, {} nodes, {decoded}/{} textures decoded, {:?}",
                meshes.len(),
                read.materials().len(),
                read.nodes().len(),
                read.textures().len(),
                read.geometry(),
            );
            if let Some(b) = read.bounds() {
                let v = |v: scene_data::glam::Vec3| format!("{:.3} {:.3} {:.3}", v.x, v.y, v.z).replace("-0.000", "0.000");
                let _ = writeln!(out, "  bounds: {} .. {}", v(b.min), v(b.max));
            }
        }
        Content::Hex(src) => {
            let _ = writeln!(out, "  hex: {} bytes", src.len());
        }
        _ => {
            let _ = writeln!(out, "  (a content model this summary doesn't know)");
        }
    }
}

fn tree(t: &dyn TreeSource, parent: Option<NodeId>, depth: usize, shown: &mut usize, out: &mut String) {
    let children = match t.children(parent, 0..(SHOWN - *shown) as u64) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(out, "  {}children failed: {e}", "  ".repeat(depth - 1));
            return;
        }
    };
    for n in children {
        if *shown >= SHOWN {
            return;
        }
        *shown += 1;
        let _ = writeln!(out, "  {}{} = {}", "  ".repeat(depth - 1), n.label, n.value);
        if n.has_children {
            tree(t, Some(n.id), depth + 1, shown, out);
        }
    }
}

/// FNV-1a: a stable checksum for pixels, so a summary notices a changed image.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}
