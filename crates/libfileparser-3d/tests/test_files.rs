//! Opens the models in `test_3d_files/` (not in git) when they're present.

use libfileparser_core::{Content, FileSource, ParseContext};

fn describe(src: &FileSource) {
    let t = std::time::Instant::now();
    match libfileparser_3d::open(src, &ParseContext::new()) {
        Ok(doc) => {
            let kind = match doc.facet(0, &ParseContext::new()) {
                Ok(Content::Scene(s)) => format!("scene, {} MB on the CPU", s.scene().read().cpu_bytes() >> 20),
                Ok(Content::Raster(r)) => format!("image {}×{}", r.width(), r.height()),
                _ => "other".into(),
            };
            println!("{:>8.1} ms  {}  [{kind}] {}", t.elapsed().as_secs_f64() * 1000.0, src.name(), doc.format().name);
            for (k, v) in doc.metadata() {
                println!("             {k}: {v}");
            }
            for d in doc.dependencies().list() {
                println!("             file {} {:?}", d.reference, d.status);
            }
        }
        Err(e) => println!("  FAILED  {}  {e}", src.name()),
    }
}

#[test]
fn local_test_models_load() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_3d_files");
    let Ok(walk) = walk(&dir) else {
        eprintln!("no test_3d_files; skipping");
        return;
    };
    for path in walk {
        let src = FileSource::open(&path).unwrap();
        if !libfileparser_3d::detect(src.header(), src.name()).is_empty() {
            describe(&src);
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

/// `VIEWER_MODEL=path cargo test -p libfileparser-3d --test test_files model -- --nocapture`
/// prints what a model opens as. Textures decode inline (the default context), so the
/// time includes them.
#[test]
fn model_from_env() {
    let Ok(path) = std::env::var("VIEWER_MODEL") else { return };
    describe(&FileSource::open(&path).unwrap());
}
