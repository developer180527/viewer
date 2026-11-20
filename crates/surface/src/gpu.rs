//! libviewer's [`Host`] on a wgpu device: image tiles as textures, and RendererX
//! for 3D scenes with its pipelines compiled on a thread.

use std::sync::Arc;

use anyhow::Context as _;
use libgui::TextureId;
use libviewer::{Frame3d, Host, SceneGpu, draw_scene};
use rendererx::Camera;
use rendererx::scene_data::CpuScene;

/// Wakes the shell from another thread: a texture decoded, a pipeline compiled.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The device, with the limits of the adapter and the compressed texture formats
/// it has.
pub fn open_device(adapter: &wgpu::Adapter) -> anyhow::Result<(wgpu::Device, wgpu::Queue)> {
    let desc = wgpu::DeviceDescriptor {
        required_limits: adapter.limits(),
        required_features: adapter.features() & rendererx::OPTIONAL_FEATURES,
        ..Default::default()
    };
    pollster::block_on(adapter.request_device(&desc)).context("creating the GPU device")
}

/// RendererX for windows, in background-compile mode, and the thread that compiles
/// its pipelines: on a cold driver cache each takes 65–100 ms, which on the UI thread
/// froze the window on the first 3D file and stalled a frame per shading model. Now
/// a model shows as soon as clay is ready and switches to its own shading as each
/// pipeline arrives.
pub struct Rx {
    renderer: Arc<rendererx::Renderer>,
    /// Wakes the worker; one slot, so nudges while it is busy merge.
    kick: std::sync::mpsc::SyncSender<()>,
}

impl Rx {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, wake: Wake) -> Self {
        let desc = rendererx::RendererDesc { background_compile: true, ..Default::default() };
        let renderer = Arc::new(rendererx::Renderer::new(device, queue, desc));
        let (kick, nudged) = std::sync::mpsc::sync_channel::<()>(1);
        let worker = renderer.clone();
        // Exits when the shell (and so `kick`) goes away.
        let spawned = std::thread::Builder::new().name("shader compile".into()).spawn(move || {
            while nudged.recv().is_ok() {
                while worker.compile_one() {
                    wake();
                }
            }
        });
        if spawned.is_err() {
            // No thread: compile here instead, as foreground hosts do.
            renderer.compile_pending();
        }
        let rx = Self { renderer, kick };
        // The clay pipelines queued by `new`.
        rx.nudge();
        rx
    }

    fn nudge(&self) {
        let _ = self.kick.try_send(());
    }
}

/// libviewer's host for one frame on a wgpu device.
pub struct Gpu<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub renderer: &'a mut libgui_wgpu::Renderer,
    /// RendererX on the same device, created the first time a 3D view needs it so
    /// startup never pays for its pipelines.
    pub rx: &'a mut Option<Rx>,
    pub scale: f32,
    /// Set by `uploads_done`: the shell waits for the GPU before the next frame.
    pub settle: &'a mut bool,
    /// Textures views freed this frame; unregistered once the frame is prepared.
    pub freed: &'a mut Vec<TextureId>,
    /// See `Host::trackpad_pinch`.
    pub pinch: f32,
    pub wake: &'a Wake,
    /// An iPhone or iPad: hints speak of taps and fingers.
    pub touch_screen: bool,
}

impl Host for Gpu<'_> {
    fn image_opener(&self) -> Option<libviewer::ImageOpener> {
        Some(Arc::new(viewer_formats::open_image))
    }

    fn trackpad_pinch(&self) -> f32 {
        self.pinch
    }

    fn touch_screen(&self) -> bool {
        self.touch_screen
    }

    fn waker(&self) -> libviewer::Waker {
        let wake = self.wake.clone();
        Arc::new(move || wake())
    }

    fn max_side(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    fn upload(&mut self, width: u32, height: u32, rgba: &[u8]) -> TextureId {
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image tile"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            texture.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
            size,
        );
        // The renderer's bind group keeps the texture alive.
        self.renderer.register_texture(&texture.create_view(&Default::default()))
    }

    fn pixels_per_point(&self) -> f32 {
        self.scale
    }

    fn uploads_done(&mut self) {
        *self.settle = true;
    }

    fn free(&mut self, id: TextureId) {
        self.freed.push(id);
    }

    fn render_scene(&mut self, st: &mut SceneGpu, scene: &CpuScene, size: (u32, u32), camera: &Camera, background: [f32; 4]) -> Frame3d {
        let rx = self.rx.get_or_insert_with(|| Rx::new(self.device, self.queue, self.wake.clone()));
        let busy = draw_scene((&rx.renderer, self.device, self.queue), st, scene, size, camera, background, false);
        // The frame queued pipelines it lacked (drawing clay, or nothing, meanwhile).
        if rx.renderer.pipelines_pending() {
            rx.nudge();
        }
        // The target is recreated on resize; point the UI's texture at the new one.
        let view = st.view.as_ref().expect("created by draw_scene");
        if let Some(target) = view.target_view() {
            let generation = view.target_generation();
            match st.texture {
                Some((_, g)) if g == generation => {}
                Some((id, _)) => {
                    self.renderer.update_texture(id, target);
                    st.texture = Some((id, generation));
                }
                None => st.texture = Some((self.renderer.register_texture(target), generation)),
            }
        }
        Frame3d::shown(st.texture(), busy)
    }
}
