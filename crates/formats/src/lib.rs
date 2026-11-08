//! Every format loader in one registry. The app opens files through it, and the test
//! corpus (`crates/corpus`) checks it, so the two can never disagree on what a file
//! opens as.

mod libfileparser;

use std::sync::OnceLock;

pub use libfileparser::{ReaderLoader, library, open_folder, set_notify, show_facet};
use viewer_core::{FormatLoader, Registry};

/// Every loader compiled into this build. Order breaks ties between equal confidences.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        // libfileparser categories, adapted once for the life of the process. Its plain
        // text reader isn't listed: Viewer's own (which highlights) reads text.
        let adapt = |category: ::libfileparser::Category| -> &'static [&'static dyn FormatLoader] {
            Box::leak(
                library()
                    .readers()
                    .iter()
                    .filter(|r| r.info().category == category)
                    .map(|r| &*Box::leak(Box::new(ReaderLoader(*r))) as &dyn FormatLoader)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            )
        };
        Registry::new(&[
            adapt(::libfileparser::Category::Images),
            adapt(::libfileparser::Category::Archives),
            adapt(::libfileparser::Category::Data),
            adapt(::libfileparser::Category::Docs),
            adapt(::libfileparser::Category::Sheets),
            viewer_fmt_doc::LOADERS,
            adapt(::libfileparser::Category::ThreeD),
        ])
    })
}

