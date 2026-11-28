//! HEIC and AVIF through ImageIO, on files ImageIO wrote (tests/files): a photo's tile
//! grid assembled, its orientation applied, and damaged files failing cleanly.

#![cfg(target_vendor = "apple")]

use std::path::Path;

use libfileparser_core::{Content, FileSource, ParseContext, Reader};
use libfileparser_imageio::HeifReader;
use libfileparser_testkit::pdf::pixel;
use libfileparser_testkit::{Sample, assert_fuzz_clean};

fn open(name: &str) -> (u32, u32, Vec<u8>, String) {
    let src = FileSource::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/files").join(name)).unwrap();
    assert!(HeifReader.detect(src.header(), name).is_match());
    let cx = ParseContext::new();
    let doc = HeifReader.open(&src, &cx).unwrap();
    let Content::Raster(r) = doc.facet(0, &cx).unwrap() else { panic!("not a raster") };
    (r.width(), r.height(), r.decode().unwrap(), doc.format().name.clone())
}

#[test]
fn a_grid_of_tiles_is_one_image() {
    // 1100 × 700 is three tiles across, two down: a decoder showing one tile would be
    // 512 × 512, and all red.
    let (w, h, rgba, kind) = open("quadrants-grid.heic");
    assert_eq!((w, h), (1100, 700));
    assert_eq!(kind, "HEIC image");
    let near = |p: [u8; 4], want: [u8; 3]| p[..3].iter().zip(want).all(|(a, b)| a.abs_diff(b) < 40) && p[3] == 255;
    assert!(near(pixel(&rgba, w, 100, 100), [255, 0, 0]), "top left red");
    assert!(near(pixel(&rgba, w, 1000, 100), [0, 255, 0]), "top right green");
    assert!(near(pixel(&rgba, w, 100, 600), [0, 0, 255]), "bottom left blue");
    assert!(near(pixel(&rgba, w, 1000, 600), [255, 255, 255]), "bottom right white");
}

#[test]
fn a_turned_photo_is_shown_upright() {
    // Stored 400 × 200, red left and blue right, turned a quarter right to be shown.
    let (w, h, rgba, _) = open("turned-right.heic");
    assert_eq!((w, h), (200, 400));
    assert!(pixel(&rgba, w, 100, 50)[0] > 200, "red on top");
    assert!(pixel(&rgba, w, 100, 350)[2] > 200, "blue below");
}

#[test]
fn avif() {
    let (w, h, rgba, kind) = open("sample.avif");
    assert_eq!(kind, "AVIF image");
    assert_eq!(rgba.len(), w as usize * h as usize * 4);
}

#[test]
fn not_a_photo() {
    let mp4 = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/media/h264-aac.mp4")).unwrap();
    assert!(!HeifReader.detect(&mp4[..mp4.len().min(8192)], "clip.mp4").is_match());
    assert!(!HeifReader.detect(b"\x89PNG\r\n\x1a\n", "a.png").is_match());
}

#[test]
fn damaged_files_fail_cleanly() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/files");
    assert_fuzz_clean(&[&HeifReader], |_| {
        std::fs::read_dir(&dir).unwrap().flatten().map(|e| Sample::new(e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())).collect()
    });
}
