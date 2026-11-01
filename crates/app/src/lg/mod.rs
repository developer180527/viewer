//! The UI: a winit window drawn by wgpu with libgui, or, for
//! `--screenshot`, no window at all and libgui's CPU renderer.
//!
//! libgui owns no window, clock or clipboard, so this file is the host: it turns
//! winit events into libgui input, sleeps until something needs a frame, and
//! applies what the UI asks for (cursor, title, textures to free).

mod app;
mod pages;
mod views;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use libgui::*;
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use rendererx::Camera;
use rendererx::scene_data::CpuScene;

use crate::loading::{Job, WAKE};
use crate::{Args, startup};
use app::ViewerApp;

const INTER: &[u8] = include_bytes!("../../assets/Inter.ttf");
const HACK: &[u8] = include_bytes!("../../assets/Hack-Regular.ttf");

/// Window size in logical px, for the window and for headless screenshots.
const SIZE: (f32, f32) = (1100.0, 760.0);

/// Where views put their images: the GPU, or the CPU renderer's texture table.
pub trait Textures {
    fn max_side(&self) -> u32;
    /// `rgba` is straight (not premultiplied) RGBA8, `width * height * 4` bytes.
    fn upload(&mut self, width: u32, height: u32, rgba: &[u8]) -> TextureId;
    /// Physical pixels per logical point, for views that render at native resolution.
    fn pixels_per_point(&self) -> f32;
    /// Waits for background work (texture decoding) before drawing, for screenshots.
    fn settles(&self) -> bool {
        false
    }
    /// A view finished uploading something big: after this frame's submit, wait for
    /// the GPU once, so wgpu frees the upload staging now (it frees it only when a
    /// later submit finds the GPU done, and an idle window makes none).
    fn uploads_done(&mut self) {}
    /// Frees a texture this view uploaded and no longer draws (from this frame on).
    fn free(&mut self, id: TextureId);

    /// Draws a 3D scene from `camera` into a `size`-pixel frame (RendererX on the
    /// shared device) and returns the texture to show.
    fn render_scene(&mut self, st: &mut SceneGpu, scene: &CpuScene, size: (u32, u32), camera: &Camera, background: [f32; 4]) -> Frame3d;
}

