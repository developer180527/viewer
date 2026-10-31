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
