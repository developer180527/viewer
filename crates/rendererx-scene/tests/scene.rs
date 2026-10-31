use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rendererx_scene::glam::{Mat4, Vec3};
use rendererx_scene::*;

fn triangle() -> MeshData {
    MeshData::new(vec![Vec3::ZERO, Vec3::X, Vec3::Y])
}

fn quad_indexed() -> MeshData {
    MeshData::new(vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::ONE]).with_indices(Indices::U16(vec![0, 1, 2, 2, 1, 3].into()))
}

#[test]
fn stream_borrows_aligned_bytes_and_copies_misaligned() {
    let floats: Vec<f32> = vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    let mut raw = vec![0u8; 4];
    raw.extend_from_slice(bytemuck::cast_slice(&floats));
    let bytes: Bytes = Arc::new(raw);

    let aligned = Stream::<Vec3>::from_bytes(bytes.clone(), 4, 3).unwrap();
    assert!(aligned.is_borrowed(), "heap buffers are at least 4-aligned");
    assert_eq!(aligned.as_slice()[1], Vec3::X);

    let mut shifted = vec![0u8; 5];
    shifted.extend_from_slice(bytemuck::cast_slice(&floats));
    let misaligned = Stream::<Vec3>::from_bytes(Arc::new(shifted), 5, 3).unwrap();
    assert!(!misaligned.is_borrowed());
    assert_eq!(misaligned.as_slice()[2], Vec3::Y);

    assert!(matches!(Stream::<Vec3>::from_bytes(bytes.clone(), 4, 4), Err(Error::ByteRange { .. })));
    assert!(matches!(Stream::<Vec3>::from_bytes(bytes, usize::MAX, 2), Err(Error::ByteRange { .. })));
}

#[test]
fn mesh_validation_rejects_what_the_gpu_would_trust() {
    let limits = SceneLimits::default();
    let info = quad_indexed().validate(&limits).unwrap();
    assert_eq!((info.vertices, info.primitives), (4, 2));
    assert_eq!(info.bounds, Aabb::new(Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0)));

    let bad_index = triangle().with_indices(Indices::U32(vec![0, 1, 3].into()));
    assert_eq!(bad_index.validate(&limits), Err(Error::IndexOutOfRange { index: 3, vertices: 3 }));

    let partial = triangle().with_indices(Indices::U16(vec![0, 1].into()));
    assert!(matches!(partial.validate(&limits), Err(Error::PartialPrimitive { count: 2, .. })));

    let mut short_normals = triangle();
    short_normals.normals = Some(vec![Vec3::Z].into());
    assert!(matches!(short_normals.validate(&limits), Err(Error::StreamLength { stream: "normals", .. })));

    // NaN positions don't poison the bounds.
    let nan = MeshData::new(vec![Vec3::ZERO, Vec3::NAN, Vec3::ONE]);
    assert_eq!(nan.validate(&limits).unwrap().bounds, Aabb::new(Vec3::ZERO, Vec3::ONE));
}

#[test]
fn limits_are_enforced_and_leave_the_scene_untouched() {
    let limits = SceneLimits { max_triangles: 2, max_drawn_triangles: 3, max_node_depth: 2, ..Default::default() };
    let (scene, mut b) = CpuScene::with_limits(limits);
    let quad = b.add_mesh(quad_indexed()).unwrap();
    assert_eq!(b.add_mesh(triangle()), Err(Error::LimitExceeded { what: "triangles", limit: 2 }));

    // One mesh placed many times is capped by drawn triangles.
    b.add_node(Node::mesh(quad, None, Mat4::IDENTITY, None)).unwrap();
    let err = b.add_node(Node::mesh(quad, None, Mat4::IDENTITY, None));
    assert_eq!(err, Err(Error::LimitExceeded { what: "drawn triangles", limit: 3 }));

    let root = NodeId(0);
    let child = b.add_node(Node::group(Mat4::IDENTITY, Some(root))).unwrap();
    assert!(matches!(b.add_node(Node::group(Mat4::IDENTITY, Some(child))), Err(Error::LimitExceeded { what: "node depth", .. })));
    assert!(matches!(b.add_node(Node::group(Mat4::IDENTITY, Some(NodeId(99)))), Err(Error::UnknownId { kind: "node", .. })));
    assert!(matches!(b.add_node(Node::mesh(MeshId(7), None, Mat4::IDENTITY, None)), Err(Error::UnknownId { kind: "mesh", .. })));
    assert!(matches!(b.add_node(Node::group(Mat4::from_scale(Vec3::NAN), None)), Err(Error::BadNode(_))));

    let read = scene.read();
    assert_eq!((read.meshes().len(), read.nodes().len()), (1, 2));
}

