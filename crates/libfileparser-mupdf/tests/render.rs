#![cfg(not(windows))]

use libfileparser_core::{Cancel, FileSource, PageRect, PageSource, ParseError};
use libfileparser_mupdf::MupdfDocument;
use libfileparser_testkit::pdf::{pdf, pixel};

fn open(bytes: Vec<u8>) -> MupdfDocument {
    MupdfDocument::open(FileSource::from_bytes("t.pdf", bytes), "application/pdf").unwrap()
}

fn render(doc: &MupdfDocument, page: u64, scale: f32, region: PageRect) -> (u32, u32, Vec<u8>) {
    let mut rgba = Vec::new();
    let (w, h) = doc.render(page, scale, region, &mut rgba, &Cancel::new()).unwrap();
    (w, h, rgba)
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

    let (w, h, img) = render(&doc, 0, 2.0, PageRect::whole(s0));
    assert_eq!((w, h), (400, 200));
    assert_eq!(pixel(&img, 400, 10, 10), [255, 0, 0, 255]);
    assert_eq!(pixel(&img, 400, 390, 190), [255, 255, 255, 255]);

    // A region: the bottom-right quarter alone is all white, the top-left all red.
    let (.., br) = render(&doc, 0, 1.0, PageRect::new(100.0, 50.0, 100.0, 50.0));
    assert!(br.chunks(4).all(|p| p == [255, 255, 255, 255]));
    let (.., tl) = render(&doc, 0, 1.0, PageRect::new(0.0, 0.0, 100.0, 50.0));
    assert!(tl.chunks(4).all(|p| p == [255, 0, 0, 255]));

    // Turned 90° clockwise, the red corner is now top-right.
    let (.., img) = render(&doc, 1, 1.0, PageRect::whole(s1));
    assert_eq!(pixel(&img, 100, 95, 5), [255, 0, 0, 255]);
    assert_eq!(pixel(&img, 100, 5, 5), [255, 255, 255, 255]);
}

#[test]
fn bad_input_fails_cleanly() {
    assert!(matches!(MupdfDocument::open(FileSource::from_bytes("x.pdf", b"not a pdf at all".to_vec()), "application/pdf"), Err(ParseError::Corrupt { .. })));
    let doc = open(pdf(&[(10.0, 10.0, 0)]));
    assert!(matches!(doc.page_size(5), Err(ParseError::InvalidRequest(_))));
    let whole = PageRect::whole(doc.page_size(0).unwrap());
    let mut buf = Vec::new();
    assert!(doc.render(0, 0.0, whole, &mut buf, &Cancel::new()).is_err());
    assert!(doc.render(0, 10_000.0, whole, &mut buf, &Cancel::new()).is_err());
    let cancelled = Cancel::new();
    cancelled.cancel();
    assert!(matches!(doc.render(0, 1.0, whole, &mut buf, &cancelled), Err(ParseError::Cancelled)));
}

#[test]
fn info_names_the_format() {
    let info = open(pdf(&[(10.0, 10.0, 0)])).info();
    assert_eq!(info.format.as_deref(), Some("PDF 1.4"));
}

#[test]
fn text_is_found_where_it_is_drawn() {
    use libfileparser_testkit::pdf::pdf_text;
    let doc = open(pdf_text(300.0, 200.0, &[&[(20.0, 40.0, 20.0, "Hello world"), (20.0, 100.0, 10.0, "say HELLO")], &[(50.0, 150.0, 12.0, "nothing here")]]));
    let hits = doc.find_text(0, "hello").unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");
    let r = hits[0][0];
    // "Hello" at 20 pt, its baseline 40 pt down: the box sits above it, from x = 20.
    assert!((r.x - 20.0).abs() < 1.0 && r.y > 15.0 && r.y < 30.0 && r.y + r.height > 38.0 && r.y + r.height < 46.0, "{r:?}");
    assert!(r.width > 30.0 && r.width < 60.0, "{r:?}");
    assert!(hits[1][0].y > 85.0 && hits[1][0].y < 100.0, "{:?}", hits[1]);
    assert!(doc.find_text(1, "hello").unwrap().is_empty());
    assert!(doc.find_text(9, "hello").is_err());
}
