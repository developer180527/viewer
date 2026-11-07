//! What particular readers make of particular files, through the public API.

use libfileparser_3d::{GltfReader, ObjReader, UsdReader};
use libfileparser_core::{Confidence, Content, Document, FileSource, ParseContext, Reader};
use scene_data::CpuScene;

    const CUBE_OBJ: &str = "\
o cube
v -1 -1 -1
v  1 -1 -1
v  1  1 -1
v -1  1 -1
v -1 -1  1
v  1 -1  1
v  1  1  1
v -1  1  1
usemtl missing
f 1 4 3 2
f 5 6 7 8
f 1 2 6 5
f 2 3 7 6
f 3 4 8 7
f 4 1 5 8
";

    fn scene(doc: &Document) -> CpuScene {
        let Ok(Content::Scene(s)) = doc.facet(0, &ParseContext::new()) else { panic!("expected a scene") };
        s.scene().clone()
    }

    #[test]
    fn obj_quads_are_triangulated_and_unlit_materials_stay_clay() {
        let src = FileSource::from_bytes("cube.obj", CUBE_OBJ.as_bytes().to_vec());
        let doc = ObjReader.open(&src, &ParseContext::new()).unwrap();
        let scene = scene(&doc);
        let read = scene.read();
        assert_eq!(read.meshes().iter().map(|m| m.info.primitives).sum::<u64>(), 12);
        let b = read.bounds().unwrap();
        assert!((b.size() - scene_data::glam::Vec3::splat(2.0)).length() < 1e-4);
        // `usemtl missing` names a material that no .mtl defines: no colour, so clay.
        assert!(read.materials().iter().all(|m| m.material.params.get("base_color").is_none()));
    }

    #[test]
    fn usd_meshes_split_by_material_in_y_up_metres() {
        let src = FileSource::from_bytes("scene.usda", include_bytes!("../../usd-lite/tests/fixtures/scene.usda").to_vec());
        assert_eq!(UsdReader.detect(src.header(), src.name()).confidence, Confidence::Magic);
        let doc = UsdReader.open(&src, &ParseContext::new()).unwrap();
        let scene = scene(&doc);
        let read = scene.read();
        // The quad (Red) and the triangle (its GeomSubset, Textured) are separate meshes;
        // the hidden prim adds nothing.
        assert_eq!(read.meshes().len(), 2);
        assert_eq!(read.materials().len(), 2);
        let names: Vec<_> = read.materials().iter().map(|m| m.material.name.as_str()).collect();
        assert!(names.contains(&"Red") && names.contains(&"Textured"), "{names:?}");
        // /World is at (10, 0, 0) cm, Z up: 0.1 m along X in Y-up metres. Points span
        // 0..1 (×2 scale, rotated 90° about the up axis), so everything stays within
        // ~0.1 m of it, and the up axis is now Y.
        let b = read.bounds().unwrap();
        assert!((b.center().x - 0.08).abs() < 0.05, "{b:?}");
        assert!(b.size().y.abs() < 1e-3, "flat quad: no height after Z-up → Y-up, {b:?}");
        // Its reference (../other.usda) doesn't exist: recorded, and the rest still loads.
        assert!(doc.dependencies().with_status(libfileparser_core::DependencyStatus::Missing).iter().any(|d| d.ends_with("other.usda")), "{:?}", doc.dependencies());
    }

    #[test]
    fn files_with_no_geometry_fall_through() {
        // A compiler object file named .obj: ufbx reads it as an empty OBJ, which must
        // fail so the front door falls back to the next reader (hex).
        let coff = FileSource::from_bytes("main.obj", vec![0x4c, 0x01, 0x03, 0x00, 0, 0, 0, 0, 0xff, 0xfe, 0x00]);
        assert!(ObjReader.open(&coff, &ParseContext::new()).err().expect("an empty OBJ fails").try_next_reader());
    }

    #[test]
    fn a_usdz_outranks_a_plain_zip_and_text_formats_name_their_language() {
        let usdz = b"PK\x03\x04";
        assert_eq!(UsdReader.detect(usdz, "a.usdz").confidence, Confidence::Certain);
        assert!(!UsdReader.detect(usdz, "a.zip").is_match(), "a plain zip isn't USD");
        assert_eq!(UsdReader.detect(b"#usda 1.0\n", "a.usda").language, Some("usd"));
        assert_eq!(GltfReader.detect(b"{", "a.gltf").language, Some("json"));
    }

