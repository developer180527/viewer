//! A format added from outside the crate, with only its public API: a reader of a small
//! made-up format (`SQR`, laid out like OFF) built from `libfileparser_3d::build`, joined to the crate's
//! readers by passing its own list to `-core`, passing the same conformance suite.
//!
//! It is the whole recipe for a 3D format (docs/design/libfileparser-3d.md §4): parse,
//! fill a scene from `new_scene`, describe the file's own structure as a tree whose
//! nodes have paths, key the scene's items by those paths (`SceneOrigins`), and hand it
//! all to `SceneDocument`. Dependencies, Materials, Textures, Source (in the language
//! `detect` names), Raw, the counts and the links between them come with it.

use libfileparser_3d::build::{self, SceneDocument};
use libfileparser_core::{Category, Content, Detection, Document, FacetInfo, FileSource, Model, ParseContext, ParseError, Reader, ReaderInfo, Result, Role};
use libfileparser_testkit::{Sample, assert_conforms};
use scene_data::glam::{Mat4, Vec3};
use scene_data::{Indices, MeshData, Node};

struct OffReader;

static SQR: ReaderInfo = ReaderInfo::new("sqr", "SQR model", Category::ThreeD).extensions(&["sqr"]);

impl Reader for OffReader {
    fn info(&self) -> &ReaderInfo {
        &SQR
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        build::detect_by(&SQR, header.starts_with(b"SQR"), name).with_language("sqr")
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let text = std::str::from_utf8(src.bytes()).map_err(|_| ParseError::NotThisFormat)?;
        let mut words = text.split_ascii_whitespace();
        if words.next() != Some("SQR") {
            return Err(ParseError::NotThisFormat);
        }
        let mut number = || words.next().and_then(|w| w.parse::<f32>().ok()).ok_or_else(|| ParseError::corrupt("truncated"));
        let (vertices, faces) = (number()? as u64, number()? as u64);
        number()?; // edges
        cx.limits.check(libfileparser_core::Limit::Geometry, vertices + faces)?;
        let positions = (0..vertices).map(|_| Ok(Vec3::new(number()?, number()?, number()?))).collect::<Result<Vec<_>>>()?;
        let mut indices = Vec::new();
        for _ in 0..faces {
            let corners: Vec<u32> = (0..number()? as u32).map(|_| number().map(|i| i as u32)).collect::<Result<_>>()?;
            for i in 1..corners.len().saturating_sub(1) {
                indices.extend([corners[0], corners[i], corners[i + 1]]);
            }
        }
        let (scene, mut builder) = build::new_scene(cx);
        let mesh = builder.add_mesh(MeshData::new(positions).with_indices(Indices::U32(indices.into()))).map_err(|e| build::scene_error(&e))?;
        let node = builder.add_node(Node::mesh(mesh, None, Mat4::IDENTITY, None)).map_err(|e| build::scene_error(&e))?;
        builder.finish();

        // The file's own structure, each node with a path…
        let mut elements = libfileparser_core::OwnedTree::new();
        elements.add_at(None, "Vertices", vertices.to_string(), "/vertices");
        elements.add_at(None, "Faces", faces.to_string(), "/faces");
        // …and the scene's items keyed by the same paths: the Elements facet and the
        // Scene now link both ways.
        let mut origins = build::SceneOrigins::new();
        origins.mesh("/faces", mesh);
        origins.node("/faces", node);
        SceneDocument::new(self, src, "SQR model", scene)
            .facets(move |doc| doc.with_facet(Role::Structure, "Elements", Content::Tree(std::sync::Arc::new(elements))))
            .origins(origins, "Elements")
            .build()
    }
}

const SQUARE: &str = "SQR\n4 1 0\n0 0 0\n1 0 0\n1 1 0\n0 1 0\n4 0 1 2 3\n";

