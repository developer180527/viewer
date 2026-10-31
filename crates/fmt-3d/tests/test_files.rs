//! Loads the models in `test_3d_files/` (not in git) when they're present.

use viewer_core::{Content, FileSource};

#[test]
fn local_test_models_load() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_3d_files");
    let Ok(walk) = walk(&dir) else {
        eprintln!("no test_3d_files; skipping");
        return;
    };
    for path in walk {
        let src = FileSource::open(&path).unwrap();
        let Some(loader) = viewer_fmt_3d::LOADERS.iter().find(|l| l.probe(src.header(), src.ext().as_deref(), src.name()).0 > 0) else {
            continue;
        };
        let t = std::time::Instant::now();
        match loader.open(&src) {
            Ok(doc) => {
                let kind = match &doc.content {
                    Content::Scene(_) => "scene",
                    Content::Raster(r) => &format!("image {}×{}", r.width, r.height),
                    _ => "other",
                };
                println!("{:>8.1} ms  {}  [{}] {}", t.elapsed().as_secs_f64() * 1000.0, src.name(), kind, doc.format);
                for (k, v) in &doc.metadata {
                    println!("             {k}: {v}");
                }
            }
            Err(e) => println!("  FAILED  {}  {e:#}", src.name()),
        }
    }
}

fn walk(dir: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() { out.extend(walk(&p)?) } else { out.push(p) }
    }
    out.sort();
    Ok(out)
}

/// `VIEWER_MODEL=path cargo test -p viewer-fmt-3d --test test_files model -- --nocapture`
/// prints what a model loads as.
#[test]
fn model_from_env() {
    let Ok(path) = std::env::var("VIEWER_MODEL") else { return };
    let src = FileSource::open(&path).unwrap();
    let loader = viewer_fmt_3d::LOADERS.iter().find(|l| l.probe(src.header(), src.ext().as_deref(), src.name()).0 > 0).expect("a 3D loader");
    let t = std::time::Instant::now();
    let doc = loader.open(&src).unwrap();
    println!("{path}: {} in {:.1} ms", doc.format, t.elapsed().as_secs_f64() * 1000.0);
    if let Content::Scene(scene) = &doc.content {
        while scene.textures_pending.load(std::sync::atomic::Ordering::Acquire) > 0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        println!("  textures decoded after {:.1} ms, {} MB on the CPU", t.elapsed().as_secs_f64() * 1000.0, scene.scene.read().cpu_bytes() >> 20);
    }
    for (k, v) in &doc.metadata {
        println!("  {k}: {v}");
    }
}