fn corpus(path: &str) -> FileSource {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files").join(path);
    FileSource::open(&path).unwrap()
}

#[test]
fn ascii_fbx_and_blend_previews_open() {
    let fbx = libfileparser_3d::open(&corpus("3d/tri.fbx"), &ParseContext::new()).unwrap();
    assert_eq!((fbx.format().reader, fbx.format().name.as_str()), ("fbx", "FBX (ASCII)"));
    assert_eq!(scene(&fbx).read().meshes().iter().map(|m| m.info.primitives).sum::<u64>(), 1);

    let blend = libfileparser_3d::open(&corpus("3d/preview.blend"), &ParseContext::new()).unwrap();
    let Ok(Content::Raster(r)) = blend.facet(0, &ParseContext::new()) else { panic!("expected the preview") };
    assert_eq!((r.width(), r.height()), (4, 4));
}

/// A corpus model opened as a viewer opens one: its neighbours readable.
fn open_in_corpus(path: &str, spawn: impl libfileparser_core::Spawn + 'static) -> (Document, ParseContext) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files");
    let cx = ParseContext::new().with_files(libfileparser_core::FolderFiles::within(&root)).with_spawn(spawn);
    (libfileparser_3d::open(&corpus(path), &cx).unwrap(), cx)
}

/// The rows of the facet named `name`, or `None` when the document has no such facet.
fn table(doc: &Document, cx: &ParseContext, name: &str) -> Option<Vec<Vec<String>>> {
    let i = doc.facets().position(|f| f.name == name)?;
    let Ok(Content::Table(t)) = doc.facet(i, cx) else { panic!("{name} isn't a table") };
    Some(t.rows(0..100).unwrap())
}

fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|c| c.to_string()).collect()
}

#[test]
fn dependencies_say_what_each_file_is_for_and_where_it_was_found() {
    let (doc, cx) = open_in_corpus("3d/cube-gltf/cube.gltf", libfileparser_core::InlineSpawn);
    let facets: Vec<_> = doc.facets().map(|f| (f.role, f.name.as_str())).collect();
    use libfileparser_core::Role::*;
    assert_eq!(facets, [(Primary, "Scene"), (Structure, "glTF"), (Dependencies, "Dependencies"), (Inspector, "Materials"), (Inspector, "Textures")]);
    assert_eq!(
        table(&doc, &cx, "Dependencies").unwrap(),
        [row(&["cube.bin", "buffer", "", "read"]), row(&["checker.png", "texture", "textures/checker.png", "read"])]
    );

    let (doc, cx) = open_in_corpus("3d/tri-obj/tri.obj", libfileparser_core::InlineSpawn);
    assert_eq!(
        table(&doc, &cx, "Dependencies").unwrap(),
        [row(&["tri.mtl", "material library", "", "read"]), row(&["paint.png", "texture", "", "missing"])]
    );

    // A file that names no other file has no Dependencies facet.
    let (doc, cx) = open_in_corpus("3d/cube.stl", libfileparser_core::InlineSpawn);
    assert!(table(&doc, &cx, "Dependencies").is_none() && table(&doc, &cx, "Textures").is_none());
}

#[test]
fn materials_list_shading_maps_and_factors() {
    let (doc, cx) = open_in_corpus("3d/cube-gltf/cube.gltf", libfileparser_core::InlineSpawn);
    assert_eq!(table(&doc, &cx, "Materials").unwrap(), [row(&["Checker", "pbr", "base_color: checker.png", "1, 1, 1, 1", "0", "1", "opaque"])]);
    let (doc, cx) = open_in_corpus("3d/tri-obj/tri.obj", libfileparser_core::InlineSpawn);
    assert_eq!(table(&doc, &cx, "Materials").unwrap(), [row(&["Paint", "blinn_phong", "", "0.8, 0.2, 0.2, 1", "", "", "opaque"])]);
    // An STL has no materials of its own, only the clay every such file is drawn with.
    let (doc, cx) = open_in_corpus("3d/cube.stl", libfileparser_core::InlineSpawn);
    assert!(table(&doc, &cx, "Materials").is_none());
}

