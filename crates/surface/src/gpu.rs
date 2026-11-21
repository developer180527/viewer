//! libviewer's [`Host`] on a wgpu device: image tiles as textures, and RendererX
//! for 3D scenes with its pipelines compiled on a thread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use libgui::TextureId;
use libviewer::{Frame3d, Host, SceneGpu, TileUploader, Uploaded, draw_scene};
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
    let (device, queue) = pollster::block_on(adapter.request_device(&desc)).context("creating the GPU device")?;
    rendererx::announce_features(device.features());
    Ok((device, queue))
}

/// Finds out, on a thread of its own, which compressed textures this machine's GPU
/// reads, and tells the scene loaders: for shells that make their device only when a
/// drawn page first shows, after a model's textures have started decoding. A few
/// milliseconds for the adapter, off the launch path.
pub fn probe_texture_compression() {
    let _ = std::thread::Builder::new().name("gpu probe".into()).spawn(|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        if let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) {
            rendererx::announce_features(adapter.features() & rendererx::OPTIONAL_FEATURES);
        }
    });
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

/// Textures uploaded through the host, kept for reuse when freed: page tiles are all
/// one size, and zooming frees and asks for them by the thousand. A freed texture of
/// the size asked for is written over instead of a new one made, so zooming allocates
/// nothing, and nothing waits on wgpu's deferred destruction to be given back. The
/// spares are shared with the [`GpuUploader`], which writes over them from views'
/// worker threads.
#[derive(Default)]
pub struct TexturePool {
    /// Every texture the pool made, by its id: the texture and its size.
    made: HashMap<TextureId, (wgpu::Texture, u32, u32)>,
    spares: Arc<Mutex<Spares>>,
}

/// Freed textures, ready to be written over: their id (still registered with the
/// renderer), the texture and its size.
#[derive(Default)]
struct Spares {
    list: Vec<(TextureId, wgpu::Texture, u32, u32)>,
    bytes: usize,
}

impl Spares {
    fn take(&mut self, width: u32, height: u32) -> Option<(TextureId, wgpu::Texture)> {
        let i = self.list.iter().position(|(_, _, w, h)| (*w, *h) == (width, height))?;
        let (id, texture, _, _) = self.list.swap_remove(i);
        self.bytes -= (width * height * 4) as usize;
        Some((id, texture))
    }
}

fn lock_spares(s: &Mutex<Spares>) -> std::sync::MutexGuard<'_, Spares> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// Freed textures kept for reuse, at most: about a screenful of tiles.
const SPARE_BYTES: usize = if cfg!(any(target_os = "ios", target_os = "android")) { 32 << 20 } else { 64 << 20 };

impl TexturePool {
    /// A texture of a page that closed: given up (its sizes won't be asked for again,
    /// and a spare of them would only hold memory). Returns true when the caller should
    /// unregister it.
    pub fn forget(&mut self, id: TextureId) -> bool {
        let mut spares = lock_spares(&self.spares);
        if let Some(i) = spares.list.iter().position(|(s, ..)| *s == id) {
            let (_, _, w, h) = spares.list.swap_remove(i);
            spares.bytes -= (w * h * 4) as usize;
        }
        self.made.remove(&id);
        true
    }

    /// The spares, given up: a page closed, and what it freed may not be asked for
    /// again. Returns them, for the caller to unregister.
    pub fn drop_spares(&mut self) -> Vec<TextureId> {
        let mut spares = lock_spares(&self.spares);
        spares.bytes = 0;
        let ids: Vec<TextureId> = spares.list.drain(..).map(|(id, ..)| id).collect();
        for id in &ids {
            self.made.remove(id);
        }
        ids
    }

    /// A texture a view freed while it goes on (an evicted tile): kept for reuse if
    /// there's room, else given up. Returns true when the caller should unregister it.
    pub fn release(&mut self, id: TextureId) -> bool {
        let Some((texture, w, h)) = self.made.get(&id) else { return true };
        let bytes = (*w * *h * 4) as usize;
        let mut spares = lock_spares(&self.spares);
        if spares.bytes + bytes <= SPARE_BYTES {
            spares.list.push((id, texture.clone(), *w, *h));
            spares.bytes += bytes;
            false
        } else {
            drop(spares);
            self.made.remove(&id);
            true
        }
    }

