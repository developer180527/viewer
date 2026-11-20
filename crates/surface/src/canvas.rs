//! One page drawn by libviewer into a surface another UI owns: the SwiftUI shell's
//! `CAMetalLayer`, through wgpu's Metal backend. The shell owns the window, the
//! chrome and the event loop; it pushes input in, asks for a frame when woken or
//! when [`Drawn::repaint_after`] says, and acts on what the view asked for.
//!
//! A canvas is not `Send` (libgui's `Ui` holds `Rc`s): make it, draw it and drop it on
//! the thread that owns the layer, the main thread.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use libgui::*;
use viewer_core::Page;

use crate::chrome::ViewerApp;
use crate::views::{self, Action, ViewState};
use crate::{Gpu, Rx, Wake, fonts, open_device};

/// What a frame asks of the shell.
pub struct Drawn {
    /// Seconds until the next frame is wanted (0: straight away); `None`: only on
    /// input or a wake.
    pub repaint_after: Option<f32>,
    /// What the view asked for (open a container's entry).
    pub action: Option<Action>,
    pub cursor: Cursor,
}

pub struct Canvas {
    /// The shell's surface; `None` for an offscreen canvas.
    surface: Option<wgpu::Surface<'static>>,
    /// An offscreen canvas's target, read back by [`Canvas::read_pixels`].
    offscreen: Option<wgpu::Texture>,
    /// The size and format drawn at (configured on the surface, if any).
    config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: libgui_wgpu::Renderer,
    rx: Option<Rx>,
    ui: Ui,
    /// Per-page view state (scroll, zoom, camera), so going back restores it.
    views: HashMap<u64, ViewState>,
    /// Places to restore in views not made yet.
    saved: HashMap<u64, libviewer::PagesPlace>,
    wake: Wake,
    scale: f32,
    dark: bool,
    /// Drawn on a touch screen (iPhone, iPad): see [`Canvas::set_touch_screen`].
    touch_screen: bool,
    /// Frames still to draw after a resize, so layout catches up with the size.
    relayout: u8,
    last: Instant,
    /// Time since the last frame that was built.
    idle: f32,
    /// Trackpad pinches since the last frame, multiplied together.
    pinch: f32,
    /// Something outside libgui changed what is shown: build the next frame.
    stale: bool,
    batches: Vec<Batch>,
    clear: Color,
}

