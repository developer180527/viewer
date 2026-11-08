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
mod fuzz;
mod summary;

pub use conformance::{Sample, assert_conforms, check};
pub use fuzz::{Failure, Fuzz, Rng, assert_bytes_fuzz_clean, assert_fuzz_clean, corpus_samples, exercise, read};
pub use fakes::{CountingNotify, DenyFiles, MemFiles, QueueSpawn, RecordingFiles, tiny_limits};
pub use summary::{content_summary, summarize};