/// What a 3D view keeps on the GPU between frames. Dropping it frees the uploaded
/// model and the view's buffers; the registered texture is retired by the page.
#[derive(Default)]
pub struct SceneGpu {
    gpu: Option<rendererx::GpuScene>,
    view: Option<rendererx::View>,
    /// The frame registered with the UI renderer, and the view target generation it shows.
    texture: Option<(TextureId, u64)>,
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
/// would change. Returns whether uploads remain.
fn draw_scene(
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
    // Pipelines aren't counted: windows compile them on `Rx`'s worker, which wakes the
    // window as each is ready; screenshots compiled them in `prepare` above.
    gpu.needs_sync() || scene.read().geometry() == rendererx::scene_data::GeometryState::Loading
}

fn new_ui(dark: bool) -> anyhow::Result<Ui> {
    let theme = if dark { Theme::dark() } else { Theme::light() };
    // Inter first, then the system's fonts, opened lazily the first time a
    // script Inter lacks (CJK file names, say) is drawn.
    let stack = FontStack::from_fonts(&[INTER]).map_err(|e| anyhow!("loading fonts: {e:?}"))?;
    let mut ui = Ui::with_rasterizer(theme, Box::new(stack.with_source(Box::new(LazySystemFonts))));
    startup::mark("  body font + system font source");
    let mono = ui.fonts.add_font(HACK).map_err(|e| anyhow!("loading mono font: {e:?}"))?;
    ui.set_font_role(FontRole::Mono, mono);
    libgui_keymap::Keymap::<u8>::for_current_platform().install(&mut ui);
    Ok(ui)
}

/// The installed fonts, listed on a thread (`scan_system_fonts`, started first
/// thing in `main`): listing them reads every font directory, 5–10 ms warm but
/// close to 200 ms from a cold disk, and most files never need one.
static SYSTEM_FONTS: std::sync::OnceLock<libgui_sysfonts::SystemFonts> = std::sync::OnceLock::new();
static SYSTEM_FONTS_SCAN: std::sync::Mutex<Option<std::thread::JoinHandle<libgui_sysfonts::SystemFonts>>> =
    std::sync::Mutex::new(None);

pub fn scan_system_fonts() {
    *SYSTEM_FONTS_SCAN.lock().unwrap() = Some(std::thread::spawn(libgui_sysfonts::SystemFonts::new));
}

/// The system fonts, waiting for the scan the first time a character needs them.
struct LazySystemFonts;

impl FontSource for LazySystemFonts {
    fn face_for(&self, ch: char) -> Option<Box<dyn FontRasterizer>> {
        SYSTEM_FONTS
            .get_or_init(|| {
                let scan = SYSTEM_FONTS_SCAN.lock().unwrap().take();
                scan.and_then(|t| t.join().ok()).unwrap_or_default()
            })
            .face_for(ch)
    }
}

pub fn run(title: String, args: Args, job: Option<Job>) -> anyhow::Result<()> {
    if let Some(path) = &args.screenshot {
        return headless(path, args.size.unwrap_or(SIZE), args.info, job);
    }
    let el = EventLoop::<()>::with_user_event().build().context("creating the event loop")?;
    startup::mark("event loop created");
    let display = el.owned_display_handle();
    // The GPU device doesn't need the window: open it on a thread while the window
    // is built.
    let gpu = std::thread::spawn(move || open_gpu(display));
    let proxy = el.create_proxy();
    WAKE.get_or_init(|| {
        Box::new(move || {
            let _ = proxy.send_event(());
        })
    });
    let mut app = ViewerApp::new(job, true);
    app.show_info = args.info;
    let mut host = Host { title, app: Some(app), benchmark: args.benchmark, gpu: Some(gpu), ..Default::default() };
    el.run_app(&mut host).context("running the event loop")?;
    host.error.map_or(Ok(()), Err)
}

/// The size to draw at: the whole window. On iOS winit's inner size is only the
/// safe area, but the drawing layer covers the screen; the gap is `safe_area`.
fn surface_size(window: &Window) -> winit::dpi::PhysicalSize<u32> {
    if cfg!(any(target_os = "ios", target_os = "android")) { window.outer_size() } else { window.inner_size() }
}

/// What the notch, status bar, home indicator and rounded corners cover, in points:
/// the window minus winit's inner (safe) rect. Zero on desktops.
fn safe_area(window: &Window, scale: f32) -> Insets {
    if !cfg!(any(target_os = "ios", target_os = "android")) {
        return Insets::all(0.0);
    }
    let (outer, inner) = (window.outer_size(), window.inner_size());
    let (op, ip) = match (window.outer_position(), window.inner_position()) {
        (Ok(o), Ok(i)) => (o, i),
        _ => return Insets::all(0.0),
    };
    let left = (ip.x - op.x).max(0) as f32;
    let top = (ip.y - op.y).max(0) as f32;
    let right = (outer.width as f32 - inner.width as f32 - left).max(0.0);
    let bottom = (outer.height as f32 - inner.height as f32 - top).max(0.0);
    Insets { left: left / scale, top: top / scale, right: right / scale, bottom: bottom / scale }
}

/// iOS 27 runs only apps that use UIKit scenes, and a window outside a scene is
/// never shown. winit 0.30 makes its window before the scene exists and never puts
/// it in one, so once UIKit has connected the scene (from `Info.plist`), move the
/// window there. Returns whether it is in a scene; elsewhere there is nothing to do.
#[cfg(target_os = "ios")]
fn adopt_scene(window: &Window) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return false };
    let RawWindowHandle::UiKit(h) = handle.as_raw() else { return false };
    let view = h.ui_view.as_ptr().cast::<AnyObject>();
    // SAFETY: UIKit objects winit hands out, messaged on the main thread (the
    // event loop's), with selectors and types from UIKit's headers.
    unsafe {
        let win: *mut AnyObject = msg_send![view, window];
        if win.is_null() {
            return false;
        }
        let current: *mut AnyObject = msg_send![win, windowScene];
        if !current.is_null() {
            return true;
        }
        let app: *mut AnyObject = msg_send![objc2::class!(UIApplication), sharedApplication];
        let scenes: *mut AnyObject = msg_send![app, connectedScenes];
        let scene: *mut AnyObject = msg_send![scenes, anyObject];
        if scene.is_null() {
            return false;
        }
        let () = msg_send![win, setWindowScene: scene];
        let () = msg_send![win, makeKeyAndVisible];
    }
    true
}

