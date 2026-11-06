//! glTF 2.0, GLB and VRM, read by `gltf-lite` straight from the mapped file. Buffers
//! and images outside the file come through the files hook; embedded images are
//! decoded in the background like any other texture.

use libfileparser_core::{Category, Dependencies, DependencyStatus, Detection, Document, FileSource, ParseContext, Reader, ReaderInfo};

use crate::build::{ImageJob, ImageSource, SceneDocument, decode_textures, detect_by};
use crate::textures;

pub struct GltfReader;

static GLTF: ReaderInfo = ReaderInfo::new("gltf", "glTF model", Category::ThreeD)
    .extensions(&["gltf", "glb", "vrm"])
    .mime_types(&["model/gltf+json", "model/gltf-binary"]);

impl Reader for GltfReader {
    fn info(&self) -> &ReaderInfo {
        &GLTF
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        detect_by(&GLTF, header.starts_with(b"glTF"), name).with_language("json")
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> libfileparser_core::Result<Document> {
        cx.cancel.check()?;
        let deps = Dependencies::new();
        let (scene, mut builder) = crate::build::new_scene(cx);
        let files = GltfFiles { from: src, cx, deps: &deps };
        let loaded = gltf_lite::load(src.shared_bytes(), &files, &mut builder).map_err(|e| super::parse_error(e.into()))?;
        let format = if src.header().starts_with(b"glTF") { "glTF binary (GLB)" } else { "glTF 2.0" };
        let jobs: Vec<_> = loaded.images.into_iter().map(|i| ImageJob::new(i.texture, ImageSource::Bytes(i.data), i.color_space)).collect();
        let maps = jobs.len();
        let pending = decode_textures(builder.texture_sink(), jobs, cx);
        SceneDocument::new("gltf", format, scene).warnings(loaded.warnings).textures(pending, maps).dependencies(deps).build()
    }
}

/// glTF's buffers and images, read through the files hook and recorded under their
/// file name. Paths that climb out of the model's folder are refused.
struct GltfFiles<'a> {
    from: &'a FileSource,
    cx: &'a ParseContext,
    deps: &'a Dependencies,
}

impl gltf_lite::Resolver for GltfFiles<'_> {
    fn resolve(&self, uri: &str) -> std::result::Result<scene_data::Bytes, gltf_lite::ResolveError> {
        use gltf_lite::{ResolveError, ResolveErrorKind};
        let path = gltf_lite::relative_path(uri)?;
        let name = textures::report_name(&path, "");
        match self.deps.resolve_any(self.cx, self.from, &name, &[path.as_str()]) {
            Some(file) if file.len() > self.cx.limits.allocation => {
                Err(ResolveError::new(ResolveErrorKind::Refused, format!("{uri:?}: {} bytes is over the limit", file.len())))
            }
            Some(file) => Ok(file.shared_bytes()),
            None => {
                let blocked = self.deps.with_status(DependencyStatus::Blocked).contains(&name);
                let kind = if blocked { ResolveErrorKind::NotPermitted } else { ResolveErrorKind::NotFound };
                Err(ResolveError::new(kind, format!("{uri:?} couldn't be read")))
            }
        }
    }
}
