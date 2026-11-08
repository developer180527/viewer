//! usd-lite on damaged layers (docs/design/libfileparser.md §9): text and crate
//! files cut, flipped and spliced, read, composed with a resolver that hands back the
//! damaged layer for every reference (cycles), and every prim's geometry, transform
//! and material read. Errors are fine; panics and hangs aren't.

use std::path::Path;

fn seeds() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = [
        dir.join("tests/fixtures/scene.usda"),
        dir.join("tests/fixtures/scene.usdc"),
        dir.join("tests/fixtures/compose/root.usda"),
        dir.join("tests/fixtures/compose/sub.usda"),
        dir.join("../../corpus/files/3d/tri.usda"),
    ];
    files.iter().map(|f| (f.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(f).unwrap())).collect()
}

/// Hands back the same bytes for whatever is asked: references, sublayers and
/// payloads all lead back to the damaged layer.
struct Echo(Vec<u8>);

impl usd_lite::Resolver for Echo {
    fn resolve(&self, asset: &str, _: &str) -> Option<(String, Vec<u8>)> {
        Some((asset.to_string(), self.0.clone()))
    }
}

#[test]
fn layers_survive_fuzzing() {
    libfileparser_testkit::assert_bytes_fuzz_clean("usd-lite", &seeds(), |_, bytes| {
        // Few prims: composition is copying, and this looks for crashes, not throughput.
        let limits = usd_lite::Limits::default().with_max_elements(1 << 20).with_max_decompressed(64 << 20).with_max_prims(2_000);
        let _ = usd_lite::detect(bytes);
        let Ok(layer) = usd_lite::read_layer(bytes, &limits) else { return };
        let stage = usd_lite::compose(layer, "root.usda", &Echo(bytes.to_vec()), &limits);
        let layer = &stage.layer;
        let _ = (usd_lite::geom::up_axis(layer), usd_lite::geom::meters_per_unit(layer));
        let mut queue = layer.root_prims();
        let mut seen = 0;
        while let Some(prim) = queue.pop() {
            seen += 1;
            if seen > 5000 {
                break;
            }
            let _ = layer.type_name(&prim);
            let _ = usd_lite::geom::local_transform(layer, &prim);
            let _ = usd_lite::geom::is_drawn(layer, &prim);
            let _ = usd_lite::geom::mesh(layer, &prim);
            if let Some(m) = usd_lite::geom::bound_material(layer, &prim, true) {
                let _ = usd_lite::geom::preview_surface(layer, &m);
            }
            queue.extend(layer.children(&prim));
        }
    });
}

/// A payload naming the file's own default prim, from inside it (and a resolver that
/// finds the file): Pcp calls it a cycle. Followed, every level would hold the whole
/// file again.
#[test]
fn a_prim_that_brings_in_its_own_ancestor_is_a_cycle() {
    let file = b"#usda 1.0\n(\n    defaultPrim = \"World\"\n)\ndef \"World\"\n{\n    def \"again\" (\n        payload = @self.usda@\n    )\n    {\n    }\n    def Cube \"cube\"\n    {\n    }\n}\n";
    let limits = usd_lite::Limits::default();
    let layer = usd_lite::read_layer(file, &limits).unwrap();
    let stage = usd_lite::compose(layer, "self.usda", &Echo(file.to_vec()), &limits);
    let mut queue = stage.layer.root_prims();
    let mut prims = 0;
    while let Some(p) = queue.pop() {
        prims += 1;
        queue.extend(stage.layer.children(&p));
    }
    // World, again, cube, and again's own copy of cube: not a million.
    assert!(prims <= 4, "{prims} prims");
    assert!(stage.warnings.iter().any(|w| w.contains("cycle")), "{:?}", stage.warnings);
}
