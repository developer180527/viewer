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
mod view;

use std::sync::{Arc, OnceLock};

pub use camera::Camera;
#[cfg(feature = "controls")]
pub use camera::OrbitControls;
pub use rendererx_scene as scene_data;
pub use scene::GpuScene;
pub use view::{TARGET_FORMAT, View, ViewDesc};

use rendererx_scene::{CpuScene, Topology};
use scene::INSTANCE_FLOATS;
use view::{DEPTH_FORMAT, FrameKey, LINEAR_FORMAT, Targets, UNIFORM_BYTES};

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
}

impl Default for RendererDesc {
    fn default() -> Self {
        Self { msaa: Msaa::Auto, upload_budget: 32 << 20 }
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
    /// Clay pipelines by topology, without and with a vertex colour stream. Triangles
    /// are compiled up front; lines and points on first use.
    clay: [[OnceLock<wgpu::RenderPipeline>; 2]; 3],
    /// The sRGB → target copy, when MSAA is on.
    encode: Option<Encode>,
    trace: Option<Trace>,
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rendererx clay"),
            bind_group_layouts: &[Some(&view_layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rendererx clay"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shading/clay.wgsl").into()),
        });
        let encode = (desc.msaa.samples() > 1).then(|| encode_pipeline(device));
        let r = Self {
            device: device.clone(),
            queue: queue.clone(),
            desc,
            view_layout,
            clay_layout: layout,
            clay_module: module,
            clay: Default::default(),
            encode,
            trace: None,
        };
        // The pipelines a typical model needs for its first frame.
        r.pipeline(Topology::Triangles, false);
        r.pipeline(Topology::Triangles, true);
        r
    }

    fn pipeline(&self, topology: Topology, colors: bool) -> &wgpu::RenderPipeline {
        let t = match topology {
            Topology::Triangles => 0,
            Topology::Lines => 1,
            Topology::Points => 2,
        };
        self.clay[t][colors as usize].get_or_init(|| {
            clay_pipeline(&self.device, &self.clay_layout, &self.clay_module, self.desc.msaa.samples(), topology, colors)
        })
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
    pub fn sync(&self, scene: &mut GpuScene) -> bool {
        let synced = scene.sync(&self.device, &self.queue, self.desc.upload_budget);
        for id in &synced.done {
            scene.reader.release_mesh_data(*id);
        }
        if synced.uploaded > 0 && !scene.first_mesh_traced {
            scene.first_mesh_traced = true;
            self.trace("first mesh uploaded");
        }
        synced.changed
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
        view.targets.is_none() || view.last != Some(frame_key(scene, view, camera, options))
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
                pass.set_pipeline(self.pipeline(mesh.topology, mesh.colors.is_some()));
                pass.set_vertex_buffer(0, mesh.positions.slice(..));
                pass.set_vertex_buffer(1, mesh.normals.slice(..));
                let instance_slot = match &mesh.colors {
                    Some(colors) => {
                        pass.set_vertex_buffer(2, colors.slice(..));
                        3
                    }
                    None => 2,
                };
                pass.set_vertex_buffer(instance_slot, instances.slice(..));
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
        view.last = Some(frame_key(scene, view, camera, options));
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
        let msaa = self.encode.as_ref().filter(|_| samples > 1).map(|encode| {
            let samples_tex = texture("rendererx msaa", LINEAR_FORMAT, samples, wgpu::TextureUsages::RENDER_ATTACHMENT);
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
        let depth = texture("rendererx depth", DEPTH_FORMAT, samples, wgpu::TextureUsages::RENDER_ATTACHMENT);
        // Target, depth, and with MSAA the sample and resolve textures.
        let bytes = pixels * 4 + pixels * 4 * samples as u64 + msaa.as_ref().map_or(0, |_| pixels * 4 * (samples as u64 + 1));
        Targets { color_view: color.create_view(&Default::default()), color, msaa, depth: depth.create_view(&Default::default()), bytes }
    }
}

fn frame_key(scene: &GpuScene, view: &View, camera: &Camera, options: &RenderOptions) -> FrameKey {
    FrameKey { scene: scene.id, generation: scene.generation, camera: *camera, options: *options, size: (view.width, view.height) }
}

fn clay_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    samples: u32,
    topology: Topology,
    colors: bool,
) -> wgpu::RenderPipeline {
    const POSITIONS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x3];
    const NORMALS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![1 => Float32x3];
    const COLORS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![2 => Float32x4];
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
    let (format, fs) = if samples > 1 { (LINEAR_FORMAT, "fs_linear") } else { (TARGET_FORMAT, "fs_main") };
    let buffers = if colors {
        vec![stream(12, &POSITIONS), stream(12, &NORMALS), stream(16, &COLORS), instance]
    } else {
        vec![stream(12, &POSITIONS), stream(12, &NORMALS), instance]
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("rendererx clay"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(if colors { "vs_color" } else { "vs_main" }),
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
