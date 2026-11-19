//! Kindle books, read: a small, embeddable reader of Mobipocket (MOBI, AZW, AZW3's
//! MOBI part, PRC) and PalmDOC (PDB) books.
//!
//! It reads the Palm database and its records, the PalmDOC and MOBI headers, the EXTH
//! metadata (author, publisher, description, ISBN, language…), and the text, through
//! each compression a book uses: none, PalmDOC's LZ77, and Mobipocket's Huffman
//! (HUFF/CDIC, as calibre and libmobi read it), with trailing record entries removed.
//! The text is the book's own HTML. Protected (DRM) books are refused. Its pictures are
//! the image records, in the order its HTML numbers them (`<img recindex="00001">` is
//! the first); their bytes are JPEG, PNG, GIF or BMP files, as written.
//!
//! ```
//! # fn book() -> Vec<u8> { std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/files/docs/sample.mobi")).unwrap() }
//! let bytes = book();
//! assert!(mobi_lite::is_mobi(&bytes));
//! let book = mobi_lite::read(&bytes, &mobi_lite::Limits::default(), &|| false).unwrap();
//! assert_eq!(book.title, "Corpus Kindle book");
//! assert!(book.html && book.text.contains("<h1>Corpus Kindle book</h1>"));
//! ```

use std::fmt;

/// Why a book can't be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a Palm database of a book (`BOOKMOBI` or `TEXtREAd` at offset 60).
    NotMobi,
    /// Protected by DRM.
    Protected,
    /// A compression this reader doesn't know, by its number.
    Unsupported(u16),
    /// Its text would be larger than [`Limits::max_text`].
    TooLarge { max: u64 },
    /// `stop` said to.
    Stopped,
    /// Damaged: what was found wrong.
    Damaged(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotMobi => write!(f, "not a Kindle or PalmDOC book"),
            Error::Protected => write!(f, "the book is protected (DRM)"),
            Error::Unsupported(c) => write!(f, "a book compressed as {c}"),
            Error::TooLarge { max } => write!(f, "its text is larger than {max} bytes"),
            Error::Stopped => write!(f, "stopped"),
            Error::Damaged(why) => write!(f, "damaged: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// What reading may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The text's size once decompressed, in bytes.
    pub max_text: u64,
}

impl Default for Limits {
    /// 512 MB of text: far past any real book.
    fn default() -> Self {
        Limits { max_text: 512 << 20 }
    }
}

/// A Mobipocket book or a PalmDOC, from its first 68 bytes.
pub fn is_mobi(header: &[u8]) -> bool {
    palm_kind(header).is_some()
}

/// A Palm database's type and creator, at offset 60: a Mobipocket book or a PalmDOC.
fn palm_kind(header: &[u8]) -> Option<&'static str> {
    match header.get(60..68)? {
        b"BOOKMOBI" => Some("BOOKMOBI"),
        b"TEXtREAd" => Some("TEXtREAd"),
        _ => None,
    }
}

/// A Palm database's book: its text (HTML for Mobipocket, plain for PalmDOC), title
/// and metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    /// The book's text: HTML for a Mobipocket book (and a PalmDOC that holds HTML),
    /// plain otherwise.
    pub text: String,
    pub html: bool,
    /// From the MOBI header's full name or EXTH 503, else the Palm database's name.
    pub title: String,
    /// Its EXTH metadata: Author, Publisher, Description, ISBN, Subject, Published,
    /// Language.
    pub meta: Vec<(&'static str, String)>,
    /// Its pictures, by the number its HTML gives them (`recindex`, from 1): the
    /// records from the first image record on, each a JPEG, PNG, GIF or BMP file, or
    /// empty where a record there isn't one (fonts, resources the reader skips).
    pub images: Vec<Vec<u8>>,
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn corrupt(why: &str) -> Error {
    Error::Damaged(why.to_string())
}

/// The book in `data`, its text decompressed within `limits`; `stop` is asked between
/// records, and reading ends with [`Error::Stopped`] when it says so.
pub fn read(data: &[u8], limits: &Limits, stop: &dyn Fn() -> bool) -> Result<Book, Error> {
    if palm_kind(data).is_none() {
        return Err(Error::NotMobi);
    }
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
        return Err(Error::Protected);
    }
    let palm_name = String::from_utf8_lossy(&data[..32.min(data.len())]).trim_end_matches('\0').replace('_', " ");
    let mobi = r0.get(16..20) == Some(b"MOBI");
    let (mut title, mut meta, mut utf8, mut extra_flags, mut huff, mut first_image) = (palm_name, Vec::new(), false, 0u16, None, None);
    if mobi {
        let header_len = be32(r0, 20).unwrap_or(0) as usize;
        utf8 = be32(r0, 28) == Some(65001);
        if let (Some(off), Some(len)) = (be32(r0, 84), be32(r0, 88))
            && let Some(name) = r0.get(off as usize..off as usize + len as usize) {
                title = decode(name, utf8);
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
                        meta.push((k, if k == "Description" { strip_tags(&value) } else { value }));
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
        if i % 64 == 0 && stop() {
            return Err(Error::Stopped);
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
            other => return Err(Error::Unsupported(other)),
        }
        if text.len() as u64 > limits.max_text {
            return Err(Error::TooLarge { max: limits.max_text });
        }
    }
    let is_image = |r: &[u8]| r.starts_with(&[0xFF, 0xD8]) || r.starts_with(b"\x89PNG") || r.starts_with(b"GIF8") || r.starts_with(b"BM");
    let mut images: Vec<Vec<u8>> = first_image.map_or_else(Vec::new, |f| (f..count).map(|i| record(i).filter(|r| is_image(r)).map(<[u8]>::to_vec).unwrap_or_default()).collect());
    // Past the last picture: nothing the HTML names.
    while images.last().is_some_and(Vec::is_empty) {
        images.pop();
    }
    let text = decode(&text, utf8);
    // PalmDOC is plain text, unless its text is HTML (as some converters write it).
    let lead = text.trim_start().get(..64).unwrap_or(text.trim_start()).to_ascii_lowercase();
    let html = mobi || lead.starts_with("<html") || lead.starts_with("<!doctype") || lead.starts_with("<?xml") || lead.starts_with("<body") || lead.starts_with("<p");
    Ok(Book { text, html, title, meta, images })
}

/// HTML's text: its tags removed (EXTH descriptions are often HTML).
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out
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
    if extra_flags & 1 != 0
        && let Some(&b) = rec.len().checked_sub(num + 1).and_then(|at| rec.get(at)) {
            num += (b & 3) as usize + 1;
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

