//! The **Blocks** facet (datablocks by type, then the other blocks by kind) and the
//! **Packed files** (files packed into the `.blend`, each as a file of its own), from
//! `blend-lite`'s reading of the file.

use std::ops::Range;

use blend_lite::{Block, File, text};
use libfileparser_core::OwnedTree;

/// The Blocks facet: each kind of datablock (Object, Mesh, Material…) with its
/// datablocks by name, then the other blocks by code and struct, with counts.
pub fn tree(file: &File<'_>) -> OwnedTree {
    let mut tree = OwnedTree::new();
    let dna = file.dna.as_ref();
    let struct_name = |bl: &Block| dna.and_then(|d| d.struct_name(bl.sdna)).unwrap_or("").to_string();
    // Datablocks, by struct, in the order first met.
    let mut kinds: Vec<(String, Vec<(String, usize)>)> = Vec::new();
    let mut other: Vec<((String, String), (u64, u64))> = Vec::new();
    for bl in &file.blocks {
        let code = String::from_utf8_lossy(&bl.code).trim_end_matches('\0').to_string();
        let is_id = dna.is_some_and(|d| d.is_id(bl.sdna)) && code != "DATA";
        if is_id {
            let kind = struct_name(bl);
            let name = file.id_name(bl).unwrap_or_default();
            match kinds.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, list)) => list.push((name, bl.data.len())),
                None => kinds.push((kind, vec![(name, bl.data.len())])),
            }
        } else {
            // Struct 0 is also what Blender writes for blocks that aren't structs
            // (REND, TEST, DNA1) and for raw arrays.
            let st = match (dna.is_some(), bl.sdna, code.as_str()) {
                (false, ..) => String::new(),
                (true, 0, "DATA") => "raw data".to_string(),
                (true, 0, _) => String::new(),
                _ => struct_name(bl),
            };
            let key = (code, st);
            match other.iter_mut().find(|(k, _)| *k == key) {
                Some((_, (n, bytes))) => {
                    *n += 1;
                    *bytes += bl.data.len() as u64;
                }
                None => other.push((key, (1, bl.data.len() as u64))),
            }
        }
    }
    for (kind, list) in kinds {
        let node = tree.add(None, kind_name(&kind), count(list.len() as u64, "datablock"));
        for (name, size) in list {
            tree.add(Some(node), name, bytes(size as u64));
        }
    }
    if !other.is_empty() {
        let total: u64 = other.iter().map(|(_, (n, _))| n).sum();
        let node = tree.add(None, "Other blocks", count(total, "block"));
        for ((code, st), (n, size)) in other {
            let label = if st.is_empty() { code } else { format!("{code} · {st}") };
            tree.add(Some(node), label, format!("{} · {}", count(n, "block"), bytes(size)));
        }
    }
    tree
}

/// The files packed into the `.blend`: each `PackedFile`'s bytes, named after the
/// file path recorded beside it (an image's, a font's, a sound's).
pub fn packed(file: &File<'_>) -> Vec<(String, Range<usize>)> {
    let Some(dna) = &file.dna else { return Vec::new() };
        let mut out = Vec::new();
    let mut used = std::collections::HashSet::new();
    for (i, bl) in file.blocks.iter().enumerate() {
        if dna.struct_name(bl.sdna) != Some("PackedFile") || &bl.code != b"DATA" {
            continue;
        }
        let data = file.data(bl);
        let (Some(size), Some(ptr)) = (dna.field(bl.sdna, "size"), dna.field(bl.sdna, "data")) else { continue };
        let Some(len) = file.header.u32_at(data, size.offset) else { continue };
        let Some(target) = file.pointer(data, &ptr).and_then(|p| file.block_at(i, p)) else { continue };
        let len = (len as usize).min(target.data.len());
        let name = owner_path(file, i).unwrap_or_else(|| format!("packed {}", out.len()));
        let mut path = format!("packed/{name}");
        if !used.insert(path.clone()) {
            path = format!("packed/{} ({name})", out.len());
            used.insert(path.clone());
        }
        out.push((path, target.data.start..target.data.start + len));
    }
    out
}

/// The file name recorded for the packed file at block `i`: the nearest block before
/// it with a `filepath` (an `ImagePackedFile`, or the datablock itself), stopping at
/// the datablock that owns it.
fn owner_path(file: &File<'_>, i: usize) -> Option<String> {
    let dna = file.dna.as_ref()?;
    for bl in file.blocks[..i].iter().rev() {
        let id = dna.is_id(bl.sdna) && &bl.code != b"DATA";
        let field = dna.field(bl.sdna, "filepath").or_else(|| if id { dna.field(bl.sdna, "name") } else { None });
        if let Some(path) = field.filter(|f| !f.pointer).and_then(|f| text(file.data(bl), &f)).filter(|p| !p.is_empty()) {
            let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
        if id {
            return file.id_name(bl).filter(|n| !n.is_empty());
        }
    }
    None
}

/// A datablock struct as Blender's interface names it (`Lamp` is a Light).
fn kind_name(st: &str) -> String {
    match st {
        "Lamp" => "Light",
        "bScreen" => "Screen",
        "wmWindowManager" => "Window manager",
        "bNodeTree" => "Node tree",
        "bAction" => "Action",
        "bArmature" => "Armature",
        "bSound" => "Sound",
        "bGPdata" => "Grease pencil",
        "Tex" => "Texture",
        "VFont" => "Font",
        "FreestyleLineStyle" => "Line style",
        other => other,
    }
    .to_string()
}

fn count(n: u64, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

fn bytes(n: u64) -> String {
    match n {
        0..1024 => format!("{n} bytes"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

