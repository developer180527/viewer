//! Every format loader in one registry. The app opens files through it, and the test
//! corpus (`crates/corpus`) checks it, so the two can never disagree on what a file
//! opens as.

use std::sync::OnceLock;

use viewer_core::Registry;

/// Every loader compiled into this build. Order breaks ties between equal confidences.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        Registry::new(&[
            viewer_fmt_image::LOADERS,
            viewer_fmt_archive::LOADERS,
            viewer_fmt_data::LOADERS,
            viewer_fmt_doc::LOADERS,
            viewer_fmt_3d::LOADERS,
            viewer_fmt_pdf::LOADERS,
        ])
    })
}
