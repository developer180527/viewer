//! The text reader passes the conformance suite (docs/design/libfileparser.md §9).

use libfileparser_core::{Content, FileSource, ParseContext, Role};
use libfileparser_testkit::{Sample, assert_conforms};

fn corpus(path: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files").join(path)).unwrap()
}

#[test]
fn every_reader_conforms() {
    // A few lines' index fits even the tiny limits.
    let samples = [
        Sample::new("people.tsv", corpus("data/delimited/people.tsv")).fits_tiny_limits(),
        Sample::new("tri.usda", corpus("3d/tri.usda")).fits_tiny_limits(),
        Sample::new("notes.txt", b"one\ntwo\r\nthree".to_vec()).fits_tiny_limits(),
    ];
    for reader in libfileparser_text::readers() {
        assert_conforms(*reader, &samples);
    }
}

#[test]
fn text_opens_by_line_in_its_language() {
    let cx = ParseContext::new();
    let doc = libfileparser_text::open(&FileSource::from_bytes("main.rs", b"fn main() {\r\n}\n".to_vec()), &cx).unwrap();
    assert_eq!(doc.facets().map(|f| (f.role, f.name.as_str())).collect::<Vec<_>>(), [(Role::Primary, "Text"), (Role::Raw, "Raw")]);
    let Ok(Content::Text(t)) = doc.facet(0, &cx) else { panic!() };
    assert_eq!(t.info().language.as_deref(), Some("rust"));
    let lines: Vec<Vec<u8>> = t.lines(0..10).unwrap().iter().map(|l| l.as_slice().to_vec()).collect();
    assert_eq!(lines, [b"fn main() {".to_vec(), b"}".to_vec()]);
    // Binary isn't text: the next reader gets it.
    assert!(libfileparser_text::open(&FileSource::from_bytes("x.txt", b"\0\x01\x02".to_vec()), &cx).is_err());
    // A line index past the limits isn't made.
    let tiny = ParseContext::new().with_limits(libfileparser_testkit::tiny_limits());
    let many = FileSource::from_bytes("big.log", vec![b'\n'; 10_000]);
    assert!(matches!(libfileparser_text::open(&many, &tiny), Err(libfileparser_core::ParseError::TooLarge(_))));
    // The Source facet: for text only, in the language asked for or found.
    let usda = FileSource::from_bytes("tri.usda", b"#usda 1.0\n".to_vec());
    let Some(Content::Text(s)) = libfileparser_text::source(&usda, None, &cx).unwrap() else { panic!() };
    assert_eq!(s.info().language.as_deref(), Some("usd"));
    assert!(libfileparser_text::source(&FileSource::from_bytes("a.glb", b"glTF\x02\0\0\0".to_vec()), None, &cx).unwrap().is_none());
}

fn corpus_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files")
}

/// Damaged thousands of ways (`LFP_FUZZ_ITERATIONS` for more), nothing panics or hangs.
#[test]
fn every_reader_survives_fuzzing() {
    libfileparser_testkit::assert_fuzz_clean(libfileparser_text::readers(), |id| {
        let reader = libfileparser_text::readers().iter().find(|r| r.info().id == id).copied().expect("a reader");
        let mut s = vec![Sample::new("notes.txt", b"one\ntwo\r\nthree".to_vec()), Sample::new("main.rs", b"fn main() {\n    println!(\"hi\");\n}\n".to_vec())];
        s.extend(libfileparser_testkit::corpus_samples(&corpus_dir(), reader));
        s
    });
}
