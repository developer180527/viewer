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
pub mod pdf;
mod summary;

pub use conformance::{Sample, assert_conforms, check};
pub use fuzz::{Failure, Fuzz, Rng, assert_bytes_fuzz_clean, assert_fuzz_clean, corpus_samples, exercise, read};
pub use fakes::{CountingNotify, DenyFiles, MemFiles, QueueSpawn, RecordingFiles, tiny_limits};
pub use summary::{content_summary, summarize};

/// With `LFP_TESTED` set to a file, each conformance check and fuzz run appends
/// `<suite>\t<reader id>\t<passed>` to it: the evidence `scripts/formats.sh` builds
/// docs/formats.md's ticks from. Off otherwise.
pub(crate) fn record(suite: &str, id: &str, passed: bool) {
    let Some(path) = std::env::var_os("LFP_TESTED") else { return };
    use std::io::Write;
    // One write a record: tests run in parallel, and an append this small is atomic.
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{suite}\t{id}\t{passed}\n").as_bytes());
    }
}

