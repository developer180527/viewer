//! libfileparser inside Viewer's registry, while the app still has its own document
//! type: each libfileparser reader appears as a Viewer loader and opens through the
//! front door (`libfileparser::open_with`), so its document has the generic Source,
//! Package and Raw facets too. The document is kept with the page, so View as can show
//! any of its facets ([`show_facet`]) without reading the file again; a facet goes to
//! the page as libfileparser made it, for libviewer's views.
//!
//! Viewer's policy, as hooks: other files from the model's folder and below, never
//! elsewhere (one folder up from a model in `~/Downloads` would be the whole home
//! folder), with the OS's own refusals reported as blocked; texture decoding on a two-thread pool, every
//! texture that arrives waking the window, and the platform's limits.

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, anyhow};
use libfileparser::{Confidence, Content, FolderFiles, Limits, Notify, ParseContext, Reader, Role, Spawn};
use viewer_core::{Document, Facets, FileSource, FormatLoader, Unreadables};

/// A libfileparser reader as a Viewer loader.
pub struct ReaderLoader(pub &'static dyn Reader);

impl FormatLoader for ReaderLoader {
    fn id(&self) -> &'static str {
        self.0.info().id
    }

    fn name(&self) -> &'static str {
        self.0.info().name
    }

    fn probe(&self, header: &[u8], _: Option<&str>, name: &str) -> viewer_core::Confidence {
        match self.0.detect(header, name).confidence {
            // A format it owns. A guess from content at that strength, or a format it
            // only also reads (an SVG, to `-data`), loses to a reader that owns it.
            Confidence::Extension if self.0.info().matches_extension(name) => viewer_core::Confidence::EXTENSION,
            Confidence::Extension => viewer_core::Confidence::LIKELY,
            // What the content proves outranks an extension, as in libfileparser's own
            // order: an OpenAPI description named `.yaml` is OpenAPI, not YAML.
            Confidence::Content => viewer_core::Confidence(viewer_core::Confidence::EXTENSION.0 + 5),
            Confidence::Magic => viewer_core::Confidence::MAGIC,
            // A format inside a generic container (USDZ, DOCX) outranks the container.
            Confidence::Certain => viewer_core::Confidence(viewer_core::Confidence::MAGIC.0 + 1),
            _ => viewer_core::Confidence::NONE,
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let cx = context();
        let doc = library().open_with(self.0.info().id, src, &cx).map_err(|e| anyhow!("{e}"))?;
        let start = starting_facet(&doc, &cx);
        viewer_document(Arc::new(doc), start, src, &cx)
    }
}

/// Which facet a file opens on: the first, unless that's a 3D scene with nothing in it
/// (a file of bones or references), whose structure says more than an empty viewport.
fn starting_facet(doc: &libfileparser::Document, cx: &ParseContext) -> usize {
    match doc.facet(0, cx) {
        Ok(Content::Scene(s)) if s.scene().read().bounds().is_none() => doc.facets().position(|f| f.role == Role::Structure).unwrap_or(0),
        _ => 0,
    }
}

/// Viewer's libfileparser: every category, with the PDF reader this platform uses.
/// libfileparser-docs bundles MuPDF only, so PDFKit is wired here, on iOS (no MuPDF
/// there) and on macOS when `VIEWER_PDF_BACKEND=pdfkit`, for comparing the two
/// (docs/design/pdf.md §4). Every open goes through this one library, so the reader
/// that's listed is the one that opens.
pub fn library() -> &'static libfileparser::Library {
    static LIBRARY: OnceLock<libfileparser::Library> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        let lib = libfileparser::Library::new();
        #[cfg(target_vendor = "apple")]
        if cfg!(target_os = "ios") || std::env::var("VIEWER_PDF_BACKEND").is_ok_and(|b| b.eq_ignore_ascii_case("pdfkit")) {
            return lib.replace("pdf", &libfileparser_pdfkit::PdfKitReader);
        }
        lib
    })
}

/// Facet `index` of the page's libfileparser document, as a Viewer document.
pub fn show_facet(facets: &Facets, index: usize, src: &FileSource) -> Result<Document> {
    viewer_document(facets.doc.clone(), index, src, &context())
}

