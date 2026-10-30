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
        ])
    })
}