/// A tree's nodes to `depth` levels, as `label = value` lines indented by level.
fn tree(doc: &Document, cx: &ParseContext, name: &str, depth: usize) -> Vec<String> {
    fn walk(t: &dyn libfileparser_core::TreeSource, parent: Option<libfileparser_core::NodeId>, level: usize, depth: usize, out: &mut Vec<String>) {
        for n in t.children(parent, 0..1000).unwrap() {
            out.push(format!("{}{} = {}", "  ".repeat(level), n.label, n.value));
            if n.has_children && level + 1 < depth {
                walk(t, Some(n.id), level + 1, depth, out);
            }
        }
    }
    let i = doc.facets().position(|f| f.name == name).unwrap_or_else(|| panic!("no {name} facet"));
    let Ok(Content::Tree(t)) = doc.facet(i, cx) else { panic!("{name} isn't a tree") };
    let mut out = Vec::new();
    walk(&*t, None, 0, depth, &mut out);
    out
}

#[test]
fn usd_shows_its_stage_layers_and_variants() {
    // usd-lite's composition fixture: a sublayer, references, a payload, variants.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../usd-lite/tests/fixtures/compose").canonicalize().unwrap();
    let cx = ParseContext::new().with_files(libfileparser_core::FolderFiles::within(&dir)).with_spawn(libfileparser_core::InlineSpawn);
    let doc = UsdReader.open(&FileSource::open(dir.join("root.usda")).unwrap(), &cx).unwrap();
    use libfileparser_core::Role::*;
    let facets: Vec<_> = doc.facets().map(|f| (f.role, f.name.as_str())).collect();
    assert_eq!(
        facets,
        [
            (Primary, "Scene"),
            (Structure, "Stage"),
            (Structure, "Layers"),
            (Inspector, "Variants"),
            (Dependencies, "Dependencies"),
            (Inspector, "Materials"),
            (Inspector, "Textures")
        ]
    );

    // The composed stage: what the references brought in is there, under the prim that
    // references it, with its connections mapped into the stage's namespace.
    let stage = tree(&doc, &cx, "Stage", 4);
    for line in [
        "Stage metadata = 3 fields",
        "  upAxis = Y",
        "World = Xform",
        "  Car = Xform",
        "    xformOp:translate = double3 (1, 0, 0)",
        "    Tire = Mesh",
        "      points = point3f[3]",
        "      material:binding = rel → /World/Car/Looks/Rubber",
        "_class_Red = class",
    ] {
        assert!(stage.contains(&line.to_string()), "no {line:?} in\n{}", stage.join("\n"));
    }

    assert_eq!(
        tree(&doc, &cx, "Layers", 3),
        [
            "root.usda = root layer",
            "  sub.usda = sublayer · read",
            "  parts/wheel.usda = reference · read · into /World/Car",
            "  ./parts/wheel.usda = reference · read · into /World/Car2",
            "  parts/box.usda = payload · read · into /World/Pay",
        ]
    );

    let variants = tree(&doc, &cx, "Variants", 3);
    assert_eq!(variants[..4], ["/World/Car = ", "  size = small", "    big = ", "    small = selected"]);
    assert!(variants.contains(&"  shape = tri".to_string()) && variants.contains(&"    tri = selected".to_string()), "{variants:?}");

    // A file without arcs or variants has only its Stage.
    let (doc, _) = open_in_corpus("3d/tri.usda", libfileparser_core::InlineSpawn);
    let names: Vec<_> = doc.facets().map(|f| f.name.clone()).collect();
    assert!(names.contains(&"Stage".to_string()) && !names.iter().any(|n| n == "Layers" || n == "Variants"), "{names:?}");
}

