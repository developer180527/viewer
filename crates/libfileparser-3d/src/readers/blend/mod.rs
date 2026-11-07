//! `.blend`: Blender's own save format is a dump of its in-memory structs, laid out
//! differently in every version, so the 3D data isn't read (yet). What every file
//! Blender saves with "Save Preview Images" on (the default) does carry is a small
//! preview picture in a `TEST` block right after the header; that is shown instead.
//! Its blocks are listed by kind and name, and its packed files opened, through the
//! file's own SDNA ([`blocks`]).

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use libfileparser_core::{FacetInfo, Limit, Limits, Model, Raster};

mod blocks;

/// A `.blend` file's preview image, and the Blender version that saved it.
pub fn preview(bytes: &[u8], limits: &Limits) -> Result<(Raster, String)> {
    match bytes.get(..4) {
        Some([0x1f, 0x8b, ..]) => bail!("compressed .blend (gzip); not read yet"),
        Some([0x28, 0xb5, 0x2f, 0xfd]) => bail!("compressed .blend (zstd); not read yet"),
        _ => {}
    }
    let header = Header::parse(bytes).context("not a .blend file")?;
    let raster = thumbnail(bytes, &header, limits)?;
    Ok((raster, header.version))
}

struct Header {
    /// Bytes of the file header; blocks start here.
    len: usize,
    pointer: usize,
    little: bool,
    /// The newer layout (Blender 5.0+): 64-bit block lengths.
    large_blocks: bool,
    version: String,
}

impl Header {
    fn parse(b: &[u8]) -> Option<Self> {
        let rest = b.strip_prefix(b"BLENDER")?;
        // Legacy: "BLENDER" + '_'|'-' (4/8-byte pointers) + 'v'|'V' (endianness) + "283".
        if let [p @ (b'_' | b'-'), e @ (b'v' | b'V'), v @ ..] = rest {
            let version = std::str::from_utf8(v.get(..3)?).ok()?;
            let version = format!("{}.{}", &version[..1], version[1..].trim_start_matches('0'));
            return Some(Self { len: 12, pointer: if *p == b'_' { 4 } else { 8 }, little: *e == b'v', large_blocks: false, version });
        }
        // Blender 5.0+: "BLENDER17-01v0500": header size, format version, 'v', version.
        let text = std::str::from_utf8(rest.get(..10)?).ok()?;
        let len: usize = text.get(..2)?.parse().ok()?;
        let version = text.get(6..10)?;
        let version = format!("{}.{}", version[..2].trim_start_matches('0'), version[2..].trim_start_matches('0'));
        let little = text.get(5..6)? == "v";
        Some(Self { len, pointer: 8, little, large_blocks: true, version })
    }

    fn u32_at(&self, b: &[u8], at: usize) -> Option<u32> {
        let x: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little { u32::from_le_bytes(x) } else { u32::from_be_bytes(x) })
    }

    fn u64_at(&self, b: &[u8], at: usize) -> Option<u64> {
        let x: [u8; 8] = b.get(at..at + 8)?.try_into().ok()?;
        Some(if self.little { u64::from_le_bytes(x) } else { u64::from_be_bytes(x) })
    }
}

/// The `TEST` block: width, height, then RGBA rows stored bottom-up. It sits among
/// the first few blocks (after `REND`); stop at the first data block or after a few.
fn thumbnail(b: &[u8], h: &Header, limits: &Limits) -> Result<Raster> {
    let block_header = if h.large_blocks { 32 } else { 16 + h.pointer };
    let mut at = h.len;
    let (len, data) = loop {
        let code = b.get(at..at + 4).context("truncated")?;
        // Block header: code, then the length; the rest (old pointer, SDNA index,
        // count) is laid out differently by version and not needed here.
        let len = if h.large_blocks {
            // code(4) sdna(4) old(8) len(8) nr(8)
            h.u64_at(b, at + 16).context("truncated")? as usize
        } else {
            h.u32_at(b, at + 4).context("truncated")? as usize
        };
        if code == b"TEST" {
            break (len, at + block_header);
        }
        if !matches!(code, b"REND" | b"GLOB") || len > 1 << 20 {
            bail!("no preview image saved in this file");
        }
        at = at.checked_add(block_header + len).context("truncated")?;
    };
    let w = h.u32_at(b, data).context("truncated")?;
    let ht = h.u32_at(b, data + 4).context("truncated")?;
    let pixels = (w as usize).checked_mul(ht as usize).and_then(|n| n.checked_mul(4)).context("bad size")?;
    if w == 0 || ht == 0 || w > 4096 || ht > 4096 || pixels + 8 > len {
        bail!("bad preview image ({w}×{ht})");
    }
    limits.check(Limit::ImagePixels, w as u64 * ht as u64)?;
    let src = b.get(data + 8..data + 8 + pixels).context("truncated preview")?;
    let row = w as usize * 4;
    let rgba: Vec<u8> = src.chunks_exact(row).rev().flatten().copied().collect();
    Ok(Raster::new(w, ht, rgba)?)
}

