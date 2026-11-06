#![cfg(not(windows))]

use viewer_core::testing::{pdf, pixel};
use viewer_core::{FileSource, PageRect, PageSource};
use viewer_pdf_mupdf::MupdfDocument;

fn open(bytes: Vec<u8>) -> MupdfDocument {
    MupdfDocument::open(FileSource::from_bytes("t.pdf", bytes), "application/pdf").unwrap()
}

#[test]
fn sizes_and_orientation() {
    let doc = open(pdf(&[(200.0, 100.0, 0), (200.0, 100.0, 90)]));
    assert_eq!(doc.page_count(), 2);
    let s0 = doc.page_size(0).unwrap();
    assert_eq!((s0.width, s0.height), (200.0, 100.0));
    // Rotated pages report their size as shown.
    let s1 = doc.page_size(1).unwrap();
    assert_eq!((s1.width, s1.height), (100.0, 200.0));

    let img = doc.render(0, 2.0, PageRect::whole(s0)).unwrap();
    assert_eq!((img.width, img.height), (400, 200));
    assert_eq!(pixel(&img.rgba, 400, 10, 10), [255, 0, 0, 255]);
    assert_eq!(pixel(&img.rgba, 400, 390, 190), [255, 255, 255, 255]);

    // A region: the bottom-right quarter alone is all white, the top-left all red.
    let br = doc.render(0, 1.0, PageRect { x: 100.0, y: 50.0, width: 100.0, height: 50.0 }).unwrap();
    assert!(br.rgba.chunks(4).all(|p| p == [255, 255, 255, 255]));
    let tl = doc.render(0, 1.0, PageRect { x: 0.0, y: 0.0, width: 100.0, height: 50.0 }).unwrap();
    assert!(tl.rgba.chunks(4).all(|p| p == [255, 0, 0, 255]));

    // Turned 90° clockwise, the red corner is now top-right.
    let img = doc.render(1, 1.0, PageRect::whole(s1)).unwrap();
    assert_eq!(pixel(&img.rgba, 100, 95, 5), [255, 0, 0, 255]);
    assert_eq!(pixel(&img.rgba, 100, 5, 5), [255, 255, 255, 255]);
}

#[test]
fn bad_input_fails_cleanly() {
    assert!(MupdfDocument::open(FileSource::from_bytes("x.pdf", b"not a pdf at all".to_vec()), "application/pdf").is_err());
    let doc = open(pdf(&[(10.0, 10.0, 0)]));
    assert!(doc.page_size(5).is_err());
    assert!(doc.render(0, 0.0, PageRect::whole(doc.page_size(0).unwrap())).is_err());
    assert!(doc.render(0, 10_000.0, PageRect::whole(doc.page_size(0).unwrap())).is_err());
}

#[test]
fn info_names_the_format() {
    let info = open(pdf(&[(10.0, 10.0, 0)])).info();
    assert_eq!(info.format.as_deref(), Some("PDF 1.4"));
}
