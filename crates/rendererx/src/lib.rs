//! RendererX: a small 3D renderer on wgpu for showing models.
//!
//! It uses the host's device and queue, draws into a texture per [`View`], and owns no
//! window, event loop or threads. Scenes are built on the CPU with
//! [`rendererx_scene`] (re-exported as [`scene_data`]) and attached here when the GPU is
//! ready:
//!
//! ```ignore
//! let r = Renderer::new(&device, &queue, RendererDesc::default());
//! let mut gpu_scene = r.attach(&cpu_scene);
//! let mut view = r.create_view(ViewDesc { width, height });
//! // each tick:
//! r.sync(&mut gpu_scene);
//! if r.needs_redraw(&gpu_scene, &view, &camera, &opts) {
//!     r.render(&mut encoder, &gpu_scene, &mut view, &camera, &opts);
//! }
//! ```
//!
//! Milestone 1: clay shading only; textures and the authored shading models come next.
//! See docs/3d-renderer.md.

mod camera;
mod scene;
mod textures;
mod view;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub use camera::Camera;
#[cfg(feature = "controls")]
pub use camera::OrbitControls;
pub use rendererx_scene as scene_data;
pub use scene::GpuScene;
pub use view::{TARGET_FORMAT, View, ViewDesc};

use rendererx_scene::{CpuScene, Topology};
use scene::INSTANCE_FLOATS;
use textures::Model;
use view::{DEPTH_FORMAT, FrameKey, LINEAR_FORMAT, Targets, UNIFORM_BYTES};

/// GPU features RendererX uses when the device has them: compressed texture formats
/// (BC, ETC2, ASTC). Request `adapter.features() & rendererx::OPTIONAL_FEATURES`
/// when creating the device, or compressed textures are refused as unsupported.
pub const OPTIONAL_FEATURES: wgpu::Features = wgpu::Features::TEXTURE_COMPRESSION_BC
    .union(wgpu::Features::TEXTURE_COMPRESSION_ETC2)
    .union(wgpu::Features::TEXTURE_COMPRESSION_ASTC);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msaa {
    Off,
    X4,
    /// 4× (every WebGPU device supports it for these formats). Mobile hosts that want
    /// less pass `Off`.
    Auto,
}