#[test]
fn a_missing_layer_shows_in_layers() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../usd-lite/tests/fixtures/compose").canonicalize().unwrap();
    // Only the root may be read: the files hook is a folder that has nothing else.
    let empty = std::env::temp_dir().join(format!("usd-missing-layer-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::copy(dir.join("root.usda"), empty.join("root.usda")).unwrap();
    let cx = ParseContext::new().with_files(libfileparser_core::FolderFiles::within(&empty));
    let doc = UsdReader.open(&FileSource::open(empty.join("root.usda")).unwrap(), &cx).unwrap();
    let layers = tree(&doc, &cx, "Layers", 2);
    std::fs::remove_dir_all(&empty).ok();
    assert_eq!(layers[1], "  sub.usda = sublayer · missing");
    assert_eq!(layers[4], "  parts/box.usda = payload · missing · into /World/Pay");
}

#[test]
fn textures_show_decoding_then_decoded_and_list_the_missing_ones() {
    let spawn = libfileparser_testkit::QueueSpawn::default();
    let (doc, cx) = open_in_corpus("3d/cube.glb", spawn.clone());
    assert_eq!(table(&doc, &cx, "Textures").unwrap(), [row(&["image 0", "embedded", "", "sRGB", "decoding"])]);
    spawn.run_all();
    // The same facet, asked again: it shows the textures as they are now.
    assert_eq!(table(&doc, &cx, "Textures").unwrap(), [row(&["image 0", "embedded", "64×64", "sRGB", "decoded"])]);

    let (doc, cx) = open_in_corpus("3d/missing-texture/cube.gltf", libfileparser_core::InlineSpawn);
    assert_eq!(table(&doc, &cx, "Textures").unwrap(), [row(&["not-here.png", "file", "", "", "missing"])]);
}

#[test]
fn glb_shows_its_gltf_json_chunk_and_chunks() {
    use libfileparser_core::Role::*;
    let (doc, cx) = open_in_corpus("3d/cube.glb", libfileparser_core::InlineSpawn);
    let facets: Vec<_> = doc.facets().map(|f| (f.role, f.name.as_str())).collect();
    assert_eq!(facets, [(Primary, "Scene"), (Structure, "glTF"), (Source, "JSON chunk"), (Package, "Chunks"), (Inspector, "Materials"), (Inspector, "Textures")]);

    let gltf = tree(&doc, &cx, "glTF", 1);
    assert!(gltf.iter().any(|l| l.starts_with("asset = ")) && gltf.iter().any(|l| l.starts_with("meshes = [")), "{gltf:?}");

    let i = doc.facets().position(|f| f.name == "JSON chunk").unwrap();
    let Ok(Content::Text(text)) = doc.facet(i, &cx) else { panic!("not text") };
    assert_eq!(text.info().language.as_deref(), Some("json"));
    let first = text.lines(0..1).unwrap();
    assert!(first[0].as_slice().starts_with(b"{"));

    let i = doc.facets().position(|f| f.name == "Chunks").unwrap();
    let Ok(Content::Container(chunks)) = doc.facet(i, &cx) else { panic!("not a container") };
    let entries = chunks.entries(0..10).unwrap();
    let paths: Vec<_> = entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths[..2], ["JSON.json", "BIN.bin"]);
    let image = paths.iter().position(|p| p.starts_with("images/")).expect("the embedded image");
    // An entry opens as a file of its own, a view of the GLB's bytes.
    let png = chunks.open(image as u64).unwrap();
    assert!(png.bytes().starts_with(b"\x89PNG"), "{}", png.name());
    assert_eq!(png.len(), entries[image].size);
    let json = chunks.open(0).unwrap();
    assert!(serde_json::from_slice::<serde_json::Value>(json.bytes()).is_ok());
}

