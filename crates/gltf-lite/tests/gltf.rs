use std::sync::Arc;

use base64::Engine;
use gltf_lite::{Error, NoResolver, Resolver, load, resolver_for};
use scene_data::glam::Vec3;
use scene_data::*;
use serde_json::json;

/// A GLB container: JSON chunk, then BIN chunk, both padded to 4 bytes.
fn glb(json: serde_json::Value, bin: &[u8]) -> Bytes {
    let mut j = serde_json::to_vec(&json).unwrap();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let mut b = bin.to_vec();
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
    let total = 12 + 8 + j.len() + 8 + b.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(j.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    out.extend_from_slice(&j);
    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    out.extend_from_slice(&b);
    Arc::new(out)
}

fn text(json: serde_json::Value) -> Bytes {
    Arc::new(serde_json::to_vec(&json).unwrap())
}

const CUBE_POSITIONS: [[f32; 3]; 8] = [
    [-0.5, -0.5, -0.5], [0.5, -0.5, -0.5], [-0.5, 0.5, -0.5], [0.5, 0.5, -0.5],
    [-0.5, -0.5, 0.5], [0.5, -0.5, 0.5], [-0.5, 0.5, 0.5], [0.5, 0.5, 0.5],
];
#[rustfmt::skip]
const CUBE_INDICES: [u16; 36] = [
    0,2,1, 1,2,3,  4,5,6, 5,7,6,  0,1,4, 1,5,4,
    2,6,3, 3,6,7,  0,4,2, 2,4,6,  1,3,5, 3,7,5,
];

/// A cube mesh in a GLB, placed by `nodes` with `material`.
fn cube_glb(nodes: serde_json::Value, scene_nodes: serde_json::Value, material: serde_json::Value) -> Bytes {
    let mut bin = bytemuck::cast_slice::<_, u8>(&CUBE_POSITIONS).to_vec();
    bin.extend_from_slice(bytemuck::cast_slice(&CUBE_INDICES));
    // Corner normals, so the streams stay borrowed (a mesh without normals gets them
    // generated, which splits the cube's hard edges into copied streams).
    let normals: Vec<[f32; 3]> = CUBE_POSITIONS.iter().map(|p| Vec3::from(*p).normalize().to_array()).collect();
    bin.extend_from_slice(bytemuck::cast_slice(&normals));
    glb(
        json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": scene_nodes}],
            "nodes": nodes,
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 2}, "indices": 1, "material": 0}]}],
            "materials": [material],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 8, "type": "VEC3", "min": [-0.5, -0.5, -0.5], "max": [0.5, 0.5, 0.5]},
                {"bufferView": 1, "componentType": 5123, "count": 36, "type": "SCALAR"},
                {"bufferView": 2, "componentType": 5126, "count": 8, "type": "VEC3"}
            ],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 96},
                {"buffer": 0, "byteOffset": 96, "byteLength": 72},
                {"buffer": 0, "byteOffset": 168, "byteLength": 96}
            ],
            "buffers": [{"byteLength": 264}]
        }),
        &bin,
    )
}

#[test]
fn glb_streams_are_borrowed_from_the_file_and_bounds_come_first() {
    let file = cube_glb(
        json!([{"translation": [10.0, 0.0, 0.0], "children": [1]}, {"mesh": 0, "scale": [2.0, 2.0, 2.0]}]),
        json!([0]),
        json!({"name": "flat", "extensions": {"KHR_materials_unlit": {}}, "pbrMetallicRoughness": {"baseColorFactor": [1.0, 0.0, 0.0, 1.0]}}),
    );
    let (scene, mut b) = CpuScene::new();
    let loaded = load(file, &NoResolver, &mut b).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);

    let read = scene.read();
    assert_eq!(read.geometry(), GeometryState::Done);
    let mesh = read.mesh(MeshId(0)).unwrap().data.as_ref().unwrap();
    assert!(mesh.positions.is_borrowed(), "positions point into the GLB");
    assert!(mesh.normals.as_ref().is_some_and(|n| n.is_borrowed()));
    assert!(matches!(&mesh.indices, Indices::U16(s) if s.is_borrowed()));
    assert_eq!(read.bounds(), Some(Aabb::new(Vec3::new(9.0, -1.0, -1.0), Vec3::new(11.0, 1.0, 1.0))));

    let mat = &read.materials()[0].material;
    assert_eq!((mat.shading.clone(), mat.source, mat.name.as_str()), (ShadingId::UNLIT, ShadingSource::FromFile, "flat"));
    assert_eq!(mat.params.get("base_color"), Some(ParamValue::Vec4([1.0, 0.0, 0.0, 1.0].into())));
    assert_eq!(read.nodes().len(), 2);
    assert_eq!(read.nodes()[1].node.parent, Some(NodeId(0)));
}

