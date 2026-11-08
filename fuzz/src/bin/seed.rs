//! Seeds each target's corpus from real files: `seed <out> <dir>...` writes, for every
//! file a reader of a category claims, that reader's byte and the file to
//! `<out>/<target>/`, and the files themselves to the lite targets by extension. Files
//! over 1 MiB are left out: libFuzzer works on small inputs.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use libfileparser_core::{Confidence, Reader};

const MAX: u64 = 1 << 20;

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else if e.metadata().is_ok_and(|m| m.len() <= MAX && m.len() > 0) {
            out.push(p);
        }
    }
}

fn write(out: &Path, target: &str, bytes: &[u8]) {
    let dir = out.join(target);
    let _ = std::fs::create_dir_all(&dir);
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    let _ = std::fs::write(dir.join(format!("{:016x}", h.finish())), bytes);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((out, dirs)) = args.split_first() else {
        eprintln!("usage: seed <out> <dir>...");
        std::process::exit(2);
    };
    let out = Path::new(out);
    let mut all = Vec::new();
    for d in dirs {
        files(Path::new(d), &mut all);
    }
    let categories: [(&str, &[&'static dyn Reader]); 7] = [
        ("data", libfileparser_data::readers()),
        ("sheets", libfileparser_sheets::readers()),
        ("docs", libfileparser_docs::readers()),
        ("threed", libfileparser_3d::readers()),
        ("text", libfileparser_text::readers()),
        ("archives", libfileparser_archives::readers()),
        ("images", libfileparser_images::readers()),
    ];
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for path in &all {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let head = &bytes[..bytes.len().min(4096)];
        for (target, readers) in categories {
            let best = readers.iter().enumerate().map(|(i, r)| (i, r.detect(head, name).confidence)).filter(|(_, c)| *c > Confidence::None).max_by_key(|(_, c)| *c);
            if let Some((i, _)) = best {
                let mut seed = vec![i as u8];
                seed.extend_from_slice(&bytes);
                write(out, target, &seed);
                *counts.entry(target).or_default() += 1;
            }
        }
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let lite = match ext.as_str() {
            "rtf" => Some("rtf"),
            "fods" => Some("ods_flat"),
            "usda" | "usdc" | "usd" => Some("usd"),
            "gltf" | "glb" => Some("gltf"),
            _ => None,
        };
        if let Some(t) = lite {
            write(out, t, &bytes);
            *counts.entry(t).or_default() += 1;
        }
    }
    for (t, n) in counts {
        println!("{t}: {n}");
    }
}
