//! Reads Blender's `.blend` files, of every version from 2.4 to 5.0.
//!
//! A `.blend` is a dump of Blender's memory: blocks of structs laid out differently in
//! every version, with a catalogue of them all (the SDNA) in the file. Offsets here come
//! from that catalogue, so versions differ only where Blender moved its data, and those
//! places are spelled out in [`scene`]. Every offset is checked; a damaged file is an
//! error or a mesh skipped with a warning, never a panic.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let bytes = std::fs::read("model.blend")?;
//! let limits = blend_lite::Limits::default();
//! let inflated = blend_lite::inflate(&bytes, &limits)?;
//! let file = blend_lite::File::read(inflated.as_deref().unwrap_or(&bytes), &limits)?;
//! for object in &blend_lite::scene(&file).objects {
//!     println!("{}", object.name);
//! }
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::ops::Range;

mod mesh;

pub use mesh::{Material, Mesh, Object, Scene, scene};

/// Why a file couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// It doesn't start like a `.blend`.
    NotBlend,
    /// It ends in the middle of something.
    Truncated,
    /// Its compressed data is damaged.
    Inflate(String),
    /// It's over one of the [`Limits`].
    TooLarge { what: &'static str, value: u64, max: u64 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotBlend => write!(f, "not a .blend file"),
            Error::Truncated => write!(f, "the file ends early"),
            Error::Inflate(e) => write!(f, "damaged compressed data: {e}"),
            Error::TooLarge { what, value, max } => write!(f, "{what}: {value}, more than the {max} allowed"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// What a file may cost.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Limits {
    /// Most bytes a compressed file may inflate to.
    pub inflated: u64,
    /// Above `unpack_always` bytes, most bytes out per byte in.
    pub unpack_ratio: u64,
    pub unpack_always: u64,
    pub blocks: u64,
    /// Largest preview image, in pixels.
    pub preview_pixels: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { inflated: 4 << 30, unpack_ratio: 200, unpack_always: 64 << 20, blocks: 1 << 24, preview_pixels: 1 << 24 }
    }
}

impl Limits {
    pub fn with_inflated(mut self, max: u64) -> Self {
        self.inflated = max;
        self
    }

    pub fn with_unpack(mut self, ratio: u64, always: u64) -> Self {
        self.unpack_ratio = ratio;
        self.unpack_always = always;
        self
    }

    pub fn with_blocks(mut self, max: u64) -> Self {
        self.blocks = max;
        self
    }

    pub fn with_preview_pixels(mut self, max: u64) -> Self {
        self.preview_pixels = max;
        self
    }
}

/// Whether these bytes are a compressed `.blend`'s (gzip or zstd).
pub fn is_compressed(b: &[u8]) -> bool {
    b.starts_with(&[0x1f, 0x8b]) || b.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
}

/// The file's bytes inflated, or `None` when it isn't compressed.
pub fn inflate(b: &[u8], limits: &Limits) -> Result<Option<Vec<u8>>> {
    if !is_compressed(b) {
        return Ok(None);
    }
    let cap = limits.inflated.min(limits.unpack_always.max((b.len() as u64).saturating_mul(limits.unpack_ratio)));
    let mut out = Vec::new();
    // One past the cap, to tell "exactly at it" from "over it".
    let read = |r: &mut dyn Read, out: &mut Vec<u8>| -> Result<()> {
        let room = (cap + 1).saturating_sub(out.len() as u64);
        r.take(room).read_to_end(out).map_err(|e| Error::Inflate(e.to_string()))?;
        Ok(())
    };
    if b.starts_with(&[0x1f, 0x8b]) {
        read(&mut flate2::read::MultiGzDecoder::new(b), &mut out)?;
    } else {
        // Blender writes many frames, so it can seek, then a skippable seek table.
        let mut rest = b;
        while rest.len() >= 4 && out.len() as u64 <= cap {
            if rest[0] & 0xf0 == 0x50 && rest[1..4] == [0x2a, 0x4d, 0x18] {
                let len = rest.get(4..8).ok_or(Error::Truncated)?;
                let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
                rest = rest.get(8usize.saturating_add(len)..).ok_or(Error::Truncated)?;
                continue;
            }
            let mut frame = ruzstd::decoding::StreamingDecoder::new(&mut rest).map_err(|e| Error::Inflate(e.to_string()))?;
            read(&mut frame, &mut out)?;
        }
    }
    if out.len() as u64 > cap {
        return Err(Error::TooLarge { what: "inflated size", value: out.len() as u64, max: cap });
    }
    Ok(Some(out))
}