#[test]
fn a_mesh_placed_twice_is_stored_once() {
    let file = cube_glb(json!([{"mesh": 0}, {"mesh": 0, "translation": [3.0, 0.0, 0.0]}]), json!([0, 1]), json!({}));
    let (scene, mut b) = CpuScene::new();
    load(file, &NoResolver, &mut b).unwrap();
    let read = scene.read();
    assert_eq!((read.meshes().len(), read.nodes().len()), (1, 2));
    assert_eq!(read.materials()[0].material.shading, ShadingId::PBR);
}

#[test]
fn gltf_text_with_data_uris_strips_quantized_colours_and_images() {
    // Triangle strip of 4 vertices, colours as normalized u8 VEC3 with a stride of 4.
    let positions: [[f32; 3]; 4] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]];
    let mut bin = bytemuck::cast_slice::<_, u8>(&positions).to_vec();
    bin.extend_from_slice(&[255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0, 255, 255, 255, 0]);
    let uri = format!("data:application/octet-stream;base64,{}", base64::engine::general_purpose::STANDARD.encode(&bin));
    let file = text(json!({
        "asset": {"version": "2.0"},
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "COLOR_0": 1}, "mode": 5, "material": 0}]}],
        "materials": [{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}, "normalTexture": {"index": 0, "scale": 0.5}, "alphaMode": "MASK", "doubleSided": true}],
        "textures": [{"source": 0}],
        "images": [{"uri": "albedo%20map.png"}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]},
            {"bufferView": 1, "componentType": 5121, "normalized": true, "count": 4, "type": "VEC3"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 48},
            {"buffer": 0, "byteOffset": 48, "byteLength": 16, "byteStride": 4}
        ],
        "buffers": [{"byteLength": 64, "uri": uri}]
    }));

    struct Images;
    impl Resolver for Images {
        fn resolve(&self, uri: &str) -> std::result::Result<Bytes, gltf_lite::ResolveError> {
            assert_eq!(uri, "albedo%20map.png");
            Ok(Arc::new(b"\x89PNG fake".to_vec()))
        }
    }
    let (scene, mut b) = CpuScene::new();
    let loaded = load(file, &Images, &mut b).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);

    // One image used as colour and as a normal map: two textures, two colour spaces.
    let spaces: Vec<_> = loaded.images.iter().map(|i| i.color_space).collect();
    assert_eq!(spaces, [ColorSpace::Srgb, ColorSpace::Linear]);
    assert_eq!(loaded.images[0].data.as_slice(), b"\x89PNG fake");

    let read = scene.read();
    let mesh = read.mesh(MeshId(0)).unwrap().data.as_ref().unwrap();
    assert_eq!(mesh.topology, Topology::Triangles);
    match &mesh.indices {
        Indices::U32(s) => assert_eq!(s.as_slice(), [0, 1, 2, 2, 1, 3]),
        other => panic!("{other:?}"),
    }
    let colors = mesh.colors.as_ref().unwrap().as_slice();
    assert_eq!(colors[1].to_array(), [0.0, 1.0, 0.0, 1.0]);
    let mat = &read.materials()[0].material;
    assert_eq!(mat.alpha, AlphaMode::Mask(0.5));
    assert!(mat.double_sided);
    assert_eq!(mat.params.get("normal_scale"), Some(ParamValue::Float(0.5)));
    assert_eq!(mat.textures.len(), 2);
}

