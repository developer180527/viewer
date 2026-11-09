//! Real-world files, kept out of git: a folder (`LFP_REAL_CORPUS`, usually
//! `corpus/real`) of category folders (`data/`, `docs/`, `sheets/`), each holding folders
//! named by the reader their files should open with (`toml/`, `x12/`), or `<reader>-invalid/`
//! for files broken on purpose (test suites' negative cases), which must fail or fall
//! back cleanly. Every file opens through the front door on a thread of its own with
//! a time limit, every facet is made and read; a panic or hang fails, and a report
//! says, per folder, how many opened as their reader, as another, or not at all.
//! `3d/` holds whole repositories, kept as they are for relative paths: real_3d.rs
//! reads it.
//!
//! ```text
//! LFP_REAL_CORPUS=$PWD/corpus/real cargo test -p libfileparser --test real_world -- --ignored --nocapture
//! ```
//!
//! `LFP_REAL_ONLY=xls,docx` checks only those reader folders; `LFP_REAL_VERBOSE=1` prints
//! every file that didn't open, with its whole error.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use libfileparser::{FileSource, ParseContext};

/// What opening one file did.
enum Outcome {
    /// Opened, by this reader, after these readers failed.
    Opened(String, Vec<String>),
    Failed(String),
    Panicked(String),
    Hung,
}

fn open(path: PathBuf) -> Outcome {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("fuzz real".into())
        .stack_size(8 << 20)
        .spawn(move || {
            let result = std::panic::catch_unwind(|| {
                let cx = ParseContext::new().with_limits(libfileparser_testkit::Fuzz::new().limits);
                let opened = FileSource::open(&path).map_err(|e| e.to_string()).and_then(|src| {
                    let (doc, tried) = libfileparser::open_reporting(&src, &cx);
                    doc.map(|d| (d, tried.iter().map(|a| format!("{}: {}", a.reader, a.error)).collect())).map_err(|e| e.to_string())
                });
                match opened {
                    Ok((doc, tried)) => {
                        libfileparser_testkit::exercise(&doc, &cx);
                        Outcome::Opened(doc.format().reader.to_string(), tried)
                    }
                    Err(e) => Outcome::Failed(e),
                }
            });
            let _ = tx.send(result.unwrap_or_else(|p| {
                let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
                Outcome::Panicked(msg)
            }));
        })
        .expect("a thread");
    rx.recv_timeout(Duration::from_secs(20)).unwrap_or(Outcome::Hung)
}

#[test]
#[ignore = "needs LFP_REAL_CORPUS: real-world files kept out of git"]
fn real_world_files() {
    let Some(root) = std::env::var_os("LFP_REAL_CORPUS").map(PathBuf::from) else {
        eprintln!("LFP_REAL_CORPUS isn't set; nothing to check");
        return;
    };
    let dirs = |d: &PathBuf| -> Vec<PathBuf> { std::fs::read_dir(d).map(|r| r.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default() };
    // Reader folders, inside category folders (or, as before, at the top).
    let mut folders: Vec<PathBuf> = Vec::new();
    for top in dirs(&root) {
        let name = top.file_name().unwrap().to_string_lossy().into_owned();
        match name.as_str() {
            "3d" => {}
            "data" | "docs" | "sheets" | "text" | "images" | "archives" => folders.extend(dirs(&top)),
            _ => folders.push(top),
        }
    }
    folders.sort_by_key(|f| f.file_name().map(|n| n.to_owned()));
    if let Ok(only) = std::env::var("LFP_REAL_ONLY") {
        let only: Vec<&str> = only.split(',').map(str::trim).collect();
        folders.retain(|f| only.contains(&&*f.file_name().unwrap().to_string_lossy()));
    }
    let verbose = std::env::var_os("LFP_REAL_VERBOSE").is_some();
    let mut broken = Vec::new();
    for folder in folders {
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        let (expected, invalid) = match name.strip_suffix("-invalid") {
            Some(r) => (r.to_string(), true),
            None => (name.clone(), false),
        };
        let mut files: Vec<PathBuf> = std::fs::read_dir(&folder).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        let (mut ok, mut other, mut failed) = (0, BTreeMap::<String, Vec<String>>::new(), BTreeMap::<String, Vec<String>>::new());
        for path in &files {
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            match open(path.clone()) {
                Outcome::Opened(r, _) if r == expected => ok += 1,
                Outcome::Opened(r, tried) => {
                    if verbose {
                        println!("  {name}/{file}: opened as {r}, after {}", tried.join("; "));
                    }
                    other.entry(r).or_default().push(file)
                }
                Outcome::Failed(e) => {
                    if verbose {
                        println!("  {name}/{file}: {e}");
                    }
                    // Group errors by their first words.
                    let key: String = e.split([':', '(']).take(2).collect::<Vec<_>>().join(":").chars().take(70).collect();
                    failed.entry(key).or_default().push(file);
                }
                Outcome::Panicked(p) => broken.push(format!("{name}/{file}: panicked: {p}")),
                Outcome::Hung => broken.push(format!("{name}/{file}: hung")),
            }
        }
        let n = files.len();
        let others: usize = other.values().map(Vec::len).sum();
        let fails: usize = failed.values().map(Vec::len).sum();
        println!("\n{name}: {n} files — {ok} opened as {expected}, {others} as another reader, {fails} didn't open{}", if invalid { " (broken on purpose)" } else { "" });
        for (r, fs) in &other {
            println!("    as {r}: {} (e.g. {})", fs.len(), fs.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
        }
        for (e, fs) in &failed {
            println!("    {e}: {} (e.g. {})", fs.len(), fs.iter().take(2).cloned().collect::<Vec<_>>().join(", "));
        }
    }
    assert!(broken.is_empty(), "{} files crashed or hung:\n{}", broken.len(), broken.join("\n"));
}
