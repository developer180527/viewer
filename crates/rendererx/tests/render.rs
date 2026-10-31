//! Headless rendering on whatever adapter the machine has. Skips (with a note) when
//! there is no GPU at all; CI provides a software adapter (llvmpipe / WARP).

use std::sync::{Arc, Mutex};

use rendererx::scene_data::glam::{Mat4, Vec3, Vec4};
use rendererx::scene_data::*;
use rendererx::*;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    pollster::block_on(adapter.request_device(&Default::default())).ok()
}

macro_rules! gpu_or_skip {
    () => {
        match gpu() {
            Some(g) => g,
            None => {
                eprintln!("no GPU adapter; skipping");
                return;
            }
        }
    };
}

fn cube() -> MeshData {
    let p: Vec<Vec3> = (0..8).map(|i| Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32) - 0.5).collect();
    #[rustfmt::skip]
    let idx: Vec<u16> = vec![
        0,2,1, 1,2,3,  4,5,6, 5,7,6,  0,1,4, 1,5,4,
        2,6,3, 3,6,7,  0,4,2, 2,4,6,  1,3,5, 3,7,5,
    ];
    MeshData::new(p).with_indices(Indices::U16(idx.into()))
}

fn cube_scene() -> (CpuScene, SceneBuilder, MeshId) {
    let (scene, mut b) = CpuScene::new();
    let mesh = b.add_mesh(cube()).unwrap();
    b.add_node(Node::mesh(mesh, None, Mat4::IDENTITY, None)).unwrap();
    (scene, b, mesh)
}

fn px(pixels: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    pixels[i..i + 4].try_into().unwrap()
}

fn render_once(r: &Renderer, device: &wgpu::Device, queue: &wgpu::Queue, scene: &GpuScene, view: &mut View, camera: &Camera) -> Vec<u8> {
    let mut encoder = device.create_command_encoder(&Default::default());
    r.render(&mut encoder, scene, view, camera, &RenderOptions::default());
    queue.submit([encoder.finish()]);
    view.read_pixels(device, queue).unwrap()
}

#[test]
fn clay_cube_renders_shaded_on_the_background() {
    let (device, queue) = gpu_or_skip!();
    let r = Renderer::new(&device, &queue, RendererDesc::default());
    let (scene, _b, _) = cube_scene();
    let mut gpu_scene = r.attach(&scene);
    assert!(r.sync(&mut gpu_scene));
    let mut view = r.create_view(ViewDesc { width: 64, height: 64 });
    let camera = Camera::framing(gpu_scene.bounds().unwrap());

    let pixels = render_once(&r, &device, &queue, &gpu_scene, &mut view, &camera);
    let bg = px(&pixels, 64, 0, 0);
    assert_eq!(bg, [28, 28, 31, 255], "corner is the sRGB-encoded background");
    let center = px(&pixels, 64, 32, 32);
    assert_ne!(center, bg, "the cube covers the centre");
    assert_eq!(center[3], 255);

    // Clay is lit: the visible faces aren't all one colour.
    let mut shades: Vec<[u8; 4]> = pixels.as_chunks::<4>().0.iter().copied().filter(|p| *p != bg).collect();
    shades.sort();
    shades.dedup();
    assert!(shades.len() >= 3, "expected several shades, got {shades:?}");

    if let Ok(dir) = std::env::var("RENDERERX_DUMP") {
        let mut ppm = b"P6 64 64 255\n".to_vec();
        pixels.as_chunks::<4>().0.iter().for_each(|p| ppm.extend_from_slice(&p[..3]));
        std::fs::write(format!("{dir}/clay_cube.ppm"), ppm).unwrap();
    }
}

#[test]
fn an_idle_view_renders_nothing_until_something_changes() {
    let (device, queue) = gpu_or_skip!();
    let r = Renderer::new(&device, &queue, RendererDesc { msaa: Msaa::Off, ..Default::default() });
    let (scene, mut b, mesh) = cube_scene();
    let mut gpu_scene = r.attach(&scene);
    r.sync(&mut gpu_scene);
    let mut view = r.create_view(ViewDesc { width: 32, height: 32 });
    let camera = Camera::framing(gpu_scene.bounds().unwrap());
    let opts = RenderOptions::default();

    assert!(r.needs_redraw(&gpu_scene, &view, &camera, &opts));
    render_once(&r, &device, &queue, &gpu_scene, &mut view, &camera);
    assert!(!r.needs_redraw(&gpu_scene, &view, &camera, &opts));
    assert!(!gpu_scene.needs_sync());
    assert!(!r.sync(&mut gpu_scene), "nothing new to upload");

    let moved = Camera { eye: camera.eye + Vec3::X, ..camera };
    assert!(r.needs_redraw(&gpu_scene, &view, &moved, &opts));

    // A new node in the CPU scene shows up after one sync.
    b.add_node(Node::mesh(mesh, None, Mat4::from_translation(Vec3::X * 2.0), None)).unwrap();
    assert!(gpu_scene.needs_sync());
    assert!(r.sync(&mut gpu_scene));
    assert!(r.needs_redraw(&gpu_scene, &view, &camera, &opts));

    view.resize(16, 16);
    assert!(r.needs_redraw(&gpu_scene, &view, &camera, &opts));
}

