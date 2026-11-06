//! One module per format: its [`Reader`](libfileparser_core::Reader) and the parsing
//! behind it, built from the shared pieces in [`crate::build`]. Adding a format is a
//! module here and one line in [`crate::readers`].

mod blend;
mod fbx;
mod gltf;
mod ply;
mod stl;
mod usd;

pub use blend::BlendReader;
pub use fbx::{FbxReader, ObjReader};
pub use gltf::GltfReader;
pub use ply::PlyReader;
pub use stl::StlReader;
pub use usd::UsdReader;

use libfileparser_core::ParseError;

/// A reader's internal error as a [`ParseError`]: a scene limit becomes `TooLarge`,
/// cancellation stays cancellation, anything else is damage.
fn parse_error(e: anyhow::Error) -> ParseError {
    for cause in e.chain() {
        let scene = cause.downcast_ref::<scene_data::Error>().or(match cause.downcast_ref::<gltf_lite::Error>() {
            Some(gltf_lite::Error::Scene(e)) => Some(e),
            _ => None,
        });
        if let Some(e @ scene_data::Error::LimitExceeded { .. }) = scene {
            return crate::build::scene_error(e);
        }
        if let Some(p) = cause.downcast_ref::<ParseError>()
            && !p.try_next_reader()
        {
            return match p {
                ParseError::TooLarge(l) => ParseError::too_large(l.limit, l.value, l.max),
                ParseError::Cancelled => ParseError::Cancelled,
                other => ParseError::corrupt(other),
            };
        }
    }
    ParseError::corrupt(format!("{e:#}"))
}