#[test]
fn scene_bytes_limit_counts_textures_too() {
    let limits = SceneLimits { max_bytes: 100, ..Default::default() };
    let (_scene, mut b) = CpuScene::with_limits(limits);
    let t = b.reserve_texture().unwrap();
    let big = TextureData::rgba8(8, 8, ColorSpace::Srgb, vec![0u8; 256]);
    assert_eq!(b.set_texture(t, big), Err(Error::LimitExceeded { what: "scene bytes", limit: 100 }));
}

#[test]
fn readers_follow_changes_by_revision() {
    let (scene, mut b) = CpuScene::new();
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    scene.set_on_change(move || {
        w.fetch_add(1, Ordering::Relaxed);
    });

    let m = b.add_mesh(triangle()).unwrap();
    let seen = scene.revision();
    let mat = b.add_material(Material::clay()).unwrap();
    b.add_node(Node::mesh(m, Some(mat), Mat4::from_translation(Vec3::X), None)).unwrap();

    let read = scene.read();
    assert_eq!(read.revision(), scene.revision());
    let fresh: Vec<_> = read.meshes().iter().filter(|e| e.changed > seen).collect();
    assert!(fresh.is_empty(), "the mesh was already seen");
    assert!(read.materials()[0].changed > seen && read.nodes()[0].changed > seen);
    assert_eq!(wakes.load(Ordering::Relaxed), 3);

    // Failed changes don't bump the revision or wake anyone.
    drop(read);
    let before = scene.revision();
    assert!(b.add_mesh(triangle().with_indices(Indices::U16(vec![9, 9, 9].into()))).is_err());
    assert_eq!((scene.revision(), wakes.load(Ordering::Relaxed)), (before, 3));
}

#[test]
fn bounds_come_from_the_loader_or_the_placed_meshes() {
    let (scene, mut b) = CpuScene::new();
    assert_eq!(scene.read().bounds(), None);
    let m = b.add_mesh(triangle()).unwrap();
    let parent = b.add_node(Node::group(Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)), None)).unwrap();
    b.add_node(Node::mesh(m, None, Mat4::from_scale(Vec3::splat(2.0)), Some(parent))).unwrap();
    assert_eq!(scene.read().bounds(), Some(Aabb::new(Vec3::new(10.0, 0.0, 0.0), Vec3::new(12.0, 2.0, 0.0))));

    // The loader's early bounds win while loading, so the camera doesn't jump…
    b.set_bounds(Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0))).unwrap();
    assert_eq!(scene.read().bounds(), Some(Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0))));
    // …but once geometry is done the measured bounds replace a wrong file hint.
    b.finish();
    assert_eq!(scene.read().bounds(), Some(Aabb::new(Vec3::new(10.0, 0.0, 0.0), Vec3::new(12.0, 2.0, 0.0))));
}

