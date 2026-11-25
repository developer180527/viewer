//! The canvas the SwiftUI shell's Metal view uses, drawn offscreen: each kind of
//! drawn content, opened through a session, waited on (decodes, shader compiles)
//! as a shell would when woken, then its pixels checked. Needs a GPU; without one
//! the test says so and passes. Frames are saved to target/canvas-test/.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use viewer_core::{Hooks, Session};
use viewer_surface::{Canvas, Wake};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Draws until nothing more is coming: no frame asked for, and no wake for a while.
fn draw_settled(canvas: &mut Canvas, session: &mut Session, woke: &AtomicBool) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut frames = 0;
    let mut quiet = Instant::now();
    while Instant::now() < deadline {
        session.poll();
        let drawn = canvas.draw(session.pages(), session.current());
        frames += 1;
        if matches!(drawn.repaint_after, Some(t) if t <= 0.05) {
            quiet = Instant::now();
            continue;
        }
        std::thread::sleep(Duration::from_millis(5));
        if woke.swap(false, Ordering::SeqCst) {
            canvas.touch();
            quiet = Instant::now();
        } else if quiet.elapsed() > Duration::from_millis(400) {
            return frames;
        }
    }
    panic!("never settled");
}

#[test]
fn drawn_content_reaches_the_pixels() {
    let woke = Arc::new(AtomicBool::new(false));
    let w = woke.clone();
    let wake: Wake = Arc::new(move || w.store(true, Ordering::SeqCst));
    let ws = woke.clone();
    let mut session = Session::new(Hooks::new(Arc::new(move || ws.store(true, Ordering::SeqCst))));
    let mut canvas = match Canvas::offscreen(800, 600, 1.0, true, wake) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("no GPU, skipped: {e:#}");
            return;
        }
    };
    let out = root().join("target/canvas-test");
    std::fs::create_dir_all(&out).unwrap();
    let background = {
        canvas.draw(&[], None);
        let (_, _, px) = canvas.read_pixels().unwrap();
        [px[0], px[1], px[2]]
    };
    // Each file, the colours its frame needs at least, and how much of the frame
    // (a share) differs from the empty canvas: text is sparse, pages and pictures not.
    for (file, min_colors, min_drawn) in [
        ("samples/data.json", 4, 0.005),
        ("samples/gradient.png", 64, 0.1),
        ("samples/logo.svg", 4, 0.02),
        ("samples/README.md", 4, 0.005),
        ("corpus/files/docs/two-pages.pdf", 4, 0.2),
        ("corpus/files/3d/cube.glb", 8, 0.02),
        ("corpus/files/3d/cube.stl", 8, 0.02),
    ] {
        session.open_path(root().join(file));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !session.poll() {
            assert!(Instant::now() < deadline, "{file}: the open never finished");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(session.error().is_none(), "{file}: {:?}", session.error());
        let frames = draw_settled(&mut canvas, &mut session, &woke);
        let (w, h, px) = canvas.read_pixels().unwrap();
        let name = file.rsplit('/').next().unwrap();
        image::save_buffer(out.join(format!("{name}.png")), &px, w, h, image::ColorType::Rgba8).unwrap();
        let colors: HashSet<[u8; 3]> = px.chunks(4).map(|p| [p[0], p[1], p[2]]).collect();
        let drawn = px.chunks(4).filter(|p| [p[0], p[1], p[2]] != background).count();
        eprintln!("{file}: {frames} frames, {} colours, {:.0}% drawn", colors.len(), 100.0 * drawn as f64 / (w * h) as f64);
        assert!(colors.len() >= min_colors, "{file}: only {} colours", colors.len());
        assert!(drawn as f64 > min_drawn * (w * h) as f64, "{file}: hardly anything drawn");
    }
    // Back to the root: the closed pages' views (and textures) are freed.
    while session.back() {}
    canvas.draw(session.pages(), session.current());
}

/// A resize lays the page out again at the new size, without waiting for input. (On
/// an iPad turned on its side the page stayed where portrait had it until touched:
/// views place themselves by the last frame's rects, so the canvas draws two frames
/// after a resize. Offscreen, the page's own redraws hid that, so this checks the
/// result rather than catching that case.)
#[test]
fn a_resize_lays_out_again() {
    let woke = Arc::new(AtomicBool::new(false));
    let w = woke.clone();
    let wake: Wake = Arc::new(move || w.store(true, Ordering::SeqCst));
    let ws = woke.clone();
    let mut session = Session::new(Hooks::new(Arc::new(move || ws.store(true, Ordering::SeqCst))));
    let Ok(mut canvas) = Canvas::offscreen(600, 800, 1.0, true, wake) else {
        eprintln!("no GPU, skipped");
        return;
    };
    let background = {
        canvas.draw(&[], None);
        let (_, _, px) = canvas.read_pixels().unwrap();
        [px[0], px[1], px[2]]
    };
    session.open_path(root().join("corpus/files/docs/two-pages.pdf"));
    while !session.poll() {
        std::thread::sleep(Duration::from_millis(1));
    }
    draw_settled(&mut canvas, &mut session, &woke);
    canvas.resize(1400, 800, 1.0);
    draw_settled(&mut canvas, &mut session, &woke);
    // The page's left and right edges along a row through its middle: centred.
    let (w, h, px) = canvas.read_pixels().unwrap();
    let row = (h / 2) as usize * w as usize * 4;
    let drawn: Vec<usize> = (0..w as usize).filter(|x| px[row + x * 4..row + x * 4 + 3] != background).collect();
    let (left, right) = (drawn[0], w as usize - 1 - drawn[drawn.len() - 1]);
    assert!(left.abs_diff(right) <= 4, "page not centred after the resize: {left} px left, {right} px right");
}

/// The SwiftUI shell's windows draw on one GPU: one device, and RendererX's pipelines
/// compiled once, not once per window; a new one once none is left.
#[test]
fn canvases_share_one_gpu_context() {
    let Ok(first) = viewer_surface::GpuContext::shared() else {
        eprintln!("no GPU; skipped");
        return;
    };
    let second = viewer_surface::GpuContext::shared().unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    let weak = std::sync::Arc::downgrade(&first);
    drop((first, second));
    assert!(weak.upgrade().is_none(), "freed with the last canvas");
}
