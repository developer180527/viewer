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

/// A text 3D format of an app's own, `.tri`: "tri" then nine numbers, one triangle.
struct TriReader;

static TRI: libfileparser::ReaderInfo = libfileparser::ReaderInfo::new("tri", "Triangle", libfileparser::Category::ThreeD).extensions(&["tri"]);

impl libfileparser::Reader for TriReader {
    fn info(&self) -> &libfileparser::ReaderInfo {
        &TRI
    }

    fn detect(&self, header: &[u8], name: &str) -> libfileparser::Detection {
        libfileparser_3d::build::detect_by(&TRI, header.starts_with(b"tri\n"), name).with_language("tri")
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> libfileparser::Result<libfileparser::Document> {
        use scene_data::glam::{Mat4, Vec3};
        let text = std::str::from_utf8(src.bytes()).map_err(|_| ParseError::NotThisFormat)?;
        let v: Vec<f32> = text.split_whitespace().skip(1).filter_map(|n| n.parse().ok()).collect();
        if v.len() != 9 {
            return Err(ParseError::corrupt("a triangle is nine numbers"));
        }
        let (scene, mut b) = libfileparser_3d::build::new_scene(cx);
        let positions: Vec<Vec3> = v.chunks(3).map(|c| Vec3::new(c[0], c[1], c[2])).collect();
        let mesh = b.add_mesh(scene_data::MeshData::new(positions)).map_err(|e| libfileparser_3d::build::scene_error(&e))?;
        b.add_node(scene_data::Node::mesh(mesh, None, Mat4::IDENTITY, None)).map_err(|e| libfileparser_3d::build::scene_error(&e))?;
        b.finish();
        libfileparser_3d::build::SceneDocument::new(self, src, "Triangle", scene).build()
    }
}

#[test]
fn an_app_adds_a_format_to_the_front_door_or_replaces_one() {
    let cx = cx();
    let src = FileSource::from_bytes("one.tri", b"tri\n0 0 0\n1 0 0\n0 1 0\n".to_vec());
    // Without it, a .tri is just text.
    assert_eq!(names(&libfileparser::open(&src, &cx).unwrap()), ["Text", "Raw"]);
    // With it: a scene, its Source in its own language, its Raw.
    let lib = libfileparser::Library::new().reader(&TriReader);
    let doc = lib.open(&src, &cx).unwrap();
    assert_eq!(doc.format().reader, "tri");
    assert_eq!(names(&doc), ["Scene", "Source", "Raw"]);
    let Ok(Content::Text(t)) = doc.facet(1, &cx) else { panic!() };
    assert_eq!(t.info().language.as_deref(), Some("tri"));
    // Ahead of plain text, behind the built-ins.
    let ids: Vec<_> = lib.readers().iter().map(|r| r.info().id).collect();
    assert!(ids.iter().position(|&i| i == "tri") < ids.iter().position(|&i| i == "text"));
    // Replacing a built-in by id.
    let lib = libfileparser::Library::new().replace("stl", &TriReader);
    assert!(lib.get("stl").is_none() && lib.get("tri").is_some());
    assert_eq!(lib.readers().len(), libfileparser::readers().len());
}

#[test]
fn owners_open_their_formats_and_other_categories_add_their_reading() {
    let cx = cx();
    // An SVG is an image (-images owns it); -data's XML tree comes after, before Source and Raw.
    let svg = FileSource::from_bytes("logo.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"4\"><rect width=\"4\" height=\"4\"/></svg>".to_vec());
    let doc = libfileparser::open(&svg, &cx).unwrap();
    assert_eq!(doc.format().reader, "svg");
    let xml = doc.facet_index("XML").expect("the XML reading");
    assert_eq!(facets(&doc)[xml..], [(Role::Structure, "XML".into()), (Role::Source, "Source".into()), (Role::Raw, "Raw".into())]);
    assert!(!doc.is_made(xml), "made when asked for");
    let Ok(Content::Tree(tree)) = doc.facet(xml, &cx) else { panic!() };
    assert!(tree.find("/svg/rect/@width").unwrap().is_some());
    // Used alone, -data opens it as XML.
    assert_eq!(libfileparser::open_with("xml", &svg, &cx).unwrap().format().reader, "xml");

    // Data files open as data, not text; broken ones as text.
    let json = FileSource::from_bytes("a.json", b"{\"a\": 1}".to_vec());
    assert_eq!(names(&libfileparser::open(&json, &cx).unwrap()), ["Tree", "Source", "Raw"]);
    let broken = FileSource::from_bytes("a.json", b"{\"a\": ".to_vec());
    assert_eq!(libfileparser::open(&broken, &cx).unwrap().format().reader, "text");
}
