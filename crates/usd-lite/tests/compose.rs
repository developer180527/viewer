//! `fixtures/compose/` uses every arc this crate composes; `flattened.usda` there is
//! Pixar's `usdcat --flatten root.usda`. Composing must give the same stage.

use std::path::Path;

use usd_lite::{FsResolver, Layer, Limits, SpecType, Value, compose, geom, read_layer};

fn dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compose").canonicalize().unwrap()
}

fn composed() -> usd_lite::Stage {
    let root = dir().join("root.usda");
    let layer = read_layer(&std::fs::read(&root).unwrap(), &Limits::default()).unwrap();
    compose(layer, &root.to_string_lossy(), &FsResolver::default(), &Limits::default())
}

fn pixar() -> Layer {
    read_layer(&std::fs::read(dir().join("flattened.usda")).unwrap(), &Limits::default()).unwrap()
}

fn same(a: &Value, b: &Value) -> bool {
    let close = |x: f64, y: f64| (x - y).abs() < 1e-6;
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => close(*x, *y),
        (Value::Vec3(x), Value::Vec3(y)) => x.iter().zip(y).all(|(p, q)| close(*p, *q)),
        // Composition anchors asset paths to their layer; Pixar's flatten keeps them as
        // authored.
        (Value::Asset(x), Value::Asset(y)) => x.ends_with(y.as_str()),
        _ => a == b,
    }
}

#[test]
fn composition_matches_usdcat_flatten() {
    let stage = composed();
    assert!(stage.warnings.is_empty(), "{:?}", stage.warnings);
    assert_eq!(stage.layers.len(), 4, "root, sub, wheel, box: {:?}", stage.layers);
    let (ours, theirs) = (&stage.layer, &pixar());

    let prims = |l: &Layer| {
        let mut p: Vec<_> = l.specs.iter().filter(|(_, s)| s.spec_type == SpecType::Prim).map(|(p, _)| p.clone()).collect();
        p.sort();
        p
    };
    assert_eq!(prims(ours), prims(theirs));

    for (path, spec) in &theirs.specs {
        let mine = ours.spec(path).unwrap_or_else(|| panic!("{path}: missing"));
        for field in ["typeName", "default", "targetPaths", "connectionPaths", "primChildren", "specifier"] {
            match (mine.field(field), spec.field(field)) {
                (Some(a), Some(b)) => assert!(same(a, b), "{path} {field}:\n ours   {a:?}\n pixar  {b:?}"),
                (None, Some(b)) => panic!("{path} {field}: missing, pixar has {b:?}"),
                _ => {}
            }
        }
    }
    // And nothing extra (a property only we have would mean a wrong opinion won).
    for path in ours.specs.keys() {
        if path != "/" {
            assert!(theirs.spec(path).is_some(), "{path}: not in pixar's stage");
        }
    }
}

#[test]
fn composed_assets_resolve_next_to_the_layer_that_wrote_them() {
    let stage = composed();
    let tex = geom::preview_surface(&stage.layer, "/World/Car/Looks/Rubber").unwrap();
    let geom::Input::Texture(t) = tex.diffuse_color else { panic!("a texture") };
    assert_eq!(Path::new(&t.file), dir().join("parts/textures/rubber.png"));
    // The binding was remapped into the composed namespace and resolves.
    assert_eq!(geom::bound_material(&stage.layer, "/World/Car/Tire", true).as_deref(), Some("/World/Car/Looks/Rubber"));
}

#[test]
fn instances_share_their_arrays() {
    let stage = composed();
    let points = |p: &str| match stage.layer.attr(p, "points") {
        Some(Value::Vec3Array(a)) => a.clone(),
        other => panic!("{p}: {other:?}"),
    };
    // Car and Car2 reference the same wheel: one copy of its points.
    assert!(std::sync::Arc::ptr_eq(&points("/World/Car/Tire"), &points("/World/Car2/Tire")));
}