/// The file header.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Header {
    /// Bytes of the file header; blocks start here.
    pub len: usize,
    /// 4 or 8.
    pub pointer: usize,
    pub little: bool,
    /// The newer layout (Blender 5.0+): 64-bit block lengths.
    pub large_blocks: bool,
    /// As Blender shows it: "4.2".
    pub version: String,
}

impl Header {
    pub fn parse(b: &[u8]) -> Option<Self> {
        let rest = b.strip_prefix(b"BLENDER")?;
        // Legacy: "BLENDER" + '_'|'-' (4/8-byte pointers) + 'v'|'V' (endianness) + "283".
        if let [p @ (b'_' | b'-'), e @ (b'v' | b'V'), v @ ..] = rest {
            let version = std::str::from_utf8(v.get(..3)?).ok()?;
            let version = format!("{}.{}", version.get(..1)?, version.get(1..)?.trim_start_matches('0'));
            return Some(Self { len: 12, pointer: if *p == b'_' { 4 } else { 8 }, little: *e == b'v', large_blocks: false, version });
        }
        // Blender 5.0+: "BLENDER17-01v0500": header size, format version, 'v', version.
        let text = std::str::from_utf8(rest.get(..10)?).ok()?;
        let len: usize = text.get(..2)?.parse().ok()?;
        let version = text.get(6..10)?;
        let version = format!("{}.{}", version.get(..2)?.trim_start_matches('0'), version.get(2..)?.trim_start_matches('0'));
        let little = text.get(5..6)? == "v";
        Some(Self { len, pointer: 8, little, large_blocks: true, version })
    }

    pub fn u32_at(&self, b: &[u8], at: usize) -> Option<u32> {
        let x: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.little { u32::from_le_bytes(x) } else { u32::from_be_bytes(x) })
    }

    pub fn u64_at(&self, b: &[u8], at: usize) -> Option<u64> {
        let x: [u8; 8] = b.get(at..at.checked_add(8)?)?.try_into().ok()?;
        Some(if self.little { u64::from_le_bytes(x) } else { u64::from_be_bytes(x) })
    }

    pub(crate) fn u16_at(&self, b: &[u8], at: usize) -> Option<u16> {
        let x: [u8; 2] = b.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.little { u16::from_le_bytes(x) } else { u16::from_be_bytes(x) })
    }

    pub(crate) fn f32_at(&self, b: &[u8], at: usize) -> Option<f32> {
        self.u32_at(b, at).map(f32::from_bits)
    }
}

/// One block: its code, where its data is, the address it had in Blender's memory
/// (pointers in other blocks refer to it by that) and its SDNA struct.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Block {
    pub code: [u8; 4],
    pub data: Range<usize>,
    pub old: u64,
    pub sdna: usize,
}