/// A folder, listed by libfileparser's archives.
pub fn open_folder(path: &Path) -> Result<Document> {
    let cx = context();
    let src = FileSource::folder(path);
    let doc = library().open(&src, &cx).map_err(|e| anyhow!("{e}"))?;
    viewer_document(Arc::new(doc), 0, &src, &cx)
}

fn viewer_document(doc: Arc<libfileparser::Document>, index: usize, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let content = content(&doc, index, src, cx)?;
    let mut out = Document::new(doc.format().name.clone(), content);
    for (k, v) in doc.metadata() {
        out = out.with_meta(k.clone(), v);
    }
    out.unreadable = Unreadables::from_dependencies(doc.dependencies().clone());
    out.facets = Some(Facets { doc, shown: index });
    Ok(out)
}

/// A facet's content, for libviewer's views as it is: trees read a level at a time,
/// tables live, images decoded as they're shown. The one exception is the file's own
/// text (its Source), which goes through Viewer's text loader to be highlighted.
fn content(doc: &libfileparser::Document, index: usize, src: &FileSource, cx: &ParseContext) -> Result<viewer_core::Content> {
    let info = doc.facets().nth(index).ok_or_else(|| anyhow!("no facet {index}"))?.clone();
    let content = doc.facet(index, cx).map_err(|e| anyhow!("{}: {e}", info.name))?;
    if info.role == Role::Source && matches!(content, Content::Text(_)) {
        let loader = crate::registry().get("text").ok_or_else(|| anyhow!("no text loader"))?;
        return Ok(loader.open(src)?.content);
    }
    Ok(viewer_core::Content::Model(content))
}

/// What the app hears when a document gains something (a texture). Set once at
/// startup, before the first file opens.
pub fn set_notify(f: impl Fn() + Send + Sync + 'static) {
    let _ = NOTIFY.set(Box::new(f));
}

static NOTIFY: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

struct AppNotify;

impl Notify for AppNotify {
    fn notify(&self) {
        if let Some(f) = NOTIFY.get() {
            f();
        }
    }
}

/// Two threads for texture decoding: a 4K float EXR is ~256 MB while it decodes, so
/// more at once would cost more memory than it saves time.
struct TexturePool;

type Job = Box<dyn FnOnce() + Send + 'static>;

impl Spawn for TexturePool {
    fn spawn(&self, job: Job) {
        static QUEUE: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
        let queue = QUEUE.get_or_init(|| {
            let (tx, rx) = channel::<Job>();
            let rx = Arc::new(Mutex::new(rx));
            for i in 0..2 {
                let rx: Arc<Mutex<Receiver<Job>>> = rx.clone();
                std::thread::Builder::new()
                    .name(format!("texture decode {i}"))
                    .spawn(move || {
                        loop {
                            let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                            match job {
                                Ok(job) => job(),
                                Err(_) => return,
                            }
                        }
                    })
                    .expect("spawning a texture decoder");
            }
            Mutex::new(tx)
        });
        let _ = queue.lock().unwrap_or_else(|e| e.into_inner()).send(job);
    }
}

/// Viewer's hooks for one document.
fn context() -> ParseContext {
    let limits = if cfg!(any(target_os = "ios", target_os = "android")) { Limits::mobile() } else { Limits::desktop() };
    ParseContext::new()
        .with_files(FolderFiles::new())
        .with_spawn(TexturePool)
        .with_notify(AppNotify)
        .with_limits(limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened_on(text: &'static [u8]) -> String {
        let src = FileSource::from_bytes("a.usda", text.to_vec());
        let doc = crate::registry().get("usd").unwrap().open(&src).unwrap();
        let facets = doc.facets.unwrap();
        facets.doc.facets().nth(facets.shown).unwrap().name.clone()
    }

    #[test]
    fn a_file_with_nothing_to_draw_opens_on_its_structure() {
        assert_eq!(opened_on(b"#usda 1.0\ndef Camera \"c\" {}\n"), "Stage");
        assert_eq!(opened_on(b"#usda 1.0\ndef Sphere \"s\" {}\n"), "Scene");
    }
}
