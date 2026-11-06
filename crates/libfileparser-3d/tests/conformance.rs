//! Every 3D reader passes the conformance suite (docs/design/libfileparser.md §9), on
//! the corpus's committed samples. FBX and `.blend` have no small sample we can write
//! yet; the corpus checks them on local files.

use libfileparser_testkit::{Sample, assert_conforms};

fn corpus(path: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files").join(path)).unwrap()
}

fn sample(path: &str) -> Sample {
    Sample::new(path.rsplit('/').next().unwrap(), corpus(path))
}

#[test]
fn gltf() {
    let gltf = sample("3d/cube-gltf/cube.gltf")
        .with_neighbour("cube.bin", corpus("3d/cube-gltf/cube.bin"))
        .with_neighbour("textures/checker.png", corpus("3d/cube-gltf/textures/checker.png"));
    let missing = sample("3d/missing-texture/cube.gltf").with_neighbour("cube.bin", corpus("3d/missing-texture/cube.bin"));
    assert_conforms(&libfileparser_3d::GltfReader, &[gltf, missing, sample("3d/cube.glb")]);
}

#[test]
fn obj() {
    let tri = sample("3d/tri-obj/tri.obj").with_neighbour("tri.mtl", corpus("3d/tri-obj/tri.mtl")).fits_tiny_limits();
    let spaced = sample("3d/spaced-mtl/tri.obj").with_neighbour("My Paint.mtl", corpus("3d/spaced-mtl/My Paint.mtl")).fits_tiny_limits();
    // Not spaced-mtl-missing: when neither the whole name nor any word of it is there,
    // the words are looked for but only the whole name is reported, by design.
    assert_conforms(&libfileparser_3d::ObjReader, &[tri, spaced]);
}

#[test]
fn stl() {
    assert_conforms(&libfileparser_3d::StlReader, &[sample("3d/cube.stl"), sample("3d/tetra.stl").fits_tiny_limits()]);
}

#[test]
fn ply() {
    assert_conforms(&libfileparser_3d::PlyReader, &[sample("3d/spiral.ply")]);
}

#[test]
fn usd() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../usd-lite/tests/fixtures/scene.usda");
    let scene = Sample::new("scene.usda", std::fs::read(fixture).unwrap());
    assert_conforms(&libfileparser_3d::UsdReader, &[sample("3d/tri.usda").fits_tiny_limits(), scene]);
}
