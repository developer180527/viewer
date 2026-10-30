//! Background loading: every open (initial file, archive entry, "View as") runs on
//! its own thread so the UI never blocks on decoding.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use viewer_core::{Document, FileSource};

use crate::{formats::registry, startup};

/// Set by the frontend once its event loop exists, so loader threads can wake the UI
/// when they finish.
pub static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

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

pub struct Loaded {
    pub src: FileSource,
    pub doc: Document,
    pub loader: &'static str,
    pub attempts: Vec<(&'static str, String)>,
    pub load_time: Duration,
}

pub struct Job {
    pub name: String,
    pub target: Target,
    rx: Receiver<Result<Loaded, String>>,
}

impl Job {
    pub fn open_path(path: PathBuf, view_as: Option<String>) -> Self {
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
        Self::spawn(name, Target::Root, view_as, move || {
            let src = FileSource::open(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            startup::mark("file mapped");
            Ok(src)
        })
    }

    pub fn spawn(
        name: String,
        target: Target,
        view_as: Option<String>,
        source: impl FnOnce() -> anyhow::Result<FileSource> + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("loader".into())
            .spawn(move || {
                let started = Instant::now();
                let result = source().map_err(|e| format!("{e:#}")).and_then(|src| load(src, view_as.as_deref(), started));
                if let Ok(l) = &result {
                    startup::mark(&format!("document decoded by '{}'", l.loader));
                }
                // Send before checking WAKE: the UI sets WAKE before polling, so either it
                // sees our message or we see its waker. No wake-up is ever lost.
                let _ = tx.send(result);
                if let Some(wake) = WAKE.get() {
                    wake();
                }
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
        Some(id) => {
            let loader = registry().get(id).ok_or_else(|| format!("no loader called '{id}'"))?;
            let doc = viewer_core::open_guarded(loader, &src).map_err(|e| format!("{} could not open this file: {e:#}", loader.name()))?;
            (doc, loader.id(), Vec::new())
        }
        None => registry().open(&src),
    };
    Ok(Loaded { src, doc, loader, attempts, load_time: started.elapsed() })
}
