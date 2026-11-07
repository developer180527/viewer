//! glTF 2.0, GLB and VRM, read by `gltf-lite` straight from the mapped file. Buffers
//! and images outside the file come through the files hook; embedded images are
//! decoded in the background like any other texture.

use libfileparser_core::{Category, Dependencies, DependencyStatus, Detection, Document, FileSource, ParseContext, Reader, ReaderInfo};

use std::sync::Arc;

use libfileparser_core::{Content, FacetInfo, Model, Role};

use crate::build::{ImageJob, ImageSource, SceneDocument, decode_textures, detect_by};
use crate::textures;

mod facets;

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
        let jobs: Vec<_> = loaded
            .images
            .into_iter()
            .map(|i| {
                let job = ImageJob::new(i.texture, ImageSource::Bytes(i.data), i.color_space).named(i.name);
                if i.embedded { job } else { job.origin("file") }
            })
            .collect();
        let textures = decode_textures(builder.texture_sink(), jobs, cx);
        // Scene items by the JSON they came from, for links with the glTF tree.
        let mut origins = crate::build::SceneOrigins::new();
        for &(node, id) in &loaded.nodes {
            origins.node(format!("/nodes/{node}"), id);
        }
        for &(mesh, primitive, id) in &loaded.meshes {
            origins.mesh(format!("/meshes/{mesh}/primitives/{primitive}"), id);
        }
        for &(material, id) in &loaded.materials {
            origins.material(format!("/materials/{material}"), id);
        }
        let glb = src.header().starts_with(b"glTF");
        let extensions = facets::extensions_table(&loaded.extensions_used, &loaded.extensions_required);
        let from = src.clone();
        SceneDocument::new(self, src, format, scene)
            .origins(origins, "glTF")
            .warnings(loaded.warnings)
            .textures(textures)
            .dependencies(deps)
            .facets(move |doc| {
                let tree_src = from.clone();
                let mut doc = doc.with_lazy_facet(FacetInfo::new(Role::Structure, "glTF", Model::Tree), move |_| {
                    Ok(Content::Tree(Arc::new(facets::GltfTree::new(facets::parse(&tree_src)?))))
                });
                if glb {
                    let text_src = from.clone();
                    doc = doc.with_lazy_facet(FacetInfo::new(Role::Source, "JSON chunk", Model::Text), move |_| {
                        Ok(Content::Text(Arc::new(facets::json_text(&text_src)?)))
                    });
                    doc = doc.with_lazy_facet(FacetInfo::new(Role::Package, "Chunks", Model::Container), move |_| {
                        let json = facets::parse(&from).ok();
                        Ok(Content::Container(Arc::new(facets::glb_parts(&from, json.as_ref())?)))
                    });
                }
                if let Some(table) = extensions {
                    doc = doc.with_facet(Role::Inspector, "Extensions used", Content::Table(Arc::new(table)));
                }
                doc
            })
            .build()
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
        let found = self.deps.resolve_any(self.cx, self.from, &name, &[path.as_str()]);
        self.deps.set_kind(&name, if is_image(&path) { "texture" } else { "buffer" });
        match found {
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

/// Whether a glTF URI names an image rather than a buffer; the resolver isn't told.
fn is_image(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "ktx2" | "avif" | "bmp" | "tga" | "gif")
}
