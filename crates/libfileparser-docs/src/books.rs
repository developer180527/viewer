//! Kindle books (MOBI, AZW, AZW3, PRC, PalmDOC) read by our own decoder of the Palm
//! database and its compression (PalmDOC's LZ77 and Mobipocket's HUFF/CDIC, as
//! calibre and libmobi read them), their text as the HTML it is;
//! and Hangul Word Processor documents (HWP 3 and 5, HWPX) through `unhwp`, written
//! out as Markdown and read by the Markdown reader.

use std::sync::Arc;

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, OwnedFlow, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, Warning};

use crate::build::Builder;

static MOBI: ReaderInfo = ReaderInfo::new("mobi", "Kindle book", Category::Docs).extensions(&["mobi", "azw", "azw3", "prc", "pdb"]).mime_types(&["application/x-mobipocket-ebook", "application/vnd.amazon.ebook"]);
static HWP: ReaderInfo = ReaderInfo::new("hwp", "Hangul document", Category::Docs).extensions(&["hwp", "hwpx", "hwt"]).mime_types(&["application/x-hwp", "application/vnd.hancom.hwp", "application/hwp+zip"]);

pub struct MobiReader;
pub struct HwpReader;

/// A Palm database's type and creator, at offset 60: a Mobipocket book or a PalmDOC.
fn palm_kind(header: &[u8]) -> Option<&'static str> {
    match header.get(60..68)? {
        b"BOOKMOBI" => Some("BOOKMOBI"),
        b"TEXtREAd" => Some("TEXtREAd"),
        _ => None,
    }
}

impl Reader for MobiReader {
    fn info(&self) -> &ReaderInfo {
        &MOBI
    }

