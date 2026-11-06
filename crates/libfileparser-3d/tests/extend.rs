//! A format added from outside the crate, with only its public API: an OFF reader
//! (Object File Format) built from `libfileparser_3d::build`, joined to the crate's
//! readers by passing its own list to `-core`, passing the same conformance suite.

use libfileparser_3d::build::{self, SceneDocument};
use libfileparser_core::{Category, Detection, Document, FileSource, ParseContext, ParseError, Reader, ReaderInfo, Result};
use libfileparser_testkit::{Sample, assert_conforms};
use scene_data::glam::{Mat4, Vec3};
use scene_data::{Indices, MeshData, Node};

struct OffReader;

static OFF: ReaderInfo = ReaderInfo::new("off", "OFF model", Category::ThreeD).extensions(&["off"]);

impl Reader for OffReader {
    fn info(&self) -> &ReaderInfo {
        &OFF
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        build::detect_by(&OFF, header.starts_with(b"OFF"), name)
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let text = std::str::from_utf8(src.bytes()).map_err(|_| ParseError::NotThisFormat)?;
        let mut words = text.split_ascii_whitespace();
        if words.next() != Some("OFF") {
            return Err(ParseError::NotThisFormat);
        }
        let mut number = || words.next().and_then(|w| w.parse::<f32>().ok()).ok_or_else(|| ParseError::corrupt("truncated"));
        let (vertices, faces) = (number()? as u64, number()? as u64);
        number()?; // edges
        cx.limits.check(libfileparser_core::Limit::Items, vertices + faces)?;
        let positions = (0..vertices).map(|_| Ok(Vec3::new(number()?, number()?, number()?))).collect::<Result<Vec<_>>>()?;
        let mut indices = Vec::new();
        for _ in 0..faces {
            let corners: Vec<u32> = (0..number()? as u32).map(|_| number().map(|i| i as u32)).collect::<Result<_>>()?;
            for i in 1..corners.len().saturating_sub(1) {
                indices.extend([corners[0], corners[i], corners[i + 1]]);
            }
        }
        let (scene, mut builder) = build::new_scene(cx);
        let mesh = builder.add_mesh(MeshData::new(positions).with_indices(Indices::U32(indices.into()))).map_err(|e| build::scene_error(&e))?;
        builder.add_node(Node::mesh(mesh, None, Mat4::IDENTITY, None)).map_err(|e| build::scene_error(&e))?;
        builder.finish();
        SceneDocument::new("off", "OFF model", scene).build()
    }
}

const SQUARE: &str = "OFF\n4 1 0\n0 0 0\n1 0 0\n1 1 0\n0 1 0\n4 0 1 2 3\n";

#[test]
fn an_outside_reader_conforms_and_joins_the_category() {
    assert_conforms(&OffReader, &[Sample::new("square.off", SQUARE.as_bytes().to_vec()).fits_tiny_limits()]);

    let mut list = libfileparser_3d::readers().to_vec();
    list.push(&OffReader);
    let doc = libfileparser_core::open(&list, &FileSource::from_bytes("square.off", SQUARE.as_bytes().to_vec()), &ParseContext::new()).unwrap();
    assert_eq!(doc.format().reader, "off");
    assert!(doc.metadata().iter().any(|(k, v)| k == "Triangles" && v == "2"), "{:?}", doc.metadata());
}
