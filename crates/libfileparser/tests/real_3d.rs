//! Real-world 3D files, kept out of git (corpus/fetch-real-3d.sh): every model under
//! `LFP_REAL_3D`, opened through the front door with its own folder readable, as an app
//! would, every facet made and read. Opening isn't enough to pass here: a report also
//! counts models that opened empty, with broken bounds, or with files they couldn't
//! find. A panic or hang fails the test; `LFP_REAL_3D_OUT` writes one line per file.
//!
//! ```text
//! LFP_REAL_3D=$PWD/corpus/real-3d cargo test --release -p libfileparser --features docs-mupdf --test real_3d -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use libfileparser::{FileSource, FolderFiles, ParseContext};

const LIMIT: Duration = Duration::from_secs(120);

/// Where the models are: references may climb to anywhere inside (test suites share
/// folders of common parts), as when a user has granted the whole folder.
fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("LFP_REAL_3D").unwrap_or_default())
}

fn reader_for(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "gltf" | "glb" => "gltf",
        "fbx" => "fbx",
        "obj" => "obj",
        "stl" => "stl",
        "ply" => "ply",
        "off" => "off",
        "usd" | "usda" | "usdc" | "usdz" => "usd",
        "blend" => "blend",
        _ => return None,
    })
}

fn extension(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if reader_for(&extension(&p)).is_some() {
            out.push(p);
        }
    }
}

/// Fixtures broken on purpose, by their path.
fn broken_on_purpose(rel: &str) -> bool {
    let r = rel.to_ascii_lowercase();
    ["negative", "invalid", "fuzz", "corrupt", "truncat", "_fail", "fail_", "/bad"].iter().any(|w| r.contains(w))
}

#[derive(Clone)]
enum Outcome {
    /// reader, summary
    Opened(String, String),
    Failed(String),
    Panicked(String),
    Hung,
}

fn open(path: PathBuf) -> Outcome {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let result = std::panic::catch_unwind(|| {
                let cx = ParseContext::new().with_files(FolderFiles::within(root()));
                match FileSource::open(&path).map_err(|e| e.to_string()).and_then(|src| libfileparser::open(&src, &cx).map_err(|e| e.to_string())) {
                    Ok(doc) => {
                        libfileparser_testkit::exercise(&doc, &cx);
                        Outcome::Opened(doc.format().reader.to_string(), libfileparser_testkit::summarize(&doc, &cx))
                    }
                    Err(e) => Outcome::Failed(e),
                }
            });
            let _ = tx.send(result.unwrap_or_else(|p| Outcome::Panicked(p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default())));
        })
        .expect("a thread");
    rx.recv_timeout(LIMIT).unwrap_or(Outcome::Hung)
}

/// The reader's own answer, for a file the front door quietly gave to another reader.
fn why_refused(path: &Path, expected: &str) -> String {
    let path = path.to_path_buf();
    let expected = expected.to_string();
    std::thread::spawn(move || {
        let cx = ParseContext::new().with_files(FolderFiles::within(root()));
        let src = FileSource::open(&path).unwrap();
        let Some(r) = libfileparser_3d::readers().iter().find(|r| r.info().id == expected) else { return "no such reader".to_string() };
        match r.open(&src, &cx) {
            Ok(_) => "the reader opens it; detection chose another".to_string(),
            Err(e) => e.to_string(),
        }
    })
    .join()
    .unwrap_or_else(|_| "panicked".to_string())
}

#[derive(Default)]
struct Row {
    files: usize,
    ok: usize,
    as_other: usize,
    failed: usize,
    empty: usize,
    bad_bounds: usize,
    missing_deps: usize,
    undecoded: usize,
    purpose: usize,
    secs: f64,
    slowest: (f64, String),
    examples: BTreeMap<String, Vec<String>>,
}

impl Row {
    fn note(&mut self, why: String, file: &str) {
        self.examples.entry(why).or_default().push(file.to_string());
    }
}

