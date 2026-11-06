//! Every 3D reader passes the conformance suite (docs/design/libfileparser.md §9) on
//! the corpus's committed samples. The suite walks `readers()`, so a reader added
//! without samples fails here instead of passing unchecked.

use libfileparser_testkit::{Sample, assert_conforms};

fn corpus(path: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files").join(path)).unwrap()
}

fn sample(path: &str) -> Sample {
    Sample::new(path.rsplit('/').next().unwrap(), corpus(path))
}

/// The samples each reader is checked on, by reader id.
fn samples(reader: &str) -> Vec<Sample> {
    match reader {
        "gltf" => {
            let gltf = sample("3d/cube-gltf/cube.gltf")
                .with_neighbour("cube.bin", corpus("3d/cube-gltf/cube.bin"))
                .with_neighbour("textures/checker.png", corpus("3d/cube-gltf/textures/checker.png"));
            let missing = sample("3d/missing-texture/cube.gltf").with_neighbour("cube.bin", corpus("3d/missing-texture/cube.bin"));
            vec![gltf, missing, sample("3d/cube.glb")]
        }
        "fbx" => vec![sample("3d/tri.fbx").fits_tiny_limits()],
        // Not spaced-mtl-missing: when neither the whole name nor any word of it is there,
        // the words are looked for but only the whole name is reported, by design.
        "obj" => vec![
            sample("3d/tri-obj/tri.obj").with_neighbour("tri.mtl", corpus("3d/tri-obj/tri.mtl")).fits_tiny_limits(),
            sample("3d/spaced-mtl/tri.obj").with_neighbour("My Paint.mtl", corpus("3d/spaced-mtl/My Paint.mtl")).fits_tiny_limits(),
        ],
        "stl" => vec![sample("3d/cube.stl"), sample("3d/tetra.stl").fits_tiny_limits()],
        "ply" => vec![sample("3d/spiral.ply")],
        "usd" => {
            let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../usd-lite/tests/fixtures/scene.usda");
            vec![sample("3d/tri.usda").fits_tiny_limits(), Sample::new("scene.usda", std::fs::read(fixture).unwrap())]
        }
        "blend" => vec![sample("3d/preview.blend").fits_tiny_limits()],
        _ => Vec::new(),
    }
}

#[test]
fn every_reader_conforms() {
    for reader in libfileparser_3d::readers() {
        let id = reader.info().id;
        let samples = samples(id);
        assert!(!samples.is_empty(), "reader {id:?} has no conformance samples: add a corpus file (corpus/make.py) and list it in `samples`");
        assert_conforms(*reader, &samples);
    }
}
