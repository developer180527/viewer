//! Headless rendering on whatever adapter the machine has. Skips (with a note) when
//! there is no GPU at all; CI provides a software adapter (llvmpipe / WARP).

use std::sync::{Arc, Mutex};

use rendererx::scene_data::glam::{Mat4, Vec2, Vec3, Vec4};
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
    let camera = Camera::framing(gpu_scene.bounds().unwrap(), 1.0);

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
    let camera = Camera::framing(gpu_scene.bounds().unwrap(), 1.0);
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
    render_once(&r, &device, &queue, &gpu_scene, &mut view, &Camera::framing(gpu_scene.bounds().unwrap(), 2.0));
    let full = view.gpu_bytes();
    // Target + 4× depth + 4× MSAA colour + its resolve, 4 bytes each.
    assert_eq!(full - empty, 100 * 50 * 4 * 10);
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

fn quad(transform: Mat4) -> CpuScene {
    let (scene, mut b) = CpuScene::new();
    let mut m = MeshData::new(vec![Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(1.0, 1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0)])
        .with_indices(Indices::U16(vec![0, 1, 2, 0, 2, 3].into()));
    m.normals = Some(vec![Vec3::Z; 4].into());
    let mesh = b.add_mesh(m).unwrap();
    b.add_node(Node::mesh(mesh, None, transform, None)).unwrap();
    b.finish();
    scene
}

fn centre_of(r: &Renderer, device: &wgpu::Device, queue: &wgpu::Queue, scene: &GpuScene) -> [u8; 4] {
    let mut view = r.create_view(ViewDesc { width: 32, height: 32 });
    let camera = Camera { eye: Vec3::new(0.0, 0.0, 4.0), ..Default::default() };
    px(&render_once(r, device, queue, scene, &mut view, &camera), 32, 16, 16)
}

#[test]
fn mirrored_nodes_are_lit_like_the_original() {
    let (device, queue) = gpu_or_skip!();
    let r = Renderer::new(&device, &queue, RendererDesc::default());
    let shade = |m: Mat4| {
        let mut g = r.attach(&quad(m));
        r.sync(&mut g);
        centre_of(&r, &device, &queue, &g)
    };
    let plain = shade(Mat4::IDENTITY);
    assert_eq!(shade(Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0))), plain, "a mirrored copy isn't shaded inside-out");
}

#[test]
fn two_gpu_scenes_of_one_model_both_draw_it() {
    let (device, queue) = gpu_or_skip!();
    let r = Renderer::new(&device, &queue, RendererDesc::default());
    let scene = quad(Mat4::IDENTITY);
    let (mut first, mut second) = (r.attach(&scene), r.attach(&scene));
    r.sync(&mut first);
    assert!(scene.read().cpu_bytes() > 0, "kept until the second scene uploads too");
    r.sync(&mut second);
    assert_eq!(scene.read().cpu_bytes(), 0);
    assert_eq!(centre_of(&r, &device, &queue, &first), centre_of(&r, &device, &queue, &second));
    assert_eq!(second.unavailable_meshes(), 0);
}

#[test]
fn meshes_over_the_device_buffer_limit_are_refused_not_a_panic() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else { return };
    let desc = wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits { max_buffer_size: 1024, ..wgpu::Limits::downlevel_defaults() },
        ..Default::default()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&desc)).unwrap();
    let r = Renderer::new(&device, &queue, RendererDesc::default());
    let (scene, mut b) = CpuScene::new();
    let big = b.add_mesh(MeshData::new(vec![Vec3::ZERO; 300])).unwrap(); // 3600-byte positions
    b.add_node(Node::mesh(big, None, Mat4::IDENTITY, None)).unwrap();
    let small = b.add_mesh(cube()).unwrap();
    b.add_node(Node::mesh(small, None, Mat4::IDENTITY, None)).unwrap();
    b.finish();
    let mut g = r.attach(&scene);
    while g.needs_sync() {
        r.sync(&mut g);
    }
    assert_eq!(g.upload_errors().len(), 1);
    assert_eq!(g.upload_errors()[0].0, big);
    assert!(g.gpu_bytes() > 0, "the rest of the model still uploads");
}

#[test]
fn framing_fits_portrait_views() {
    let bounds = Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0));
    for aspect in [9.0 / 19.5, 1.0, 2.0] {
        let cam = Camera::framing(bounds, aspect);
        let vp = cam.view_proj(aspect);
        for i in 0..8 {
            let p = Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32) * 2.0 - 1.0;
            let c = vp * p.extend(1.0);
            assert!((c.x / c.w).abs() <= 1.0 && (c.y / c.w).abs() <= 1.0, "aspect {aspect}: corner {p} is cut off");
        }
    }
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
    let camera = Camera::framing(gpu_scene.bounds().unwrap(), 1.0);
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
    let mut c = OrbitControls::framing(bounds, 1.0);
    let start = c.camera();
    assert!((start.eye - Camera::framing(bounds, 1.0).eye).length() < 1e-4);

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

