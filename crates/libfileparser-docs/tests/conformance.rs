//! Every docs reader passes the conformance suite (docs/design/libfileparser.md §9).

use libfileparser_core::{Content, FileSource, ParseContext, ParseError, Reader};
use libfileparser_docs::{ComicReader, DocxReader, EpubReader, MarkdownReader, OdtReader, PdfReader, RtfReader};
use libfileparser_testkit::pdf::pdf;
use libfileparser_testkit::{Sample, assert_conforms};

#[test]
fn markdown() {
    let md = Sample::new("notes.md", "# Title\n\nSome *text*, and a [link](x.md).\n\n- one\n- two\n").fits_tiny_limits();
    assert_conforms(&MarkdownReader, &[md]);
    let doc = MarkdownReader.open(&FileSource::from_bytes("a.md", b"# A\n\nb\n".to_vec()), &ParseContext::new()).unwrap();
    assert!(matches!(doc.facet(0, &ParseContext::new()), Ok(Content::Flow(_))));
    assert!(doc.facets().any(|f| f.name == "Source"), "its source as text too");
}

#[test]
#[cfg(all(feature = "mupdf", not(windows)))]
fn pdf_through_mupdf() {
    assert_conforms(&PdfReader, &[Sample::new("two.pdf", pdf(&[(612.0, 792.0, 0), (612.0, 792.0, 90)]))]);
    let doc = PdfReader.open(&FileSource::from_bytes("t.pdf", pdf(&[(10.0, 10.0, 0)])), &ParseContext::new()).unwrap();
    assert!(doc.metadata().iter().any(|(k, v)| k == "Rendered by" && v == "MuPDF"));
}

/// Without MuPDF, a PDF is still recognised and named, then refused plainly, so an
/// app can say why, or register an engine of its own.
#[test]
#[cfg(not(all(feature = "mupdf", not(windows))))]
fn pdf_without_mupdf_is_refused_plainly() {
    let src = FileSource::from_bytes("t.pdf", pdf(&[(10.0, 10.0, 0)]));
    assert!(PdfReader.detect(src.header(), src.name()).is_match());
    assert!(matches!(PdfReader.open(&src, &ParseContext::new()), Err(ParseError::Unsupported(_))));
}

#[test]
fn not_a_pdf_is_not_this_format() {
    let src = FileSource::from_bytes("x.pdf", b"hello".to_vec());
    assert!(matches!(PdfReader.open(&src, &ParseContext::new()), Err(ParseError::NotThisFormat)));
}

fn corpus(name: &str) -> Sample {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/docs").join(name);
    Sample::new(name, std::fs::read(path).unwrap())
}

#[test]
fn flowing_documents() {
    for (reader, file) in [(&DocxReader as &dyn Reader, "sample.docx"), (&OdtReader, "sample.odt"), (&EpubReader, "sample.epub"), (&RtfReader, "sample.rtf"), (&libfileparser_docs::OrgReader, "sample.org")] {
        // Small documents fit tiny limits; `limits_stop_big_documents` checks a big one doesn't.
        assert_conforms(reader, &[corpus(file).fits_tiny_limits()]);
        let doc = reader.open(&FileSource::from_bytes(file, corpus_bytes(file)), &ParseContext::new()).unwrap();
        let Ok(Content::Flow(flow)) = doc.facet(0, &ParseContext::new()) else { panic!("{file}: not formatted text") };
        let blocks = flow.blocks(0..100).unwrap();
        let text: String = blocks.iter().map(|b| format!("{b:?}")).collect();
        assert!(text.contains("Heading"), "{file}: a heading in {blocks:?}");
        assert!(text.contains("Strong") || text.contains("Emphasis"), "{file}: marks in {blocks:?}");
    }
}

/// A document with more blocks than the limit allows stops with `TooLarge`, in every
/// formatted-text reader (they share the builder that counts).
#[test]
fn limits_stop_big_documents() {
    let mut limits = libfileparser_testkit::tiny_limits();
    limits.allocation = 1 << 20;
    let cx = ParseContext::new().with_limits(limits);
    let md: String = (0..100).map(|i| format!("Paragraph {i}.\n\n")).collect();
    let r = MarkdownReader.open(&FileSource::from_bytes("big.md", md.into_bytes()), &cx);
    assert!(matches!(r, Err(ParseError::TooLarge(_))), "{:?}", r.err());
    let rtf: String = format!("{{\\rtf1 {}}}", (0..100).map(|i| format!("p{i}\\par ")).collect::<String>());
    assert!(matches!(RtfReader.open(&FileSource::from_bytes("big.rtf", rtf.into_bytes()), &cx), Err(ParseError::TooLarge(_))));
}

#[test]
fn comics() {
    for file in ["sample.cbz", "sample.cbt"] {
        assert_conforms(&ComicReader, &[corpus(file)]);
        let doc = ComicReader.open(&FileSource::from_bytes(file, corpus_bytes(file)), &ParseContext::new()).unwrap();
        let Ok(Content::Pages(p)) = doc.facet(0, &ParseContext::new()) else { panic!("{file}: no pages") };
        assert_eq!(p.page_count(), 3);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Corpus Comic"));
        let mut rgba = Vec::new();
        let size = p.page_size(0).unwrap();
        assert_eq!((size.width, size.height), (48.0, 64.0));
        let (w, h) = p.render(0, 0.5, libfileparser_core::PageRect::whole(size), &mut rgba, &libfileparser_core::Cancel::new()).unwrap();
        assert_eq!((w, h, rgba.len()), (24, 32, 24 * 32 * 4));
    }
}

#[test]
#[cfg(all(feature = "mupdf", not(windows)))]
fn xps_through_mupdf() {
    assert_conforms(&libfileparser_docs::XpsReader, &[corpus("sample.xps").fits_tiny_limits()]);
}

fn corpus_bytes(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/docs").join(name)).unwrap()
}

/// Damaged Markdown and PDFs (when MuPDF is built in) open or fail cleanly: no panic,
/// no hang.
#[test]
fn fuzz() {
    libfileparser_testkit::assert_fuzz_clean(libfileparser_docs::readers(), |id| match id {
        "markdown" => vec![Sample::new("a.md", "# T\n\n> quote\n\n```rust\nfn x() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n")],
        "pdf" => vec![Sample::new("p.pdf", pdf(&[(200.0, 100.0, 0), (100.0, 200.0, 90)]))],
        "xps" => vec![corpus("sample.xps")],
        "comic" => vec![corpus("sample.cbz"), corpus("sample.cbt")],
        "epub" => vec![corpus("sample.epub")],
        "docx" => vec![corpus("sample.docx")],
        "odt" => vec![corpus("sample.odt")],
        "rtf" => vec![corpus("sample.rtf")],
        "org" => vec![corpus("sample.org")],
        _ => Vec::new(),
    });
}