#[test]
fn memory_follows_the_rules() {
    let (device, queue) = gpu_or_skip!();
    let r = Renderer::new(&device, &queue, RendererDesc::default());
    let (scene, _b, mesh) = cube_scene();
    assert!(scene.read().cpu_bytes() > 0);
    let mut gpu_scene = r.attach(&scene);
    r.sync(&mut gpu_scene);

    // Viewer mode: the CPU copy is gone once on the GPU; the summary stays.
    assert_eq!(scene.read().cpu_bytes(), 0);
    assert_eq!(scene.read().mesh(mesh).unwrap().info.primitives, 12);
    assert!(gpu_scene.gpu_bytes() > 0);

    // A view holds window-sized targets only while it has them.
    let mut view = r.create_view(ViewDesc { width: 100, height: 50 });
    let empty = view.gpu_bytes();
    render_once(&r, &device, &queue, &gpu_scene, &mut view, &Camera::framing(gpu_scene.bounds().unwrap()));
    let full = view.gpu_bytes();
    // Colour + 4× MSAA colour + 4× depth, 4 bytes each.
    assert_eq!(full - empty, 100 * 50 * 4 * 9);
    view.release_targets();
    assert_eq!(view.gpu_bytes(), empty);
    assert!(view.target().is_none());

    // Editor mode keeps the CPU data.
    let (scene, _b, _) = cube_scene();
    scene.set_keep_cpu_data(true);
    let mut kept = r.attach(&scene);
    r.sync(&mut kept);
    assert!(scene.read().cpu_bytes() > 0);
}

#[test]
fn big_scenes_stream_in_over_several_syncs() {
    let (device, queue) = gpu_or_skip!();
    let r = Renderer::new(&device, &queue, RendererDesc { upload_budget: 1, ..Default::default() });
    let (scene, mut b) = CpuScene::new();
    for i in 0..3 {
        let m = b.add_mesh(cube()).unwrap();
        b.add_node(Node::mesh(m, None, Mat4::from_translation(Vec3::X * i as f32 * 2.0), None)).unwrap();
    }
    b.finish();
    let mut gpu_scene = r.attach(&scene);
    let mut syncs = 0;
    while gpu_scene.needs_sync() {
        assert!(r.sync(&mut gpu_scene), "each sync uploads at least one mesh");
        syncs += 1;
        assert!(syncs <= 3);
    }
    assert_eq!(syncs, 3);
    let _ = queue;
}

#[test]
fn one_scene_many_views_and_every_topology() {
    let (device, queue) = gpu_or_skip!();
    let mut r = Renderer::new(&device, &queue, RendererDesc::default());
    let events = Arc::new(Mutex::new(Vec::new()));
    let e = events.clone();
    r.set_trace(move |what| e.lock().unwrap().push(what));

    let (scene, mut b) = CpuScene::new();
    let mut colored = cube();
    colored.colors = Some(vec![Vec4::new(1.0, 0.0, 0.0, 1.0); 8].into());
    let red = b.add_mesh(colored).unwrap();
    let mut lines = MeshData::new(vec![Vec3::ZERO, Vec3::ONE, Vec3::X, Vec3::Y]);
    lines.topology = Topology::Lines;
    let lines = b.add_mesh(lines).unwrap();
    let mut points = MeshData::new(vec![Vec3::ZERO, Vec3::ONE]);
    points.topology = Topology::Points;
    let points = b.add_mesh(points).unwrap();
    let blue = b.add_material(Material::clay().with_param("base_color", ParamValue::Vec3(Vec3::new(0.0, 0.0, 1.0)))).unwrap();
    for m in [red, lines, points] {
        b.add_node(Node::mesh(m, Some(blue), Mat4::IDENTITY, None)).unwrap();
    }
    b.finish();

    let mut gpu_scene = r.attach(&scene);
    r.sync(&mut gpu_scene);
    let camera = Camera::framing(gpu_scene.bounds().unwrap());
    let mut a = r.create_view(ViewDesc { width: 48, height: 48 });
    let mut c = r.create_view(ViewDesc { width: 80, height: 40 });
    let pa = render_once(&r, &device, &queue, &gpu_scene, &mut a, &camera);
    let pc = render_once(&r, &device, &queue, &gpu_scene, &mut c, &camera);
    assert_eq!((pa.len(), pc.len()), (48 * 48 * 4, 80 * 40 * 4));

    // Vertex red × material blue = black-ish, lit only by the rim: never red or blue.
    let center = px(&pa, 48, 24, 24);
    assert!(center[0] < 80 && center[2] < 80, "{center:?}");

    assert_eq!(*events.lock().unwrap(), ["pipeline clay ready", "first mesh uploaded", "first frame"]);
}

#[cfg(feature = "controls")]
#[test]
fn orbit_controls_keep_the_model_in_view() {
    let bounds = Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0));
    let mut c = OrbitControls::framing(bounds);
    let start = c.camera();
    assert!((start.eye - Camera::framing(bounds).eye).length() < 1e-4);

    c.orbit(10_000.0, 10_000.0, 100.0);
    assert!(c.pitch <= 1.55, "pitch is clamped short of the pole");
    let cam = c.camera();
    assert!(((cam.eye - cam.target).length() - c.distance).abs() < 1e-3);

    c.zoom(1e9);
    assert!(c.distance >= c.radius * 0.01);
    c.zoom(f32::NAN);
    assert!(c.distance.is_finite());
    c.pan(50.0, 0.0, 100.0);
    assert_ne!(c.target, Vec3::ZERO);
    assert!(c.camera().view_proj(1.5).is_finite());
}