#[test]
fn hostile_files_fail_cleanly_or_load_what_they_can() {
    // Node cycle: each node is placed once, nothing hangs.
    let file = cube_glb(json!([{"mesh": 0, "children": [1]}, {"children": [0]}]), json!([0]), json!({}));
    let (scene, mut b) = CpuScene::new();
    load(file, &NoResolver, &mut b).unwrap();
    assert_eq!(scene.read().nodes().len(), 2);

    // An accessor past the end of its view: a warning, the rest of the file loads.
    let (_scene, mut b) = CpuScene::new();
    let file = text(json!({
        "asset": {"version": "2.0"},
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 1000000, "type": "VEC3"}],
        "bufferViews": [{"buffer": 0, "byteLength": 12}],
        "buffers": [{"byteLength": 12, "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAA"}]
    }));
    let loaded = load(file, &NoResolver, &mut b).unwrap();
    assert!(loaded.warnings.iter().any(|w| w.contains("runs past")), "{:?}", loaded.warnings);

    // Sparse indices of a signed type (spec: unsigned only), right at the end of the
    // buffer: a warning, not an out-of-bounds read.
    let mut buf: Vec<u8> = bytemuck::cast_slice(&[0f32, 0., 0., 1., 0., 0., 0., 1., 0., 5., 5., 5.]).to_vec();
    buf.push(0);
    let uri = format!("data:application/octet-stream;base64,{}", base64::engine::general_purpose::STANDARD.encode(&buf));
    let file = text(json!({
        "asset": {"version": "2.0"},
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "sparse": {"count": 1, "indices": {"bufferView": 2, "componentType": 5120}, "values": {"bufferView": 1}}}],
        "bufferViews": [{"buffer": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 12}, {"buffer": 0, "byteOffset": 48, "byteLength": 1}],
        "buffers": [{"byteLength": 49, "uri": uri}]
    }));
    let (_s, mut b) = CpuScene::new();
    let loaded = load(file, &NoResolver, &mut b).unwrap();
    assert!(loaded.warnings.iter().any(|w| w.contains("sparse indices")), "{:?}", loaded.warnings);

    // Compressed geometry can't be read: refused up front.
    let draco = text(json!({"asset": {"version": "2.0"}, "extensionsRequired": ["KHR_draco_mesh_compression"]}));
    let (_s, mut b) = CpuScene::new();
    assert_eq!(load(draco, &NoResolver, &mut b).err(), Some(Error::Unsupported("KHR_draco_mesh_compression".into())));

    // Truncated GLB and garbage.
    let (_s, mut b) = CpuScene::new();
    assert!(matches!(load(Arc::new(b"glTF\x02\0\0\0\xff\0\0\0\x10\0\0\0JSON".to_vec()), &NoResolver, &mut b), Err(Error::Format(_))));
    let (_s, mut b) = CpuScene::new();
    assert!(matches!(load(Arc::new(b"\0\x01garbage".to_vec()), &NoResolver, &mut b), Err(Error::Format(_))));
    let (_s, mut b) = CpuScene::new();
    assert!(matches!(load(text(json!({"asset": {"version": "1.0"}})), &NoResolver, &mut b), Err(Error::Unsupported(_))));
}

#[test]
fn vrm0_mtoon_materials_are_toon() {
    let file = cube_glb(json!([{"mesh": 0}]), json!([0]), json!({"name": "Skin"}));
    // Re-wrap with the root VRM 0.x extension naming the material's shader.
    let mut bytes = (*file).as_ref().to_vec();
    let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let mut root: serde_json::Value = serde_json::from_slice(&bytes[20..20 + len]).unwrap();
    root["extensions"] = json!({"VRM": {"materialProperties": [{"name": "Skin", "shader": "VRM/MToon"}]}});
    let bin_at = 20 + len;
    let bin_len = u32::from_le_bytes(bytes[bin_at..bin_at + 4].try_into().unwrap()) as usize;
    let bin = bytes.split_off(bin_at + 8);
    let file = glb(root, &bin[..bin_len]);

    let (scene, mut b) = CpuScene::new();
    load(file, &NoResolver, &mut b).unwrap();
    assert_eq!(scene.read().materials()[0].material.shading, ShadingId::MTOON);
}