#[cfg(not(target_os = "ios"))]
fn adopt_scene(_: &Window) -> bool {
    true
}

type GpuParts = (wgpu::Instance, wgpu::Adapter, wgpu::Device, wgpu::Queue);

/// The adapter and device, picked without a surface; `Host::start` checks the
/// adapter can draw to the window once it exists.
fn open_gpu(display: winit::event_loop::OwnedDisplayHandle) -> anyhow::Result<GpuParts> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display)));
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        ..Default::default()
    }))
    .context("no GPU adapter")?;
    let (device, queue) = open_device(&adapter)?;
    startup::mark("  GPU device opened");
    Ok((instance, adapter, device, queue))
}

fn open_device(adapter: &wgpu::Adapter) -> anyhow::Result<(wgpu::Device, wgpu::Queue)> {
    let desc = wgpu::DeviceDescriptor {
        required_limits: adapter.limits(),
        // Compressed texture formats, where the GPU has them.
        required_features: adapter.features() & rendererx::OPTIONAL_FEATURES,
        ..Default::default()
    };
    pollster::block_on(adapter.request_device(&desc)).context("creating the GPU device")
}

// --- Headless ------------------------------------------------------------------

/// `--screenshot` textures: the CPU renderer's table, plus a windowless GPU device for
/// 3D scenes (rendered, then read back), created only if a scene is shown.
struct Headless {
    soft: libgui_soft::SoftRenderer,
    /// `None` until a scene asks; `Some(Err)` when there is no adapter.
    gpu: Option<Result<(wgpu::Device, wgpu::Queue, rendererx::Renderer), String>>,
}

impl Textures for Headless {
    fn max_side(&self) -> u32 {
        8192
    }

    fn upload(&mut self, width: u32, height: u32, rgba: &[u8]) -> TextureId {
        self.soft.register_texture(libgui_soft::Texture { width, height, data: rgba.to_vec() })
    }

    fn pixels_per_point(&self) -> f32 {
        HEADLESS_SCALE
    }

    fn free(&mut self, id: TextureId) {
        self.soft.unregister_texture(id);
    }

    fn settles(&self) -> bool {
        true
    }

    fn render_scene(&mut self, st: &mut SceneGpu, scene: &CpuScene, size: (u32, u32), camera: &Camera, background: [f32; 4]) -> Frame3d {
        let gpu = self.gpu.get_or_insert_with(|| {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let adapter = pollster::block_on(instance.request_adapter(&Default::default())).map_err(|e| e.to_string())?;
            let desc = wgpu::DeviceDescriptor {
                required_limits: adapter.limits(),
                // Compressed texture formats, where the GPU has them.
                required_features: adapter.features() & rendererx::OPTIONAL_FEATURES,
                ..Default::default()
            };
            let (device, queue) = pollster::block_on(adapter.request_device(&desc)).map_err(|e| e.to_string())?;
            let rx = rendererx::Renderer::new(&device, &queue, Default::default());
            Ok((device, queue, rx))
        });
        let (device, queue, rx) = match gpu {
            Ok(g) => g,
            Err(e) => return Frame3d { error: Some(format!("no GPU for 3D: {e}")), ..Default::default() },
        };
        // A screenshot wants the finished model, so upload all of it now.
        draw_scene((rx, device, queue), st, scene, size, camera, background, true);
        let Some(pixels) = st.view.as_ref().and_then(|v| v.read_pixels(device, queue)) else { return Frame3d::default() };
        let texture = libgui_soft::Texture { width: size.0, height: size.1, data: pixels };
        let id = match st.texture {
            Some((id, _)) => {
                self.soft.update_texture(id, texture);
                id
            }
            None => self.soft.register_texture(texture),
        };
        st.texture = Some((id, 0));
        Frame3d { texture: Some(id), busy: false, error: None }
    }
}

const HEADLESS_SCALE: f32 = 2.0;

