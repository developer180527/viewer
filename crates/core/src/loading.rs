//! Background loading: every open (initial file, archive entry, "View as") runs on
//! its own thread so the UI never blocks on decoding.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use viewer_formats::registry;
use viewer_model::{Document, FileSource};

use crate::{Hooks, Waker};

/// Where a finished load goes in the navigation stack.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Replace everything (a newly opened file).
    Root,
    /// On top of the current page (an entry inside a container).
    Push,
    /// Swap the current page (a different "View as" for the same file).
    ReplaceTop,
}

pub(crate) struct Loaded {
    pub src: FileSource,
    pub doc: Document,
    pub loader: &'static str,
    pub attempts: Vec<(&'static str, String)>,
    pub load_time: Duration,
}

pub(crate) struct Job {
    pub name: String,
    pub target: Target,
    rx: Receiver<Result<Loaded, String>>,
}

impl Job {
    pub fn open_path(path: PathBuf, view_as: Option<String>, hooks: &Hooks) -> Self {
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
        if path.is_dir() {
            return Self::run(name, Target::Root, hooks, move |started| {
                let doc = viewer_formats::open_folder(&path).map_err(|e| format!("{}: {e:#}", path.display()))?;
                Ok(Loaded { src: FileSource::folder(&path), doc, loader: "folder", attempts: Vec::new(), load_time: started.elapsed() })
            });
        }
        let trace = hooks.trace.clone();
        Self::spawn(name, Target::Root, view_as, hooks, move || {
            let src = FileSource::open(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            if let Some(t) = &trace {
                t("file mapped");
            }
            Ok(src)
        })
    }

    pub fn spawn(
        name: String,
        target: Target,
        view_as: Option<String>,
        hooks: &Hooks,
        source: impl FnOnce() -> anyhow::Result<FileSource> + Send + 'static,
    ) -> Self {
        Self::run(name, target, hooks, move |started| {
            source().map_err(|e| format!("{e:#}")).and_then(|src| load(src, view_as.as_deref(), started))
        })
    }

    /// Shows another facet of the page's libfileparser document: the file isn't read
    /// again, but the facet may be made now (a USD Stage is composed), so off the UI
    /// thread like any load.
    pub fn facet(src: FileSource, facets: viewer_model::Facets, index: usize, loader: &'static str, hooks: &Hooks) -> Self {
        Self::run(src.name().into(), Target::ReplaceTop, hooks, move |started| {
            let doc = viewer_formats::show_facet(&facets, index, &src).map_err(|e| format!("{e:#}"))?;
            Ok(Loaded { src, doc, loader, attempts: Vec::new(), load_time: started.elapsed() })
        })
    }

    /// Runs `work` on a loader thread, handing it the time it started.
    fn run(name: String, target: Target, hooks: &Hooks, work: impl FnOnce(Instant) -> Result<Loaded, String> + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let (wake, trace): (Waker, _) = (hooks.wake.clone(), hooks.trace.clone());
        std::thread::Builder::new()
            .name("loader".into())
            .spawn(move || {
                let started = Instant::now();
                let result = work(started);
                if let (Ok(l), Some(t)) = (&result, &trace) {
                    t(&format!("document decoded by '{}'", l.loader));
                }
                // Send, then wake: the shell polls after each wake, so it sees this.
                let _ = tx.send(result);
                wake();
            })
            .expect("spawn loader thread");
        Self { name, target, rx }
    }

    /// `None` while still running.
    pub fn poll(&self) -> Option<Result<Loaded, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("loader thread crashed".into())),
        }
    }
}

fn load(src: FileSource, view_as: Option<&str>, started: Instant) -> Result<Loaded, String> {
    let (doc, loader, attempts) = match view_as {
        Some(id) => match registry().get(id) {
            Some(loader) => {
                let doc = viewer_model::open_guarded(loader, &src).map_err(|e| format!("{} could not open this file: {e:#}", loader.name()))?;
                (doc, loader.id(), Vec::new())
            }
            // A facet of the document, by name ("Stage", "Raw").
            None => {
                let (doc, loader, attempts) = registry().open(&src);
                let facets = doc.facets.clone().ok_or_else(|| format!("no loader called '{id}' (see --list-loaders)"))?;
                let index = facets
                    .doc
                    .facets()
                    .position(|f| f.name.eq_ignore_ascii_case(id))
                    .ok_or_else(|| format!("no loader or facet called '{id}' for {}", src.name()))?;
                let doc = viewer_formats::show_facet(&facets, index, &src).map_err(|e| format!("{e:#}"))?;
                (doc, loader, attempts)
            }
        },
        None => registry().open(&src),
    };
    Ok(Loaded { src, doc, loader, attempts, load_time: started.elapsed() })
}
