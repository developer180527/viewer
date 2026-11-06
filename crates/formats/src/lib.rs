//! Every format loader in one registry. The app opens files through it, and the test
//! corpus (`crates/corpus`) checks it, so the two can never disagree on what a file
//! opens as.

mod libfileparser;

use std::sync::OnceLock;

pub use libfileparser::{ReaderLoader, set_notify};
use viewer_core::{FormatLoader, Registry};

/// Every loader compiled into this build. Order breaks ties between equal confidences.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        // libfileparser categories, adapted once for the life of the process.
        let three_d: &'static [&'static dyn FormatLoader] = Box::leak(
            libfileparser_3d::readers().iter().map(|r| &*Box::leak(Box::new(ReaderLoader(*r))) as &dyn FormatLoader).collect::<Vec<_>>().into_boxed_slice(),
        );
        Registry::new(&[
            viewer_fmt_image::LOADERS,
            viewer_fmt_archive::LOADERS,
            viewer_fmt_data::LOADERS,
            viewer_fmt_doc::LOADERS,
            three_d,
            viewer_fmt_pdf::LOADERS,
        ])
    })
}
