//! `.blend`: Blender's own save format, read by `blend-lite` (our own crate: on the
//! real-world corpus the `blend` crate panicked on ordinary files of several versions).
//! Meshes, objects and viewport colours become the Scene, as saved (modifiers aren't
//! applied); the preview saved in the file is a facet, and what's shown when the file
//! has nothing to draw. Blocks are listed by kind and name, packed files opened.

use std::sync::Arc;

use blend_lite::{File, Limits};
use libfileparser_core::{Category, Confidence, Content, Detection, Document, FacetInfo, FileSource, FormatInfo, Limit, Model, ParseContext, ParseError, Raster, Reader, ReaderInfo, Result, Role};
use scene_data::glam::{Mat4, Vec3};
use scene_data::{Indices, Material, MeshData, Node, ParamValue, ShadingId, ShadingSource};

use crate::build::{SceneDocument, new_scene};

mod blocks;

pub struct BlendReader;

static BLEND: ReaderInfo = ReaderInfo::new("blend", "Blender file", Category::ThreeD).extensions(&["blend"]);

fn limits(cx: &ParseContext) -> Limits {
    let l = &cx.limits;
    Limits::default().with_inflated(l.allocation).with_unpack(l.unpack_ratio, l.unpack_always).with_blocks(l.nodes).with_preview_pixels(l.image_pixels)
}

fn error(e: blend_lite::Error) -> ParseError {
    match e {
        blend_lite::Error::TooLarge { what: "blocks", value, max } => ParseError::too_large(Limit::Nodes, value, max),
        blend_lite::Error::TooLarge { what: "preview pixels", value, max } => ParseError::too_large(Limit::ImagePixels, value, max),
        blend_lite::Error::TooLarge { value, max, .. } => ParseError::too_large(Limit::Unpacked, value, max),
        blend_lite::Error::NotBlend => ParseError::NotThisFormat,
        e => ParseError::corrupt(e),
    }
}

/// A material slot's vertices so far, the map from the mesh's, and its triangles.
type SlotPart = (u16, Vec<Vec3>, Vec<u32>, Vec<u32>);

/// Polygons as triangles, one mesh per material slot in use, each with only its vertices.
fn parts(mesh: &blend_lite::Mesh) -> Vec<(u16, MeshData)> {
    let mut by_slot: Vec<SlotPart> = Vec::new();
    let mut at = 0usize;
    for (face, &n) in mesh.face_sizes.iter().enumerate() {
        let corners = &mesh.corners[at..at + n as usize];
        at += n as usize;
        let slot = mesh.face_slots.get(face).copied().unwrap_or(0);
        let i = by_slot.iter().position(|p| p.0 == slot).unwrap_or_else(|| {
            by_slot.push((slot, Vec::new(), vec![u32::MAX; mesh.positions.len()], Vec::new()));
            by_slot.len() - 1
        });
        let (_, positions, remap, indices) = &mut by_slot[i];
        let mut vertex = |c: u32| {
            let r = &mut remap[c as usize];
            if *r == u32::MAX {
                *r = positions.len() as u32;
                positions.push(Vec3::from(mesh.positions[c as usize]));
            }
            *r
        };
        if let Some((&first, rest)) = corners.split_first() {
            let first = vertex(first);
            for w in rest.windows(2) {
                indices.extend([first, vertex(w[0]), vertex(w[1])]);
            }
        }
    }
    by_slot.into_iter().map(|(slot, positions, _, indices)| (slot, MeshData::new(positions).with_indices(Indices::U32(indices.into())))).collect()
}

