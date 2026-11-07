//! The hooks: how an app decides policy for a reader (docs/design/libfileparser.md §3).
//!
//! A reader never touches the file system beyond the bytes it was given, never starts
//! a thread, and keeps no globals: it asks these instead. Each is owned (`Arc`),
//! because background work (texture decoding) outlives the `open` call that started it.

use std::path::{Component, Path, PathBuf};
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
    /// Facets the app adds to documents of some formats (`FacetProvider`).
    pub facet_providers: Vec<Arc<dyn crate::FacetProvider>>,
}

impl ParseContext {
    /// No other files read ([`NoFiles`]; choose [`FolderFiles`] to read a model's
    /// textures), desktop limits, background work run at once on the caller's thread,
    /// and no notifications: safe for any file, and enough to use a category library
    /// on its own. The front door supplies a thread pool and folder access instead.
    pub fn new() -> Self {
        Self {
            files: Arc::new(NoFiles),
            limits: Limits::desktop(),
            cancel: Cancel::new(),
            spawn: Arc::new(InlineSpawn),
            notify: Arc::new(NoNotify),
            facet_providers: Vec::new(),
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

    /// Adds facets from `provider` to the documents of the formats it names.
    pub fn with_facet_provider(mut self, provider: impl crate::FacetProvider + 'static) -> Self {
        self.facet_providers.push(Arc::new(provider));
        self
    }

    /// Runs `job` through the spawn hook with a panic caught, so a panic in format code
    /// (a decoder) can't take down the thread it runs on, or the caller's thread with
    /// an inline spawner. `job` must leave things consistent either way: count
    /// pending work in a guard or after a nested [`ParseContext::catching`].
    pub fn spawn_job(&self, job: impl FnOnce() + Send + 'static) {
        self.spawn.spawn(Box::new(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        }));
    }

    /// Runs `f`, giving `None` if it panics: for the part of background work that runs
    /// format code, so the bookkeeping around it still happens.
    pub fn catching<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok().flatten()
    }
}

impl Default for ParseContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads nothing beyond the source: every reference is missing. The default, so a
/// library opening untrusted files (a server making thumbnails of uploads) reads
/// nothing else on its machine unless it chooses to.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFiles;

impl FileResolver for NoFiles {
    fn resolve(&self, _: &FileSource, _: &str) -> std::result::Result<FileSource, Unreadable> {
        Err(Unreadable::Missing)
    }
}

/// Reads references inside one folder: by default the source's own folder and
/// everything below it, which is what a viewer or editor wants. A reference that
/// leaves it (`../secret.png`, an absolute path, a symlink pointing out) is
/// [`Unreadable::Outside`], so a hostile file can't make the app read other files on
/// the machine. Exporters that record absolute paths are still served: readers look
/// for the file by name in the folder ([`Dependencies::resolve_any`](crate::Dependencies::resolve_any)).
///
/// [`FolderFiles::within`] widens the folder to one the user granted (a folder they
/// opened or picked), so `../textures/` works for a model in a subfolder of it. The
/// root is always a folder someone chose, never "n levels up from wherever the file
/// is": one level up from a model in `~/Downloads` is the whole home folder.
#[derive(Clone, Debug, Default)]
pub struct FolderFiles {
    root: Option<PathBuf>,
}

impl FolderFiles {
    /// The source's own folder and below.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything inside `root`, which should contain the sources opened with it (a
    /// source outside `root` reads nothing). References may climb (`../textures/a.png`)
    /// as long as they stay inside.
    pub fn within(root: impl Into<PathBuf>) -> Self {
        Self { root: Some(root.into()) }
    }

    /// The folder references may not leave, and the path `reference` names, when it
    /// stays inside by its words alone (before symlinks are followed).
    fn place(&self, from: &FileSource, reference: &str) -> std::result::Result<(PathBuf, PathBuf), Unreadable> {
        let reference = reference.replace('\\', "/");
        // `C:/x` on a Unix machine parses as a relative `C:` folder; it's still absolute.
        if reference.as_bytes().get(1) == Some(&b':') || reference.contains("://") {
            return Err(Unreadable::Outside);
        }
        let dir = from.path().and_then(Path::parent).ok_or(Unreadable::Missing)?;
        // Absolute, so a relative source path and an absolute root compare.
        let dir = std::path::absolute(dir).map_err(|_| Unreadable::Missing)?;
        let root = match &self.root {
            Some(root) => std::path::absolute(root).map_err(|_| Unreadable::Missing)?,
            None => dir.clone(),
        };
        let mut path = dir;
        for c in Path::new(&reference).components() {
            match c {
                Component::Normal(name) => path.push(name),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !path.pop() {
                        return Err(Unreadable::Outside);
                    }
                }
                // `/x`, `C:\x`, `\\server\x`: absolute, never inside.
                Component::RootDir | Component::Prefix(_) => return Err(Unreadable::Outside),
            }
            // Checked at every step: `a/../../secret` must not pass by coming back.
            if !path.starts_with(&root) {
                return Err(Unreadable::Outside);
            }
        }
        if !path.starts_with(&root) {
            return Err(Unreadable::Outside);
        }
        Ok((root, path))
    }
}

impl FileResolver for FolderFiles {
    fn resolve(&self, from: &FileSource, reference: &str) -> std::result::Result<FileSource, Unreadable> {
        let (root, path) = self.place(from, reference)?;
        // Symlinks: where it really is must be inside too. Where the root can't be
        // resolved (a sandbox that hides the folders above a grant), the check by
        // words above stands.
        if let (Ok(real), Ok(root)) = (path.canonicalize(), root.canonicalize())
            && !real.starts_with(&root)
        {
            return Err(Unreadable::Outside);
        }
        read_file(&path)
    }
}

/// Reads any reference: relative ones from the source's folder, absolute ones as
/// given, `..` wherever it leads. Only for files you trust (your own pipeline's), never
/// for files from elsewhere.
#[derive(Clone, Copy, Debug, Default)]
pub struct AnyFiles;

impl FileResolver for AnyFiles {
    fn resolve(&self, from: &FileSource, reference: &str) -> std::result::Result<FileSource, Unreadable> {
        let reference = reference.replace('\\', "/");
        let path = Path::new(&reference);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            from.path().and_then(Path::parent).ok_or(Unreadable::Missing)?.join(path)
        };
        read_file(&path)
    }
}

fn read_file(path: &Path) -> std::result::Result<FileSource, Unreadable> {
    if !path.is_file() {
        // A sandbox can refuse the stat as well as the read: tell the two apart.
        return Err(std::fs::metadata(path).map_or_else(|e| Unreadable::of(&e), |_| Unreadable::Missing));
    }
    FileSource::open(path).map_err(|e| Unreadable::of(&e))
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
