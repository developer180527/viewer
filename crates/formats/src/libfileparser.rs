//! libfileparser categories inside Viewer's registry, while the app still has its own
//! document type: each libfileparser reader appears as a Viewer loader, opened with
//! Viewer's hooks (docs/design/libfileparser.md §3), and its document converted.
//!
//! Viewer's policy, as hooks: other files from the model's folder and below, or one
//! folder up (some exporters put textures in `../textures/`), never elsewhere, with
//! the OS's own refusals reported as blocked; texture decoding on a two-thread pool, every
//! texture that arrives waking the window, and the platform's limits.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, anyhow, bail};
use libfileparser_core::{Confidence, Content, FolderFiles, Limits, Notify, ParseContext, Reader, Spawn};
use viewer_core::{Document, FileSource, FormatLoader, Raster, SceneDoc, Unreadables};

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
            Confidence::Extension => viewer_core::Confidence::EXTENSION,
            Confidence::Content => viewer_core::Confidence::LIKELY,
            Confidence::Magic => viewer_core::Confidence::MAGIC,
            // A format inside a generic container (USDZ, DOCX) outranks the container.
            Confidence::Certain => viewer_core::Confidence(viewer_core::Confidence::MAGIC.0 + 1),
            _ => viewer_core::Confidence::NONE,
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let cx = context();
        let doc = self.0.open(src, &cx).map_err(|e| anyhow!("{e}"))?;
        let content = match doc.facet(0, &cx).map_err(|e| anyhow!("{e}"))? {
            Content::Scene(s) => viewer_core::Content::Scene(SceneDoc { scene: s.scene().clone(), textures_pending: s.pending_counter() }),
            Content::Raster(r) => viewer_core::Content::Raster(Raster::full(r.width(), r.height(), r.rgba().to_vec())),
            _ => bail!("{} made content Viewer can't show yet", self.0.info().id),
        };
        let mut out = Document::new(doc.format().name.clone(), content);
        for (k, v) in doc.metadata() {
            out = out.with_meta(k.clone(), v);
        }
        out.unreadable = Unreadables::from_dependencies(doc.dependencies().clone());
        Ok(out)
    }
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
        .with_files(FolderFiles::new().allow_parent_levels(1))
        .with_spawn(TexturePool)
        .with_notify(AppNotify)
        .with_limits(limits)
}