impl Msaa {
    fn samples(self) -> u32 {
        match self {
            Msaa::Off => 1,
            Msaa::X4 | Msaa::Auto => 4,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RendererDesc {
    pub msaa: Msaa,
    /// Bytes uploaded per [`Renderer::sync`] call, so a huge model streams in over
    /// several frames instead of stalling one.
    pub upload_budget: u64,
    /// No shader is ever compiled on the frame path, not even clay: `new`, `sync` and
    /// `render` only queue pipelines, the host compiles them on a worker with
    /// [`Renderer::compile_one`] / [`compile_pending`](Renderer::compile_pending), and
    /// batches whose pipeline isn't ready yet are left out of the frame. For
    /// interactive hosts: on a cold driver cache each pipeline takes tens of
    /// milliseconds (65–100 ms on an M-series Mac), a visible stall on the UI thread.
    /// Off (the default), clay compiles where needed, which suits tests and one-shot
    /// renders.
    pub background_compile: bool,
}

impl Default for RendererDesc {
    fn default() -> Self {
        Self { msaa: Msaa::Auto, upload_budget: 32 << 20, background_compile: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderOptions {
    /// sRGB-encoded, straight alpha. Alpha 0 gives a transparent background (thumbnails).
    pub background: [f32; 4],
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { background: [0.11, 0.11, 0.12, 1.0] }
    }
}

type Trace = Arc<dyn Fn(&'static str) + Send + Sync>;

/// One per device; shared by every window and scene.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    desc: RendererDesc,
    view_layout: wgpu::BindGroupLayout,
    clay_layout: wgpu::PipelineLayout,
    clay_module: wgpu::ShaderModule,
    /// Pipelines by shading model, topology, then without/with a vertex colour stream,
    /// then without/with uvs. Clay triangles are compiled up front (the first frame);
    /// the rest on first use.
    pipelines: [[[[OnceLock<wgpu::RenderPipeline>; 2]; 2]; 3]; 3],
    /// Pipelines a frame needed but didn't have (drawn as clay meanwhile), for
    /// [`compile_pending`](Self::compile_pending) or the next [`sync`](Self::sync).
    wanted: Mutex<Vec<PipelineKey>>,
    /// Bumped when a wanted pipeline is ready, so views redraw with it.
    pipelines_ready: AtomicU64,
    materials: textures::MaterialBinding,
    /// The sRGB → target copy, when MSAA is on.
    encode: Option<Encode>,
    trace: Option<Trace>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PipelineKey {
    model: Model,
    topology: Topology,
    colors: bool,
    uvs: bool,
}

struct Encode {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl Renderer {
    /// Compiles the clay pipelines: the only shader work on the path to the first frame.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, desc: RendererDesc) -> Self {
        let view_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rendererx view"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM_BYTES),
                },
                count: None,
            }],
        });
        let materials = textures::MaterialBinding::new(device, queue);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rendererx clay"),
            bind_group_layouts: &[Some(&view_layout), Some(&materials.layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rendererx clay"),
            // One module: the shared vertex stage, then each model's fragment stage.
            source: wgpu::ShaderSource::Wgsl(
                concat!(include_str!("shading/common.wgsl"), include_str!("shading/clay.wgsl"), include_str!("shading/pbr.wgsl")).into(),
            ),
        });
        let encode = (desc.msaa.samples() > 1).then(|| encode_pipeline(device));
        let r = Self {
            device: device.clone(),
            queue: queue.clone(),
            desc,
            view_layout,
            clay_layout: layout,
            clay_module: module,
            pipelines: Default::default(),
            wanted: Mutex::new(Vec::new()),
            pipelines_ready: AtomicU64::new(0),
            materials,
            encode,
            trace: None,
        };
        // What a typical model's first frame needs: clay triangles, with and without
        // uvs (vertex colours are rarer and compile on first use). Everything else
        // draws as clay until its own pipeline is compiled (§7.3). In background mode
        // even these are only queued for the host's worker.
        for uvs in [true, false] {
            let k = PipelineKey { model: Model::Clay, topology: Topology::Triangles, colors: false, uvs };
            if r.desc.background_compile {
                r.want(k);
            } else {
                r.pipeline(k);
            }
        }
        r
    }

    fn slot(&self, k: PipelineKey) -> &OnceLock<wgpu::RenderPipeline> {
        let t = match k.topology {
            Topology::Triangles => 0,
            Topology::Lines => 1,
            Topology::Points => 2,
        };
        &self.pipelines[k.model.index()][t][k.colors as usize][k.uvs as usize]
    }

    /// Compiles (if needed) and returns a pipeline, blocking.
    fn pipeline(&self, k: PipelineKey) -> &wgpu::RenderPipeline {
        self.slot(k).get_or_init(|| {
            mesh_pipeline(&self.device, &self.clay_layout, &self.clay_module, self.desc.msaa.samples(), k.model, k.topology, k.colors, k.uvs)
        })
    }

    /// Queues `k` for compiling (once). The queue is taken from the end.
    fn want(&self, k: PipelineKey) {
        let mut wanted = self.wanted.lock().unwrap_or_else(|e| e.into_inner());
        if !wanted.contains(&k) {
            wanted.push(k);
        }
    }

    /// Queues `k` to compile next, ahead of everything already queued.
    fn want_next(&self, k: PipelineKey) {
        let mut wanted = self.wanted.lock().unwrap_or_else(|e| e.into_inner());
        wanted.retain(|w| *w != k);
        wanted.push(k);
    }

    /// The pipeline for `k` if it is compiled. Otherwise it is queued and the clay
    /// pipeline for the same vertex streams stands in: compiled now if need be (clay
    /// is the one shader allowed on the frame path), or, in background mode, queued
    /// ahead of `k` with `None` returned so the batch waits a frame or two.
    fn pipeline_or_clay(&self, k: PipelineKey) -> Option<&wgpu::RenderPipeline> {
        if let Some(p) = self.slot(k).get() {
            return Some(p);
        }
        self.want(k);
        let clay = PipelineKey { model: Model::Clay, ..k };
        if let Some(p) = self.slot(clay).get() {
            return Some(p);
        }
        if self.desc.background_compile {
            // Ahead of `k` and every other model: one clay pipeline gives every mesh
            // using these vertex streams a shape.
            self.want_next(clay);
            return None;
        }
        Some(self.pipeline(clay))
    }

    /// Compiles one queued pipeline, blocking. Safe to call from any thread: hosts in
    /// background mode run it on a worker until it returns false, telling their views
    /// to redraw after each (a frame drawn without it is stale once it's ready).
    pub fn compile_one(&self) -> bool {
        let Some(k) = self.next_wanted() else { return false };
        self.pipeline(k);
        self.pipelines_ready.fetch_add(1, Ordering::Release);
        true
    }

    /// Compiles every queued pipeline (see [`compile_one`](Self::compile_one)).
    /// Returns true when something was compiled.
    pub fn compile_pending(&self) -> bool {
        let mut any = false;
        while self.compile_one() {
            any = true;
        }
        any
    }

    /// Compiles, blocking, every pipeline `scene` draws with as it stands, so the next
    /// frame has its final shading instead of clay stand-ins. For one-shot renders
    /// (thumbnails, screenshots, tests); interactive hosts let pipelines arrive through
    /// [`sync`](Self::sync) or [`compile_pending`](Self::compile_pending).
    pub fn prepare(&self, scene: &GpuScene) {
        for batch in &scene.batches {
            if let Some(mesh) = scene.meshes.get(batch.mesh.index()).and_then(Option::as_ref) {
                self.pipeline(PipelineKey { model: batch.model, topology: mesh.topology, colors: mesh.colors.is_some(), uvs: mesh.uvs.is_some() });
            }
        }
    }

    /// Some material is drawing as clay until its pipeline is compiled: keep ticking
    /// (call [`sync`](Self::sync) or [`compile_pending`](Self::compile_pending)).
    pub fn pipelines_pending(&self) -> bool {
        !self.wanted.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    fn next_wanted(&self) -> Option<PipelineKey> {
        self.wanted.lock().unwrap_or_else(|e| e.into_inner()).pop()
    }

    /// Receives milestones (`first mesh uploaded`, `first frame`) for startup tracing.
    pub fn set_trace(&mut self, f: impl Fn(&'static str) + Send + Sync + 'static) {
        let f: Trace = Arc::new(f);
        f("pipeline clay ready");
        self.trace = Some(f);
    }

    fn trace(&self, what: &'static str) {
        if let Some(t) = &self.trace {
            t(what);
        }
    }

    /// Starts following a CPU scene. Nothing is uploaded until [`sync`](Self::sync).
    pub fn attach(&self, scene: &CpuScene) -> GpuScene {
        GpuScene::new(scene)
    }

    /// Uploads what the scene gained or changed since the last call, within the upload
    /// budget, then frees the CPU copies of what was uploaded (unless the scene keeps
    /// them for editing). Returns true when the next frame would look different.
    /// Call once per tick while [`GpuScene::needs_sync`] is true.
    ///
    /// Also compiles one pipeline a frame asked for, if any (so hosts that never call
    /// [`compile_pending`](Self::compile_pending) still get every shading model, one
    /// per tick, without a frame waiting on all of them). Not in background mode,
    /// where the host's worker does it.
    pub fn sync(&self, scene: &mut GpuScene) -> bool {
        // In background mode the host's worker compiles; never here.
        let compiled = !self.desc.background_compile && self.compile_one();
        let synced = scene.sync(&self.device, &self.queue, &self.materials, self.desc.upload_budget);
        for id in &synced.done {
            scene.reader.release_mesh_data(*id);
        }
        for id in &synced.done_textures {
            scene.reader.release_texture_data(*id);
        }
        if synced.uploaded > 0 && !scene.first_mesh_traced {
            scene.first_mesh_traced = true;
            self.trace("first mesh uploaded");
        }
        synced.changed || compiled
    }

    pub fn create_view(&self, desc: ViewDesc) -> View {
        let uniforms = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rendererx view uniforms"),
            size: UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rendererx view"),
            layout: &self.view_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        View {
            width: desc.width.max(1),
            height: desc.height.max(1),
            targets: None,
            uniforms,
            bind_group,
            last: None,
            target_generation: 0,
        }
    }

    /// False when the view already shows exactly this scene state, camera and options:
    /// an idle viewer renders nothing.
    pub fn needs_redraw(&self, scene: &GpuScene, view: &View, camera: &Camera, options: &RenderOptions) -> bool {
        view.targets.is_none() || view.last != Some(self.frame_key(scene, view, camera, options))
    }

    /// Records a frame into `encoder`; the host submits it (alongside its UI work, on
    /// the shared queue). Never waits: meshes not uploaded yet are simply absent.
    pub fn render(&self, encoder: &mut wgpu::CommandEncoder, scene: &GpuScene, view: &mut View, camera: &Camera, options: &RenderOptions) {
        if view.targets.is_none() {
            view.targets = Some(self.create_targets(view.width, view.height));
            view.target_generation += 1;
        }
        let aspect = view.width as f32 / view.height as f32;
        let mut uniforms = [0f32; 24];
        uniforms[..16].copy_from_slice(&camera.view_proj(aspect).to_cols_array());
        uniforms[16..19].copy_from_slice(&camera.eye.to_array());
        uniforms[20..23].copy_from_slice(&camera.key_light().to_array());
        self.queue.write_buffer(&view.uniforms, 0, bytemuck::cast_slice(&uniforms));

        let targets = view.targets.as_ref().expect("created above");
        let [r, g, b, a] = options.background.map(|c| c as f64);
        // With MSAA the frame is drawn in linear light into an sRGB target, so the
        // clear colour is decoded; without, the shader writes encoded bytes directly.
        // The MSAA clear is also premultiplied: resolving averages edge samples with
        // it, which gives premultiplied colour, and the encode pass divides alpha back
        // out so the target stays straight alpha (no fringes on transparent
        // backgrounds). Opaque backgrounds are unchanged (a = 1).
        let (attachment, resolve, clear) = match &targets.msaa {
            Some(msaa) => (&msaa.samples, Some(&msaa.resolve), [srgb_to_linear(r) * a, srgb_to_linear(g) * a, srgb_to_linear(b) * a, a]),
            None => (&targets.color_view, None, [r, g, b, a]),
        };
        let [r, g, b, a] = clear;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rendererx frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: attachment,
                depth_slice: None,
                resolve_target: resolve,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                    // The multisampled buffer is only needed until it's resolved.
                    store: if resolve.is_some() { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store },
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &targets.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(instances) = &scene.instances {
            pass.set_bind_group(0, &view.bind_group, &[]);
            for batch in &scene.batches {
                let Some(mesh) = scene.meshes.get(batch.mesh.index()).and_then(Option::as_ref) else { continue };
                let key = PipelineKey { model: batch.model, topology: mesh.topology, colors: mesh.colors.is_some(), uvs: mesh.uvs.is_some() };
                // Background mode: no pipeline yet, not even clay; it's queued and the
                // batch shows a frame or two later.
                let Some(pipeline) = self.pipeline_or_clay(key) else { continue };
                pass.set_pipeline(pipeline);
                let groups = batch.material.and_then(|m| scene.material_groups.get(m.index())).and_then(Option::as_ref);
                // Without uvs there is nothing to sample maps with: the factors alone.
                let group = groups.map(|g| if mesh.uvs.is_some() { &g.textured } else { &g.plain });
                pass.set_bind_group(1, group.unwrap_or(&self.materials.default_group), &[]);
                // Streams in the pipeline's slot order: positions, normals, then the
                // optional colours and uvs, then instances.
                let streams = [Some(&mesh.positions), Some(&mesh.normals), mesh.colors.as_ref(), mesh.uvs.as_ref(), Some(instances)];
                for (slot, buffer) in streams.into_iter().flatten().enumerate() {
                    pass.set_vertex_buffer(slot as u32, buffer.slice(..));
                }
                match &mesh.indices {
                    Some((indices, format)) => {
                        pass.set_index_buffer(indices.slice(..), *format);
                        pass.draw_indexed(0..mesh.elements, 0, batch.instances.clone());
                    }
                    None => pass.draw(0..mesh.elements, batch.instances.clone()),
                }
            }
        }
        drop(pass);
        if let (Some(msaa), Some(encode)) = (&targets.msaa, &self.encode) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rendererx encode"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.color_view,
                    depth_slice: None,
                    resolve_target: None,
                    // Every pixel is overwritten.
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&encode.pipeline);
            pass.set_bind_group(0, &msaa.encode, &[]);
            pass.draw(0..3, 0..1);
        }
        view.last = Some(self.frame_key(scene, view, camera, options));
        if !scene.batches.is_empty() && !scene.first_frame_traced.swap(true, std::sync::atomic::Ordering::Relaxed) {
            self.trace("first frame");
        }
    }

    fn create_targets(&self, width: u32, height: u32) -> Targets {
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let samples = self.desc.msaa.samples();
        let texture = |label: &str, format: wgpu::TextureFormat, sample_count: u32, usage: wgpu::TextureUsages| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color = texture(
            "rendererx target",
            TARGET_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        let pixels = width as u64 * height as u64;
        // MSAA samples and depth live only within the frame's pass (cleared on load,
        // discarded on store): on tile-based GPUs (Apple, most mobile) they need no
        // memory at all; elsewhere this is a no-op.
        let transient = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TRANSIENT_ATTACHMENT;
        let msaa = self.encode.as_ref().filter(|_| samples > 1).map(|encode| {
            let samples_tex = texture("rendererx msaa", LINEAR_FORMAT, samples, transient);
            let resolve_tex = texture(
                "rendererx resolve",
                LINEAR_FORMAT,
                1,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            let resolve = resolve_tex.create_view(&Default::default());
            let encode = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rendererx encode"),
                layout: &encode.layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&resolve) }],
            });
            view::Msaa { samples: samples_tex.create_view(&Default::default()), resolve, encode }
        });
        let depth = texture("rendererx depth", DEPTH_FORMAT, samples, transient);
        // Target, depth, and with MSAA the sample and resolve textures.
        // Target and resolve are real memory; samples and depth are transient.
        let bytes = pixels * 4 + msaa.as_ref().map_or(0, |_| pixels * 4);
        let transient_bytes = pixels * 4 * samples as u64 + msaa.as_ref().map_or(0, |_| pixels * 4 * samples as u64);
        Targets { color_view: color.create_view(&Default::default()), color, msaa, depth: depth.create_view(&Default::default()), bytes, transient_bytes }
    }
}

