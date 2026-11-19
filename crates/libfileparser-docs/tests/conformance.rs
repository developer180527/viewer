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
    for (reader, file) in [(&DocxReader as &dyn Reader, "sample.docx"), (&OdtReader, "sample.odt"), (&EpubReader, "sample.epub"), (&RtfReader, "sample.rtf"), (&libfileparser_docs::OrgReader, "sample.org"), (&libfileparser_docs::HtmlReader, "sample.html")] {
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

/// PowerPoint and Word 97–2003, through office_oxide: slides as headings, their text,
/// a Slides table; a Word document's text.
#[test]
fn office_documents() {
    use libfileparser_docs::{DocReader, PptReader, PptxReader};
    for (reader, file, slides) in [(&PptxReader as &dyn Reader, "sample.pptx", Some(2)), (&PptReader, "sample.ppt", Some(2)), (&DocReader, "sample.doc", None)] {
        assert_conforms(reader, &[corpus(file)]);
        let cx = ParseContext::new();
        let doc = reader.open(&FileSource::from_bytes(file, corpus_bytes(file)), &cx).unwrap();
        let Ok(Content::Flow(flow)) = doc.facet(0, &cx) else { panic!("{file}: not formatted text") };
        let blocks = flow.blocks(0..1000).unwrap();
        assert!(blocks.len() > 2, "{file}: {blocks:?}");
        let names: Vec<String> = doc.facets().map(|f| f.name.clone()).collect();
        match slides {
            Some(n) => {
                assert_eq!(doc.metadata().iter().find(|(k, _)| k == "Slides").map(|(_, v)| v.clone()), Some(n.to_string()), "{file}");
                assert!(names.contains(&"Slides".to_string()), "{file}: {names:?}");
                assert_eq!(blocks.iter().filter(|b| matches!(b, libfileparser_core::Block::Heading { level: 2, .. })).count(), n, "{file}");
            }
            None => assert!(!names.contains(&"Slides".to_string())),
        }
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

/// Every reader added 2026-11-18/19 (markups, wikis, manuals, XML documents, mail,
/// books), on its corpus sample: the conformance suite, then formatted text with a
/// heading (and marks, where the format has them) as its first facet.
const FLOWING: &[(&str, &str, bool)] = &[
    ("rst", "sample.rst", true),
    ("djot", "sample.dj", true),
    ("asciidoc", "sample.adoc", true),
    ("typst", "sample.typ", true),
    ("textile", "sample.textile", true),
    ("ipynb", "sample.ipynb", true),
    ("mediawiki", "sample.mediawiki", true),
    ("creole", "sample.creole", true),
    ("dokuwiki", "sample.dokuwiki", true),
    ("jira", "sample.jira", true),
    ("txt2tags", "sample.t2t", true),
    ("muse", "sample.muse", true),
    ("haddock", "sample.haddock", true),
    ("xwiki", "sample.xwiki", true),
    ("tikiwiki", "sample.tikiwiki", true),
    ("twiki", "sample.twiki", true),
    ("zimwiki", "sample.zim", true),
    ("vimwiki", "sample.vimwiki", true),
    ("gemtext", "sample.gmi", false),
    ("man", "sample.1", true),
    ("latex", "sample.tex", true),
    ("texinfo", "sample.texi", true),
    ("pod", "sample.pod", true),
    ("fountain", "sample.fountain", true),
    ("docbook", "sample-docbook.xml", true),
    ("tei", "sample-tei.xml", true),
    ("fb2", "sample.fb2", true),
    ("odp", "sample.odp", true),
    ("mobi", "sample.mobi", true),
    ("eml", "sample.eml", true),
    ("mbox", "sample.mbox", true),
    ("mhtml", "sample.mht", true),
    ("msg", "sample.msg", true),
    ("hwp", "sample.hwpx", false),
];

fn reader(id: &str) -> &'static dyn Reader {
    *libfileparser_docs::readers().iter().find(|r| r.info().id == id).unwrap_or_else(|| panic!("no reader {id}"))
}

#[test]
fn every_new_reader_on_its_sample() {
    for &(id, file, marks) in FLOWING {
        let r = reader(id);
        let src = FileSource::from_bytes(file, corpus_bytes(file));
        // Each sample is detected as its own format, ahead of every other reader.
        let best = libfileparser_docs::detect(src.header(), src.name());
        assert_eq!(best.first().map(|(r, _)| r.info().id), Some(id), "{file} is detected as {:?}", best.first().map(|(r, _)| r.info().id));
        // Small documents fit tiny limits; `limits_stop_big_documents` checks a big one
        // doesn't (every one of these builds through the same counting builder).
        assert_conforms(r, &[corpus(file).fits_tiny_limits()]);
        let cx = ParseContext::new();
        let doc = r.open(&src, &cx).unwrap_or_else(|e| panic!("{file}: {e}"));
        let Ok(Content::Flow(flow)) = doc.facet(0, &cx) else { panic!("{file}: not formatted text") };
        let text = format!("{:?}", flow.blocks(0..1000).unwrap());
        assert!(text.contains("Heading") || id == "hwp" || id == "msg", "{file}: a heading in {text}");
        if marks {
            assert!(text.contains("Strong") || text.contains("Emphasis"), "{file}: marks in {text}");
        }
    }
}

#[test]
fn djvu_pages() {
    let r = reader("djvu");
    assert_conforms(r, &[corpus("sample.djvu")]);
    let cx = ParseContext::new();
    let doc = r.open(&FileSource::from_bytes("sample.djvu", corpus_bytes("sample.djvu")), &cx).unwrap();
    let Ok(Content::Pages(p)) = doc.facet(0, &cx) else { panic!("no pages") };
    let size = p.page_size(0).unwrap();
    let mut rgba = Vec::new();
    let (w, h) = p.render(0, 0.5, libfileparser_core::PageRect::whole(size), &mut rgba, &libfileparser_core::Cancel::new()).unwrap();
    assert_eq!(rgba.len(), (w * h * 4) as usize);
    // A scan: not one colour.
    assert!(rgba.chunks(4).any(|c| c[0] < 128) && rgba.chunks(4).any(|c| c[0] > 128));
}

#[test]
fn comics() {
    for file in ["sample.cbz", "sample.cbt", "sample.cb7", "sample.cbr"] {
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
        "comic" => vec![corpus("sample.cbz"), corpus("sample.cbt"), corpus("sample.cb7"), corpus("sample.cbr")],
        "djvu" => vec![corpus("sample.djvu")],
        id if FLOWING.iter().any(|(r, _, _)| *r == id) => FLOWING.iter().filter(|(r, _, _)| *r == id).map(|(_, f, _)| corpus(f)).collect(),
        "epub" => vec![corpus("sample.epub")],
        "docx" => vec![corpus("sample.docx")],
        "odt" => vec![corpus("sample.odt")],
        "rtf" => vec![corpus("sample.rtf")],
        "org" => vec![corpus("sample.org")],
        "html" => vec![corpus("sample.html")],
        "pptx" => vec![corpus("sample.pptx")],
        "ppt" => vec![corpus("sample.ppt")],
        "doc" => vec![corpus("sample.doc")],
        _ => Vec::new(),
    });
}

