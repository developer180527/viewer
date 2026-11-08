#![cfg(target_vendor = "apple")]

use libfileparser_core::{Cancel, FileSource, PageRect, PageSource, ParseError};
use libfileparser_pdfkit::{PdfKitDocument, PdfKitReader};
use libfileparser_testkit::pdf::{pdf, pixel};

fn open(bytes: Vec<u8>) -> PdfKitDocument {
    PdfKitDocument::open(&FileSource::from_bytes("t.pdf", bytes)).unwrap()
}

fn render(doc: &PdfKitDocument, page: u64, scale: f32, region: PageRect) -> (u32, u32, Vec<u8>) {
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
    assert!(matches!(PdfKitDocument::open(&FileSource::from_bytes("x.pdf", b"not a pdf at all".to_vec())), Err(ParseError::Corrupt { .. })));
    let doc = open(pdf(&[(10.0, 10.0, 0)]));
    assert!(matches!(doc.page_size(5), Err(ParseError::InvalidRequest(_))));
}

#[test]
fn the_reader_conforms_and_shapes_documents_as_docs_does() {
    let sample = libfileparser_testkit::Sample::new("two.pdf", pdf(&[(612.0, 792.0, 0), (612.0, 792.0, 0)]));
    libfileparser_testkit::assert_conforms(&PdfKitReader, &[sample]);
    let doc = libfileparser_core::Reader::open(&PdfKitReader, &FileSource::from_bytes("t.pdf", pdf(&[(10.0, 10.0, 0)])), &libfileparser_core::ParseContext::new()).unwrap();
    assert!(doc.metadata().iter().any(|(k, v)| k == "Rendered by" && v == "PDFKit"));
}
