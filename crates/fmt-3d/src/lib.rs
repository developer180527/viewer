//! 3D models. Each loader fills a RendererX CPU scene ([`Content::Scene`]); the app's
//! 3D view uploads and draws it. Textures aren't shown yet: models draw in clay with
//! their material colours.

mod blend;
mod textures;
mod ufbx_scene;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use rendererx_scene::{CpuScene, SceneRead};
use viewer_core::{Confidence, Content, Document, FileSource, FormatLoader, Result, SceneDoc};

pub use blend::BlendLoader;

pub static LOADERS: &[&dyn FormatLoader] = &[&GltfLoader, &FbxLoader, &ObjLoader, &BlendLoader];

/// The document for a loaded scene, with the counts the info panel shows. A scene
/// with nothing to draw is an error, so a file that only looks like a model (a
/// compiler's `.obj`) falls through to the next loader.
fn scene_document(format: &str, scene: CpuScene, warnings: &[String], textures_pending: Arc<AtomicUsize>) -> Result<Document> {
    let mut doc = {
        let read = scene.read();
        let stats = Stats::of(&read);
        if stats.placed == 0 {
            anyhow::bail!("no geometry{}", warnings.first().map(|w| format!(" ({w})")).unwrap_or_default());
        }
        Document::new(format, Content::Scene(SceneDoc { scene: scene.clone(), textures_pending }))
            .with_meta("Meshes", stats.meshes)
            .with_meta("Placed meshes", stats.placed)
            .with_meta("Triangles", stats.triangles)
            .with_meta("Vertices", stats.vertices)
            .with_meta("Materials", stats.materials)
    };
    for w in warnings.iter().take(20) {
        doc = doc.with_meta("Warning", w);
    }
    if warnings.len() > 20 {
        doc = doc.with_meta("Warning", format!("…and {} more", warnings.len() - 20));
    }
    Ok(doc)
}

struct Stats {
    meshes: usize,
    placed: usize,
    triangles: u64,
    vertices: u64,
    materials: usize,
}

impl Stats {
    fn of(read: &SceneRead<'_>) -> Self {
        Self {
            meshes: read.meshes().len(),
            placed: read.nodes().iter().filter(|n| n.node.mesh.is_some()).count(),
            triangles: read.meshes().iter().map(|m| m.info.primitives).sum(),
            vertices: read.meshes().iter().map(|m| m.info.vertices as u64).sum(),
            materials: read.materials().len(),
        }
    }
}

fn texture_note(maps: usize) -> String {
    match maps {
        0 => "none".into(),
        n => format!("{n} map(s), up to {} px", textures::MAX_SIDE),
    }
}

// ---------------------------------------------------------------- glTF

pub struct GltfLoader;

impl FormatLoader for GltfLoader {
    fn id(&self) -> &'static str {
        "gltf"
    }

    fn name(&self) -> &'static str {
        "glTF model"
    }

    fn probe(&self, header: &[u8], ext: Option<&str>, _: &str) -> Confidence {
        if header.starts_with(b"glTF") {
            Confidence::MAGIC
        } else if matches!(ext, Some("gltf" | "glb" | "vrm")) {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let (scene, mut builder) = CpuScene::new();
        let loaded = match src.path() {
            Some(path) => rendererx_gltf::load(src.shared_bytes(), &rendererx_gltf::resolver_for(path), &mut builder),
            // An archive entry: no folder to look in for external files.
            None => rendererx_gltf::load(src.shared_bytes(), &rendererx_gltf::NoResolver, &mut builder),
        }?;
        let format = if src.header().starts_with(b"glTF") { "glTF binary (GLB)" } else { "glTF 2.0" };
        let jobs: Vec<_> = loaded
            .images
            .into_iter()
            .map(|i| textures::ImageJob { texture: i.texture, source: textures::ImageSource::Bytes(i.data), color_space: i.color_space })
            .collect();
        let decoding = jobs.len();
        let pending = textures::spawn(builder.texture_sink(), jobs);
        Ok(scene_document(format, scene, &loaded.warnings, pending)?.with_meta("Textures", texture_note(decoding)))
    }
}

// ---------------------------------------------------------------- FBX / OBJ

pub struct FbxLoader;

impl FormatLoader for FbxLoader {
    fn id(&self) -> &'static str {
        "fbx"
    }

    fn name(&self) -> &'static str {
        "FBX model"
    }

    fn probe(&self, header: &[u8], ext: Option<&str>, _: &str) -> Confidence {
        if header.starts_with(b"Kaydara FBX Binary") {
            Confidence::MAGIC
        } else if ext == Some("fbx") {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let loaded = ufbx_scene::load(src, ufbx::FileFormat::Fbx)?;
        let binary = src.header().starts_with(b"Kaydara FBX Binary");
        let note = texture_note(loaded.textures);
        Ok(scene_document(if binary { "FBX (binary)" } else { "FBX (ASCII)" }, loaded.scene, &loaded.warnings, loaded.pending)?.with_meta("Textures", note))
    }
}

pub struct ObjLoader;

impl FormatLoader for ObjLoader {
    fn id(&self) -> &'static str {
        "obj"
    }

    fn name(&self) -> &'static str {
        "OBJ model"
    }

    fn probe(&self, _: &[u8], ext: Option<&str>, _: &str) -> Confidence {
        // No magic: `.obj` is also a compiler object file, which fails here and falls
        // through to the next loader.
        if ext == Some("obj") { Confidence::EXTENSION } else { Confidence::NONE }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let loaded = ufbx_scene::load(src, ufbx::FileFormat::Obj)?;
        let note = texture_note(loaded.textures);
        Ok(scene_document("Wavefront OBJ", loaded.scene, &loaded.warnings, loaded.pending)?.with_meta("Textures", note))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE_OBJ: &str = "\
o cube
v -1 -1 -1
v  1 -1 -1
v  1  1 -1
v -1  1 -1
v -1 -1  1
v  1 -1  1
v  1  1  1
v -1  1  1
usemtl missing
f 1 4 3 2
f 5 6 7 8
f 1 2 6 5
f 2 3 7 6
f 3 4 8 7
f 4 1 5 8
";

    #[test]
    fn obj_quads_are_triangulated_and_unlit_materials_stay_clay() {
        let src = FileSource::from_bytes("cube.obj", CUBE_OBJ.as_bytes().to_vec());
        let doc = ObjLoader.open(&src).unwrap();
        let Content::Scene(SceneDoc { scene, .. }) = doc.content else { panic!("expected a scene") };
        let read = scene.read();
        assert_eq!(read.meshes().iter().map(|m| m.info.primitives).sum::<u64>(), 12);
        let b = read.bounds().unwrap();
        assert!((b.size() - rendererx_scene::glam::Vec3::splat(2.0)).length() < 1e-4);
        // `usemtl missing` names a material that no .mtl defines: no colour, so clay.
        assert!(read.materials().iter().all(|m| m.material.params.get("base_color").is_none()));
    }

    #[test]
    fn files_with_no_geometry_fall_through() {
        // A compiler object file named .obj: ufbx reads it as an empty OBJ, which must
        // fail so the registry falls back to the next loader (hex).
        let coff = FileSource::from_bytes("main.obj", vec![0x4c, 0x01, 0x03, 0x00, 0, 0, 0, 0, 0xff, 0xfe, 0x00]);
        assert!(ObjLoader.open(&coff).is_err());
    }
}