#[test]
fn missing_and_cyclic_references_are_warnings() {
    let dir = std::env::temp_dir().join(format!("usd-lite-compose-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.usda"), "#usda 1.0\n(\n defaultPrim = \"A\"\n subLayers = [@b.usda@]\n)\ndef \"A\" (\n references = @a.usda@\n)\n{\n}\ndef \"M\" (\n references = @missing.usda@\n)\n{\n}\n").unwrap();
    std::fs::write(dir.join("b.usda"), "#usda 1.0\n(\n subLayers = [@a.usda@]\n)\n").unwrap();
    let root = dir.join("a.usda");
    let layer = read_layer(&std::fs::read(&root).unwrap(), &Limits::default()).unwrap();
    let stage = compose(layer, &root.canonicalize().unwrap().to_string_lossy(), &FsResolver::default(), &Limits::default());
    assert!(stage.warnings.iter().any(|w| w.contains("missing.usda")), "{:?}", stage.warnings);
    assert!(stage.layer.spec("/A").is_some() && stage.layer.spec("/M").is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn damaged_root_layers_compose_without_panicking() {
    let root = dir().join("root.usda");
    let id = root.to_string_lossy().into_owned();
    let bytes = std::fs::read(&root).unwrap();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rand = move |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n.max(1) as u64) as usize
    };
    let mut composed = 0;
    for _ in 0..1000 {
        let mut b = bytes.clone();
        for _ in 0..1 + rand(4) {
            let i = rand(b.len());
            b[i] = b"\"@<>{}[]()=/.:abc019 \n"[rand(22)];
        }
        if let Ok(layer) = read_layer(&b, &Limits::default()) {
            let stage = compose(layer, &id, &FsResolver::default(), &Limits::default());
            for path in stage.layer.specs.keys().filter(|p| !p.contains('.')) {
                let _ = geom::mesh(&stage.layer, path);
                let _ = geom::bound_material(&stage.layer, path, true);
            }
            composed += 1;
        }
    }
    assert!(composed > 20, "some damaged files still parse and compose: {composed}");
}

#[test]
fn stage_records_its_arcs_and_variant_sets() {
    use usd_lite::ArcKind;
    let stage = composed();
    let name = |id: &str| id.rsplit('/').next().unwrap().to_string();
    type Row<'a> = (String, ArcKind, &'a str, Option<String>, &'a [String]);
    let arcs: Vec<Row> =
        stage.arcs.iter().map(|a| (name(&a.from), a.kind, a.asset.as_str(), a.layer.as_deref().map(name), a.prims.as_slice())).collect();
    let prims = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(arcs.len(), 4, "{arcs:?}");
    assert_eq!(arcs[0], ("root.usda".into(), ArcKind::Sublayer, "sub.usda", Some("sub.usda".into()), &[][..]));
    assert_eq!(arcs[1], ("root.usda".into(), ArcKind::Reference, "parts/wheel.usda", Some("wheel.usda".into()), &prims(&["/World/Car"])[..]));
    // Spelled differently, so its own arc, though it is the same layer.
    assert_eq!(arcs[2], ("root.usda".into(), ArcKind::Reference, "./parts/wheel.usda", Some("wheel.usda".into()), &prims(&["/World/Car2"])[..]));
    assert_eq!(arcs[3], ("root.usda".into(), ArcKind::Payload, "parts/box.usda", Some("box.usda".into()), &prims(&["/World/Pay"])[..]));

    let sets: Vec<_> = stage.variant_sets.iter().map(|s| (s.prim.as_str(), s.name.as_str(), s.variants.clone(), s.selection.as_deref())).collect();
    assert!(sets.contains(&("/World/Switch", "shape", vec!["quad".into(), "tri".into()], Some("tri"))), "{sets:?}");
    // From the referenced wheel: its own selection, unless the referencing prim's wins.
    let selection = |prim: &str| stage.variant_sets.iter().find(|s| s.prim == prim && s.name == "size").and_then(|s| s.selection.clone());
    assert_eq!(selection("/World/Car").as_deref(), Some("small"));
    assert_eq!(selection("/World/Car2").as_deref(), Some("big"));
}
