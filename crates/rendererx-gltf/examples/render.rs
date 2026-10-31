//! Loads a glTF/GLB and renders it with clay to a PPM image.
//!
//!     cargo run -p rendererx-gltf --example render -- model.glb out.ppm [size]

use std::sync::Arc;
use std::time::Instant;

use rendererx_scene::CpuScene;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(input), Some(output)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: render <model.gltf|glb> <out.ppm> [size]");
        std::process::exit(2);
    };
    let size: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(512);

    let start = Instant::now();
    let file = Arc::new(std::fs::read(input).expect("read model"));
    let (scene, mut builder) = CpuScene::new();
    let loaded = rendererx_gltf::load(file, &rendererx_gltf::resolver_for(input.as_ref()), &mut builder).expect("load");
    let parsed = start.elapsed();
    {
        let read = scene.read();
        let tris: u64 = read.meshes().iter().map(|m| m.info.primitives).sum();
        let borrowed = read.meshes().iter().filter(|m| m.data.as_ref().is_some_and(|d| d.positions.is_borrowed())).count();
        println!(
            "{input}: {} meshes ({borrowed} borrowed), {} nodes, {} materials, {tris} primitives, {} images, parsed in {:.1} ms",
            read.meshes().len(),
            read.nodes().len(),
            read.materials().len(),
            loaded.images.len(),
            parsed.as_secs_f64() * 1000.0
        );
        for w in &loaded.warnings {
            println!("  warning: {w}");
        }
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).expect("adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).expect("device");
    let r = rendererx::Renderer::new(&device, &queue, Default::default());
    let mut gpu_scene = r.attach(&scene);
    while gpu_scene.needs_sync() {
        r.sync(&mut gpu_scene);
    }
    let Some(bounds) = gpu_scene.bounds() else {
        eprintln!("nothing to show");
        return;
    };
    let mut view = r.create_view(rendererx::ViewDesc { width: size, height: size });
    let camera = rendererx::Camera::framing(bounds, 1.0);
    r.prepare(&gpu_scene);
    let mut encoder = device.create_command_encoder(&Default::default());
    r.render(&mut encoder, &gpu_scene, &mut view, &camera, &Default::default());
    queue.submit([encoder.finish()]);
    let pixels = view.read_pixels(&device, &queue).expect("read back");
    println!("  total {:.1} ms, GPU {} KB", start.elapsed().as_secs_f64() * 1000.0, gpu_scene.gpu_bytes() / 1024);

    let mut ppm = format!("P6 {size} {size} 255\n").into_bytes();
    pixels.as_chunks::<4>().0.iter().for_each(|p| ppm.extend_from_slice(&p[..3]));
    std::fs::write(output, ppm).expect("write image");
}