/// `decoded/total` from a summary's scene line.
fn textures(scene: &str) -> Option<(usize, usize)> {
    let t = scene.split(" textures decoded").next()?.rsplit(' ').next()?;
    let (a, b) = t.split_once('/')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

#[test]
#[ignore = "needs LFP_REAL_3D: real-world files kept out of git"]
fn real_3d_files() {
    let Some(root) = std::env::var_os("LFP_REAL_3D").map(PathBuf::from) else {
        eprintln!("LFP_REAL_3D isn't set; nothing to check");
        return;
    };
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    let queue = Arc::new(Mutex::new(files.into_iter().rev().collect::<Vec<_>>()));
    let results = Arc::new(Mutex::new(Vec::new()));
    let workers: Vec<_> = (0..std::thread::available_parallelism().map_or(4, |n| n.get()).min(6))
        .map(|_| {
            let (queue, results) = (queue.clone(), results.clone());
            std::thread::spawn(move || {
                loop {
                    let Some(path) = queue.lock().unwrap().pop() else { break };
                    let t = Instant::now();
                    let o = open(path.clone());
                    results.lock().unwrap().push((path, o, t.elapsed().as_secs_f64()));
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    let mut results = std::mem::take(&mut *results.lock().unwrap());
    results.sort_by(|a, b| a.0.cmp(&b.0));

    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut broken = Vec::new();
    let mut lines = String::new();
    for (path, outcome, secs) in &results {
        let rel = path.strip_prefix(&root).unwrap().to_string_lossy().into_owned();
        let ext = extension(path);
        let on_purpose = broken_on_purpose(&rel);
        let row = rows.entry(ext.clone()).or_default();
        row.files += 1;
        row.secs += secs;
        if *secs > row.slowest.0 {
            row.slowest = (*secs, rel.clone());
        }
        let status = match outcome {
            Outcome::Panicked(p) => {
                broken.push(format!("{rel}: panicked: {p}"));
                "PANIC".to_string()
            }
            Outcome::Hung => {
                broken.push(format!("{rel}: took over {LIMIT:?}"));
                "HUNG".to_string()
            }
            Outcome::Failed(e) if on_purpose => {
                row.purpose += 1;
                format!("refused-on-purpose\t{e}")
            }
            Outcome::Failed(e) => {
                row.failed += 1;
                let key: String = e.chars().take(80).collect();
                row.note(format!("failed: {key}"), &rel);
                format!("FAILED\t{e}")
            }
            Outcome::Opened(reader, _) if reader != reader_for(&ext).unwrap() => {
                let why = why_refused(path, reader_for(&ext).unwrap());
                if on_purpose {
                    row.purpose += 1;
                } else {
                    row.as_other += 1;
                    let key: String = why.chars().take(70).collect();
                    row.note(format!("opened as {reader}, because: {key}"), &rel);
                }
                format!("AS\t{reader}\t{why}")
            }
            Outcome::Opened(_, summary) => {
                row.ok += 1;
                let scene = summary.lines().find(|l| l.trim_start().starts_with("scene:")).unwrap_or("");
                // A .blend shows its preview today, so it has no scene to check.
                if !scene.is_empty() {
                    if scene.contains(": 0 meshes") {
                        row.empty += 1;
                        row.note("opened with no meshes".into(), &rel);
                    }
                    let bounds = summary.lines().find(|l| l.trim_start().starts_with("bounds:")).unwrap_or("").to_ascii_lowercase();
                    if bounds.contains("nan") || bounds.contains("inf") {
                        row.bad_bounds += 1;
                        row.note("bounds not finite".into(), &rel);
                    }
                    if let Some((d, t)) = textures(scene) {
                        if d < t {
                            row.undecoded += 1;
                            row.note("textures not decoded".into(), &rel);
                        }
                    }
                }
                let missing = summary.lines().filter(|l| l.starts_with("dependency:") && !l.ends_with(" Read")).count();
                if missing > 0 {
                    row.missing_deps += 1;
                    row.note("dependencies not found".into(), &rel);
                }
                format!("ok\t{}", scene.trim())
            }
        };
        lines += &format!("{secs:.3}\t{rel}\t{status}\n");
    }
    if let Some(out) = std::env::var_os("LFP_REAL_3D_OUT") {
        std::fs::write(out, &lines).unwrap();
    }
    println!("\n{:<6} {:>6} {:>6} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>7} {:>8}", "ext", "files", "ok", "other", "fail", "purpose", "empty", "bounds", "deps", "undecod", "seconds");
    for (ext, r) in &rows {
        println!("{ext:<6} {:>6} {:>6} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>7} {:>8.1}", r.files, r.ok, r.as_other, r.failed, r.purpose, r.empty, r.bad_bounds, r.missing_deps, r.undecoded, r.secs);
    }
    for (ext, r) in &rows {
        println!("\n{ext}: slowest {:.1}s {}", r.slowest.0, r.slowest.1);
        for (why, fs) in &r.examples {
            println!("    {why}: {} (e.g. {})", fs.len(), fs.iter().take(2).cloned().collect::<Vec<_>>().join(", "));
        }
    }
    assert!(broken.is_empty(), "{} files crashed or hung:\n{}", broken.len(), broken.join("\n"));
}
