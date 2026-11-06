//! The hooks: how an app decides policy for a reader (docs/design/libfileparser.md §3).
//!
//! A reader never touches the file system beyond the bytes it was given, never starts
//! a thread, and keeps no globals: it asks these instead. Each is owned (`Arc`),
//! because background work (texture decoding) outlives the `open` call that started it.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{FileSource, Limits, ParseError, Result, Unreadable};

/// Reads the files a document refers to: textures, buffers, layers.
pub trait FileResolver: Send + Sync {
    /// Opens `reference` as the document `from` names it (a relative path, usually).
    fn resolve(&self, from: &FileSource, reference: &str) -> std::result::Result<FileSource, Unreadable>;
}

/// Runs background work.
pub trait Spawn: Send + Sync {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>);
}

/// Hears that a document gained something: a texture, a band, a page.
pub trait Notify: Send + Sync {
    fn notify(&self);
}

/// Set when a document is closed; work in progress stops at its next check.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// `Err(Cancelled)` once cancelled, for `?` in loops.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() { Err(ParseError::Cancelled) } else { Ok(()) }
    }
}

/// Everything a reader may use besides the file's bytes. Cheap to clone.
#[derive(Clone)]
#[non_exhaustive]
pub struct ParseContext {
    pub files: Arc<dyn FileResolver>,
    pub limits: Limits,
    pub cancel: Cancel,
    pub spawn: Arc<dyn Spawn>,
    pub notify: Arc<dyn Notify>,
}

impl ParseContext {
    /// Files read from beside the source, desktop limits, background work run at
    /// once on the caller's thread, and no notifications: enough to use a category
    /// library on its own. The front door supplies a thread pool instead.
    pub fn new() -> Self {
        Self {
            files: Arc::new(StdFiles),
            limits: Limits::desktop(),
            cancel: Cancel::new(),
            spawn: Arc::new(InlineSpawn),
            notify: Arc::new(NoNotify),
        }
    }

    pub fn with_files(mut self, files: impl FileResolver + 'static) -> Self {
        self.files = Arc::new(files);
        self
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_cancel(mut self, cancel: Cancel) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn with_spawn(mut self, spawn: impl Spawn + 'static) -> Self {
        self.spawn = Arc::new(spawn);
        self
    }

    pub fn with_notify(mut self, notify: impl Notify + 'static) -> Self {
        self.notify = Arc::new(notify);
        self
    }
}

impl Default for ParseContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads references from the file system: relative ones from the source's folder,
/// absolute ones as given. A source with no path (in memory) has no neighbours.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdFiles;

impl FileResolver for StdFiles {
    fn resolve(&self, from: &FileSource, reference: &str) -> std::result::Result<FileSource, Unreadable> {
        let reference = reference.replace('\\', "/");
        let path = Path::new(&reference);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            from.path().and_then(Path::parent).ok_or(Unreadable::Missing)?.join(path)
        };
        if !path.is_file() {
            // A sandbox can refuse the stat as well as the read: tell the two apart.
            return Err(std::fs::metadata(&path).map_or_else(|e| Unreadable::of(&e), |_| Unreadable::Missing));
        }
        FileSource::open(&path).map_err(|e| Unreadable::of(&e))
    }
}

/// Reads nothing beyond the source: every reference is missing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFiles;

impl FileResolver for NoFiles {
    fn resolve(&self, _: &FileSource, _: &str) -> std::result::Result<FileSource, Unreadable> {
        Err(Unreadable::Missing)
    }
}

/// Runs background work at once, on the calling thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct InlineSpawn;

impl Spawn for InlineSpawn {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        job();
    }
}

/// Ignores progress.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoNotify;

impl Notify for NoNotify {
    fn notify(&self) {}
}