fn blocks(b: &[u8], h: &Header, max: u64) -> Result<Vec<Block>> {
    let header = if h.large_blocks { 32 } else { 16 + h.pointer };
    let mut out = Vec::new();
    let mut at = h.len;
    while at + header <= b.len() {
        let code: [u8; 4] = [b[at], b[at + 1], b[at + 2], b[at + 3]];
        if &code == b"ENDB" {
            break;
        }
        let (len, old, sdna) = if h.large_blocks {
            // code(4) sdna(4) old(8) len(8) nr(8)
            (h.u64_at(b, at + 16).ok_or(Error::Truncated)?, h.u64_at(b, at + 8).ok_or(Error::Truncated)?, h.u32_at(b, at + 4).ok_or(Error::Truncated)?)
        } else {
            // code(4) len(4) old(pointer) sdna(4) nr(4)
            let p = h.pointer;
            let old = if p == 8 { h.u64_at(b, at + 8) } else { h.u32_at(b, at + 8).map(u64::from) };
            (h.u32_at(b, at + 4).ok_or(Error::Truncated)? as u64, old.ok_or(Error::Truncated)?, h.u32_at(b, at + 8 + p).ok_or(Error::Truncated)?)
        };
        let start = at + header;
        let end = usize::try_from(len).ok().and_then(|l| start.checked_add(l)).filter(|&e| e <= b.len()).ok_or(Error::Truncated)?;
        if out.len() as u64 >= max {
            return Err(Error::TooLarge { what: "blocks", value: max + 1, max });
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
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub offset: usize,
    pub size: usize,
    pub pointer: bool,
    /// Index of its type, for a field that is itself a struct.
    pub ty: usize,
}

impl Dna {
    fn parse(d: &[u8], h: &Header) -> std::result::Result<Self, String> {
        let mut at = 0usize;
        let tag = |at: &mut usize, t: &[u8]| -> std::result::Result<(), String> {
            *at = at.next_multiple_of(4);
            if d.get(*at..*at + 4) != Some(t) {
                return Err(format!("no {} section", String::from_utf8_lossy(t)));
            }
            *at += 4;
            Ok(())
        };
        let u32_at = |at: usize| h.u32_at(d, at).map(|v| v as usize).ok_or("truncated".to_string());
        let u16_at = |at: usize| -> std::result::Result<usize, String> {
            let x: [u8; 2] = d.get(at..at + 2).ok_or("truncated")?.try_into().map_err(|_| "truncated")?;
            Ok(if h.little { u16::from_le_bytes(x) } else { u16::from_be_bytes(x) } as usize)
        };
        let strings = |at: &mut usize| -> std::result::Result<Vec<String>, String> {
            let n = u32_at(*at)?;
            *at += 4;
            if n > d.len() {
                return Err(format!("{n} names in {} bytes", d.len()));
            }
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let len = d.get(*at..).and_then(|r| r.iter().position(|&c| c == 0)).ok_or("truncated")?;
                out.push(String::from_utf8_lossy(&d[*at..*at + len]).into_owned());
                *at += len + 1;
            }
            Ok(out)
        };
        if d.get(..4) != Some(b"SDNA") {
            return Err("no SDNA".into());
        }
        at += 4;
        tag(&mut at, b"NAME")?;
        let names = strings(&mut at)?;
        tag(&mut at, b"TYPE")?;
        let types = strings(&mut at)?;
        tag(&mut at, b"TLEN")?;
        let sizes = (0..types.len()).map(|i| u16_at(at + i * 2).map(|v| v as u16)).collect::<std::result::Result<Vec<_>, _>>()?;
        at += types.len() * 2;
        tag(&mut at, b"STRC")?;
        let n = u32_at(at)?;
        at += 4;
        if n > d.len() / 4 {
            return Err(format!("{n} structs in {} bytes", d.len()));
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
                    return Err(format!("a field names type {t} or name {name}, which don't exist"));
                }
                fields.push((t, name));
                at += 4;
            }
            if ty >= types.len() {
                return Err(format!("struct of type {ty}, which doesn't exist"));
            }
            structs.push((ty, fields));
        }
        Ok(Self { names, types, sizes, structs, pointer: h.pointer })
    }

    /// The size of struct `index`, in bytes.
    pub fn struct_size(&self, index: usize) -> Option<usize> {
        self.structs.get(index).map(|(t, _)| self.sizes[*t] as usize)
    }

    /// The type name of struct `index`.
    pub fn struct_name(&self, index: usize) -> Option<&str> {
        self.structs.get(index).map(|(t, _)| self.types[*t].as_str())
    }

    pub(crate) fn struct_of_type(&self, ty: usize) -> Option<usize> {
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
                return Some(Field { offset, size, pointer, ty });
            }
            offset = offset.checked_add(size)?;
        }
        None
    }

    /// The `ID` field's `name` within a datablock: the ID is its first field.
    pub(crate) fn id_name(&self) -> Option<Field> {
        let id = self.struct_of_type(self.types.iter().position(|t| t == "ID")?)?;
        self.field(id, "name")
    }
}


