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
