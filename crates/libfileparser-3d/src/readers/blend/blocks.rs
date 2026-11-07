//! A `.blend`'s blocks, named through the file's own SDNA (the catalogue of every
//! struct Blender wrote, which every file carries in its `DNA1` block): the **Blocks**
//! facet (datablocks by type, then the other blocks by kind) and the **Packed files**
//! (files packed into the `.blend`, each as a file of its own).
//!
//! Nothing here depends on a Blender version: field offsets come from the SDNA, so a
//! file from 2.4 and one from 5.0 are read alike. Every offset is checked.

use std::collections::HashMap;
use std::ops::Range;

use anyhow::{Context, Result, bail};
use libfileparser_core::{OwnedTree, ParseError};

use super::Header;

/// One block: its code, where its data is, the address it had in Blender's memory
/// (pointers in other blocks refer to it by that) and its SDNA struct.
pub struct Block {
    pub code: [u8; 4],
    pub data: Range<usize>,
    pub old: u64,
    pub sdna: usize,
}

/// Every block up to `ENDB`, at most `max` of them.
pub fn blocks(b: &[u8], h: &Header, max: u64) -> Result<Vec<Block>> {
    let header = if h.large_blocks { 32 } else { 16 + h.pointer };
    let mut out = Vec::new();
    let mut at = h.len;
    while at + header <= b.len() {
        let code: [u8; 4] = b[at..at + 4].try_into().unwrap();
        if &code == b"ENDB" {
            break;
        }
        let (len, old, sdna) = if h.large_blocks {
            // code(4) sdna(4) old(8) len(8) nr(8)
            (h.u64_at(b, at + 16).context("truncated")?, h.u64_at(b, at + 8).context("truncated")?, h.u32_at(b, at + 4).context("truncated")?)
        } else {
            // code(4) len(4) old(pointer) sdna(4) nr(4)
            let p = h.pointer;
            let old = if p == 8 { h.u64_at(b, at + 8) } else { h.u32_at(b, at + 8).map(u64::from) };
            (h.u32_at(b, at + 4).context("truncated")? as u64, old.context("truncated")?, h.u32_at(b, at + 8 + p).context("truncated")?)
        };
        let start = at + header;
        let end = usize::try_from(len).ok().and_then(|l| start.checked_add(l)).filter(|&e| e <= b.len()).context("a block runs past the end of the file")?;
        if out.len() as u64 >= max {
            bail!(ParseError::too_large(libfileparser_core::Limit::Items, max + 1, max));
        }
        out.push(Block { code, data: start..end, old, sdna: sdna as usize });
        at = end;
    }
    Ok(out)
}

/// The SDNA: names, types with their sizes, and structs as (type, name) fields.
pub struct Dna {
    names: Vec<String>,
    types: Vec<String>,
    sizes: Vec<u16>,
    /// Per struct: its type, then its fields as (type, name) indices.
    structs: Vec<(usize, Vec<(usize, usize)>)>,
    pointer: usize,
}

/// A field found in a struct: where it is, how big, and whether it's a pointer.
pub struct Field {
    pub offset: usize,
    pub size: usize,
    pub pointer: bool,
}