#[test]
fn an_outside_reader_conforms_and_joins_the_category() {
    assert_conforms(&OffReader, &[Sample::new("square.sqr", SQUARE.as_bytes().to_vec()).fits_tiny_limits()]);

    let mut list = libfileparser_3d::readers().to_vec();
    list.push(&OffReader);
    let doc = libfileparser_core::open(&list, &FileSource::from_bytes("square.sqr", SQUARE.as_bytes().to_vec()), &ParseContext::new()).unwrap();
    assert_eq!(doc.format().reader, "sqr");
    assert!(doc.metadata().iter().any(|(k, v)| k == "Triangles" && v == "2"), "{:?}", doc.metadata());

    // Everything a built-in reader has: its structure, its Source in its language, Raw…
    let cx = ParseContext::new();
    assert_eq!(doc.facets().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Scene", "Elements", "Source", "Raw"]);
    let Ok(Content::Text(t)) = doc.facet(2, &cx) else { panic!() };
    assert_eq!(t.info().language.as_deref(), Some("sqr"));
    // …and links: the Faces node and the scene's mesh and node.
    let Ok(Content::Tree(tree)) = doc.facet(1, &cx) else { panic!() };
    let faces = libfileparser_core::Place::Node { facet: 1, node: tree.find("/faces").unwrap().unwrap() };
    let links = doc.links(&faces, &cx).unwrap();
    use libfileparser_core::{Place, SceneItem};
    assert!(links.contains(&Place::Scene { facet: 0, item: SceneItem::Mesh(0) }) && links.contains(&Place::Scene { facet: 0, item: SceneItem::Node(0) }), "{links:?}");
}

/// An app's own facet for USD files, added without touching the USD reader: it reads
/// the Stage facet the reader made and counts its top-level prims.
struct PrimCount;

impl libfileparser_core::FacetProvider for PrimCount {
    fn formats(&self) -> &[&str] {
        &["usd"]
    }

    fn facets(&self, _: &Document) -> Vec<FacetInfo> {
        vec![FacetInfo::new(Role::Inspector, "Prim count", Model::Table)]
    }

    fn make(&self, doc: &Document, _: &FacetInfo, cx: &ParseContext) -> libfileparser_core::Result<Content> {
        let stage = doc.facet_index("Stage").ok_or_else(|| libfileparser_core::ParseError::invalid("no Stage"))?;
        let Content::Tree(t) = doc.facet(stage, cx)? else { return Err(libfileparser_core::ParseError::invalid("Stage isn't a tree")) };
        let n = t.child_count(None)?.known();
        Ok(Content::Table(std::sync::Arc::new(libfileparser_core::OwnedTable::new(vec!["Top-level items".into()], vec![vec![libfileparser_core::Cell::Int(n as i64)]]))))
    }
}

/// A provider that panics: its facets are left out, and nothing else breaks.
struct Broken;

impl libfileparser_core::FacetProvider for Broken {
    fn formats(&self) -> &[&str] {
        &[]
    }

    fn facets(&self, _: &Document) -> Vec<FacetInfo> {
        panic!("a bug in an app's provider")
    }

    fn make(&self, _: &Document, _: &FacetInfo, _: &ParseContext) -> libfileparser_core::Result<Content> {
        unreachable!()
    }
}

#[test]
fn an_app_adds_its_own_facet_to_a_format_without_changing_its_reader() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/3d/tri.usda");
    let cx = ParseContext::new().with_facet_provider(PrimCount).with_facet_provider(Broken);
    let doc = libfileparser_3d::open(&FileSource::open(&path).unwrap(), &cx).unwrap();
    let names: Vec<_> = doc.facets().map(|f| f.name.clone()).collect();
    // Before the file's own Source and Raw, which stay last.
    assert_eq!(names[names.len() - 3..], ["Prim count", "Source", "Raw"]);
    let Ok(Content::Table(t)) = doc.facet(doc.facet_index("Prim count").unwrap(), &cx) else { panic!() };
    assert_eq!(t.rows(0..1).unwrap(), [vec![libfileparser_core::Cell::Int(2)]]);
    // Other formats don't get it.
    let stl = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/3d/cube.stl");
    let doc = libfileparser_3d::open(&FileSource::open(&stl).unwrap(), &cx).unwrap();
    assert!(doc.facet_index("Prim count").is_none());
}