#[test]
fn a_window_closing_before_the_other_synced_doesnt_take_the_model_with_it() {
    let (device, queue) = gpu_or_skip!();
    let r = Renderer::new(&device, &queue, RendererDesc::default());
    let scene = quad(Mat4::IDENTITY);
    let (mut first, mut second) = (r.attach(&scene), r.attach(&scene));
    r.sync(&mut first);
    drop(first);
    r.sync(&mut second);
    assert_eq!(second.unavailable_meshes(), 0);
    assert!(second.gpu_bytes() > 0);
    assert_eq!(scene.read().cpu_bytes(), 0, "freed once the remaining reader has it");
}

#[test]
fn transparent_backgrounds_stay_straight_alpha_at_antialiased_edges() {
    let (device, queue) = gpu_or_skip!();
    let r = Renderer::new(&device, &queue, RendererDesc::default());
    let (scene, mut b) = CpuScene::new();
    let tri = MeshData::new(vec![Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -0.7, 0.0), Vec3::new(-0.6, 1.0, 0.0)]);
    let m = b.add_mesh(tri).unwrap();
    b.add_node(Node::mesh(m, None, Mat4::IDENTITY, None)).unwrap();
    let mut g = r.attach(&scene);
    r.sync(&mut g);
    let mut view = r.create_view(ViewDesc { width: 64, height: 64 });
    let camera = Camera { eye: Vec3::new(0.0, 0.0, 3.0), ..Default::default() };
    // Magenta at alpha 0: any of it leaking into edge colours is easy to spot.
    let opts = RenderOptions { background: [1.0, 0.0, 1.0, 0.0] };
    let mut encoder = device.create_command_encoder(&Default::default());
    r.render(&mut encoder, &g, &mut view, &camera, &opts);
    queue.submit([encoder.finish()]);
    let pixels = view.read_pixels(&device, &queue).unwrap();
    let all = pixels.as_chunks::<4>().0;

    assert_eq!(px(&pixels, 64, 63, 63)[3], 0, "the background is transparent");
    let interior = *all.iter().find(|p| p[3] == 255).unwrap();
    let edges: Vec<[u8; 4]> = all.iter().filter(|p| p[3] > 20 && p[3] < 235).copied().collect();
    assert!(!edges.is_empty(), "MSAA produced partially covered edge pixels");
    for e in &edges {
        // Straight alpha: an edge pixel has the surface's colour, neither tinted by the
        // clear colour nor darkened by its coverage.
        let close = (0..3).all(|c| (e[c] as i32 - interior[c] as i32).abs() < 40);
        assert!(close, "edge {e:?} vs interior {interior:?}");
    }
}

#[test]
fn base_colour_textures_are_sampled_and_freed_after_upload() {
    let (device, queue) = gpu_or_skip!();
    let r = Renderer::new(&device, &queue, RendererDesc { msaa: Msaa::Off, ..Default::default() });
    let (scene, mut b) = CpuScene::new();
    // A full-view quad; left half of the texture red, right half green, with a mip.
    // Four texels a row, so linear filtering at the sample points stays in one colour.
    let mut mesh = MeshData::new(vec![Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0), Vec3::new(1.0, 1.0, 0.0)])
        .with_indices(Indices::U16(vec![0, 1, 2, 2, 1, 3].into()));
    mesh.uvs = Some(vec![Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)].into());
    let m = b.add_mesh(mesh).unwrap();
    let tex = b.reserve_texture().unwrap();
    let mat = b.add_material(Material::clay().with_param("base_color", ParamValue::Vec4(Vec4::ONE)).with_texture(Slot::BASE_COLOR, tex)).unwrap();
    b.add_node(Node::mesh(m, Some(mat), Mat4::IDENTITY, None)).unwrap();
    let mut level0 = Vec::new();
    for _y in 0..2 {
        level0.extend_from_slice(&[255, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255]);
    }
    let mut data = TextureData::rgba8(4, 2, ColorSpace::Srgb, level0);
    data.levels.push(vec![255, 0, 0, 255, 0, 255, 0, 255].into());
    b.set_texture(tex, data).unwrap();

    let mut g = r.attach(&scene);
    while g.needs_sync() {
        r.sync(&mut g);
    }
    assert!(g.texture_errors().is_empty(), "{:?}", g.texture_errors());
    assert_eq!(scene.read().cpu_bytes(), 0, "mesh and texture freed once on the GPU");
    let mut view = r.create_view(ViewDesc { width: 64, height: 64 });
    let camera = Camera { eye: Vec3::new(0.0, 0.0, 2.0), ..Default::default() };
    let pixels = render_once(&r, &device, &queue, &g, &mut view, &camera);
    let (left, right) = (px(&pixels, 64, 16, 32), px(&pixels, 64, 48, 32));
    assert!(left[0] > 3 * left[1].max(10), "left should be red: {left:?}");
    assert!(right[1] > 3 * right[0].max(10), "right should be green: {right:?}");
}
