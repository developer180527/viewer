//! A 3D scene drawn by RendererX (through the host) into a texture. Drag to orbit,
//! Shift-drag (or two-finger pan) to pan, scroll or pinch to zoom, double-click or 0 to
//! reframe.

use libfileparser_core::SceneDoc;
use libgui::*;

use crate::{Frame3d, Host, SceneGpu};

#[derive(Default)]
pub struct SceneView {
    pub(crate) gpu: SceneGpu,
    controls: Option<rendererx::OrbitControls>,
    /// The user moved the camera, so don't reframe when loading finishes.
    moved: bool,
    /// Framed on the finished model's measured bounds (not the reader's early hint).
    framed_final: bool,
    /// The scene wakes the window when it changes (textures arriving).
    wakes: bool,
    /// Everything, textures included, reached the GPU.
    settled: bool,
}

impl SceneView {
    pub fn textures(&self) -> Vec<TextureId> {
        self.gpu.texture().into_iter().collect()
    }
}

pub fn scene(ui: &mut Ui, doc: &SceneDoc, st: &mut SceneView, host: &mut dyn Host) {
    if !std::mem::replace(&mut st.wakes, true) {
        let wake = host.waker();
        doc.scene().set_on_change(move || wake());
    }
    let pending = || doc.textures_pending();
    if host.settles() {
        // Screenshots show the finished model; decoding is bounded, but don't hang.
        let start = std::time::Instant::now();
        while pending() > 0 && start.elapsed().as_secs() < 60 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    let id = ui.make_id("scene");
    let resp = ui.interact_drag(id);
    let rect = resp.rect; // last frame's: one frame of latency, as for images
    let aspect = if rect.h > 0.0 { rect.w / rect.h } else { 1.0 };

    let (bounds, loading) = {
        let read = doc.scene().read();
        (read.bounds(), read.geometry() == rendererx::scene_data::GeometryState::Loading)
    };
    let reset = resp.double_clicked || (resp.hovered && ui.key_pressed(Key::Num0));
    if let Some(b) = bounds {
        // Frame as soon as there are bounds, and once more on the finished model's
        // measured bounds unless the user has taken over the camera.
        if st.controls.is_none() || reset || (!st.moved && !st.framed_final && !loading) {
            st.controls = Some(rendererx::OrbitControls::framing(b, aspect));
            st.framed_final = !loading;
            st.moved &= !reset;
        }
    }
    if let Some(c) = st.controls.as_mut() {
        let h = rect.h.max(1.0);
        if resp.active && resp.drag_delta != Vec2::ZERO {
            if resp.modifiers.shift {
                c.pan(resp.drag_delta.x, resp.drag_delta.y, h);
            } else {
                c.orbit(resp.drag_delta.x, resp.drag_delta.y, h);
            }
            st.moved = true;
        }
        if resp.hovered && resp.scroll.y != 0.0 {
            c.zoom((resp.scroll.y / 300.0).exp());
            st.moved = true;
        }
        let trackpad = if resp.hovered { host.trackpad_pinch() } else { 0.0 };
        if resp.pinch != 0.0 || trackpad != 0.0 {
            c.zoom((1.0 + resp.pinch) * (1.0 + trackpad));
            st.moved = true;
        }
        if resp.pan2 != Vec2::ZERO {
            c.pan(resp.pan2.x, resp.pan2.y, h);
            st.moved = true;
        }
    }

    let camera = st.controls.map(|c| c.camera()).unwrap_or_default();
    let pal = ui.theme.palette;
    let bg = [pal.bg_inset.r, pal.bg_inset.g, pal.bg_inset.b, 1.0];
    let scale = host.pixels_per_point();
    let size = ((rect.w * scale).round() as u32, (rect.h * scale).round() as u32);
    let frame = if size.0 > 0 && size.1 > 0 {
        host.render_scene(&mut st.gpu, doc.scene(), size, &camera, bg)
    } else {
        // The first frame has no rect yet; the next one will.
        Frame3d { busy: true, ..Default::default() }
    };
    if !frame.busy && pending() == 0 && frame.texture.is_some() && !std::mem::replace(&mut st.settled, true) {
        // All uploaded: let the host free the upload staging.
        host.uploads_done();
    }
    if frame.busy {
        ui.request_repaint();
    } else if pending() > 0 {
        // Arriving textures wake the window, but a failed decode changes nothing; look
        // again now and then so the status line can't get stuck.
        ui.request_repaint_in(0.25);
    }

    let (texture, error) = (frame.texture, frame.error);
    let faint = pal.text_muted;
    let status = match (loading, pending()) {
        (true, _) => "Loading model…".to_string(),
        (false, n) if n > 0 => format!("Decoding textures… ({n} left)"),
        _ => "Drag to orbit · Shift-drag to pan · Scroll to zoom · Double-click to reframe".to_string(),
    };
    let frame_style = Frame { fill: pal.bg_inset, clip: true, ..Frame::none() };
    ui.container(Layout::column(), frame_style, |ui| {
        ui.add_leaf(id, Layout::leaf(Size::Grow(1.0), Size::Grow(1.0)), Vec2::ZERO, true, move |p, r| {
            match (texture, &error) {
                (Some(t), _) => p.image_with_alpha(r, t, [0.0, 0.0, 1.0, 1.0], 0.0, Color::WHITE, ImageAlpha::Straight),
                (None, Some(e)) => p.text_centered(r, 14.0, faint, e),
                (None, None) => {}
            }
            p.text(Vec2::new(r.x + 8.0, r.y + r.h - 22.0), 12.0, faint, status);
        });
    });
}
