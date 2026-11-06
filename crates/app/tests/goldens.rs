//! Golden screenshots: each width class (docs/design/responsive-ui.md) drawn headless by
//! the CPU renderer and compared with the PNGs in `tests/goldens/`.
//!
//! After an intended UI change, regenerate them and look at the result:
//!     UPDATE_GOLDENS=1 cargo test -p viewer --test goldens
//! A failure leaves the new image and a diff (changed pixels in red) in
//! `target/tmp/goldens/`, which CI uploads.

use std::path::{Path, PathBuf};
use std::process::Command;

/// (name, sample, width × height in points, info panel open)
const CASES: &[(&str, &str, (u32, u32), bool)] = &[
    ("text-compact", "main.rs", (390, 844), false),
    ("text-medium", "main.rs", (800, 600), false),
    ("text-expanded", "main.rs", (1100, 760), false),
    ("info-compact", "main.rs", (390, 844), true),
    ("info-medium", "main.rs", (800, 600), true),
    ("info-expanded", "main.rs", (1100, 760), true),
    ("short-landscape", "main.rs", (844, 390), false),
    ("table-compact", "table.csv", (390, 844), false),
    ("tree-medium", "data.json", (800, 600), false),
    ("image-compact", "gradient.png", (390, 844), false),
    ("archive-expanded", "bundle.zip", (1100, 760), false),
];

/// A pixel counts as changed when a channel moves more than this: font
/// rasterisation and blending round slightly differently across CPUs.
const CHANNEL_TOLERANCE: u8 = 24;
/// The share of pixels that may change before a case fails. Renaming one button
/// ("Open…" to "Open file…") changes 0.04–0.1%, so this stays well under that.
const PIXEL_TOLERANCE: f64 = 0.0001;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn shoot(sample: &str, (w, h): (u32, u32), info: bool, out: &Path) -> image::RgbaImage {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_viewer"));
    // From the workspace root, so the info panel's path reads the same everywhere.
    cmd.current_dir(root()).arg("--screenshot").arg(out).arg("--size").arg(format!("{w}x{h}"));
    if info {
        cmd.arg("--info");
    }
    let run = cmd.arg(format!("samples/{sample}")).output().expect("running viewer");
    assert!(run.status.success(), "viewer failed on {sample}: {}", String::from_utf8_lossy(&run.stderr));
    image::open(out).expect("reading the screenshot").to_rgba8()
}

/// Changed pixels, and an image of them: the golden dimmed, changes in red.
fn diff(golden: &image::RgbaImage, got: &image::RgbaImage) -> (usize, image::RgbaImage) {
    let mut changed = 0;
    let mut img = image::RgbaImage::new(got.width(), got.height());
    for ((a, b), d) in golden.pixels().zip(got.pixels()).zip(img.pixels_mut()) {
        let moved = a.0.iter().zip(b.0).any(|(x, y)| x.abs_diff(y) > CHANNEL_TOLERANCE);
        *d = if moved {
            changed += 1;
            image::Rgba([255, 0, 0, 255])
        } else {
            let l = (a.0[0] as u16 + a.0[1] as u16 + a.0[2] as u16) / 12;
            image::Rgba([l as u8, l as u8, l as u8, 255])
        };
    }
    (changed, img)
}

#[test]
fn goldens() {
    let update = std::env::var_os("UPDATE_GOLDENS").is_some();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("goldens");
    std::fs::create_dir_all(&out).unwrap();

    // Each case is its own process; run them side by side.
    let failures: Vec<String> = std::thread::scope(|s| {
        let runs: Vec<_> = CASES
            .iter()
            .map(|&(name, sample, size, info)| {
                let (dir, out) = (&dir, &out);
                s.spawn(move || -> Option<String> {
                    let got_path = out.join(format!("{name}.png"));
                    let got = shoot(sample, size, info, &got_path);
                    let golden_path = dir.join(format!("{name}.png"));
                    if update {
                        std::fs::copy(&got_path, &golden_path).unwrap();
                        return None;
                    }
                    let Ok(golden) = image::open(&golden_path) else {
                        return Some(format!("{name}: no golden (run with UPDATE_GOLDENS=1)"));
                    };
                    let golden = golden.to_rgba8();
                    if golden.dimensions() != got.dimensions() {
                        return Some(format!("{name}: size {:?}, golden {:?}", got.dimensions(), golden.dimensions()));
                    }
                    let (changed, img) = diff(&golden, &got);
                    let share = changed as f64 / (got.width() * got.height()) as f64;
                    if share <= PIXEL_TOLERANCE {
                        return None;
                    }
                    img.save(out.join(format!("{name}-diff.png"))).unwrap();
                    Some(format!("{name}: {:.2}% of pixels changed (allowed {:.2}%)", share * 100.0, PIXEL_TOLERANCE * 100.0))
                })
            })
            .collect();
        runs.into_iter().filter_map(|r| r.join().unwrap()).collect()
    });
    assert!(
        failures.is_empty(),
        "golden screenshots differ (new images and diffs in {}):\n  {}",
        out.display(),
        failures.join("\n  ")
    );
}
