//! Every format loader in one registry. The app opens files through it, and the test
//! corpus (`crates/corpus`) checks it, so the two can never disagree on what a file
//! opens as.

mod libfileparser;

use std::sync::OnceLock;

pub use libfileparser::{ReaderLoader, open_folder, set_notify, show_facet};
use viewer_core::{FormatLoader, Registry};

/// Every loader compiled into this build. Order breaks ties between equal confidences.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        // libfileparser categories, adapted once for the life of the process. Its plain
        // text reader isn't listed: Viewer's own (which highlights) reads text.
        let adapt = |category: ::libfileparser::Category| -> &'static [&'static dyn FormatLoader] {
            Box::leak(
                ::libfileparser::readers()
                    .iter()
                    .filter(|r| r.info().category == category)
                    .map(|r| pdf_engine(*r))
                    .map(|r| &*Box::leak(Box::new(ReaderLoader(r))) as &dyn FormatLoader)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            )
        };
        Registry::new(&[
            adapt(::libfileparser::Category::Images),
            adapt(::libfileparser::Category::Archives),
            adapt(::libfileparser::Category::Data),
            adapt(::libfileparser::Category::Docs),
            viewer_fmt_doc::LOADERS,
            adapt(::libfileparser::Category::ThreeD),
        ])
    })
}

/// The PDF reader Viewer uses on this platform: libfileparser-docs bundles MuPDF only,
/// so PDFKit is wired here, on iOS (no MuPDF there) and on macOS when
/// `VIEWER_PDF_BACKEND=pdfkit`, for comparing the two (docs/design/pdf.md §2).
fn pdf_engine(reader: &'static dyn ::libfileparser::Reader) -> &'static dyn ::libfileparser::Reader {
    #[cfg(target_vendor = "apple")]
    if reader.info().id == "pdf" && (cfg!(target_os = "ios") || std::env::var("VIEWER_PDF_BACKEND").is_ok_and(|b| b.eq_ignore_ascii_case("pdfkit"))) {
        return &libfileparser_pdfkit::PdfKitReader;
    }
    reader
}
