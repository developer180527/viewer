//! Opens every file in corpus/manifest.toml and compares what it opened as with
//! corpus/expected/ (corpus/README.md).
//!
//! After an intended change, rewrite the expected files and review the diff:
//!     UPDATE_CORPUS=1 cargo test -p viewer-corpus
//! A failing render leaves what was drawn and a diff in `target/tmp/corpus/`.

use std::path::{Path, PathBuf};

use image::RgbaImage;
use viewer_corpus::{Entry, manifest, open, root};

/// A pixel counts as changed when a channel moves more than this: decoders and
/// rasterisers round slightly differently across platforms.
const CHANNEL_TOLERANCE: u8 = 16;
/// The share of a render's pixels that may change before it fails.
const PIXEL_TOLERANCE: f64 = 0.01;

fn expected(entry: &Entry, ext: &str) -> PathBuf {
    root().join("corpus/expected").join(format!("{}.{ext}", entry.path))
}

/// What differs, or `None` when the file opened as expected.
fn check(entry: &Entry, update: bool, out: &Path) -> Option<String> {
    if (entry.optional && !root().join(&entry.path).exists()) || !entry.checked_here() {
        return None;
    }
    let opened = match open(entry) {
        Ok(o) => o,
        Err(e) => return Some(format!("{}: {e}", entry.path)),
    };
    let summary = opened.summary.to_string();
    let summary_path = expected(entry, "toml");
    let render_path = expected(entry, "png");
    if update {
        std::fs::create_dir_all(summary_path.parent().unwrap()).unwrap();
        std::fs::write(&summary_path, &summary).unwrap();
        if let Some(img) = &opened.render {
            img.save(&render_path).unwrap();
        }
        return None;
    }

    let mut problems = Vec::new();
    match std::fs::read_to_string(&summary_path) {
        Ok(want) if want == summary => {}
        Ok(want) => problems.push(format!("summary changed:\n{}", line_diff(&want, &summary))),
        Err(_) => problems.push(format!("no {} (run with UPDATE_CORPUS=1)", summary_path.display())),
    }
    match (&opened.render, entry.golden) {
        (Some(got), _) => match image::open(&render_path) {
            Ok(want) => {
                if let Some(p) = compare(&want.to_rgba8(), got, &out.join(entry.path.replace('/', "__"))) {
                    problems.push(p);
                }
            }
            Err(_) => problems.push(format!("no {} (run with UPDATE_CORPUS=1)", render_path.display())),
        },
        (None, true) if opened.no_gpu => {}
        (None, true) => problems.push("golden is set, but this content draws no render".into()),
        (None, false) => {}
    }
    (!problems.is_empty()).then(|| format!("{}: {}", entry.path, problems.join("\n  ")))
}

/// Compares two renders; on a difference, saves what was drawn and a diff next to `stem`.
fn compare(want: &RgbaImage, got: &RgbaImage, stem: &Path) -> Option<String> {
    if want.dimensions() != got.dimensions() {
        return Some(format!("render is {:?}, expected {:?}", got.dimensions(), want.dimensions()));
    }
    let mut diff = RgbaImage::new(got.width(), got.height());
    let mut changed = 0;
    for ((a, b), d) in want.pixels().zip(got.pixels()).zip(diff.pixels_mut()) {
        let moved = a.0.iter().zip(b.0).any(|(x, y)| x.abs_diff(y) > CHANNEL_TOLERANCE);
        changed += moved as usize;
        *d = if moved { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([a.0[0] / 3, a.0[1] / 3, a.0[2] / 3, 255]) };
    }
    let share = changed as f64 / (got.width() * got.height()).max(1) as f64;
    if share <= PIXEL_TOLERANCE {
        return None;
    }
    let _ = got.save(stem.with_extension("got.png"));
    let _ = diff.save(stem.with_extension("diff.png"));
    Some(format!("render differs in {:.2}% of pixels (see {})", share * 100.0, stem.with_extension("diff.png").display()))
}

/// The lines that differ, marked - (expected) and + (got).
fn line_diff(want: &str, got: &str) -> String {
    let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
    let mut out = Vec::new();
    for l in &w {
        if !g.contains(l) {
            out.push(format!("    - {l}"));
        }
    }
    for l in &g {
        if !w.contains(l) {
            out.push(format!("    + {l}"));
        }
    }
    out.join("\n")
}

#[test]
fn corpus() {
    let update = std::env::var_os("UPDATE_CORPUS").is_some();
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("corpus");
    std::fs::create_dir_all(&out).unwrap();
    let entries = manifest().expect("reading corpus/manifest.toml");

    // Each file on its own thread (open() adds one more for the timeout): the corpus
    // is mostly waiting on decoders, so this keeps it to a few seconds.
    let failures: Vec<String> = std::thread::scope(|s| {
        let runs: Vec<_> = entries.iter().map(|e| s.spawn(|| check(e, update, &out))).collect();
        runs.into_iter().filter_map(|r| r.join().unwrap()).collect()
    });
    let skipped = entries.iter().filter(|e| (e.optional && !root().join(&e.path).exists()) || !e.checked_here()).count();
    if skipped > 0 {
        eprintln!("corpus: skipped {skipped} files that aren't here or aren't checked on this OS");
    }
    assert!(failures.is_empty(), "{} of {} corpus files differ:\n{}", failures.len(), entries.len(), failures.join("\n"));
}

/// Loaders whose only corpus files are optional (too big, or no small file we can
/// write ourselves yet), so CI doesn't exercise them. Shrink this list.
const LOCAL_ONLY: &[&str] = &[];

/// The corpus checks the app's own registry: every loader it ships opens at least one
/// committed corpus file, so a new loader without one shows up as a failure.
#[test]
fn every_loader_has_a_corpus_file() {
    let mut used: std::collections::BTreeSet<String> = LOCAL_ONLY.iter().map(|s| s.to_string()).collect();
    for entry in manifest().unwrap().into_iter().filter(|e| !e.optional) {
        if let Ok(text) = std::fs::read_to_string(expected(&entry, "toml")) {
            let t: toml::Table = text.parse().unwrap();
            if let Some(l) = t.get("loader").and_then(toml::Value::as_str) {
                used.insert(l.to_string());
            }
        }
    }
    let missing: Vec<&str> =
        viewer_formats::registry().loaders().iter().map(|l| l.id()).filter(|id| !used.contains(*id)).collect();
    assert!(missing.is_empty(), "loaders with no corpus file opening as them: {missing:?}");
}
