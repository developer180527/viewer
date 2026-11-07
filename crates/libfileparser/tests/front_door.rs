//! The front door: the best reading, then Source, Package and Raw where they fit.

use std::io::Write;

use libfileparser::{Content, FileSource, FolderFiles, ParseContext, ParseError, Role};

fn corpus() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files").canonicalize().unwrap()
}

fn facets(doc: &libfileparser::Document) -> Vec<(Role, String)> {
    doc.facets().map(|f| (f.role, f.name.clone())).collect()
}

fn names(doc: &libfileparser::Document) -> Vec<String> {
    doc.facets().map(|f| f.name.clone()).collect()
}

fn cx() -> ParseContext {
    ParseContext::new().with_files(FolderFiles::within(corpus())).with_spawn(libfileparser::InlineSpawn)
}

#[test]
fn a_text_model_gets_its_source_in_its_language_and_raw() {
    let cx = cx();
    let doc = libfileparser::open(&FileSource::open(corpus().join("3d/cube-gltf/cube.gltf")).unwrap(), &cx).unwrap();
    assert_eq!(names(&doc), ["Scene", "glTF", "Dependencies", "Materials", "Textures", "Source", "Raw"]);
    let source = doc.facet_index("Source").unwrap();
    assert!(!doc.is_made(source), "generic facets are made when asked for");
    let Ok(Content::Text(t)) = doc.facet(source, &cx) else { panic!() };
    assert_eq!(t.info().language.as_deref(), Some("json"));
    let Ok(Content::Hex(raw)) = doc.facet(doc.facet_index("Raw").unwrap(), &cx) else { panic!() };
    assert_eq!(raw.len(), std::fs::metadata(corpus().join("3d/cube-gltf/cube.gltf")).unwrap().len());

    let doc = libfileparser::open(&FileSource::open(corpus().join("3d/tri.usda")).unwrap(), &cx).unwrap();
    let Ok(Content::Text(t)) = doc.facet(doc.facet_index("Source").unwrap(), &cx) else { panic!() };
    assert_eq!(t.info().language.as_deref(), Some("usd"));
}

#[test]
fn binary_files_get_raw_only_and_text_files_no_second_text() {
    let cx = cx();
    let doc = libfileparser::open(&FileSource::open(corpus().join("3d/cube.glb")).unwrap(), &cx).unwrap();
    assert_eq!(facets(&doc).last().unwrap(), &(Role::Raw, "Raw".to_string()));
    assert!(doc.facet_index("Source").is_none());

    let doc = libfileparser::open(&FileSource::from_bytes("notes.txt", b"hello\n".to_vec()), &cx).unwrap();
    assert_eq!(names(&doc), ["Text", "Raw"]);
}

#[test]
fn a_usdz_gets_its_package_and_entries_open_through_detection() {
    let cx = cx();
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut buf);
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    w.start_file("tri.usda", stored).unwrap();
    w.write_all(&std::fs::read(corpus().join("3d/tri.usda")).unwrap()).unwrap();
    w.finish().unwrap();
    let doc = libfileparser::open(&FileSource::from_bytes("tri.usdz", buf.into_inner()), &cx).unwrap();
    assert_eq!(doc.format().reader, "usd");
    assert!(names(&doc).ends_with(&["Package".to_string(), "Raw".to_string()]), "{:?}", names(&doc));
    let Ok(Content::Container(package)) = doc.facet(doc.facet_index("Package").unwrap(), &cx) else { panic!() };
    assert_eq!(package.entries(0..10).unwrap()[0].path, "tri.usda");
    // An entry is a file of its own, read by the best reader for it.
    let inner = libfileparser::open_entry(&package, 0, &cx).unwrap();
    assert_eq!((inner.format().reader, names(&inner)[0].as_str()), ("usd", "Scene"));

    // A plain zip is the archives' own: no second listing.
    let doc = libfileparser::open(&FileSource::from_bytes("a.zip", std::fs::read(corpus().join("hostile/fake.zip")).unwrap_or_default()), &cx);
    if let Ok(doc) = doc {
        assert!(doc.facet_index("Package").is_none());
    }
}

#[test]
fn nothing_reads_it_says_who_tried_and_raw_still_shows_it() {
    let cx = cx();
    let src = FileSource::from_bytes("mystery.bin", vec![0xde, 0xad, 0xbe, 0xef, 0, 1, 2]);
    let (result, attempts) = libfileparser::open_reporting(&src, &cx);
    assert!(matches!(result, Err(ParseError::NotThisFormat)));
    assert!(attempts.is_empty());
    assert_eq!(names(&libfileparser::open_raw(&src)), ["Raw"]);

    // A file a reader claims but can't read: the reason, and which readers tried.
    let (result, attempts) = libfileparser::open_reporting(&FileSource::open(corpus().join("hostile/truncated.glb")).unwrap(), &cx);
    assert!(result.is_err());
    assert_eq!(attempts.first().map(|a| a.reader), Some("gltf"));
}

#[test]
fn folders_and_forced_readers() {
    let cx = cx();
    let doc = libfileparser::open(&FileSource::folder(corpus().join("3d/cube-gltf")), &cx).unwrap();
    assert_eq!(names(&doc), ["Files"]);
    // Any enabled reader, by id, with the generic facets too.
    let doc = libfileparser::open_with("text", &FileSource::open(corpus().join("3d/tri.usda")).unwrap(), &cx).unwrap();
    assert_eq!(names(&doc), ["Text", "Raw"]);
    assert!(libfileparser::reader("usd").is_some() && libfileparser::reader("nope").is_none());
}