#[test]
fn gltf_lists_its_extensions() {
    // A triangle in a data: URI buffer, using one extension read here and one not.
    let mut bin = Vec::new();
    for v in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
        bin.extend_from_slice(&v.to_le_bytes());
    }
    use base64::Engine;
    let uri = format!("data:application/octet-stream;base64,{}", base64::engine::general_purpose::STANDARD.encode(&bin));
    let json = serde_json::json!({
        "asset": { "version": "2.0" },
        "extensionsUsed": ["KHR_materials_unlit", "KHR_materials_sheen"],
        "extensionsRequired": ["KHR_materials_unlit"],
        "buffers": [{ "uri": uri, "byteLength": 36 }],
        "bufferViews": [{ "buffer": 0, "byteLength": 36 }],
        "accessors": [{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0] }],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "nodes": [{ "mesh": 0 }],
        "scenes": [{ "nodes": [0] }]
    });
    let src = FileSource::from_bytes("tri.gltf", serde_json::to_vec(&json).unwrap());
    let cx = ParseContext::new();
    let doc = GltfReader.open(&src, &cx).unwrap();
    assert_eq!(
        table(&doc, &cx, "Extensions used").unwrap(),
        [row(&["KHR_materials_unlit", "yes", "read"]), row(&["KHR_materials_sheen", "no", "not read"])]
    );
    // A .gltf's JSON is the front door's Source, so no JSON chunk or Chunks here.
    let names: Vec<_> = doc.facets().map(|f| f.name.clone()).collect();
    assert!(names.contains(&"glTF".to_string()) && !names.iter().any(|n| n == "JSON chunk" || n == "Chunks"), "{names:?}");
}

/// The facets' names in order.
fn names(doc: &Document) -> Vec<String> {
    doc.facets().map(|f| f.name.clone()).collect()
}

/// A container facet's entries, and entry `i` opened.
fn container(doc: &Document, cx: &ParseContext, name: &str) -> (Vec<(String, u64)>, std::sync::Arc<dyn libfileparser_core::Container>) {
    let i = doc.facets().position(|f| f.name == name).unwrap_or_else(|| panic!("no {name} facet"));
    let Ok(Content::Container(c)) = doc.facet(i, cx) else { panic!("{name} isn't a container") };
    (c.entries(0..100).unwrap().into_iter().map(|e| (e.path, e.size)).collect(), c)
}

#[test]
fn fbx_shows_its_records_and_embedded_media() {
    let (doc, cx) = open_in_corpus("3d/tri-binary.fbx", libfileparser_core::InlineSpawn);
    assert_eq!(names(&doc), ["Scene", "Records", "Embedded media"]);
    let records = tree(&doc, &cx, "Records", 3);
    for line in [
        "FBXHeaderExtension = ",
        "  FBXVersion = 7400",
        "  Geometry = 1000, \"Geometry::Tri\", \"Mesh\"",
        "    Vertices = f64[9]",
        "    PolygonVertexIndex = i32[3]",
        "    Content = raw, 75 bytes",
        "  C = \"OO\", 1000, 2000",
    ] {
        assert!(records.contains(&line.to_string()), "no {line:?} in\n{}", records.join("\n"));
    }
    let (entries, media) = container(&doc, &cx, "Embedded media");
    assert_eq!(entries, [("media/checker.png".to_string(), 75)]);
    assert!(media.open(0).unwrap().bytes().starts_with(b"\x89PNG"));
    assert!(media.open(1).is_err());

    // ASCII FBX: the same tree, parsed when it's opened; nothing embedded.
    let (doc, cx) = open_in_corpus("3d/tri.fbx", libfileparser_core::InlineSpawn);
    assert_eq!(names(&doc), ["Scene", "Records"]);
    let records = tree(&doc, &cx, "Records", 3);
    assert!(records.contains(&"    Vertices = [9]".to_string()) && records.contains(&"  Model = 2000, \"Model::Tri\", \"Mesh\"".to_string()), "{records:?}");
}

#[test]
fn obj_lists_its_objects_and_ply_its_header() {
    let (doc, cx) = open_in_corpus("3d/tri-obj/tri.obj", libfileparser_core::InlineSpawn);
    assert_eq!(names(&doc), ["Scene", "Objects", "Dependencies", "Materials", "Textures"]);
    assert_eq!(tree(&doc, &cx, "Objects", 3), ["mtllib = tri.mtl · line 1", "Tri = 1 face · line 2", "  usemtl Paint = 1 face"]);

    let (doc, cx) = open_in_corpus("3d/spiral.ply", libfileparser_core::InlineSpawn);
    assert_eq!(names(&doc), ["Scene", "Header"]);
    let header = tree(&doc, &cx, "Header", 2);
    assert_eq!(header[..3], ["format = ascii 1.0", "vertex = 100 items", "  x = float"]);
    assert_eq!(header.last().unwrap(), "  blue = uchar");
}

