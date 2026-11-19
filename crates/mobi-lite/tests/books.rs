//! Books built here (uncompressed, PalmDOC, protected, unknown compression), the
//! corpus's sample, damage, and libmobi's real samples when the real corpus is there.

use mobi_lite::{Error, Limits, is_mobi, read};

/// A Palm database of `kind` with `records`.
fn palm(kind: &[u8; 8], name: &str, records: &[Vec<u8>]) -> Vec<u8> {
    let mut head = vec![0u8; 78];
    head[..name.len()].copy_from_slice(name.as_bytes());
    head[60..68].copy_from_slice(kind);
    head[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
    let mut offset = 78 + 8 * records.len() + 2;
    let mut out = head;
    for (i, r) in records.iter().enumerate() {
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&((i * 2) as u32).to_be_bytes());
        offset += r.len();
    }
    out.extend_from_slice(&[0, 0]);
    records.iter().for_each(|r| out.extend_from_slice(r));
    out
}

/// A PalmDOC header: compression, text length, record count, encryption.
fn palmdoc_header(compression: u16, len: usize, records: u16, encryption: u16) -> Vec<u8> {
    let mut h = Vec::new();
    for v in [compression, 0] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    h.extend_from_slice(&(len as u32).to_be_bytes());
    for v in [records, 4096, encryption, 0] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    h
}

fn no_stop() -> bool {
    false
}

#[test]
fn an_uncompressed_palmdoc() {
    let text = b"Hello, plain world.".to_vec();
    let book = palm(b"TEXtREAd", "Plain_Book", &[palmdoc_header(1, text.len(), 1, 0), text]);
    assert!(is_mobi(&book));
    let b = read(&book, &Limits::default(), &no_stop).unwrap();
    assert_eq!(b.text, "Hello, plain world.");
    assert!(!b.html);
    assert_eq!(b.title, "Plain Book");
}

#[test]
fn palmdoc_compression_with_a_back_reference() {
    // "abcabcabc": literals a, b, c, then a copy of distance 3, length 6 (0x80 | …),
    // then " ", "x" as a space-plus-character byte (0xC0 ^ 'x').
    let dist = 3u16;
    let len = 6u16;
    let pair = 0x8000 | (dist << 3) | (len - 3);
    let mut packed = vec![b'a', b'b', b'c'];
    packed.extend_from_slice(&pair.to_be_bytes());
    packed.push(b'x' ^ 0x80);
    // A run of literal bytes, as a 1..=8 count.
    packed.extend_from_slice(&[2, 0xE9, b'!']);
    let book = palm(b"TEXtREAd", "Packed", &[palmdoc_header(2, 12, 1, 0), packed]);
    let b = read(&book, &Limits::default(), &no_stop).unwrap();
    assert_eq!(b.text, "abcabcabc xé!");
}

#[test]
fn the_corpus_sample_with_its_metadata() {
    let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/files/docs/sample.mobi")).unwrap();
    let b = read(&bytes, &Limits::default(), &no_stop).unwrap();
    assert!(b.html);
    assert_eq!(b.title, "Corpus Kindle book");
    assert!(b.meta.contains(&("Author", "Corpus Author".to_string())));
    assert!(b.text.contains("<b>bold</b>"));
}

#[test]
fn refusals() {
    let text = b"secret".to_vec();
    assert_eq!(read(&palm(b"TEXtREAd", "x", &[palmdoc_header(1, 6, 1, 2), text.clone()]), &Limits::default(), &no_stop), Err(Error::Protected));
    assert_eq!(read(&palm(b"TEXtREAd", "x", &[palmdoc_header(9, 6, 1, 0), text.clone()]), &Limits::default(), &no_stop), Err(Error::Unsupported(9)));
    assert_eq!(read(&palm(b"TEXtREAd", "x", &[palmdoc_header(1, 6, 1, 0), text.clone()]), &Limits { max_text: 3 }, &no_stop), Err(Error::TooLarge { max: 3 }));
    let many: Vec<Vec<u8>> = std::iter::once(palmdoc_header(1, 6 * 200, 200, 0)).chain((0..200).map(|_| text.clone())).collect();
    assert_eq!(read(&palm(b"TEXtREAd", "x", &many), &Limits::default(), &|| true), Err(Error::Stopped));
    assert_eq!(read(b"not a book at all, nothing here, not even a palm database header", &Limits::default(), &no_stop), Err(Error::NotMobi));
    assert!(!is_mobi(&[0u8; 78]));
}

/// The sample, damaged thousands of ways: an error or a book, never a panic, and quick.
#[test]
fn damage_never_panics() {
    let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/files/docs/sample.mobi")).unwrap();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let t = std::time::Instant::now();
    for _ in 0..5_000 {
        let mut b = bytes.clone();
        for _ in 0..(next() % 8 + 1) {
            let at = (next() as usize) % b.len();
            match next() % 3 {
                0 => b[at] = next() as u8,
                1 => b.truncate(at.max(1)),
                _ => b.splice(at..at, std::iter::repeat_n(next() as u8, (next() % 64) as usize)).for_each(drop),
            }
        }
        let _ = std::panic::catch_unwind(|| read(&b, &Limits { max_text: 1 << 20 }, &no_stop)).expect("no panic");
    }
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
}

/// libmobi's samples (Huffman compression, dictionaries, cp1252), when `corpus/real/`
/// has been fetched (corpus/fetch-real.sh): every one that isn't protected reads.
#[test]
fn real_books_when_present() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/real/docs/mobi");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let bytes = std::fs::read(e.path()).unwrap();
        match read(&bytes, &Limits::default(), &no_stop) {
            Ok(b) => assert!(!b.text.trim().is_empty(), "{name}: no text"),
            Err(Error::Protected) => assert!(name.contains("drm"), "{name} refused as protected"),
            Err(other) => panic!("{name}: {other}"),
        }
    }
}
