//! Cross-checks the USDC reader against Pixar's own `usdcat` (shipped with macOS):
//! each binary file in `test_3d_files/` (not in git) is converted to USDA by usdcat,
//! both are read, and every spec and value must agree. Skips when either is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use usd_lite::{Layer, Limits, Value, geom, read_layer};

fn usdc_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_3d_files");
    let mut out = Vec::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "usdc") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn usdcat(input: &Path) -> Option<Vec<u8>> {
    let out = std::env::temp_dir().join(format!("usd-lite-ref-{}-{}.usda", std::process::id(), input.file_stem()?.to_string_lossy()));
    let ok = Command::new("usdcat").arg(input).arg("-o").arg(&out).status().ok()?.success();
    let text = ok.then(|| std::fs::read(&out).ok()).flatten();
    let _ = std::fs::remove_file(&out);
    text
}

/// Values equal up to float printing (usdcat prints ~9 significant digits) and the
/// array/scalar representations the two formats choose differently.
fn same(a: &Value, b: &Value) -> bool {
    let close = |x: f64, y: f64| (x - y).abs() <= 1e-6 * x.abs().max(y.abs()).max(1.0) || (x.is_nan() && y.is_nan());
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => close(*x, *y),
        (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => close(*x as f64, *y),
        (Value::Vec2(x), Value::Vec2(y)) => x.iter().zip(y).all(|(p, q)| close(*p, *q)),
        (Value::Vec3(x), Value::Vec3(y)) => x.iter().zip(y).all(|(p, q)| close(*p, *q)),
        (Value::Vec4(x), Value::Vec4(y)) => x.iter().zip(y).all(|(p, q)| close(*p, *q)),
        (Value::FloatArray(x), Value::FloatArray(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| close(*p as f64, *q as f64)),
        (Value::Vec2Array(x), Value::Vec2Array(y)) => x.len() == y.len() && x.iter().flatten().zip(y.iter().flatten()).all(|(p, q)| close(*p as f64, *q as f64)),
        (Value::Vec3Array(x), Value::Vec3Array(y)) => x.len() == y.len() && x.iter().flatten().zip(y.iter().flatten()).all(|(p, q)| close(*p as f64, *q as f64)),
        (Value::Vec4Array(x), Value::Vec4Array(y)) => x.len() == y.len() && x.iter().flatten().zip(y.iter().flatten()).all(|(p, q)| close(*p as f64, *q as f64)),
        (Value::String(x) | Value::Token(x), Value::String(y) | Value::Token(y)) => x == y,
        (Value::TokenArray(x) | Value::StringArray(x), Value::TokenArray(y) | Value::StringArray(y)) => x == y,
        _ => a == b,
    }
}

/// The fields both readers fill the same way (metadata dictionaries etc. aside).
const COMPARED: &[&str] = &["typeName", "default", "primChildren", "connectionPaths", "targetPaths", "interpolation", "specifier"];

#[test]
fn usdc_reader_matches_usdcat() {
    let files = usdc_files();
    if files.is_empty() {
        eprintln!("no test_3d_files; skipping");
        return;
    }
    for path in files {
        let Some(text) = usdcat(&path) else {
            eprintln!("usdcat not available; skipping");
            return;
        };
        let bin = std::fs::read(&path).unwrap();
        let t = std::time::Instant::now();
        let a: Layer = read_layer(&bin, &Limits::default()).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let ta = t.elapsed();
        let t = std::time::Instant::now();
        let b: Layer = read_layer(&text, &Limits::default()).unwrap_or_else(|e| panic!("{} (usda): {e}", path.display()));
        let tb = t.elapsed();
        println!("{}: {} specs; usdc {:.1} ms, usda {:.1} ms", path.file_name().unwrap().to_string_lossy(), a.specs.len(), ta.as_secs_f64() * 1e3, tb.as_secs_f64() * 1e3);

        let mut checked = 0;
        for (p, spec) in &a.specs {
            let other = b.spec(p).unwrap_or_else(|| panic!("{p}: in the usdc but not the usda"));
            for &field in COMPARED {
                if let (Some(x), Some(y)) = (spec.field(field), other.field(field)) {
                    assert!(same(x, y), "{p} {field}:\n usdc {x:?}\n usda {y:?}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "compared {checked} fields");
        // Every prim and property usdcat printed is in the binary read too.
        for p in b.specs.keys() {
            assert!(a.spec(p).is_some(), "{p}: in the usda but not the usdc");
        }

        // The geometry helpers see a mesh with faces and a preview material.
        let meshes: Vec<_> = a.specs.keys().filter(|p| a.type_name(p) == Some("Mesh")).collect();
        assert!(!meshes.is_empty());
        for m in meshes {
            let mesh = geom::mesh(&a, m).unwrap();
            let corners: i64 = mesh.face_vertex_counts.iter().sum();
            assert_eq!(corners as usize, mesh.face_vertex_indices.len());
            assert!(mesh.uvs.is_some(), "{m} has uvs");
            let mat = mesh.subsets.iter().find_map(|s| s.material.clone()).or_else(|| geom::bound_material(&a, m, true)).unwrap();
            let surface = geom::preview_surface(&a, &mat).unwrap();
            assert!(matches!(surface.diffuse_color, geom::Input::Texture(_)), "{mat}: diffuse is a texture");
        }
    }
}

/// The composition fixtures with every layer converted to binary (same names), via
/// usdcat: composing must give what the text layers give.
#[test]
fn composing_binary_layers_matches_text() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compose");
    let tmp = std::env::temp_dir().join(format!("usd-lite-binary-{}", std::process::id()));
    std::fs::create_dir_all(tmp.join("parts")).unwrap();
    for f in ["root.usda", "sub.usda", "parts/wheel.usda", "parts/box.usda"] {
        // usdcat only takes --usdFormat with a .usd name; rename after.
        let out = tmp.join(f).with_extension("usd");
        let ok = Command::new("usdcat").arg(src.join(f)).args(["--usdFormat", "usdc", "-o"]).arg(&out).status().is_ok_and(|s| s.success());
        if !ok || std::fs::rename(&out, tmp.join(f)).is_err() {
            eprintln!("usdcat not available; skipping");
            return;
        }
        assert!(std::fs::read(tmp.join(f)).unwrap().starts_with(b"PXR-USDC"), "{f} is binary");
    }
    let compose_at = |dir: &Path| {
        let root = dir.join("root.usda").canonicalize().unwrap();
        let layer = read_layer(&std::fs::read(&root).unwrap(), &Limits::default()).unwrap();
        usd_lite::compose(layer, &root.to_string_lossy(), &usd_lite::FsResolver::default(), &Limits::default())
    };
    let (text, binary) = (compose_at(&src), compose_at(&tmp));
    assert!(binary.warnings.is_empty(), "{:?}", binary.warnings);
    let mut a: Vec<_> = text.layer.specs.keys().collect();
    let mut b: Vec<_> = binary.layer.specs.keys().collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);
    for (path, spec) in &text.layer.specs {
        let other = binary.layer.spec(path).unwrap();
        for field in ["typeName", "default", "targetPaths", "connectionPaths", "primChildren"] {
            if let (Some(x), Some(y)) = (spec.field(field), other.field(field)) {
                let same = match (x, y) {
                    // Different roots: compare the part below them.
                    (Value::Asset(p), Value::Asset(q)) => p.rsplit("compose").next() == q.rsplit(tmp.file_name().unwrap().to_str().unwrap()).next(),
                    (Value::Float(p), Value::Float(q)) => (p - q).abs() < 1e-6,
                    _ => same(x, y),
                };
                assert!(same, "{path} {field}: text {x:?} vs binary {y:?}");
            }
        }
    }
    std::fs::remove_dir_all(tmp).unwrap();
}
