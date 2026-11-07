//! What a view needs from the app that hosts it: somewhere to put images (the GPU, or
//! a CPU renderer for screenshots), a way to draw 3D, and a way to wake the window.

use std::sync::Arc;

use libgui::TextureId;
use rendererx::Camera;
use rendererx::scene_data::CpuScene;

/// Wakes the app's window from another thread (a band of an image or a texture
/// arrived), so a sleeping window draws it.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// The app side of every view: images, 3D, waking.
pub trait Host {
    fn max_side(&self) -> u32;
    /// `rgba` is straight (not premultiplied) RGBA8, `width * height * 4` bytes.
    fn upload(&mut self, width: u32, height: u32, rgba: &[u8]) -> TextureId;
    /// Physical pixels per logical point, for views that render at native resolution.
    fn pixels_per_point(&self) -> f32;
    /// This frame's trackpad pinch over the window, as a zoom ratio minus 1 (0 = none).
    /// libgui's `Response::pinch` is touch only; windowing reports trackpads separately.
    fn trackpad_pinch(&self) -> f32 {
        0.0
    }
    /// Waits for background work (image bands, textures) before drawing: screenshots.
    fn settles(&self) -> bool {
        false
    }
    /// A view finished uploading something big: after this frame's submit, wait for
    /// the GPU once, so wgpu frees the upload staging now.
    fn uploads_done(&mut self) {}
    /// Frees a texture a view uploaded and no longer draws (from this frame on).
    fn free(&mut self, id: TextureId);
    /// Draws a 3D scene from `camera` into a `size`-pixel frame and returns the texture
    /// to show ([`draw_scene`] does the RendererX part).
    fn render_scene(&mut self, st: &mut SceneGpu, scene: &CpuScene, size: (u32, u32), camera: &Camera, background: [f32; 4]) -> Frame3d;
    /// How background work wakes the window; nothing by default.
    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

/// What a 3D view keeps on the GPU between frames. Dropping it frees the uploaded
/// model and the view's buffers; the registered texture is freed by the page.
#[derive(Default)]
pub struct SceneGpu {
    pub gpu: Option<rendererx::GpuScene>,
    pub view: Option<rendererx::View>,
    /// The frame registered with the UI renderer, and the view target generation it shows.
    pub texture: Option<(TextureId, u64)>,
}

impl SceneGpu {
    pub fn texture(&self) -> Option<TextureId> {
        self.texture.map(|(t, _)| t)
    }
}

#[derive(Default)]
pub struct Frame3d {
    pub texture: Option<TextureId>,
    /// Still uploading: draw again soon.
    pub busy: bool,
    /// Why nothing can be drawn (no GPU).
    pub error: Option<String>,
}

/// Uploads what's new (everything at once when `block`), then renders if the frame
/// would change. Returns whether uploads remain. For [`Host::render_scene`].
pub fn draw_scene(
    (rx, device, queue): (&rendererx::Renderer, &wgpu::Device, &wgpu::Queue),
    st: &mut SceneGpu,
    scene: &CpuScene,
    (w, h): (u32, u32),
    camera: &Camera,
    background: [f32; 4],
    block: bool,
) -> bool {
    let gpu = st.gpu.get_or_insert_with(|| rx.attach(scene));
    rx.sync(gpu);
    while block && gpu.needs_sync() {
        rx.sync(gpu);
    }
    let view = st.view.get_or_insert_with(|| rx.create_view(rendererx::ViewDesc { width: w, height: h }));
    view.resize(w, h);
    let opts = rendererx::RenderOptions { background };
    if block {
        // A blocking draw (screenshots) shows the final shading, not clay stand-ins.
        rx.prepare(gpu);
    }
    if rx.needs_redraw(gpu, view, camera, &opts) {
        let mut encoder = device.create_command_encoder(&Default::default());
        rx.render(&mut encoder, gpu, view, camera, &opts);
        queue.submit([encoder.finish()]);
    }
    gpu.needs_sync() || scene.read().geometry() == rendererx::scene_data::GeometryState::Loading
}