/// `--screenshot`: wait for the file, draw it on the CPU at 2× and save the
/// pixels. No window, no GPU, so it runs anywhere (CI included).
fn headless(path: &Path, size: (f32, f32), info: bool, job: Option<Job>) -> anyhow::Result<()> {
    let mut app = ViewerApp::new(job, true);
    app.show_info = info;
    app.stable = true;
    while app.loading() && !app.poll_job() {
        std::thread::sleep(Duration::from_millis(1));
    }
    if let Some(err) = app.error() {
        eprintln!("viewer: {err}");
    }
    let mut ui = new_ui(app.dark)?;
    startup::mark("ui created");
    let mut tex = Headless { soft: libgui_soft::SoftRenderer::new(), gpu: None };
    let scale = HEADLESS_SCALE;
    let info = FrameInfo { screen_size: Vec2::new(size.0, size.1), scale, dt: 1.0 / 60.0 };
    let (w, h) = ((size.0 * scale) as u32, (size.1 * scale) as u32);
    // Rects are a frame late (fit-to-window zoom, table widths), so settle first.
    for frame in 1..=3 {
        let t = Instant::now();
        ui.begin_frame(info);
        app.ui(&mut ui, &mut tex);
        let out = ui.end_frame();
        let built = t.elapsed();
        let image = tex.soft.render_to_image(&out, w, h);
        startup::mark(&format!(
            "CPU frame {frame}: built in {:.2} ms, rendered {w}×{h} in {:.2} ms",
            built.as_secs_f64() * 1000.0,
            (t.elapsed() - built).as_secs_f64() * 1000.0
        ));
        if frame == 3 {
            crate::save_png(path, image.width, image.height, &image.data);
        }
    }
    Ok(())
}

// --- Windowed --------------------------------------------------------------------

/// RendererX for windows, in background-compile mode, and the thread that compiles
/// its pipelines: on a cold driver cache each takes 65–100 ms, which on the UI thread
/// froze the window on the first 3D file and stalled a frame per shading model. Now
/// a model shows as soon as clay is ready and switches to its own shading as each
/// pipeline arrives.
struct Rx {
    renderer: std::sync::Arc<rendererx::Renderer>,
    /// Wakes the worker; one slot, so nudges while it is busy merge.
    kick: std::sync::mpsc::SyncSender<()>,
}

impl Rx {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let desc = rendererx::RendererDesc { background_compile: true, ..Default::default() };
        let renderer = std::sync::Arc::new(rendererx::Renderer::new(device, queue, desc));
        let (kick, nudged) = std::sync::mpsc::sync_channel::<()>(1);
        let worker = renderer.clone();
        // Exits when the window (and so `kick`) goes away.
        let spawned = std::thread::Builder::new().name("shader compile".into()).spawn(move || {
            while nudged.recv().is_ok() {
                while worker.compile_one() {
                    if let Some(wake) = WAKE.get() {
                        wake();
                    }
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

struct Gpu<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    renderer: &'a mut libgui_wgpu::Renderer,
    /// RendererX on the same device, created the first time a 3D view needs it so
    /// startup never pays for its pipelines.
    rx: &'a mut Option<Rx>,
    scale: f32,
    /// Set by `uploads_done`.
    settle: &'a mut bool,
    /// Textures views freed this frame; unregistered once the frame is prepared.
    freed: &'a mut Vec<TextureId>,
}

impl Textures for Gpu<'_> {
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
        let rx = self.rx.get_or_insert_with(|| Rx::new(self.device, self.queue));
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
        Frame3d { texture: st.texture(), busy, error: None }
    }
}

struct Live {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: libgui_wgpu::Renderer,
    ui: Ui,
    platform: libgui_winit::PlatformState,
    files: libgui_winit::FileDrop,
    last: Instant,
    /// Time since the last frame that was actually built.
    idle: f32,
    /// Something outside libgui changed what the UI shows (a load finished, a
    /// file was picked, a texture decoded), so the next frame must be built.
    /// `Ui::request_repaint` can't say this: it only counts during a frame.
    stale: bool,
    batches: Vec<Batch>,
    clear: Color,
    rx: Option<Rx>,
}

#[derive(Default)]
struct Host {
    title: String,
    live: Option<Live>,
    app: Option<ViewerApp>,
    benchmark: bool,
    content_frames: u32,
    first_frame_done: bool,
    error: Option<anyhow::Error>,
    /// The last frame asked for another straight away (see `about_to_wait`).
    again: bool,
    /// iOS: the window is in the app's scene (`adopt_scene`).
    in_scene: bool,
    /// The GPU thread `run` started, until `start` joins it.
    gpu: Option<std::thread::JoinHandle<anyhow::Result<GpuParts>>>,
}

impl Host {
    fn start(&mut self, el: &ActiveEventLoop) -> anyhow::Result<()> {
        let attrs = Window::default_attributes().with_title(&self.title);
        // Phones and tablets: the window is the screen (winit would otherwise size it
        // from these, desktop-sized, and the app would lay out for 1100 points).
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        let attrs =
            attrs
            .with_inner_size(winit::dpi::LogicalSize::new(SIZE.0, SIZE.1))
            .with_min_inner_size(winit::dpi::LogicalSize::new(320.0, 240.0));
        let window = Arc::new(el.create_window(attrs).context("creating the window")?);
        startup::mark("window created");

        let gpu = self.gpu.take().context("started twice")?;
        let (instance, mut adapter, mut device, mut queue) =
            gpu.join().map_err(|_| anyhow!("GPU setup panicked"))??;
        let surface = instance.create_surface(window.clone()).context("creating the surface")?;
        if !adapter.is_surface_supported(&surface) {
            // The thread's pick can't draw here (several GPUs): ask again for one that can.
            adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                ..Default::default()
            }))
            .context("no GPU adapter for the window")?;
            (device, queue) = open_device(&adapter)?;
        }