impl Dna {
    pub fn parse(d: &[u8], h: &Header) -> Result<Self> {
        let mut at = 0usize;
        let tag = |at: &mut usize, t: &[u8]| -> Result<()> {
            *at = at.next_multiple_of(4);
            if d.get(*at..*at + 4) != Some(t) {
                bail!("SDNA: no {} section", String::from_utf8_lossy(t));
            }
            *at += 4;
            Ok(())
        };
        let u32_at = |at: usize| h.u32_at(d, at).map(|v| v as usize).context("SDNA truncated");
        let u16_at = |at: usize| -> Result<usize> {
            let x: [u8; 2] = d.get(at..at + 2).context("SDNA truncated")?.try_into().unwrap();
            Ok(if h.little { u16::from_le_bytes(x) } else { u16::from_be_bytes(x) } as usize)
        };
        let strings = |at: &mut usize| -> Result<Vec<String>> {
            let n = u32_at(*at)?;
            *at += 4;
            if n > d.len() {
                bail!("SDNA: {n} names in {} bytes", d.len());
            }
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let len = d.get(*at..).and_then(|r| r.iter().position(|&c| c == 0)).context("SDNA truncated")?;
                out.push(String::from_utf8_lossy(&d[*at..*at + len]).into_owned());
                *at += len + 1;
            }
            Ok(out)
        };
        if d.get(..4) != Some(b"SDNA") {
            bail!("no SDNA");
        }
        at += 4;
        tag(&mut at, b"NAME")?;
        let names = strings(&mut at)?;
        tag(&mut at, b"TYPE")?;
        let types = strings(&mut at)?;
        tag(&mut at, b"TLEN")?;
        let sizes = (0..types.len()).map(|i| u16_at(at + i * 2).map(|v| v as u16)).collect::<Result<Vec<_>>>()?;
        at += types.len() * 2;
        tag(&mut at, b"STRC")?;
        let n = u32_at(at)?;
        at += 4;
        if n > d.len() / 4 {
            bail!("SDNA: {n} structs in {} bytes", d.len());
        }
        let mut structs = Vec::with_capacity(n);
        for _ in 0..n {
            let ty = u16_at(at)?;
            let count = u16_at(at + 2)?;
            at += 4;
            let mut fields = Vec::with_capacity(count);
            for _ in 0..count {
                let (t, name) = (u16_at(at)?, u16_at(at + 2)?);
                if t >= types.len() || name >= names.len() {
                    bail!("SDNA: a field names type {t} or name {name}, which don't exist");
                }
                fields.push((t, name));
                at += 4;
            }
            if ty >= types.len() {
                bail!("SDNA: struct of type {ty}, which doesn't exist");
            }
            structs.push((ty, fields));
        }
        Ok(Self { names, types, sizes, structs, pointer: h.pointer })
    }

    /// The type name of struct `index`.
    pub fn struct_name(&self, index: usize) -> Option<&str> {
        self.structs.get(index).map(|(t, _)| self.types[*t].as_str())
    }

    fn struct_of_type(&self, ty: usize) -> Option<usize> {
        self.structs.iter().position(|(t, _)| *t == ty)
    }

    /// Whether struct `index` is a datablock: its first field is an `ID`.
    pub fn is_id(&self, index: usize) -> bool {
        self.structs.get(index).and_then(|(_, f)| f.first()).is_some_and(|(t, _)| self.types[*t] == "ID")
    }

    /// The field called `name` (without `*` or `[n]`) in struct `index`.
    pub fn field(&self, index: usize, name: &str) -> Option<Field> {
        let (_, fields) = self.structs.get(index)?;
        let mut offset = 0usize;
        for &(ty, n) in fields {
            let raw = &self.names[n];
            let pointer = raw.starts_with('*') || raw.starts_with("(*");
            let bare: String = raw.trim_start_matches(['(', '*']).chars().take_while(|c| *c != '[' && *c != ')').collect();
            // Array dimensions: `name[66]`, `mat[4][4]`.
            let dims: usize = raw.split('[').skip(1).filter_map(|d| d.split(']').next()?.parse::<usize>().ok()).product();
            let one = if pointer { self.pointer } else { self.sizes[ty] as usize };
            let size = one.checked_mul(dims.max(1))?;
            if bare == name {
                return Some(Field { offset, size, pointer });
            }
            offset = offset.checked_add(size)?;
        }
        None
    }

    /// The `ID` field's `name` within a datablock: the ID is its first field.
    fn id_name(&self) -> Option<Field> {
        let id = self.struct_of_type(self.types.iter().position(|t| t == "ID")?)?;
        self.field(id, "name")
    }
}

/// A C string in `data` at `field`, cut at the first NUL.
fn text(data: &[u8], field: &Field) -> Option<String> {
    let bytes = data.get(field.offset..field.offset.checked_add(field.size)?)?;
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// What the Blocks and Packed files facets are made from.
pub struct BlendFile<'a> {
    b: &'a [u8],
    h: &'a Header,
    pub blocks: Vec<Block>,
    pub dna: Option<Dna>,
}

