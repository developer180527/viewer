//! The UI: a winit window drawn by wgpu with libgui, or, for
//! `--screenshot`, no window at all and libgui's CPU renderer.
//!
//! libgui owns no window, clock or clipboard, so this file is the host: it turns
//! winit events into libgui input, sleeps until something needs a frame, and
//! applies what the UI asks for (cursor, title, textures to free).

mod app;

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

/// Set once the event loop exists: wakes the window from another thread (a load
/// finished, a picker answered, a texture arrived).
pub static WAKE: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();
use crate::{Args, startup};
use app::ViewerApp;

/// Window size in logical px, for the window and for headless screenshots.
const SIZE: (f32, f32) = (1100.0, 760.0);

// Views are libviewer's; viewer-surface hosts them on the GPU (`Gpu`), as the SwiftUI
// shell's canvas does, and this file on the CPU (`Headless`).
use libviewer::{Frame3d, Host as ViewHost, SceneGpu, draw_scene};
use viewer_surface::fonts::new_ui;
pub use viewer_surface::fonts::scan_system_fonts;
use viewer_surface::{Gpu, Rx, Wake, open_device};

pub fn run(title: String, args: Args, session: viewer_core::Session) -> anyhow::Result<()> {
    if let Some(path) = &args.screenshot {
        return headless(path, args.size.unwrap_or(SIZE), args.info, session);
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
    // libfileparser documents announce what arrives (decoded textures) the same way.
    viewer_formats::set_notify(|| {
        if let Some(wake) = WAKE.get() {
            wake();
        }
    });
    let mut app = ViewerApp::new(session, true);
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

// --- Headless ------------------------------------------------------------------

/// `--screenshot` textures: the CPU renderer's table, plus a windowless GPU device for
/// 3D scenes (rendered, then read back), created only if a scene is shown.
struct Headless {
    soft: libgui_soft::SoftRenderer,
    /// `None` until a scene asks; `Some(Err)` when there is no adapter.
    gpu: Option<Result<(wgpu::Device, wgpu::Queue, rendererx::Renderer), String>>,
}

impl ViewHost for Headless {
    fn image_opener(&self) -> Option<libviewer::ImageOpener> {
        Some(std::sync::Arc::new(viewer_formats::open_image))
    }

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
            Err(e) => return Frame3d::failed(format!("no GPU for 3D: {e}")),
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
        Frame3d::shown(Some(id), false)
    }
}

const HEADLESS_SCALE: f32 = 2.0;

/// `--screenshot`: wait for the file, draw it on the CPU at 2× and save the
/// pixels. No window, no GPU, so it runs anywhere (CI included).
fn headless(path: &Path, size: (f32, f32), info: bool, session: viewer_core::Session) -> anyhow::Result<()> {
    let mut app = ViewerApp::new(session, true);
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
    /// Trackpad pinches since the last frame, multiplied together.
    pinch: f32,
    /// Something outside libgui changed what the UI shows (a load finished, a
    /// file was picked, a texture decoded), so the next frame must be built.
    /// `Ui::request_repaint` can't say this: it only counts during a frame.
    stale: bool,
    batches: Vec<Batch>,
    clear: Color,
    rx: Option<Rx>,
    wake: Wake,
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
        startup::mark("  fonts");
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
            pinch: 1.0,
            stale: false,
            batches: Vec::new(),
            rx: None,
            wake: std::sync::Arc::new(|| {
                if let Some(wake) = WAKE.get() {
                    wake();
                }
            }),
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
            let mut gpu = Gpu { device: &w.device, queue: &w.queue, renderer: &mut w.renderer, rx: &mut w.rx, scale, settle: &mut settle, freed: &mut freed, pinch: w.pinch - 1.0, wake: &w.wake, touch_screen: false };
            w.pinch = 1.0;
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
        if let WindowEvent::PinchGesture { delta, .. } = &event {
            w.pinch *= 1.0 + *delta as f32;
            w.stale = true;
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