impl Renderer {
    fn frame_key(&self, scene: &GpuScene, view: &View, camera: &Camera, options: &RenderOptions) -> FrameKey {
        FrameKey {
            scene: scene.id,
            generation: scene.generation,
            pipelines: self.pipelines_ready.load(Ordering::Acquire),
            camera: *camera,
            options: *options,
            size: (view.width, view.height),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn mesh_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    samples: u32,
    model: Model,
    topology: Topology,
    colors: bool,
    uvs: bool,
) -> wgpu::RenderPipeline {
    const POSITIONS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x3];
    const NORMALS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![1 => Float32x3];
    const COLORS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![2 => Float32x4];
    const UVS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![3 => Float32x2];
    const INSTANCE: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
        4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4,
        8 => Float32x4, 9 => Float32x4, 10 => Float32x4, 11 => Float32x4,
    ];
    let stream = |stride: u64, attributes: &'static [wgpu::VertexAttribute]| {
        Some(wgpu::VertexBufferLayout { array_stride: stride, step_mode: wgpu::VertexStepMode::Vertex, attributes })
    };
    let instance = Some(wgpu::VertexBufferLayout {
        array_stride: (INSTANCE_FLOATS * 4) as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &INSTANCE,
    });
    // With MSAA the frame is drawn linear into an sRGB target (resolved in linear light).
    let (encoded, linear) = model.entry_points();
    let (format, fs) = if samples > 1 { (LINEAR_FORMAT, linear) } else { (TARGET_FORMAT, encoded) };
    let mut buffers = vec![stream(12, &POSITIONS), stream(12, &NORMALS)];
    if colors {
        buffers.push(stream(16, &COLORS));
    }
    if uvs {
        buffers.push(stream(8, &UVS));
    }
    buffers.push(instance);
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("rendererx mesh"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(match (colors, uvs) {
                (false, false) => "vs_main",
                (true, false) => "vs_color",
                (false, true) => "vs_uv",
                (true, true) => "vs_color_uv",
            }),
            compilation_options: Default::default(),
            buffers: &buffers,
        },
        primitive: wgpu::PrimitiveState {
            topology: match topology {
                Topology::Triangles => wgpu::PrimitiveTopology::TriangleList,
                Topology::Lines => wgpu::PrimitiveTopology::LineList,
                Topology::Points => wgpu::PrimitiveTopology::PointList,
            },
            // Clay is double-sided: files with inconsistent winding still look solid.
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn encode_pipeline(device: &wgpu::Device) -> Encode {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("rendererx encode"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rendererx encode"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("rendererx encode"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shading/encode.wgsl").into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("rendererx encode"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format: TARGET_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    });
    Encode { layout, pipeline }
}