        let px = surface_size(&window);
        let mut config = surface
            .get_default_config(&adapter, px.width.max(1), px.height.max(1))
            .context("surface has no configuration")?;
        let caps = surface.get_capabilities(&adapter);
        config.format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);
        let renderer = libgui_wgpu::Renderer::new(&device, &queue, config.format);
        startup::mark("GPU ready");

        let dark = window.theme().is_none_or(|t| t == winit::window::Theme::Dark);
        let ui = new_ui(dark)?;
        if let Some(app) = self.app.as_mut() {
            app.dark = dark;
        }
        startup::mark("ui created");
        self.live = Some(Live {
            window,
            surface,
            config,
            device,
            queue,
            renderer,
            clear: ui.theme.palette.bg_app,
            ui,
            platform: libgui_winit::PlatformState::default(),
            files: libgui_winit::FileDrop::default(),
            last: Instant::now(),
            idle: 0.0,
            stale: false,
            batches: Vec::new(),
            rx: None,
        });
        Ok(())
    }

    fn draw(&mut self, el: &ActiveEventLoop) {
        let (Some(w), Some(app)) = (self.live.as_mut(), self.app.as_mut()) else { return };

        // One check covers every way the window can change size, and turns a
        // resize drag into one reconfigure per presented frame.
        let px = surface_size(&w.window);
        if px.width.max(1) != w.config.width || px.height.max(1) != w.config.height {
            w.config.width = px.width.max(1);
            w.config.height = px.height.max(1);
            w.surface.configure(&w.device, &w.config);
        }

        let now = Instant::now();
        w.idle += (now - w.last).as_secs_f32(); // real time: libgui keeps its own clocks with it
        w.last = now;
        let scale = w.window.scale_factor() as f32;
        app.safe_area = safe_area(&w.window, scale);
        let info = FrameInfo {
            screen_size: Vec2::new(w.config.width as f32 / scale, w.config.height as f32 / scale),
            scale,
            dt: w.idle,
        };

        if app.poll_job() {
            w.stale = true;
        }
        let mut platform = PlatformOutput::default();
        let mut settle = false;
        let mut freed = Vec::new();
        if std::mem::take(&mut w.stale) || w.ui.needs_frame_for(&info, w.idle) || self.content_frames < 3 {
            w.idle = 0.0;
            w.ui.begin_frame(info);
            let t = Instant::now();
            let mut gpu = Gpu { device: &w.device, queue: &w.queue, renderer: &mut w.renderer, rx: &mut w.rx, scale, settle: &mut settle, freed: &mut freed };
            app.ui(&mut w.ui, &mut gpu);
            let out = w.ui.end_frame();
            if self.content_frames == 0 { startup::mark(&format!("  frame built in {:.2} ms", t.elapsed().as_secs_f64() * 1000.0)); }
            platform = out.platform.clone();
            w.clear = out.clear_color;
            w.renderer.prepare(&out);
            if self.content_frames == 0 { startup::mark("  prepared"); }
            w.batches.clear();
            w.batches.extend_from_slice(&out.draw.batches);
            for id in app.retired.drain(..).chain(freed) {
                w.renderer.unregister_texture(id);
            }
            if let Some(title) = app.title.take() {
                w.window.set_title(&title);
            }
        }

        let frame = match w.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            // Not on screen (yet): winit sends `Occluded(false)` when it is, and
            // every window event asks for a redraw. Retrying here would spin.
            wgpu::CurrentSurfaceTexture::Occluded => return,
            _ => {
                w.surface.configure(&w.device, &w.config);
                w.window.request_redraw();
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = w.device.create_command_encoder(&Default::default());
        {
            let c = w.clear;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: c.r as f64, g: c.g as f64, b: c.b as f64, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            w.renderer.render_batches(&mut pass, &w.batches);
        }
        w.queue.submit([encoder.finish()]);
        w.window.pre_present_notify();
        w.queue.present(frame);
        w.platform.apply(&w.window, &platform);
        if settle {
            let _ = w.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(Duration::from_millis(500)) });
        }

        if !self.first_frame_done {
            self.first_frame_done = true;
            startup::mark("first frame presented");
        }
        // --benchmark: count frames once the file (or its error) is on screen.
        if app.shown() {
            self.content_frames += 1;
            if self.content_frames == 1 {
                startup::mark("first frame showing the file");
                if let Some(err) = app.error() {
                    eprintln!("viewer: {err}");
                }
            }
            if self.benchmark && self.content_frames >= 3 {
                el.exit();
            }
        }

        // Sleep until libgui, input or a loader thread needs us. "Now" is asked for
        // in `about_to_wait`: iOS drops a redraw requested while one is being drawn.
        match platform.repaint_after {
            _ if self.content_frames < 3 && (app.shown() || app.loading()) => self.again = true,
            Some(t) if t <= 0.0 => self.again = true,
            Some(t) => el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_secs_f32(t))),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }
}