    /// A Palm database of type `BOOKMOBI` (or PalmDOC's `TEXtREAd`) at offset 60.
    fn detect(&self, header: &[u8], _name: &str) -> Detection {
        if palm_kind(header).is_some() { Detection::new(Confidence::Magic) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if palm_kind(src.header()).is_none() {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        let book = mobi_book(&bytes, cx)?;
        cx.cancel.check()?;
        let mut b = Builder::new(cx);
        if book.html {
            crate::html::parse(&book.text, src.name(), &mut b)?;
        } else {
            crate::html::parse(&crate::mail::text_html(&book.text), src.name(), &mut b)?;
        }
        let mut doc = Document::new(FormatInfo::new("mobi", if book.html { "Kindle book" } else { "PalmDOC book" }, Category::Docs)).with_facet(Role::Primary, "Book", Content::Flow(Arc::new(OwnedFlow::new(b.finish()?))));
        if !book.title.trim().is_empty() {
            doc = doc.with_meta("Title", book.title.trim());
        }
        for (k, v) in &book.meta {
            doc = doc.with_meta(*k, v.trim());
        }
        if book.images > 0 {
            doc = doc.with_warning(Warning::new(format!("{} image(s) in the book aren't shown yet.", book.images)));
        }
        Ok(doc.with_file_facets(src, None))
    }
}

/// A Palm database's book: its text (HTML for Mobipocket, plain for PalmDOC), title
/// and metadata.
struct Book {
    text: String,
    html: bool,
    title: String,
    meta: Vec<(&'static str, String)>,
    images: usize,
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn corrupt(why: &str) -> ParseError {
    ParseError::corrupt(format!("not a readable Kindle book: {why}"))
}

fn mobi_book(data: &[u8], cx: &ParseContext) -> Result<Book> {
    let count = be16(data, 76).ok_or_else(|| corrupt("too short"))? as usize;
    let offsets: Vec<usize> = (0..count).map(|i| be32(data, 78 + i * 8).map(|o| o as usize)).collect::<Option<_>>().ok_or_else(|| corrupt("its record list is cut short"))?;
    let record = |i: usize| -> Option<&[u8]> {
        let start = *offsets.get(i)?;
        let end = offsets.get(i + 1).copied().unwrap_or(data.len()).min(data.len());
        data.get(start..end.max(start))
    };
    let r0 = record(0).ok_or_else(|| corrupt("no first record"))?;
    let compression = be16(r0, 0).ok_or_else(|| corrupt("no header"))?;
    let text_records = be16(r0, 8).unwrap_or(0) as usize;
    if be16(r0, 12).unwrap_or(0) != 0 {
        return Err(ParseError::unsupported("this Kindle book is protected (DRM)"));
    }
    let palm_name = String::from_utf8_lossy(&data[..32.min(data.len())]).trim_end_matches('\0').replace('_', " ");
    let mobi = r0.get(16..20) == Some(b"MOBI");
    let (mut title, mut meta, mut utf8, mut extra_flags, mut huff, mut first_image) = (palm_name, Vec::new(), false, 0u16, None, None);
    if mobi {
        let header_len = be32(r0, 20).unwrap_or(0) as usize;
        utf8 = be32(r0, 28) == Some(65001);
        if let (Some(off), Some(len)) = (be32(r0, 84), be32(r0, 88)) {
            if let Some(name) = r0.get(off as usize..off as usize + len as usize) {
                title = decode(name, utf8);
            }
        }
        first_image = be32(r0, 108).filter(|&i| i != u32::MAX && (i as usize) < count).map(|i| i as usize);
        let (h, n) = (be32(r0, 112).unwrap_or(0) as usize, be32(r0, 116).unwrap_or(0) as usize);
        if compression == 17480 {
            huff = Some((h, n));
        }
        if header_len >= 0xE4 {
            extra_flags = be16(r0, 16 + 0xE2).unwrap_or(0);
        }
        // EXTH: the book's metadata.
        if be32(r0, 0x80).is_some_and(|f| f & 0x40 != 0) {
            let at = 16 + header_len;
            if r0.get(at..at + 4) == Some(b"EXTH") {
                let n = be32(r0, at + 8).unwrap_or(0) as usize;
                let mut p = at + 12;
                for _ in 0..n.min(10_000) {
                    let (Some(kind), Some(len)) = (be32(r0, p), be32(r0, p + 4)) else { break };
                    let len = len as usize;
                    if len < 8 {
                        break;
                    }
                    let value = r0.get(p + 8..p + len).map(|v| decode(v, utf8)).unwrap_or_default();
                    let key = match kind {
                        100 => Some("Author"),
                        101 => Some("Publisher"),
                        103 => Some("Description"),
                        104 => Some("ISBN"),
                        105 => Some("Subject"),
                        106 => Some("Published"),
                        524 => Some("Language"),
                        503 => {
                            title = value.clone();
                            None
                        }
                        _ => None,
                    };
                    if let Some(k) = key.filter(|_| !value.trim().is_empty()) {
                        meta.push((k, if k == "Description" { crate::markup::strip_tags(&value) } else { value }));
                    }
                    p += len;
                }
            }
        }
    }
    let mut decoder = match huff {
        Some((h, n)) => Some(Huff::new(&(h..h + n).filter_map(record).collect::<Vec<_>>()).ok_or_else(|| corrupt("its Huffman tables are damaged"))?),
        None => None,
    };
    let mut text = Vec::new();
    for i in 1..=text_records.min(count.saturating_sub(1)) {
        if i % 64 == 0 {
            cx.cancel.check()?;
        }
        let Some(rec) = record(i) else { break };
        let rec = &rec[..rec.len() - trailing(rec, extra_flags).min(rec.len())];
        match compression {
            1 => text.extend_from_slice(rec),
            2 => palmdoc(rec, &mut text),
            17480 => {
                let d = decoder.as_mut().ok_or_else(|| corrupt("no Huffman tables"))?;
                text.extend(d.unpack(rec, 0).ok_or_else(|| corrupt("a text record won't decompress"))?);
            }
            other => return Err(ParseError::unsupported(format!("a Kindle book compressed as {other}"))),
        }
        cx.limits.check(libfileparser_core::Limit::Allocation, text.len() as u64)?;
    }
    let images = first_image.map_or(0, |f| (f..count).filter_map(record).filter(|r| r.starts_with(&[0xFF, 0xD8]) || r.starts_with(b"\x89PNG") || r.starts_with(b"GIF8") || r.starts_with(b"BM")).count());
    let text = decode(&text, utf8);
    // PalmDOC is plain text, unless its text is HTML (as some converters write it).
    let lead = text.trim_start().get(..64).unwrap_or(text.trim_start()).to_ascii_lowercase();
    let html = mobi || lead.starts_with("<html") || lead.starts_with("<!doctype") || lead.starts_with("<?xml") || lead.starts_with("<body") || lead.starts_with("<p");
    Ok(Book { text, html, title, meta, images })
}

fn decode(b: &[u8], utf8: bool) -> String {
    if utf8 { String::from_utf8_lossy(b).into_owned() } else { encoding_rs::WINDOWS_1252.decode_without_bom_handling(b).0.into_owned() }
}

/// The bytes a text record's trailing entries take (`extra_flags`: bit 0 multibyte
/// overlap, each higher bit an entry sized by a backward variable-length number).
fn trailing(rec: &[u8], extra_flags: u16) -> usize {
    let mut num = 0usize;
    let mut flags = extra_flags >> 1;
    while flags != 0 {
        if flags & 1 != 0 {
            // A size written backwards from the end, seven bits a byte.
            let (mut result, mut bits, mut end) = (0usize, 0, rec.len().saturating_sub(num));
            while end > 0 {
                let v = rec[end - 1];
                result |= ((v & 0x7F) as usize) << bits;
                bits += 7;
                end -= 1;
                if v & 0x80 != 0 || bits >= 28 {
                    break;
                }
            }
            num += result;
        }
        flags >>= 1;
    }
    if extra_flags & 1 != 0 {
        if let Some(&b) = rec.len().checked_sub(num + 1).and_then(|at| rec.get(at)) {
            num += (b & 3) as usize + 1;
        }
    }
    num
}

/// PalmDOC's LZ77.
fn palmdoc(input: &[u8], out: &mut Vec<u8>) {
    let start = out.len();
    let mut i = 0;
    while i < input.len() {
        let c = input[i];
        i += 1;
        match c {
            1..=8 => {
                let end = (i + c as usize).min(input.len());
                out.extend_from_slice(&input[i..end]);
                i = end;
            }
            0x80..=0xBF => {
                let Some(&c2) = input.get(i) else { break };
                i += 1;
                let v = ((c as usize) << 8) | c2 as usize;
                let dist = (v >> 3) & 0x7FF;
                let len = (v & 7) + 3;
                if dist == 0 || dist > out.len() - start {
                    continue;
                }
                for _ in 0..len {
                    let b = out[out.len() - dist];
                    out.push(b);
                }
            }
            0xC0..=0xFF => {
                out.push(b' ');
                out.push(c ^ 0x80);
            }
            c => out.push(c),
        }
    }
}

/// Mobipocket's Huffman decoder: a HUFF record's code tables and the CDIC records'
/// phrases (themselves compressed, decoded once when first used).
struct Huff {
    dict1: Vec<(u32, bool, u64)>,
    mincode: [u64; 33],
    maxcode: [u64; 33],
    phrases: Vec<Option<(Vec<u8>, bool)>>,
}

impl Huff {
    fn new(records: &[&[u8]]) -> Option<Self> {
        let huff = records.first()?;
        if !huff.starts_with(b"HUFF") {
            return None;
        }
        let (off1, off2) = (be32(huff, 8)? as usize, be32(huff, 12)? as usize);
        let mut dict1 = Vec::with_capacity(256);
        for k in 0..256 {
            let v = be32(huff, off1 + k * 4)?;
            let (len, term, max) = (v & 0x1F, v & 0x80 != 0, (v >> 8) as u64);
            if len == 0 {
                return None;
            }
            dict1.push((len, term, ((max + 1) << (32 - len)) - 1));
        }
        let (mut mincode, mut maxcode) = ([0u64; 33], [0u64; 33]);
        for len in 1..=32usize {
            let (lo, hi) = (be32(huff, off2 + (len - 1) * 8)? as u64, be32(huff, off2 + (len - 1) * 8 + 4)? as u64);
            mincode[len] = lo << (32 - len);
            maxcode[len] = ((hi + 1) << (32 - len)) - 1;
        }
        let mut phrases = Vec::new();
        for cdic in &records[1..] {
            if !cdic.starts_with(b"CDIC") {
                continue;
            }
            let (total, bits) = (be32(cdic, 8)? as usize, be32(cdic, 12)?);
            let n = (1usize << bits.min(16)).min(total.saturating_sub(phrases.len()));
            for k in 0..n {
                let off = be16(cdic, 16 + k * 2)? as usize;
                let blen = be16(cdic, 16 + off)? as usize;
                let slice = cdic.get(18 + off..18 + off + (blen & 0x7FFF))?.to_vec();
                phrases.push(Some((slice, blen & 0x8000 != 0)));
            }
        }
        Some(Self { dict1, mincode, maxcode, phrases })
    }

    fn unpack(&mut self, data: &[u8], depth: usize) -> Option<Vec<u8>> {
        if depth > 32 {
            return None;
        }
        let mut bits_left = data.len() as i64 * 8;
        let mut padded = data.to_vec();
        padded.extend_from_slice(&[0; 8]);
        let word = |pos: usize| u64::from_be_bytes(padded[pos..pos + 8].try_into().unwrap_or([0; 8]));
        let (mut pos, mut n) = (0usize, 32i64);
        let mut x = word(0);
        let mut out = Vec::new();
        loop {
            if n <= 0 {
                pos += 4;
                if pos + 8 > padded.len() {
                    break;
                }
                x = word(pos);
                n += 32;
            }
            let code = (x >> n) & 0xFFFF_FFFF;
            let (mut len, term, mut max) = self.dict1[(code >> 24) as usize];
            if !term {
                while (len as usize) < 32 && code < self.mincode[len as usize] {
                    len += 1;
                }
                max = self.maxcode[len as usize];
            }
            n -= len as i64;
            bits_left -= len as i64;
            if bits_left < 0 {
                break;
            }
            let r = ((max.wrapping_sub(code)) >> (32 - len)) as usize;
            // A phrase being decoded refers to itself: damaged tables.
            let (slice, done) = self.phrases.get_mut(r)?.take()?;
            let slice = if done { slice } else { self.unpack(&slice, depth + 1)? };
            out.extend_from_slice(&slice);
            self.phrases[r] = Some((slice, true));
        }
        Some(out)
    }
}

const OLE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const HWPX_MIMETYPE: &[u8] = b"mimetypeapplication/hwp+zip";

impl Reader for HwpReader {
    fn info(&self) -> &ReaderInfo {
        &HWP
    }

    /// HWPX by its `mimetype` part; HWP 3 by its signature; HWP 5 (OLE, shared with
    /// Word and Excel) by its signature with its extension.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if header.starts_with(b"PK\x03\x04") && header.get(30..30 + HWPX_MIMETYPE.len()) == Some(HWPX_MIMETYPE) {
            Detection::new(Confidence::Certain)
        } else if header.starts_with(b"HWP Document File") {
            Detection::new(Confidence::Magic)
        } else if (header.starts_with(OLE) || header.starts_with(b"PK\x03\x04")) && HWP.matches_extension(name) {
            // As Word's and PowerPoint's readers do: the signature with its own extension.
            Detection::new(Confidence::Certain)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let h = src.header();
        if !(h.starts_with(OLE) || h.starts_with(b"PK\x03\x04") || h.starts_with(b"HWP Document File")) {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        // One thread, as the app's context decides threading, and no resources: the
        // text is what's read.
        let opts = unhwp::ParseOptions::new().lenient().sequential().without_resources();
        let hwp = unhwp::parse_bytes_with_options(&bytes, &opts).map_err(|e| {
            let msg = e.to_string();
            if msg.to_ascii_lowercase().contains("encrypt") || msg.contains("password") {
                ParseError::unsupported("this document is password-protected")
            } else if msg.to_ascii_lowercase().contains("not") && msg.to_ascii_lowercase().contains("hwp") {
                ParseError::NotThisFormat
            } else {
                ParseError::corrupt(format!("not a readable HWP document: {msg}"))
            }
        })?;
        cx.cancel.check()?;
        let md = unhwp::render::render_markdown(&hwp, &unhwp::RenderOptions::default()).map_err(|e| ParseError::corrupt(e))?;
        let mut b = Builder::new(cx);
        for block in crate::markdown::parse(&md, cx)? {
            b.block(block)?;
        }
        let m = &hwp.metadata;
        let mut doc = Document::new(FormatInfo::new("hwp", "Hangul document", Category::Docs)).with_facet(Role::Primary, "Document", Content::Flow(Arc::new(OwnedFlow::new(b.finish()?))));
        for (k, v) in [("Title", &m.title), ("Author", &m.author), ("Subject", &m.subject), ("Created", &m.created), ("Modified", &m.modified), ("Written with", &m.creator_app), ("Version", &m.format_version)] {
            if let Some(v) = v.as_ref().map(|v| v.trim()).filter(|v| !v.is_empty()) {
                doc = doc.with_meta(k, v);
            }
        }
        if m.is_distribution {
            doc = doc.with_meta("Distribution document", "yes");
        }
        Ok(doc.with_file_facets(src, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection() {
        let mut palm = vec![0u8; 78];
        palm[60..68].copy_from_slice(b"BOOKMOBI");
        assert_eq!(MobiReader.detect(&palm, "x").confidence, Confidence::Magic);
        assert_eq!(MobiReader.detect(&[0u8; 78], "a.mobi"), Detection::NONE);
        assert_eq!(HwpReader.detect(b"HWP Document File V3.00 \x1a\x01\x02\x03\x04\x05", "a").confidence, Confidence::Magic);
        assert_eq!(HwpReader.detect(OLE, "a.doc"), Detection::NONE);
        assert_eq!(HwpReader.detect(OLE, "a.hwp").confidence, Confidence::Certain);
    }

    #[test]
    fn broken_books_are_errors_not_panics() {
        let mut palm = vec![0u8; 200];
        palm[60..68].copy_from_slice(b"BOOKMOBI");
        assert!(MobiReader.open(&FileSource::from_bytes("a.mobi", palm), &ParseContext::new()).is_err());
        let mut ole = OLE.to_vec();
        ole.resize(1024, 0);
        assert!(HwpReader.open(&FileSource::from_bytes("a.hwp", ole), &ParseContext::new()).is_err());
    }
}
