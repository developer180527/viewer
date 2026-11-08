//! Every image reader passes the conformance suite (docs/design/libfileparser.md §9).

use libfileparser_core::{Content, FileSource, ParseContext, ParseError};
use libfileparser_testkit::{Sample, assert_conforms, tiny_limits};

fn corpus(path: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files").join(path)).unwrap()
}

#[test]
fn every_reader_conforms() {
    for reader in libfileparser_images::readers() {
        let samples = match reader.info().id {
            "image" => ["images/checker.png", "images/ramp.bmp", "images/ramp.pgm", "images/ramp.ppm"].map(|p| Sample::new(p.rsplit('/').next().unwrap(), corpus(p))).to_vec(),
            "svg" => vec![Sample::new("dot.svg", br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><circle cx="2" cy="2" r="2"/></svg>"#.to_vec()).fits_tiny_limits()],
            id => panic!("reader {id:?} has no conformance samples"),
        };
        assert_conforms(*reader, &samples);
    }
}

#[test]
fn hostile_images_fail_cleanly() {
    let cx = ParseContext::new();
    // Claims 100,000 × 100,000: refused at the header, nothing allocated.
    let huge = libfileparser_images::open(&FileSource::from_bytes("huge.png", corpus("hostile/huge-dimensions.png")), &cx);
    assert!(matches!(huge, Err(ParseError::TooLarge(_))), "{:?}", huge.err());
    // Over the context's limit: the same, at a smaller size.
    let tiny = ParseContext::new().with_limits(tiny_limits());
    assert!(matches!(libfileparser_images::open(&FileSource::from_bytes("c.png", corpus("images/checker.png")), &tiny), Err(ParseError::TooLarge(_))));
    // Truncated: whatever opens fails when decoded, never panics.
    if let Ok(doc) = libfileparser_images::open(&FileSource::from_bytes("t.png", corpus("hostile/truncated.png")), &cx)
        && let Ok(Content::Raster(r)) = doc.facet(0, &cx)
    {
        assert!(r.decode().is_err());
    }
}

fn corpus_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files")
}

/// Damaged thousands of ways (`LFP_FUZZ_ITERATIONS` for more), nothing panics or hangs:
/// headers claiming huge sizes, truncated pixel data, broken chunks.
#[test]
fn every_reader_survives_fuzzing() {
    libfileparser_testkit::assert_fuzz_clean(libfileparser_images::readers(), |id| {
        let reader = libfileparser_images::readers().iter().find(|r| r.info().id == id).copied().expect("a reader");
        let mut s = libfileparser_testkit::corpus_samples(&corpus_dir(), reader);
        if id == "svg" {
            s.push(Sample::new("dot.svg", br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><circle cx="2" cy="2" r="2"/></svg>"#.to_vec()));
        }
        s
    });
}