impl<'a> BlendFile<'a> {
    pub fn read(b: &'a [u8], h: &'a Header, max: u64) -> Result<Self> {
        let blocks = blocks(b, h, max)?;
        // A file without its SDNA (or with a damaged one) still lists its blocks by code.
        let dna = blocks.iter().find(|bl| &bl.code == b"DNA1").and_then(|bl| Dna::parse(&b[bl.data.clone()], h).ok());
        Ok(Self { b, h, blocks, dna })
    }

    fn data(&self, block: &Block) -> &'a [u8] {
        &self.b[block.data.clone()]
    }

    /// A datablock's name, without its two-letter code (`OBCube` is `Cube`).
    fn id_name(&self, block: &Block) -> Option<String> {
        let dna = self.dna.as_ref()?;
        let name = text(self.data(block), &dna.id_name()?)?;
        Some(name.get(2..).unwrap_or("").to_string())
    }

    fn pointer(&self, data: &[u8], field: &Field) -> Option<u64> {
        if !field.pointer {
            return None;
        }
        if self.h.pointer == 8 { self.h.u64_at(data, field.offset) } else { self.h.u32_at(data, field.offset).map(u64::from) }
    }

    /// The Blocks facet: each kind of datablock (Object, Mesh, Material…) with its
    /// datablocks by name, then the other blocks by code and struct, with counts.
    pub fn tree(&self) -> OwnedTree {
        let mut tree = OwnedTree::new();
        let dna = self.dna.as_ref();
        let struct_name = |bl: &Block| dna.and_then(|d| d.struct_name(bl.sdna)).unwrap_or("").to_string();
        // Datablocks, by struct, in the order first met.
        let mut kinds: Vec<(String, Vec<(String, usize)>)> = Vec::new();
        let mut other: Vec<((String, String), (u64, u64))> = Vec::new();
        for bl in &self.blocks {
            let code = String::from_utf8_lossy(&bl.code).trim_end_matches('\0').to_string();
            let is_id = dna.is_some_and(|d| d.is_id(bl.sdna)) && code != "DATA";
            if is_id {
                let kind = struct_name(bl);
                let name = self.id_name(bl).unwrap_or_default();
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
    pub fn packed(&self) -> Vec<(String, Range<usize>)> {
        let Some(dna) = &self.dna else { return Vec::new() };
        let by_address: HashMap<u64, usize> = self.blocks.iter().enumerate().filter(|(_, b)| b.old != 0).map(|(i, b)| (b.old, i)).collect();
        let mut out = Vec::new();
        let mut used = std::collections::HashSet::new();
        for (i, bl) in self.blocks.iter().enumerate() {
            if dna.struct_name(bl.sdna) != Some("PackedFile") || &bl.code != b"DATA" {
                continue;
            }
            let data = self.data(bl);
            let (Some(size), Some(ptr)) = (dna.field(bl.sdna, "size"), dna.field(bl.sdna, "data")) else { continue };
            let Some(len) = self.h.u32_at(data, size.offset) else { continue };
            let Some(&target) = self.pointer(data, &ptr).and_then(|p| by_address.get(&p)) else { continue };
            let target = &self.blocks[target];
            let len = (len as usize).min(target.data.len());
            let name = self.owner_path(i).unwrap_or_else(|| format!("packed {}", out.len()));
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
    fn owner_path(&self, i: usize) -> Option<String> {
        let dna = self.dna.as_ref()?;
        for bl in self.blocks[..i].iter().rev() {
            let id = dna.is_id(bl.sdna) && &bl.code != b"DATA";
            let field = dna.field(bl.sdna, "filepath").or_else(|| if id { dna.field(bl.sdna, "name") } else { None });
            if let Some(path) = field.filter(|f| !f.pointer).and_then(|f| text(self.data(bl), &f)).filter(|p| !p.is_empty()) {
                let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string();
                if !name.is_empty() {
                    return Some(name);
                }
            }
            if id {
                return self.id_name(bl).filter(|n| !n.is_empty());
            }
        }
        None
    }
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

