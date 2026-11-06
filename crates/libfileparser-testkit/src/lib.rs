//! Testing readers from outside (docs/design/libfileparser.md §9): fakes for every
//! hook, a stable text summary of documents, and the conformance suite every
//! [`Reader`](libfileparser_core::Reader) passes.
//!
//! ```ignore
//! use libfileparser_testkit::{Sample, assert_conforms};
//!
//! #[test]
//! fn gltf_conforms() {
//!     let sample = Sample::new("cube.gltf", include_bytes!("cube.gltf"))
//!         .with_neighbour("cube.bin", include_bytes!("cube.bin"));
//!     assert_conforms(&GltfReader, &[sample]);
//! }
//! ```

mod conformance;
mod fakes;
mod summary;

pub use conformance::{Sample, assert_conforms, check};
pub use fakes::{CountingNotify, DenyFiles, MemFiles, QueueSpawn, RecordingFiles, tiny_limits};
pub use summary::{content_summary, summarize};