/// A preview picture: RGBA rows, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Preview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A `.blend` opened: its blocks, and its SDNA when that's readable.
pub struct File<'a> {
    bytes: &'a [u8],
    pub header: Header,
    pub blocks: Vec<Block>,
    pub dna: Option<Dna>,
    /// Why there's no [`dna`](Self::dna).
    pub dna_error: Option<String>,
    /// Block indices by address, in file order: Blender 5 reuses addresses between
    /// datablocks, so one address can name several blocks.
    by_address: HashMap<u64, Vec<usize>>,
    preview_pixels: u64,
}

impl<'a> File<'a> {
    /// Reads an uncompressed file ([`inflate`] first). A file without its SDNA (or with
    /// a damaged one) still lists its blocks by code.
    pub fn read(bytes: &'a [u8], limits: &Limits) -> Result<Self> {
        let header = Header::parse(bytes).ok_or(Error::NotBlend)?;
        let blocks = blocks(bytes, &header, limits.blocks)?;
        let dna = blocks.iter().find(|b| &b.code == b"DNA1").map(|b| Dna::parse(&bytes[b.data.clone()], &header));
        let (dna, dna_error) = match dna {
            Some(Ok(d)) => (Some(d), None),
            Some(Err(e)) => (None, Some(e)),
            None => (None, Some("no DNA1 block".to_string())),
        };
        let mut by_address: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, b) in blocks.iter().enumerate().filter(|(_, b)| b.old != 0) {
            by_address.entry(b.old).or_default().push(i);
        }
        Ok(Self { bytes, header, blocks, dna, dna_error, by_address, preview_pixels: limits.preview_pixels })
    }

    /// A block's bytes.
    pub fn data(&self, block: &Block) -> &'a [u8] {
        &self.bytes[block.data.clone()]
    }

    /// The block a pointer in block `from` refers to: the first with that address at or
    /// after `from`, since a datablock's data follows it in the file.
    pub fn block_at(&self, from: usize, address: u64) -> Option<&Block> {
        let list = self.by_address.get(&address)?;
        let at = list.partition_point(|&i| i < from);
        self.blocks.get(*list.get(at).or(list.last())?)
    }

    /// The pointer stored at `field` in `data`.
    pub fn pointer(&self, data: &[u8], field: &Field) -> Option<u64> {
        if !field.pointer {
            return None;
        }
        if self.header.pointer == 8 { self.header.u64_at(data, field.offset) } else { self.header.u32_at(data, field.offset).map(u64::from) }
    }

    /// A datablock's name, without its two-letter code (`OBCube` is `Cube`).
    pub fn id_name(&self, block: &Block) -> Option<String> {
        let dna = self.dna.as_ref()?;
        let name = text(self.data(block), &dna.id_name()?)?;
        Some(name.get(2..).unwrap_or("").to_string())
    }

    /// The preview saved in the file (a `TEST` block, among the first few), if any.
    pub fn preview(&self) -> Result<Option<Preview>> {
        let Some(block) = self.blocks.iter().take(8).find(|b| &b.code == b"TEST") else { return Ok(None) };
        let (b, h) = (self.data(block), &self.header);
        let (Some(w), Some(ht)) = (h.u32_at(b, 0), h.u32_at(b, 4)) else { return Ok(None) };
        let pixels = w as u64 * ht as u64;
        if pixels > self.preview_pixels {
            return Err(Error::TooLarge { what: "preview pixels", value: pixels, max: self.preview_pixels });
        }
        let Some(data) = b.get(8..8 + pixels as usize * 4).filter(|_| w > 0 && ht > 0) else { return Ok(None) };
        let rgba = data.chunks_exact(w as usize * 4).rev().flatten().copied().collect();
        Ok(Some(Preview { width: w, height: ht, rgba }))
    }
}

/// A C string in `data` at `field`, cut at the first NUL.
pub fn text(data: &[u8], field: &Field) -> Option<String> {
    let bytes = data.get(field.offset..field.offset.checked_add(field.size)?)?;
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}