#[test]
fn blend_lists_its_blocks_and_packed_files() {
    let (doc, cx) = open_in_corpus("3d/packed.blend", libfileparser_core::InlineSpawn);
    assert_eq!(names(&doc), ["Preview", "Blocks", "Packed files"]);
    let blocks = tree(&doc, &cx, "Blocks", 2);
    for line in ["Object = 1 datablock", "  Tri = 74 bytes", "Image = 1 datablock", "  checker.png = 1.1 KB", "  DATA · PackedFile = 1 block · 16 bytes", "  DATA · raw data = 1 block · 75 bytes", "  DNA1 = 1 block · 228 bytes"] {
        assert!(blocks.contains(&line.to_string()), "no {line:?} in\n{}", blocks.join("\n"));
    }
    let (entries, packed) = container(&doc, &cx, "Packed files");
    assert_eq!(entries, [("packed/checker.png".to_string(), 75)]);
    let png = packed.open(0).unwrap();
    assert_eq!(png.name(), "checker.png");
    assert!(png.bytes().starts_with(b"\x89PNG"));

    // Without an SDNA, blocks are listed by code, and nothing is packed.
    let (doc, cx) = open_in_corpus("3d/preview.blend", libfileparser_core::InlineSpawn);
    assert_eq!(names(&doc), ["Preview", "Blocks"]);
    assert_eq!(tree(&doc, &cx, "Blocks", 2), ["Other blocks = 2 blocks", "  REND = 1 block · 72 bytes", "  TEST = 1 block · 72 bytes"]);
}