impl ApplicationHandler for Host {
    fn new_events(&mut self, _el: &ActiveEventLoop, cause: StartCause) {
        if let (StartCause::ResumeTimeReached { .. }, Some(w)) = (cause, self.live.as_ref()) {
            w.window.request_redraw();
        }
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.live.is_none() {
            if let Err(e) = self.start(el) {
                self.error = Some(e);
                el.exit();
                return;
            }
            self.live.as_ref().unwrap().window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        if !self.in_scene
            && let Some(w) = self.live.as_ref()
        {
            self.in_scene = adopt_scene(&w.window);
            self.again |= self.in_scene;
        }
        if std::mem::take(&mut self.again)
            && let Some(w) = self.live.as_ref()
        {
            w.window.request_redraw();
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _: ()) {
        // A loader thread finished, a file was picked, or a 3D scene gained something
        // (a decoded texture): build a frame so the view can pick it up. Asked for in
        // `about_to_wait`, like every redraw (see the end of `draw`).
        if let Some(w) = self.live.as_mut() {
            w.stale = true;
            self.again = true;
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(w) = self.live.as_mut() else { return };
        // A finger that lands and lifts between two frames is shown to widgets as a
        // press in one frame and the release in the next; libgui (2709ad4) forgets
        // where that finger was by the release frame and judges the release at the
        // previous pointer position, so a tap on a menu item misses it. Moving the
        // pointer there too keeps them together.
        if let WindowEvent::Touch(t) = &event {
            let s = w.window.scale_factor();
            let pos = Vec2::new((t.location.x / s) as f32, (t.location.y / s) as f32);
            w.ui.push(InputEvent::PointerMoved { pos });
        }
        if !w.files.push_window_event(&mut w.ui, &event) {
            libgui_winit::push_window_event(&mut w.ui, &event, w.window.scale_factor());
        }
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => self.draw(el),
            // Anything else may have changed what the UI shows; `draw` asks libgui.
            _ => self.again = true,
        }
    }
}