impl Canvas {
    /// A canvas on a `CAMetalLayer`, `width`×`height` pixels at `scale` pixels per
    /// point.
    ///
    /// # Safety
    /// `layer` is a valid `CAMetalLayer` that outlives the canvas.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "visionos"))]
    pub unsafe fn on_metal_layer(layer: *mut std::ffi::c_void, width: u32, height: u32, scale: f32, dark: bool, wake: Wake) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        // SAFETY: the caller's.
        let surface = unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer)) }.context("creating the surface")?;
        Self::on_surface(&instance, surface, width, height, scale, dark, wake)
    }

    /// A canvas on a surface made from `instance`.
    pub fn on_surface(instance: &wgpu::Instance, surface: wgpu::Surface<'static>, width: u32, height: u32, scale: f32, dark: bool, wake: Wake) -> anyhow::Result<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .context("no GPU adapter for the view")?;
        let (device, queue) = open_device(&adapter)?;
        let mut config = surface.get_default_config(&adapter, width.max(1), height.max(1)).context("the surface has no configuration")?;
        let caps = surface.get_capabilities(&adapter);
        // libgui blends in non-sRGB space, as the app's window does.
        config.format = caps.formats.iter().copied().find(|f| !f.is_srgb()).ok_or_else(|| anyhow!("no non-sRGB surface format"))?;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);
        Self::build(Some(surface), device, queue, config, scale, dark, wake)
    }

    /// A canvas that draws into a texture of its own, `width`×`height` pixels: for
    /// tests, and for shells that composite the pixels themselves.
    pub fn offscreen(width: u32, height: u32, scale: f32, dark: bool, wake: Wake) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).context("no GPU adapter")?;
        let (device, queue) = open_device(&adapter)?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Rgba8Unorm,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: Vec::new(),
            color_space: Default::default(),
        };
        Self::build(None, device, queue, config, scale, dark, wake)
    }

    fn build(surface: Option<wgpu::Surface<'static>>, device: wgpu::Device, queue: wgpu::Queue, config: wgpu::SurfaceConfiguration, scale: f32, dark: bool, wake: Wake) -> anyhow::Result<Self> {
        let renderer = libgui_wgpu::Renderer::new(&device, &queue, config.format);
        let ui = fonts::new_ui(dark)?;
        let offscreen = surface.is_none().then(|| target(&device, &config));
        Ok(Self {
            clear: ui.theme.palette.bg_app,
            surface,
            offscreen,
            config,
            device,
            queue,
            renderer,
            rx: None,
            ui,
            views: HashMap::new(),
            saved: HashMap::new(),
            wake,
            scale,
            dark,
            touch_screen: false,
            relayout: 0,
            last: Instant::now(),
            idle: 0.0,
            pinch: 1.0,
            stale: true,
            batches: Vec::new(),
        })
    }

    /// The view's new size in pixels, and its scale.
    pub fn resize(&mut self, width: u32, height: u32, scale: f32) {
        let (w, h) = (width.max(1), height.max(1));
        if (w, h) != (self.config.width, self.config.height) {
            self.config.width = w;
            self.config.height = h;
            match &self.surface {
                Some(surface) => surface.configure(&self.device, &self.config),
                None => self.offscreen = Some(target(&self.device, &self.config)),
            }
        }
        self.scale = scale;
        self.stale = true;
        // Views place themselves by the last frame's rects: the frame after a resize
        // is the first laid out at the new size.
        self.relayout = 2;
    }

    pub fn set_dark(&mut self, dark: bool) {
        if dark != self.dark {
            self.dark = dark;
            self.ui.theme = if dark { Theme::dark() } else { Theme::light() };
            self.stale = true;
        }
    }

    /// Input, in points from the view's top left.
    pub fn push(&mut self, event: InputEvent) {
        self.ui.push(event);
    }

    /// A trackpad pinch: `delta` as macOS's magnification (0.1 is 10% bigger).
    pub fn pinch(&mut self, delta: f32) {
        self.pinch *= 1.0 + delta;
        self.stale = true;
    }

    /// Something outside the canvas changed (a load finished, a texture decoded):
    /// build the next frame.
    /// The canvas is on a touch screen: views word their hints for fingers.
    pub fn set_touch_screen(&mut self, touch: bool) {
        self.touch_screen = touch;
        self.stale = true;
    }

    pub fn touch(&mut self) {
        self.stale = true;
    }

    pub fn has_view(&self, id: u64) -> bool {
        self.views.contains_key(&id)
    }

    /// Where the reader is in page `id`'s view, for views that can come back to it.
    pub fn place(&self, id: u64) -> Option<libviewer::PagesPlace> {
        self.views.get(&id)?.place()
    }

    /// Puts page `id`'s view at `place`: now if it's drawn, else when it first is.
    pub fn restore(&mut self, id: u64, place: libviewer::PagesPlace) {
        match self.views.get_mut(&id) {
            Some(view) => view.restore(place),
            None => {
                self.saved.insert(id, place);
            }
        }
        self.stale = true;
    }

    /// Draws `current`, keeping view state for the `pages` still open and freeing the
    /// rest.
    pub fn draw(&mut self, pages: &[Page], current: Option<&Page>) -> Drawn {
        // Views of pages closed since the last frame go, with their textures.
        let mut retired = Vec::new();
        self.views.retain(|id, view| {
            let open = pages.iter().any(|p| p.id == *id);
            if !open {
                retired.extend(view.textures());
            }
            open
        });
        if let Some(page) = current {
            let saved = &mut self.saved;
            self.views.entry(page.id).or_insert_with(|| {
                let mut view = ViewState::new(&page.doc.content);
                if let Some(place) = saved.remove(&page.id) {
                    view.restore(place);
                }
                view
            });
        }
        self.saved.retain(|id, _| pages.iter().any(|p| p.id == *id));

        let mut views = std::mem::take(&mut self.views);
        let drawn = self.frame(retired, |ui, gpu, dark| {
            let fill = ui.theme.palette.bg_app;
            let mut action = None;
            ui.container_id(Id::from_name("canvas"), Layout::column(), Frame { fill, clip: true, ..Frame::none() }, |ui| {
                if let Some((page, view)) = current.and_then(|p| views.get_mut(&p.id).map(|v| (p, v))) {
                    action = views::show(ui, page, view, gpu, dark);
                }
            });
            action
        });
        self.views = views;
        drawn
    }

    /// Draws libgui's whole interface for Viewer ([`ViewerApp`]: toolbar, menus, info,
    /// the page), the Apple apps' libgui interface. Its views and their textures are
    /// its own; the canvas's page views stay as they were for when it draws a page
    /// again.
    pub fn draw_app(&mut self, app: &mut ViewerApp) -> Drawn {
        self.set_dark(app.dark);
        // Textures of pages it closed in the last frame.
        let retired = std::mem::take(&mut app.retired);
        self.frame(retired, |ui, gpu, _| {
            app.ui(ui, gpu);
            None
        })
    }

    /// Builds a frame with `build` if anything changed, frees `retired` textures, and
    /// presents.
    fn frame(&mut self, retired: Vec<TextureId>, build: impl FnOnce(&mut Ui, &mut Gpu, bool) -> Option<Action>) -> Drawn {
        let now = Instant::now();
        self.idle += (now - self.last).as_secs_f32();
        self.last = now;
        let scale = self.scale;
        let info = FrameInfo {
            screen_size: Vec2::new(self.config.width as f32 / scale, self.config.height as f32 / scale),
            scale,
            dt: self.idle,
        };

        let mut drawn = Drawn { repaint_after: None, action: None, cursor: Cursor::Default };
        let mut settle = false;
        let mut freed = Vec::new();
        if std::mem::take(&mut self.stale) || self.ui.needs_frame_for(&info, self.idle) {
            self.idle = 0.0;
            self.ui.begin_frame(info);
            let mut gpu = Gpu {
                device: &self.device,
                queue: &self.queue,
                renderer: &mut self.renderer,
                rx: &mut self.rx,
                scale,
                settle: &mut settle,
                freed: &mut freed,
                pinch: self.pinch - 1.0,
                wake: &self.wake,
                touch_screen: self.touch_screen,
            };
            self.pinch = 1.0;
            let action = build(&mut self.ui, &mut gpu, self.dark);
            let out = self.ui.end_frame();
            drawn.action = action;
            drawn.repaint_after = out.platform.repaint_after;
            if self.relayout > 0 {
                self.relayout -= 1;
                self.stale = true;
                drawn.repaint_after = Some(0.0);
            }
            drawn.cursor = out.platform.cursor;
            self.clear = out.clear_color;
            self.renderer.prepare(&out);
            self.batches.clear();
            self.batches.extend_from_slice(&out.draw.batches);
        }
        for id in retired.into_iter().chain(freed) {
            self.renderer.unregister_texture(id);
        }

        let frame = match &self.surface {
            None => None,
            Some(surface) => match surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => Some(f),
                // Not on screen: the shell asks again when it is.
                wgpu::CurrentSurfaceTexture::Occluded => return drawn,
                _ => {
                    surface.configure(&self.device, &self.config);
                    self.stale = true;
                    drawn.repaint_after = Some(0.0);
                    return drawn;
                }
            },
        };
        let view = match (&frame, &self.offscreen) {
            (Some(f), _) => f.texture.create_view(&Default::default()),
            (None, Some(t)) => t.create_view(&Default::default()),
            (None, None) => unreachable!("a canvas has a surface or a texture"),
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let c = self.clear;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("canvas"),
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
            self.renderer.render_batches(&mut pass, &self.batches);
        }
        self.queue.submit([encoder.finish()]);
        if let Some(frame) = frame {
            self.queue.present(frame);
        }
        if settle {
            let _ = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(Duration::from_millis(500)) });
        }
        drawn
    }
}

impl Canvas {
    /// An offscreen canvas's last frame: RGBA, `width`×`height`, rows top first.
    /// `None` for a canvas on a surface.
    pub fn read_pixels(&self) -> Option<(u32, u32, Vec<u8>)> {
        let texture = self.offscreen.as_ref()?;
        let (w, h) = (self.config.width, self.config.height);
        let row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas readback"),
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(Duration::from_secs(5)) }).ok()?;
        let data = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            out.extend_from_slice(&data[y * row as usize..][..w as usize * 4]);
        }
        Some((w, h, out))
    }
}

fn target(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("canvas"),
        size: wgpu::Extent3d { width: config.width, height: config.height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: config.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

impl Drop for Canvas {
    fn drop(&mut self) {
        for view in self.views.values() {
            for id in view.textures() {
                self.renderer.unregister_texture(id);
            }
        }
    }
}