/// The Scene and warnings from what the file holds to draw.
fn build(scene: blend_lite::Scene, cx: &ParseContext) -> Result<(scene_data::CpuScene, Vec<String>)> {
    let (cpu, mut builder) = new_scene(cx);
    let mut warnings = scene.warnings;
    let mut materials = Vec::with_capacity(scene.materials.len());
    for m in &scene.materials {
        let mut out = Material::new(ShadingId::PBR, ShadingSource::FromFile);
        out.name = m.name.clone();
        out.params.set("base_color", ParamValue::Vec4(Vec3::from(m.color).extend(1.0)));
        out.params.set("metallic", ParamValue::Float(0.0));
        out.params.set("roughness", ParamValue::Float(0.5));
        materials.push(builder.add_material(out).map_err(|e| crate::build::scene_error(&e))?);
    }
    // Z up, as Blender has it, to Y up.
    let up = Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    let mut placed: Vec<Option<Vec<_>>> = scene.meshes.iter().map(|_| None).collect();
    for object in &scene.objects {
        cx.cancel.check()?;
        let mesh = &scene.meshes[object.mesh];
        if placed[object.mesh].is_none() {
            let mut ids = Vec::new();
            for (slot, data) in parts(mesh) {
                match builder.add_mesh(data) {
                    Ok(id) => ids.push((id, mesh.slots.get(slot as usize).copied().flatten().and_then(|m| materials.get(m).copied()))),
                    Err(e @ scene_data::Error::LimitExceeded { .. }) => return Err(crate::build::scene_error(&e)),
                    Err(e) => warnings.push(format!("mesh {}: {e}", mesh.name)),
                }
            }
            placed[object.mesh] = Some(ids);
        }
        let matrix = Mat4::from_cols_array(&object.matrix);
        let transform = up * if matrix.is_finite() { matrix } else { Mat4::IDENTITY };
        for &(id, material) in placed[object.mesh].iter().flatten() {
            builder.add_node(Node::mesh(id, material, transform, None)).map_err(|e| crate::build::scene_error(&e))?;
        }
    }
    builder.finish();
    Ok((cpu, warnings))
}

/// The facets every `.blend` has, read from its uncompressed bytes.
fn add_facets(mut doc: Document, inner: &FileSource, file: &File<'_>) -> Document {
    let from = inner.clone();
    doc = doc.with_lazy_facet(FacetInfo::new(Role::Structure, "Blocks", Model::Tree), move |cx| {
        let file = File::read(from.bytes(), &limits(cx)).map_err(error)?;
        Ok(Content::Tree(Arc::new(blocks::tree(&file))))
    });
    let packed = blocks::packed(file);
    if !packed.is_empty() {
        let count = packed.len();
        doc = doc.with_facet(Role::Package, "Packed files", Content::Container(Arc::new(libfileparser_core::FileParts::new(inner, packed)))).with_meta("Packed files", count);
    }
    if file.dna.is_none() {
        doc = doc.with_meta("Warning", "no readable SDNA: blocks are listed by code only");
    }
    doc
}

impl Reader for BlendReader {
    fn info(&self) -> &ReaderInfo {
        &BLEND
    }

    /// A compressed `.blend` starts like any gzip or zstd file: its name makes it ours,
    /// and it must outrank the gzip reader.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if header.starts_with(b"BLENDER") {
            Detection::new(Confidence::Magic)
        } else if blend_lite::is_compressed(header) && BLEND.matches_extension(name) {
            Detection::new(Confidence::Certain)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let l = limits(cx);
        let inner = match blend_lite::inflate(src.bytes(), &l).map_err(error)? {
            Some(bytes) => FileSource::from_bytes(src.name().to_string(), bytes),
            None => src.clone(),
        };
        let file = File::read(inner.bytes(), &l).map_err(error)?;
        let version = file.header.version.clone();
        let preview = file.preview().map_err(error)?.and_then(|p| Raster::new(p.width, p.height, p.rgba).ok());
        let (scene, warnings) = build(blend_lite::scene(&file), cx)?;
        let drawable = scene.read().bounds().is_some();
        // Nothing to draw, but a picture of it: that's the most this file can show.
        if let (false, Some(raster)) = (drawable, preview.clone()) {
            let mut doc = Document::new(FormatInfo::new("blend", "Blender file", Category::ThreeD))
                .with_facet(Role::Primary, "Preview", Content::Raster(raster))
                .with_meta("Saved by", format!("Blender {version}"))
                .with_meta("Shown", "the preview image saved in the file: it has no mesh to draw");
            doc = add_facets(doc, &inner, &file);
            for w in warnings.iter().take(20) {
                doc = doc.with_meta("Warning", w);
            }
            return Ok(doc.with_file_facets(src, None));
        }
        let (facets_inner, has_dna) = (inner.clone(), file.dna.is_some());
        let doc = SceneDocument::new(self, src, "Blender file", scene)
            .warnings(warnings)
            .nothing_to_draw_ok(has_dna)
            .facets(move |doc| {
                let doc = match preview {
                    Some(raster) => doc.with_facet(Role::Inspector, "Preview", Content::Raster(raster)),
                    None => doc,
                };
                match File::read(facets_inner.bytes(), &Limits::default()) {
                    Ok(file) => add_facets(doc, &facets_inner, &file),
                    Err(_) => doc,
                }
            })
            .build()?;
        Ok(doc.with_meta("Saved by", format!("Blender {version}")))
    }
}
