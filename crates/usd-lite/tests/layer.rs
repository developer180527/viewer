//! `fixtures/scene.usda` is hand-written; `fixtures/scene.usdc` is Pixar's `usdcat`
//! conversion of it. Both must read to the same scene.

use usd_lite::geom::{self, Channel, Input, Interpolation, UpAxis};
use usd_lite::{Error, Layer, Limits, Value, read_layer};

const USDA: &[u8] = include_bytes!("fixtures/scene.usda");
const USDC: &[u8] = include_bytes!("fixtures/scene.usdc");

fn both() -> [(&'static str, Layer); 2] {
    let limits = Limits::default();
    [("usda", read_layer(USDA, &limits).unwrap()), ("usdc", read_layer(USDC, &limits).unwrap())]
}

fn apply(m: &geom::Matrix, p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
}

#[test]
fn layer_metadata_and_hierarchy() {
    for (name, layer) in both() {
        assert_eq!(geom::up_axis(&layer), UpAxis::Z, "{name}");
        assert_eq!(geom::meters_per_unit(&layer), 0.01, "{name}");
        assert_eq!(layer.metadata("defaultPrim").and_then(Value::as_str), Some("World"), "{name}");
        assert!(layer.metadata("documentation").and_then(Value::as_str).unwrap().contains("subsets and primvars"), "{name}: multi-line doc");
        assert_eq!(layer.root_prims(), ["/World"], "{name}");
        assert_eq!(layer.children("/World"), ["/World/Quad", "/World/Looks", "/World/Hidden", "/World/Referenced"], "{name}");
        assert_eq!(layer.type_name("/World/Quad"), Some("Mesh"), "{name}");
        assert!(!geom::is_drawn(&layer, "/World/Hidden"), "{name}");
        let arcs = layer.composition_arcs();
        assert!(arcs.contains(&"references") && arcs.contains(&"variant sets"), "{name}: {arcs:?}");
        // Variant contents don't leak into the prim tree.
        assert_eq!(layer.children("/World/Referenced"), Vec::<String>::new(), "{name}");
    }
}

#[test]
fn transforms_compose_in_xform_op_order() {
    for (name, layer) in both() {
        let (m, reset) = geom::local_transform(&layer, "/World");
        assert!(!reset);
        // translate · rotateZ(90) · scale(2): (1, 0, 0) → scale (2, 0, 0) → rotate (0, 2, 0) → (10, 2, 0).
        let p = apply(&m, [1.0, 0.0, 0.0]);
        assert!((p[0] - 10.0).abs() < 1e-9 && (p[1] - 2.0).abs() < 1e-9 && p[2].abs() < 1e-9, "{name}: {p:?}");
        // Time samples: the first one is the value.
        assert_eq!(layer.attr("/World/Quad", "radius").and_then(Value::as_f64), Some(1.5), "{name}");
    }
}

#[test]
fn meshes_primvars_and_subsets() {
    for (name, layer) in both() {
        let mesh = geom::mesh(&layer, "/World/Quad").unwrap();
        assert_eq!(mesh.face_vertex_counts, [4, 3], "{name}");
        assert_eq!(mesh.face_vertex_indices, [0, 1, 2, 3, 0, 2, 4], "{name}");
        assert_eq!(mesh.points.len(), 5);
        assert_eq!(mesh.points[4][2], f32::NEG_INFINITY, "{name}: -inf survives");
        assert!(mesh.double_sided);
        let uv = mesh.uvs.as_ref().unwrap();
        assert_eq!(uv.interpolation, Interpolation::FaceVarying, "{name}");
        // Indexed: corner 6 (the triangle's last) uses index 1 → (1, 0).
        assert_eq!(uv.at(1, 4, 6), Some([1.0, 0.0]), "{name}");
        assert_eq!(mesh.normals.as_ref().unwrap().interpolation, Interpolation::Vertex);
        let color = mesh.display_color.as_ref().unwrap();
        assert_eq!((color.interpolation, color.at(1, 3, 5)), (Interpolation::Constant, Some([1.0, 0.0, 0.0])), "{name}");
        assert_eq!(mesh.subsets.len(), 1);
        assert_eq!(mesh.subsets[0].faces, [1]);
        assert_eq!(mesh.subsets[0].material.as_deref(), Some("/World/Looks/Textured"), "{name}");
        assert_eq!(geom::bound_material(&layer, "/World/Quad", true).as_deref(), Some("/World/Looks/Red"));
    }
}

#[test]
fn preview_surfaces_resolve_values_and_textures() {
    for (name, layer) in both() {
        let red = geom::preview_surface(&layer, "/World/Looks/Red").unwrap();
        let Input::Value(c) = red.diffuse_color else { panic!("{name}: a colour") };
        assert!((c[0] - 0.8).abs() < 1e-6 && (c[1] - 0.1).abs() < 1e-6, "{name}: {c:?}");
        assert_eq!((red.metallic, red.roughness), (Input::Value(1.0), Input::Value(0.25)), "{name}");
        assert_eq!(red.opacity, Input::Value(1.0), "{name}: unauthored inputs take the spec's defaults");

        let tex = geom::preview_surface(&layer, "/World/Looks/Textured").unwrap();
        // Through a node graph's output to the texture.
        let Input::Texture(albedo) = &tex.diffuse_color else { panic!("{name}: a texture") };
        assert_eq!((albedo.file.as_str(), albedo.channel, albedo.srgb), ("textures/albedo.png", Channel::Rgb, Some(true)), "{name}");
        let Input::Texture(orm) = &tex.roughness else { panic!("{name}") };
        assert_eq!((orm.channel, orm.srgb), (Channel::G, Some(false)), "{name}");
        let normal = tex.normal.as_ref().unwrap();
        assert_eq!((normal.scale, normal.bias), ([2.0; 4], [-1.0; 4]), "{name}");
        assert_eq!(tex.opacity_threshold, 0.5);
    }
}

#[test]
fn every_spec_reads_the_same_from_text_and_binary() {
    let [(_, a), (_, b)] = both();
    for (path, spec) in &b.specs {
        let other = a.spec(path).unwrap_or_else(|| panic!("{path}: missing from the usda read"));
        for field in ["typeName", "default", "primChildren", "connectionPaths", "targetPaths", "interpolation"] {
            if let (Some(x), Some(y)) = (spec.field(field), other.field(field)) {
                let same = match (x, y) {
                    (Value::Float(p), Value::Float(q)) => (p - q).abs() < 1e-6,
                    (Value::Int(p), Value::Float(q)) | (Value::Float(q), Value::Int(p)) => (*p as f64 - q).abs() < 1e-9,
                    (Value::Vec3(p), Value::Vec3(q)) => p.iter().zip(q).all(|(p, q)| (p - q).abs() < 1e-6),
                    _ => x == y,
                };
                assert!(same, "{path} {field}: usdc {x:?} vs usda {y:?}");
            }
        }
    }
}

#[test]
fn limits_and_wrong_formats_are_errors() {
    let tiny = Limits::default().with_max_elements(3);
    assert!(matches!(read_layer(USDA, &tiny), Err(Error::Limit(_))));
    assert!(read_layer(USDC, &tiny).is_err());
    assert!(matches!(read_layer(b"hello", &Limits::default()), Err(Error::Format(_))));
    assert!(matches!(read_layer(b"PXR-USDC\x00\x01\x00\x00\x00\x00\x00\x00", &Limits::default()), Err(Error::Unsupported(_))));
}

/// Corrupts the fixtures thousands of ways: reading may fail, never panic, and the
/// geometry helpers must cope with whatever comes back.
#[test]
fn damaged_files_never_panic() {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut rand = move |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n.max(1) as u64) as usize
    };
    let limits = Limits::default().with_max_elements(1 << 16).with_max_decompressed(1 << 22);
    for source in [USDC, USDA] {
        for round in 0..3000 {
            let mut bytes = source.to_vec();
            match round % 3 {
                // Flip a few bytes.
                0 => (0..1 + rand(8)).for_each(|_| {
                    let i = rand(bytes.len());
                    bytes[i] = rand(256) as u8;
                }),
                // Truncate.
                1 => bytes.truncate(rand(bytes.len())),
                // Overwrite a run with a large value (counts, offsets).
                _ => {
                    let i = rand(bytes.len().saturating_sub(8));
                    bytes[i..i + 8].copy_from_slice(&(u64::MAX >> rand(40)).to_le_bytes());
                }
            }
            if let Ok(layer) = read_layer(&bytes, &limits) {
                for path in layer.specs.keys().filter(|p| !p.contains('.')).cloned().collect::<Vec<_>>() {
                    let _ = geom::local_transform(&layer, &path);
                    if let Some(mesh) = geom::mesh(&layer, &path) {
                        let _ = mesh.uvs.as_ref().map(|uv| uv.at(usize::MAX, usize::MAX, usize::MAX));
                    }
                    let _ = geom::preview_surface(&layer, &path);
                }
            }
        }
    }
}
