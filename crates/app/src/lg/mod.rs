//! The libgui frontend (trial): a winit window drawn by wgpu, or, for
//! `--screenshot`, no window at all and libgui's CPU renderer.
//!
//! libgui owns no window, clock or clipboard, so this file is the host: it turns
//! winit events into libgui input, sleeps until something needs a frame, and
//! applies what the UI asks for (cursor, title, textures to free).

mod app;
mod views;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use libgui::*;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
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
    if rx.needs_redraw(gpu, view, camera, &opts) {
        let mut encoder = device.create_command_encoder(&Default::default());
        rx.render(&mut encoder, gpu, view, camera, &opts);
        queue.submit([encoder.finish()]);
    }
    gpu.needs_sync() || scene.read().geometry() == rendererx::scene_data::GeometryState::Loading
}

fn new_ui(dark: bool) -> anyhow::Result<Ui> {
    let theme = if dark { Theme::dark() } else { Theme::light() };
    // Inter first, then the system's fonts, opened lazily the first time a
    // script Inter lacks (CJK file names, say) is drawn.
    let mut ui = libgui_sysfonts::ui(theme, INTER).map_err(|e| anyhow!("loading fonts: {e:?}"))?;
    startup::mark("  body font + system font source");
    let mono = ui.fonts.add_font(HACK).map_err(|e| anyhow!("loading mono font: {e:?}"))?;
    ui.set_font_role(FontRole::Mono, mono);
    libgui_keymap::Keymap::<u8>::for_current_platform().install(&mut ui);
    Ok(ui)
}

pub fn run(title: String, args: Args, job: Option<Job>) -> anyhow::Result<()> {
    if let Some(path) = &args.screenshot {
        return headless(path, job);
    }
    let el = EventLoop::<()>::with_user_event().build().context("creating the event loop")?;
    startup::mark("event loop created");
    let proxy = el.create_proxy();
    WAKE.get_or_init(|| {
        Box::new(move || {
            let _ = proxy.send_event(());
        })
    });
    let mut host = Host { title, app: Some(ViewerApp::new(job, true)), benchmark: args.benchmark, ..Default::default() };
    el.run_app(&mut host).context("running the event loop")?;
    host.error.map_or(Ok(()), Err)
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

    fn settles(&self) -> bool {
        true
    }

    fn render_scene(&mut self, st: &mut SceneGpu, scene: &CpuScene, size: (u32, u32), camera: &Camera, background: [f32; 4]) -> Frame3d {
        let gpu = self.gpu.get_or_insert_with(|| {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let adapter = pollster::block_on(instance.request_adapter(&Default::default())).map_err(|e| e.to_string())?;
            let desc = wgpu::DeviceDescriptor { required_limits: adapter.limits(), ..Default::default() };
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
fn headless(path: &Path, job: Option<Job>) -> anyhow::Result<()> {
    let mut app = ViewerApp::new(job, true);
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
    let info = FrameInfo { screen_size: Vec2::new(SIZE.0, SIZE.1), scale, dt: 1.0 / 60.0 };
    let (w, h) = ((SIZE.0 * scale) as u32, (SIZE.1 * scale) as u32);
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

struct Gpu<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    renderer: &'a mut libgui_wgpu::Renderer,
    /// RendererX on the same device, created the first time a 3D view needs it so
    /// startup never pays for its pipelines.
    rx: &'a mut Option<rendererx::Renderer>,
    scale: f32,
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

    fn render_scene(&mut self, st: &mut SceneGpu, scene: &CpuScene, size: (u32, u32), camera: &Camera, background: [f32; 4]) -> Frame3d {
        let rx = self.rx.get_or_insert_with(|| rendererx::Renderer::new(self.device, self.queue, Default::default()));
        let busy = draw_scene((rx, self.device, self.queue), st, scene, size, camera, background, false);
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
    batches: Vec<Batch>,
    clear: Color,
    rx: Option<rendererx::Renderer>,
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
}

impl Host {
    fn start(&mut self, el: &ActiveEventLoop) -> anyhow::Result<()> {
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(LogicalSize::new(SIZE.0, SIZE.1))
            .with_min_inner_size(LogicalSize::new(360.0, 240.0));
        let window = Arc::new(el.create_window(attrs).context("creating the window")?);
        startup::mark("window created");

        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(el.owned_display_handle())));
        let surface = instance.create_surface(window.clone()).context("creating the surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .context("no GPU adapter")?;
        let desc = wgpu::DeviceDescriptor { required_limits: adapter.limits(), ..Default::default() };
        let (device, queue) = pollster::block_on(adapter.request_device(&desc)).context("creating the GPU device")?;

        let px = window.inner_size();
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
            batches: Vec::new(),
            rx: None,
        });
        Ok(())
    }

    fn draw(&mut self, el: &ActiveEventLoop) {
        let (Some(w), Some(app)) = (self.live.as_mut(), self.app.as_mut()) else { return };

        // One check covers every way the window can change size, and turns a
        // resize drag into one reconfigure per presented frame.
        let px = w.window.inner_size();
        if px.width.max(1) != w.config.width || px.height.max(1) != w.config.height {
            w.config.width = px.width.max(1);
            w.config.height = px.height.max(1);
            w.surface.configure(&w.device, &w.config);
        }

        let now = Instant::now();
        w.idle += (now - w.last).as_secs_f32(); // real time: libgui keeps its own clocks with it
        w.last = now;
        let scale = w.window.scale_factor() as f32;
        let info = FrameInfo {
            screen_size: Vec2::new(w.config.width as f32 / scale, w.config.height as f32 / scale),
            scale,
            dt: w.idle,
        };

        if app.poll_job() {
            w.ui.request_repaint();
        }
        let mut platform = PlatformOutput::default();
        if w.ui.needs_frame_for(&info, w.idle) || self.content_frames < 3 {
            w.idle = 0.0;
            w.ui.begin_frame(info);
            let t = Instant::now();
            let mut gpu = Gpu { device: &w.device, queue: &w.queue, renderer: &mut w.renderer, rx: &mut w.rx, scale };
            app.ui(&mut w.ui, &mut gpu);
            let out = w.ui.end_frame();
            if self.content_frames == 0 { startup::mark(&format!("  frame built in {:.2} ms", t.elapsed().as_secs_f64() * 1000.0)); }
            platform = out.platform.clone();
            w.clear = out.clear_color;
            w.renderer.prepare(&out);
            if self.content_frames == 0 { startup::mark("  prepared"); }
            w.batches.clear();
            w.batches.extend_from_slice(&out.draw.batches);
            for id in app.retired.drain(..) {
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

        // Sleep until libgui, input or a loader thread needs us.
        match platform.repaint_after {
            _ if self.content_frames < 3 && (app.shown() || app.loading()) => w.window.request_redraw(),
            Some(t) if t <= 0.0 => w.window.request_redraw(),
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

    fn user_event(&mut self, _el: &ActiveEventLoop, _: ()) {
        // A loader thread finished, or a 3D scene gained something (a decoded texture):
        // build a frame so the view can pick it up.
        if let Some(w) = self.live.as_mut() {
            w.ui.request_repaint();
            w.window.request_redraw();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(w) = self.live.as_mut() else { return };
        if !w.files.push_window_event(&mut w.ui, &event) {
            libgui_winit::push_window_event(&mut w.ui, &event, w.window.scale_factor());
        }
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => self.draw(el),
            // Anything else may have changed what the UI shows; `draw` asks libgui.
            _ => w.window.request_redraw(),
        }
    }
}