#[test]
fn external_files_stay_inside_the_models_folder() {
    let dir = std::env::temp_dir().join(format!("gltf-lite-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a b.bin"), [1u8, 2, 3]).unwrap();
    let r = resolver_for(&dir.join("model.gltf"));
    assert_eq!((*r.resolve("a%20b.bin").unwrap()).as_ref(), [1, 2, 3]);
    for uri in ["../secret.bin", "/etc/passwd", "file:///etc/passwd", "https://example.com/x.bin", "sub/../../x"] {
        assert!(r.resolve(uri).is_err(), "{uri} should be refused");
    }
    #[cfg(unix)]
    {
        // A symlink inside the folder that points out of it.
        std::os::unix::fs::symlink("/etc/hosts", dir.join("link.bin")).unwrap();
        assert!(r.resolve("link.bin").err().unwrap().message.contains("leaves"), "symlinks can't escape");
        // Not a regular file: refused instead of blocking on read.
        std::fs::create_dir(dir.join("sub")).unwrap();
        assert!(r.resolve("sub").err().unwrap().message.contains("not a regular file"));
    }
    // Missing and refused files say which they are, so a host can tell the user.
    assert_eq!(r.resolve("gone.bin").err().unwrap().kind, gltf_lite::ResolveErrorKind::NotFound);
    assert_eq!(r.resolve("../x.bin").err().unwrap().kind, gltf_lite::ResolveErrorKind::Refused);
    let mut capped = resolver_for(&dir.join("model.gltf"));
    capped.max_bytes = 2;
    assert!(capped.resolve("a%20b.bin").err().unwrap().message.contains("limit"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn loaded_model_renders_in_its_material_colour() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();

    let file = cube_glb(json!([{"mesh": 0, "rotation": [0.0, 0.0, 0.0, 1.0]}]), json!([0]), json!({"pbrMetallicRoughness": {"baseColorFactor": [0.8, 0.05, 0.05, 1.0]}}));
    let (scene, mut b) = CpuScene::new();
    load(file, &NoResolver, &mut b).unwrap();

    let r = rendererx::Renderer::new(&device, &queue, Default::default());
    let mut gpu_scene = r.attach(&scene);
    r.sync(&mut gpu_scene);
    let mut view = r.create_view(rendererx::ViewDesc { width: 64, height: 64 });
    let camera = rendererx::Camera::framing(gpu_scene.bounds().unwrap(), 1.0);
    r.prepare(&gpu_scene);
    let mut encoder = device.create_command_encoder(&Default::default());
    r.render(&mut encoder, &gpu_scene, &mut view, &camera, &Default::default());
    queue.submit([encoder.finish()]);
    let pixels = view.read_pixels(&device, &queue).unwrap();
    let c = &pixels[(32 * 64 + 32) * 4..][..4];
    assert!(c[0] > 2 * c[1] && c[0] > 2 * c[2], "centre should be red, got {c:?}");
}

#[cfg(unix)]
#[test]
fn external_files_that_cant_be_read_are_reported_by_kind() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("gltf-lite-blocked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("locked")).unwrap();
    std::fs::write(dir.join("locked/mesh.bin"), [0u8; 36]).unwrap();
    let file = text(json!({
        "asset": {"version": "2.0"},
        "buffers": [{"byteLength": 36, "uri": "locked/mesh.bin"}, {"byteLength": 4, "uri": "absent.bin"}],
        "images": [{"uri": "locked/albedo.png"}],
        "textures": [{"source": 0}],
        "materials": [{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}],
        "bufferViews": [{"buffer": 0, "byteLength": 36}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "material": 0}]}],
        "nodes": [{"mesh": 0}]
    }));
    std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let (_s, mut b) = CpuScene::new();
    let loaded = load(file, &resolver_for(&dir.join("model.gltf")), &mut b);
    std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let loaded = loaded.unwrap();
    use gltf_lite::ResolveErrorKind::*;
    assert!(loaded.unreadable.contains(&("absent.bin".into(), NotFound)), "{:?}", loaded.unreadable);
    // Root (some CI images) can read the locked folder anyway.
    if loaded.unreadable.len() > 1 {
        assert!(loaded.unreadable.contains(&("locked/mesh.bin".into(), NotPermitted)), "{:?}", loaded.unreadable);
    }
    std::fs::remove_dir_all(dir).unwrap();
}