    /// The uploader for views' worker threads, on `device` and `queue`, sharing this
    /// pool's spares.
    pub fn uploader(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Arc<GpuUploader> {
        Arc::new(GpuUploader { device: device.clone(), queue: queue.clone(), spares: self.spares.clone() })
    }
}

/// libviewer's [`TileUploader`] on a wgpu device: creates (or reuses a spare) texture
/// and writes the pixels from the calling thread. wgpu's device and queue are safe to
/// use from any thread; the write lands before the next submit, and the tile is only
/// drawn once the main thread adopts it, after that.
pub struct GpuUploader {
    device: wgpu::Device,
    queue: wgpu::Queue,
    spares: Arc<Mutex<Spares>>,
}

/// What [`GpuUploader`] hands the main thread.
struct Upload {
    texture: Option<NewOrReused>,
    spares: Arc<Mutex<Spares>>,
    size: (u32, u32),
}

enum NewOrReused {
    New(wgpu::Texture),
    /// A spare, written over: its id and texture, still registered.
    Reused(TextureId, wgpu::Texture),
}

impl Drop for Upload {
    /// Never adopted (the view closed first): a reused spare goes back to the spares;
    /// a new texture is simply dropped.
    fn drop(&mut self) {
        if let Some(NewOrReused::Reused(id, texture)) = self.texture.take() {
            let mut spares = lock_spares(&self.spares);
            spares.bytes += (self.size.0 * self.size.1 * 4) as usize;
            spares.list.push((id, texture, self.size.0, self.size.1));
        }
    }
}

impl TileUploader for GpuUploader {
    fn upload(&self, width: u32, height: u32, rgba: &[u8]) -> Uploaded {
        let reused = lock_spares(&self.spares).take(width, height);
        let texture = match &reused {
            Some((_, t)) => t.clone(),
            None => new_texture(&self.device, width, height),
        };
        write_pixels(&self.queue, &texture, width, height, rgba);
        let texture = Some(match reused {
            Some((id, _)) => NewOrReused::Reused(id, texture),
            None => NewOrReused::New(texture),
        });
        Uploaded(Box::new(Upload { texture, spares: self.spares.clone(), size: (width, height) }))
    }
}

fn new_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("image tile"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn write_pixels(queue: &wgpu::Queue, texture: &wgpu::Texture, width: u32, height: u32, rgba: &[u8]) {
    let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
    let layout = wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) };
    queue.write_texture(texture.as_image_copy(), rgba, layout, size);
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
    /// Textures views freed this frame; released to the pool once the frame is prepared.
    pub freed: &'a mut Vec<TextureId>,
    /// Uploaded textures, reused when freed.
    pub pool: &'a mut TexturePool,
    /// Uploads from views' worker threads ([`TexturePool::uploader`]); `None` keeps
    /// them on the main thread.
    pub uploader: Option<Arc<GpuUploader>>,
    /// See `Host::trackpad_pinch`.
    pub pinch: f32,
    pub wake: &'a Wake,
    /// An iPhone or iPad: hints speak of taps and fingers.
    pub touch_screen: bool,
}

impl Gpu<'_> {
    /// Registers a new texture with the renderer: the bind group keeps its view alive;
    /// the pool keeps the texture, to write over when it's freed.
    fn register(&mut self, texture: wgpu::Texture, width: u32, height: u32) -> TextureId {
        let id = self.renderer.register_texture(&texture.create_view(&Default::default()));
        self.pool.made.insert(id, (texture, width, height));
        id
    }
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
        if let Some((id, texture)) = lock_spares(&self.pool.spares).take(width, height) {
            write_pixels(self.queue, &texture, width, height, rgba);
            return id;
        }
        let texture = new_texture(self.device, width, height);
        write_pixels(self.queue, &texture, width, height, rgba);
        self.register(texture, width, height)
    }

    fn tile_uploader(&self) -> Option<Arc<dyn TileUploader>> {
        self.uploader.clone().map(|u| u as Arc<dyn TileUploader>)
    }

    fn adopt(&mut self, uploaded: Uploaded, width: u32, height: u32) -> TextureId {
        let mut up = uploaded.0.downcast::<Upload>().expect("adopt is given this host's uploads");
        match up.texture.take().expect("adopted once") {
            NewOrReused::Reused(id, _) => id,
            NewOrReused::New(texture) => self.register(texture, width, height),
        }
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