#[test]
fn damaged_structure_never_panics() {
    // Each new sample cut short and with bytes flipped: opening and walking every facet
    // fails cleanly or succeeds, never panics.
    let cx = ParseContext::new();
    for path in ["3d/tri-binary.fbx", "3d/packed.blend", "3d/tri.fbx", "3d/spiral.ply"] {
        let good = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files").join(path)).unwrap();
        let name = path.rsplit('/').next().unwrap();
        let mut variants: Vec<Vec<u8>> = (1..good.len()).step_by(7).map(|n| good[..n].to_vec()).collect();
        for at in (0..good.len()).step_by(5) {
            let mut bad = good.clone();
            bad[at] ^= 0xA5;
            variants.push(bad);
        }
        for bytes in variants {
            let Ok(doc) = libfileparser_3d::open(&FileSource::from_bytes(name, bytes), &cx) else { continue };
            for i in 0..doc.facets().count() {
                match doc.facet(i, &cx) {
                    Ok(Content::Tree(t)) => {
                        let mut stack = vec![None];
                        let mut seen = 0;
                        while let Some(p) = stack.pop() {
                            for n in t.children(p, 0..1000).unwrap_or_default() {
                                seen += 1;
                                if n.has_children && seen < 10_000 {
                                    stack.push(Some(n.id));
                                }
                            }
                        }
                    }
                    Ok(Content::Container(c)) => {
                        for e in 0..c.entry_count().known() {
                            let _ = c.open(e);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// The node `path` names in the tree facet `facet`.
fn node_at(doc: &Document, cx: &ParseContext, facet: &str, path: &str) -> libfileparser_core::Place {
    let i = doc.facet_index(facet).unwrap();
    let Ok(Content::Tree(t)) = doc.facet(i, cx) else { panic!("{facet} isn't a tree") };
    let node = t.find(path).unwrap().unwrap_or_else(|| panic!("no {path} in {facet}"));
    libfileparser_core::Place::Node { facet: i, node }
}

/// A node place's path in its tree.
fn path_of(doc: &Document, cx: &ParseContext, place: &libfileparser_core::Place) -> Option<String> {
    let libfileparser_core::Place::Node { facet, node } = place else { return None };
    let Ok(Content::Tree(t)) = doc.facet(*facet, cx) else { return None };
    t.path(*node).unwrap()
}

#[test]
fn usd_prims_link_to_their_scene_nodes_source_lines_and_layers() {
    use libfileparser_core::{Place, SceneItem};
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../usd-lite/tests/fixtures/compose").canonicalize().unwrap();
    let cx = ParseContext::new().with_files(libfileparser_core::FolderFiles::within(&dir)).with_spawn(libfileparser_core::InlineSpawn);
    let doc = UsdReader.open(&FileSource::open(dir.join("root.usda")).unwrap(), &cx).unwrap();
    let scene = doc.facet_index("Scene").unwrap();
    let layers = doc.facet_index("Layers").unwrap();

    // A prim brought in by a reference: its scene nodes, and the layer that defines it.
    let tire = node_at(&doc, &cx, "Stage", "/World/Car/Tire");
    let links = doc.links(&tire, &cx).unwrap();
    let nodes: Vec<_> = links.iter().filter_map(|p| match p {
        Place::Scene { facet, item: SceneItem::Node(n) } if *facet == scene => Some(*n),
        _ => None,
    }).collect();
    assert_eq!(nodes.len(), 1, "{links:?}");
    let layer = links.iter().find(|p| matches!(p, Place::Node { facet, .. } if *facet == layers)).expect("its layer");
    assert_eq!(path_of(&doc, &cx, layer).as_deref(), Some("parts/wheel.usda"));
    // Not in the root layer, so no line of the file's text.
    assert!(!links.iter().any(|p| matches!(p, Place::Lines(_))));

    // And back: the scene node links to its prim.
    let back = doc.links(&Place::Scene { facet: scene, item: SceneItem::Node(nodes[0]) }, &cx).unwrap();
    assert!(back.contains(&tire), "{back:?}");

    // A prim of the root layer: its line (from 0), and a line inside it gives the prim.
    let car = node_at(&doc, &cx, "Stage", "/World/Car");
    assert!(doc.links(&car, &cx).unwrap().contains(&Place::Lines(10..11)));
    assert_eq!(doc.links(&Place::Lines(12..13), &cx).unwrap(), [car]);
    // In a variant: the line in the variant's body.
    let tri = node_at(&doc, &cx, "Stage", "/World/Switch/Tri");
    assert!(doc.links(&tri, &cx).unwrap().contains(&Place::Lines(68..69)));

    // A property links like its prim.
    let points = node_at(&doc, &cx, "Stage", "/World/Car/Tire.points");
    assert!(doc.links(&points, &cx).unwrap().iter().any(|p| matches!(p, Place::Node { facet, .. } if *facet == layers)));
}

#[test]
fn gltf_nodes_materials_and_textures_link_across_facets() {
    use libfileparser_core::{Place, SceneItem};
    let (doc, cx) = open_in_corpus("3d/cube-gltf/cube.gltf", libfileparser_core::InlineSpawn);
    let (scene, materials, textures) = (doc.facet_index("Scene").unwrap(), doc.facet_index("Materials").unwrap(), doc.facet_index("Textures").unwrap());

    let node = node_at(&doc, &cx, "glTF", "/nodes/0");
    assert_eq!(doc.links(&node, &cx).unwrap(), [Place::Scene { facet: scene, item: SceneItem::Node(0) }]);
    // A scene node: its glTF node and its material's row.
    let links = doc.links(&Place::Scene { facet: scene, item: SceneItem::Node(0) }, &cx).unwrap();
    assert!(links.contains(&node) && links.contains(&Place::Row { facet: materials, row: 0 }), "{links:?}");
    // The primitive and its scene mesh.
    let primitive = node_at(&doc, &cx, "glTF", "/meshes/0/primitives/0");
    assert_eq!(doc.links(&primitive, &cx).unwrap(), [Place::Scene { facet: scene, item: SceneItem::Mesh(0) }]);

    // A Materials row: its scene material, the Textures row of its map; and back.
    let row = Place::Row { facet: materials, row: 0 };
    let links = doc.links(&row, &cx).unwrap();
    assert!(links.contains(&Place::Scene { facet: scene, item: SceneItem::Material(0) }) && links.contains(&Place::Row { facet: textures, row: 0 }), "{links:?}");
    let links = doc.links(&Place::Row { facet: textures, row: 0 }, &cx).unwrap();
    assert!(links.contains(&row) && links.iter().any(|p| matches!(p, Place::Scene { item: SceneItem::Texture(_), .. })), "{links:?}");
    let material = doc.links(&Place::Scene { facet: scene, item: SceneItem::Material(0) }, &cx).unwrap();
    assert!(material.contains(&node_at(&doc, &cx, "glTF", "/materials/0")) && material.contains(&row), "{material:?}");

    // A place nothing links to, or that doesn't exist, has no links.
    assert!(doc.links(&Place::Row { facet: materials, row: 99 }, &cx).unwrap().is_empty());
    assert!(doc.links(&Place::Bytes(0..4), &cx).unwrap().is_empty());
}

#[test]
fn fbx_records_link_to_their_scene_nodes_and_meshes() {
    use libfileparser_core::{Place, SceneItem};
    // Binary records are read in place; ASCII ones parsed: both have the same paths.
    for path in ["3d/tri-binary.fbx", "3d/tri.fbx"] {
        let (doc, cx) = open_in_corpus(path, libfileparser_core::InlineSpawn);
        let scene = doc.facet_index("Scene").unwrap();
        let model = node_at(&doc, &cx, "Records", "Objects/Model/Tri");
        assert_eq!(doc.links(&model, &cx).unwrap(), [Place::Scene { facet: scene, item: SceneItem::Node(0) }], "{path}");
        let geometry = node_at(&doc, &cx, "Records", "Objects/Geometry/Tri");
        assert_eq!(doc.links(&geometry, &cx).unwrap(), [Place::Scene { facet: scene, item: SceneItem::Mesh(0) }], "{path}");
        // And back, to the same record.
        let back = doc.links(&Place::Scene { facet: scene, item: SceneItem::Node(0) }, &cx).unwrap();
        assert!(back.contains(&model), "{path}: {back:?}");
        assert_eq!(path_of(&doc, &cx, &model).as_deref(), Some("Objects/Model/Tri"));
        // A record outside Objects has no path, and no links.
        let records = doc.facet_index("Records").unwrap();
        let Ok(Content::Tree(t)) = doc.facet(records, &cx) else { panic!() };
        let header = t.children(None, 0..1).unwrap()[0].id;
        assert!(t.path(header).unwrap().is_none());
        assert!(doc.links(&Place::Node { facet: records, node: header }, &cx).unwrap().is_empty());
    }
}

#[test]
fn obj_objects_link_to_scene_nodes_lines_and_materials() {
    use libfileparser_core::{Place, SceneItem};
    let (doc, cx) = open_in_corpus("3d/tri-obj/tri.obj", libfileparser_core::InlineSpawn);
    let (scene, materials) = (doc.facet_index("Scene").unwrap(), doc.facet_index("Materials").unwrap());
    let tri = node_at(&doc, &cx, "Objects", "Tri");
    let links = doc.links(&tri, &cx).unwrap();
    // `o Tri` is line 2 (1 from 0).
    assert!(links.contains(&Place::Scene { facet: scene, item: SceneItem::Node(0) }) && links.contains(&Place::Lines(1..2)), "{links:?}");
    // A face line belongs to the object above it; the mtllib line is its own node.
    assert_eq!(doc.links(&Place::Lines(9..10), &cx).unwrap(), std::slice::from_ref(&tri));
    let mtllib = doc.links(&Place::Lines(0..1), &cx).unwrap();
    assert_eq!(mtllib.len(), 1);
    assert_ne!(mtllib[0], tri);
    // `usemtl Paint` and its scene material, whose row is in Materials.
    let paint = node_at(&doc, &cx, "Objects", "usemtl Paint");
    let links = doc.links(&paint, &cx).unwrap();
    let Some(Place::Scene { item: SceneItem::Material(m), .. }) = links.first().cloned() else { panic!("{links:?}") };
    let row = doc.links(&Place::Scene { facet: scene, item: SceneItem::Material(m) }, &cx).unwrap();
    assert!(row.contains(&Place::Row { facet: materials, row: 0 }) && row.contains(&paint), "{row:?}");
}
