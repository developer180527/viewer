//! blend-lite through its public API: a small file of every storage layout, built here,
//! the corpus's samples, and damaged files that fail cleanly.

use blend_lite::{Error, File, Limits, inflate, is_compressed, scene};

fn corpus(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/3d").join(name)).unwrap()
}

/// A legacy little-endian, 64-bit file: header, a REND block, then the preview.
fn with_preview(w: u32, h: u32) -> Vec<u8> {
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
fn preview_is_found_and_flipped_upright() {
    let bytes = with_preview(2, 2);
    let file = File::read(&bytes, &Limits::default()).unwrap();
    assert_eq!(file.header.version, "2.83");
    let p = file.preview().unwrap().unwrap();
    assert_eq!((p.width, p.height), (2, 2));
    assert_eq!(&p.rgba[..4], [0, 0, 255, 255], "top row first");
    assert_eq!(&p.rgba[8..12], [255, 0, 0, 255]);
}

#[test]
fn refusals() {
    let l = Limits::default();
    assert_eq!(File::read(b"not a blend", &l).err(), Some(Error::NotBlend));
    assert_eq!(File::read(&with_preview(2, 2), &l.with_blocks(1)).err(), Some(Error::TooLarge { what: "blocks", value: 2, max: 1 }));
    // An absurd preview size is refused, not allocated.
    let mut huge = with_preview(2, 2);
    let at = 12 + 24 + 72 + 24;
    huge[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(File::read(&huge, &l).unwrap().preview().is_err());
    // A compressed file over the limit stops at it.
    let gz = flate2_like(&vec![0u8; 1 << 20]);
    assert!(matches!(inflate(&gz, &l.with_inflated(1024).with_unpack(1, 16)), Err(Error::TooLarge { .. })));
}

/// A gzip file of stored (uncompressed) deflate blocks.
fn flate2_like(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
    let mut chunks = data.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        out.push(chunks.peek().is_none() as u8);
        out.extend_from_slice(&(c.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        out.extend_from_slice(c);
    }
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb88320 } else { crc >> 1 };
        }
    }
    out.extend_from_slice(&(!crc).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

#[test]
fn gzip_inflates_to_the_same_file() {
    let bytes = with_preview(2, 2);
    let gz = flate2_like(&bytes);
    assert!(is_compressed(&gz));
    assert_eq!(inflate(&gz, &Limits::default()).unwrap().as_deref(), Some(&bytes[..]));
    assert_eq!(inflate(&bytes, &Limits::default()).unwrap(), None);
}

#[test]
fn damaged_files_fail_cleanly() {
    let l = Limits::default();
    for name in ["packed.blend", "preview.blend"] {
        let good = corpus(name);
        let mut seed = 0x2545F4914F6CDD1Du64;
        for i in 0..600usize {
            let mut b = good.clone();
            if i % 3 == 0 {
                b.truncate(good.len() * (i % 97) / 97);
            } else {
                for _ in 0..16 {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let at = (seed as usize) % b.len();
                    b[at] = (seed >> 32) as u8;
                }
            }
            // Errors are fine; panics aren't.
            if let Ok(f) = File::read(&b, &l) {
                let _ = f.preview();
                let s = scene(&f);
                for m in &s.meshes {
                    assert!(m.corners.iter().all(|&c| (c as usize) < m.positions.len()));
                    assert!(m.face_sizes.iter().map(|&n| n as u64).sum::<u64>() <= m.corners.len() as u64);
                }
            }
        }
    }
}

#[test]
fn a_modern_file_has_its_mesh() {
    let bytes = corpus("blender-5.2.blend");
    let l = Limits::default();
    let inflated = inflate(&bytes, &l).unwrap();
    let file = File::read(inflated.as_deref().unwrap_or(&bytes), &l).unwrap();
    assert_eq!(file.header.version, "5.2");
    let s = scene(&file);
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    let mesh = &s.meshes[s.objects[0].mesh];
    assert_eq!((mesh.positions.len(), mesh.face_sizes.len()), (8, 6), "the default cube");
}