// ---------------------------------------------------------------- the reader

use libfileparser_core::{Category, Content, Detection, Document, FileSource, FormatInfo, ParseContext, Reader, ReaderInfo, Role};

use crate::build::detect_by;

pub struct BlendReader;

static BLEND: ReaderInfo = ReaderInfo::new("blend", "Blender preview", Category::ThreeD).extensions(&["blend"]);

impl Reader for BlendReader {
    fn info(&self) -> &ReaderInfo {
        &BLEND
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        detect_by(&BLEND, header.starts_with(b"BLENDER"), name)
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> libfileparser_core::Result<Document> {
        cx.cancel.check()?;
        let (raster, version) = preview(src.bytes(), &cx.limits).map_err(super::parse_error)?;
        let mut doc = Document::new(FormatInfo::new("blend", "Blender file", Category::ThreeD))
            .with_facet(Role::Primary, "Preview", Content::Raster(raster))
            .with_meta("Saved by", format!("Blender {version}"))
            .with_meta("Shown", "the preview image saved in the file; its 3D data isn't read yet");
        // The preview was found, so the header is good; damaged blocks further on only
        // cost these facets.
        let header = Header::parse(src.bytes()).context("not a .blend file").map_err(super::parse_error)?;
        if let Ok(file) = blocks::BlendFile::read(src.bytes(), &header, cx.limits.items) {
            let from = src.clone();
            doc = doc.with_lazy_facet(FacetInfo::new(Role::Structure, "Blocks", Model::Tree), move |cx| {
                let header = Header::parse(from.bytes()).ok_or_else(|| libfileparser_core::ParseError::corrupt("not a .blend file"))?;
                let file = blocks::BlendFile::read(from.bytes(), &header, cx.limits.items).map_err(super::parse_error)?;
                Ok(Content::Tree(Arc::new(file.tree())))
            });
            let packed = file.packed();
            if !packed.is_empty() {
                doc = doc
                    .with_facet(Role::Package, "Packed files", Content::Container(Arc::new(blocks::PackedFiles::new(src, packed.clone()))))
                    .with_meta("Packed files", packed.len());
            }
            if file.dna.is_none() {
                doc = doc.with_meta("Warning", "no readable SDNA: blocks are listed by code only");
            }
        }
        Ok(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A legacy little-endian, 64-bit .blend: header, a REND block, then the preview.
    fn blend_with_preview(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"BLENDER-v283".to_vec();
        let block = |b: &mut Vec<u8>, code: &[u8; 4], data: &[u8]| {
            b.extend_from_slice(code);
            b.extend_from_slice(&(data.len() as u32).to_le_bytes());
            b.extend_from_slice(&[0; 8 + 4 + 4]); // old pointer, SDNA index, count
            b.extend_from_slice(data);
        };
        block(&mut b, b"REND", &[0; 72]);
        let mut test = Vec::new();
        test.extend_from_slice(&w.to_le_bytes());
        test.extend_from_slice(&h.to_le_bytes());
        // Bottom row red, top row blue (stored bottom-up).
        for y in 0..h {
            for _ in 0..w {
                test.extend_from_slice(if y == 0 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
            }
        }
        block(&mut b, b"TEST", &test);
        block(&mut b, b"ENDB", &[]);
        b
    }

    #[test]
    fn preview_is_found_after_rend_and_flipped_upright() {
        let (r, version) = preview(&blend_with_preview(2, 2), &Limits::desktop()).unwrap();
        assert_eq!((r.width(), r.height()), (2, 2));
        let rgba = r.rgba().unwrap();
        assert_eq!(&rgba[..4], [0, 0, 255, 255], "top row first");
        assert_eq!(&rgba[8..12], [255, 0, 0, 255]);
        assert_eq!(version, "2.83");
    }

    #[test]
    fn files_without_a_preview_or_compressed_fail_cleanly() {
        let mut no_preview = b"BLENDER-v283".to_vec();
        no_preview.extend_from_slice(b"DATA\x10\0\0\0");
        no_preview.extend_from_slice(&[0; 32]);
        let l = Limits::desktop();
        assert!(preview(&no_preview, &l).is_err());
        assert!(preview(&[0x28, 0xb5, 0x2f, 0xfd, 0], &l).is_err());
        assert!(preview(b"BLENDER", &l).is_err());
        // An absurd size in the header is refused, not allocated.
        let mut huge = blend_with_preview(2, 2);
        let at = 12 + 24 + 72 + 24;
        huge[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(preview(&huge, &l).is_err());
    }
}
