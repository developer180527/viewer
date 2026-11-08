//! Every docs reader passes the conformance suite (docs/design/libfileparser.md §9).

use libfileparser_core::{Content, FileSource, ParseContext, ParseError, Reader};
use libfileparser_docs::{MarkdownReader, PdfReader};
use libfileparser_testkit::pdf::pdf;
use libfileparser_testkit::{Sample, assert_conforms};

#[test]
fn markdown() {
    let md = Sample::new("notes.md", "# Title\n\nSome *text*, and a [link](x.md).\n\n- one\n- two\n").fits_tiny_limits();
    assert_conforms(&MarkdownReader, &[md]);
    let doc = MarkdownReader.open(&FileSource::from_bytes("a.md", b"# A\n\nb\n".to_vec()), &ParseContext::new()).unwrap();
    assert!(matches!(doc.facet(0, &ParseContext::new()), Ok(Content::Markdown(_))));
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

/// Damaged Markdown and PDFs (when MuPDF is built in) open or fail cleanly: no panic,
/// no hang.
#[test]
fn fuzz() {
    libfileparser_testkit::assert_fuzz_clean(libfileparser_docs::readers(), |id| match id {
        "markdown" => vec![Sample::new("a.md", "# T\n\n> quote\n\n```rust\nfn x() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n")],
        "pdf" => vec![Sample::new("p.pdf", pdf(&[(200.0, 100.0, 0), (100.0, 200.0, 90)]))],
        _ => Vec::new(),
    });
}