#[test]
fn geometry_finishes_but_textures_keep_coming() {
    let (scene, mut b) = CpuScene::new();
    let tex = b.reserve_texture().unwrap();
    let mat = b.add_material(Material::clay().with_texture(Slot::BASE_COLOR, tex)).unwrap();
    assert!(matches!(b.add_material(Material::clay().with_texture(Slot::BASE_COLOR, TextureId(5))), Err(Error::UnknownId { .. })));
    b.finish();
    assert_eq!(scene.read().geometry(), GeometryState::Done);
    assert_eq!(b.add_mesh(triangle()), Err(Error::GeometryFinished));

    let sink = b.texture_sink();
    drop(b);
    assert_eq!(scene.read().geometry(), GeometryState::Done, "dropping after finish isn't a failure");
    let decoder = std::thread::spawn(move || sink.set_texture(tex, TextureData::rgba8(2, 2, ColorSpace::Srgb, vec![255u8; 16])));
    decoder.join().unwrap().unwrap();
    let read = scene.read();
    assert_eq!(read.texture(tex).unwrap().info.unwrap().width, 2);
    assert_eq!(read.material(mat).unwrap().material.textures[0].1, tex);
}

#[test]
fn a_builder_dropped_early_marks_the_scene_incomplete() {
    let (scene, mut b) = CpuScene::new();
    b.add_mesh(triangle()).unwrap();
    drop(b);
    let read = scene.read();
    assert_eq!(read.geometry(), GeometryState::Incomplete);
    assert_eq!(read.meshes().len(), 1);
}

#[test]
fn cpu_data_is_released_after_upload_unless_kept() {
    let (scene, mut b) = CpuScene::new();
    let m = b.add_mesh(quad_indexed()).unwrap();
    let held = scene.read().cpu_bytes();
    assert_eq!(held, 4 * 12 + 4 * 12 + 6 * 2, "positions, generated normals, u16 indices");

    scene.set_keep_cpu_data(true);
    assert!(!scene.release_mesh_data(m));
    scene.set_keep_cpu_data(false);
    assert!(scene.release_mesh_data(m));
    assert!(!scene.release_mesh_data(m));

    let read = scene.read();
    assert_eq!(read.cpu_bytes(), 0);
    assert_eq!(read.mesh(m).unwrap().info.primitives, 2, "the summary survives");
}

#[test]
fn shared_scenes_free_data_only_after_every_reader_uploaded() {
    let (scene, mut b) = CpuScene::new();
    let m = b.add_mesh(quad_indexed()).unwrap();
    let (first, second) = (scene.add_reader(), scene.add_reader());
    assert!(!scene.release_mesh_data(m), "readers release through their guards");
    assert!(!first.release_mesh_data(m), "one of two readers uploaded: keep it");
    assert!(!first.release_mesh_data(m), "the same reader again doesn't count twice");
    assert!(scene.read().mesh(m).unwrap().data.is_some());
    assert!(second.release_mesh_data(m), "both uploaded: free it");
    drop((first, second));

    // A reader that goes away without uploading doesn't keep the data alive forever.
    let m = b.add_mesh(quad_indexed()).unwrap();
    let (first, second) = (scene.add_reader(), scene.add_reader());
    assert!(!first.release_mesh_data(m));
    drop(second);
    assert!(scene.read().mesh(m).unwrap().data.is_none());
    drop(first);
}

#[test]
fn a_reader_that_leaves_after_uploading_doesnt_free_data_others_need() {
    let (scene, mut b) = CpuScene::new();
    let m = b.add_mesh(quad_indexed()).unwrap();
    let (first, second) = (scene.add_reader(), scene.add_reader());
    assert!(!first.release_mesh_data(m));
    // The window that uploaded closes before the other one synced.
    drop(first);
    assert!(scene.read().mesh(m).unwrap().data.is_some(), "the second reader hasn't uploaded it yet");
    assert!(second.release_mesh_data(m));
    drop(second);

    // A dropped reader's upload can't stand in for a reader that joins later.
    let m = b.add_mesh(quad_indexed()).unwrap();
    let (a, b_) = (scene.add_reader(), scene.add_reader());
    a.release_mesh_data(m);
    drop(a);
    let c = scene.add_reader();
    assert!(!b_.release_mesh_data(m), "c still needs it");
    assert!(c.release_mesh_data(m));
}

#[test]
fn normals_are_generated_when_a_mesh_is_added() {
    let (scene, mut b) = CpuScene::new();
    let m = b.add_mesh(quad_indexed()).unwrap();
    let read = scene.read();
    let data = read.mesh(m).unwrap().data.as_ref().unwrap();
    assert_eq!(data.normals.as_ref().unwrap().len(), 4);
    assert!(matches!(data.indices, Indices::U16(_)), "index width kept");
}

#[test]
fn texture_levels_and_compressed_sizes_are_checked() {
    let limits = SceneLimits::default();
    let mut t = TextureData::rgba8(4, 2, ColorSpace::Linear, vec![0u8; 32]);
    t.levels.push(vec![0u8; 8].into()); // 2×1
    t.levels.push(vec![0u8; 4].into()); // 1×1
    assert_eq!(t.validate(&limits).unwrap().levels, 3);
    t.levels.push(vec![0u8; 4].into());
    assert_eq!(t.validate(&limits), Err(Error::BadTexture("level count")));

    // BC1: 4×4 blocks of 8 bytes. Level 0 must be whole blocks; smaller mips round up.
    let bc1 = |w, h, levels: Vec<usize>| TextureData {
        width: w,
        height: h,
        format: TextureFormat::Bc1,
        color_space: ColorSpace::Srgb,
        levels: levels.into_iter().map(|n| vec![0u8; n].into()).collect(),
    };
    assert!(bc1(8, 4, vec![16, 8, 8, 8]).validate(&limits).is_ok());
    assert!(matches!(bc1(5, 5, vec![32]).validate(&limits), Err(Error::BadTexture(_))), "wgpu would reject 5×5 BC1");

    let huge = TextureData::rgba8(1 << 15, 1, ColorSpace::Srgb, Vec::<u8>::new());
    assert!(matches!(huge.validate(&limits), Err(Error::LimitExceeded { what: "texture side", .. })));
    assert_eq!(TextureData::rgba8(0, 4, ColorSpace::Srgb, Vec::<u8>::new()).validate(&limits), Err(Error::BadTexture("zero size")));
}

/// A cheap stand-in for cargo-fuzz: random meshes, nodes and limits must never panic,
/// and accepted totals must stay within the limits.
#[test]
fn random_input_never_panics_or_exceeds_limits() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n.max(1)
    };
    for _ in 0..300 {
        let limits = SceneLimits {
            max_triangles: next(200),
            max_drawn_triangles: next(400),
            max_vertices: next(300),
            max_node_depth: next(6) as u32,
            max_nodes: next(40) as u32,
            ..Default::default()
        };
        let (scene, mut b) = CpuScene::with_limits(limits);
        for _ in 0..next(30) {
            let verts = next(40) as usize;
            let positions: Vec<Vec3> = (0..verts).map(|_| Vec3::new(next(10) as f32, f32::from_bits(next(u32::MAX as u64) as u32), 0.0)).collect();
            let indices: Vec<u32> = (0..next(60)).map(|_| next(verts as u64 + 3) as u32).collect();
            let mesh = MeshData::new(positions).with_indices(if next(2) == 0 { Indices::None } else { Indices::U32(indices.into()) });
            let _ = b.add_mesh(mesh);
            let parent = (next(3) > 0).then(|| NodeId(next(50) as u32));
            let _ = b.add_node(Node::mesh(MeshId(next(10) as u32), None, Mat4::IDENTITY, parent));
            let _ = b.add_node(Node::group(Mat4::IDENTITY, parent));
        }
        let read = scene.read();
        let stored: u64 = read.meshes().iter().map(|m| m.info.primitives).sum();
        let drawn: u64 = read.nodes().iter().filter_map(|n| n.node.mesh).map(|m| read.mesh(m).unwrap().info.primitives).sum();
        assert!(stored <= limits.max_triangles && drawn <= limits.max_drawn_triangles);
        assert!(read.nodes().iter().all(|n| n.depth <= limits.max_node_depth));
        let _ = read.bounds();
    }
}
