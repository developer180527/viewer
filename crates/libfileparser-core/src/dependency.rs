//! Other files a document uses (textures, buffers, layers, `.mtl` files), and whether
//! each could be read (docs/design/document-model.md §5).

use std::fmt;
use std::sync::{Arc, Mutex};

use crate::{FileSource, Limit, ParseContext};

/// Why a referenced file couldn't be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Unreadable {
    /// It may be there, but reading it isn't allowed: on iOS the user picked one file,
    /// and its neighbours are outside that grant. Opening the folder fixes it.
    Blocked,
    /// It isn't there, or isn't a file. Opening the folder won't help.
    Missing,
    /// It's outside the folder the app lets documents read from (a `..` path, an
    /// absolute path, a symlink out), so it wasn't looked for.
    Outside,
}

impl Unreadable {
    /// Classifies a failed open, read or stat.
    pub fn of(e: &std::io::Error) -> Self {
        match e.kind() {
            // EPERM (the iOS sandbox, macOS privacy) and EACCES (file modes).
            std::io::ErrorKind::PermissionDenied => Unreadable::Blocked,
            _ => Unreadable::Missing,
        }
    }
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Unreadable::Blocked => "not allowed to read it",
            Unreadable::Missing => "not found",
            Unreadable::Outside => "outside the folder it may read from",
        })
    }
}

/// Whether a referenced file was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DependencyStatus {
    Read,
    Blocked,
    Missing,
    Outside,
}

impl From<Unreadable> for DependencyStatus {
    fn from(u: Unreadable) -> Self {
        match u {
            Unreadable::Blocked => DependencyStatus::Blocked,
            Unreadable::Missing => DependencyStatus::Missing,
            Unreadable::Outside => DependencyStatus::Outside,
        }
    }
}

/// One referenced file: its name as the document gives it, and its status.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Dependency {
    pub reference: String,
    pub status: DependencyStatus,
}

/// Every file a document refers to. Cheap to clone; every clone is the same list,
/// so a reader's background work (texture decoding) adds to what the document shows.
#[derive(Clone, Default, Debug)]
pub struct Dependencies(Arc<Mutex<Vec<Dependency>>>);

impl Dependencies {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads `reference`, relative to `from`, through the context's [`FileResolver`](crate::FileResolver),
    /// and records the outcome. This is how every reader reads another file, so the
    /// list is complete.
    pub fn resolve(&self, cx: &ParseContext, from: &FileSource, reference: &str) -> Option<FileSource> {
        if !self.contains(reference) && cx.limits.check(Limit::Dependencies, self.len() as u64 + 1).is_err() {
            self.record(reference, DependencyStatus::Missing);
            return None;
        }
        match cx.files.resolve(from, reference) {
            Ok(file) => {
                self.record(reference, DependencyStatus::Read);
                Some(file)
            }
            Err(why) => {
                self.record(reference, why.into());
                None
            }
        }
    }

    /// Reads the first of `candidates` that can be read, for a file the document names
    /// `reference` but that may be in more than one place (a texture stored with the
    /// exporter's path, found by name beside the model or in `textures/`). The outcome
    /// is recorded once, under `reference`: blocked if any place refused, else missing
    /// (a candidate outside the allowed folder just isn't one of the places).
    pub fn resolve_any<S: AsRef<str>>(&self, cx: &ParseContext, from: &FileSource, reference: &str, candidates: &[S]) -> Option<FileSource> {
        if !self.contains(reference) && cx.limits.check(Limit::Dependencies, self.len() as u64 + 1).is_err() {
            self.record(reference, DependencyStatus::Missing);
            return None;
        }
        let mut blocked = false;
        for c in candidates {
            match cx.files.resolve(from, c.as_ref()) {
                Ok(file) => {
                    self.record(reference, DependencyStatus::Read);
                    return Some(file);
                }
                Err(Unreadable::Blocked) => blocked = true,
                Err(_) => {}
            }
        }
        self.record(reference, if blocked { DependencyStatus::Blocked } else { DependencyStatus::Missing });
        None
    }

    /// Records `reference` with `status`, replacing an earlier status for it.
    pub fn record(&self, reference: impl Into<String>, status: DependencyStatus) {
        let reference = reference.into();
        let mut list = self.lock();
        match list.iter_mut().find(|d| d.reference == reference) {
            Some(d) => d.status = status,
            None => list.push(Dependency { reference, status }),
        }
    }

    pub fn list(&self) -> Vec<Dependency> {
        self.lock().clone()
    }

    /// References with `status`, in the order first seen.
    pub fn with_status(&self, status: DependencyStatus) -> Vec<String> {
        self.lock().iter().filter(|d| d.status == status).map(|d| d.reference.clone()).collect()
    }

    pub fn count(&self, status: DependencyStatus) -> usize {
        self.lock().iter().filter(|d| d.status == status).count()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn contains(&self, reference: &str) -> bool {
        self.lock().iter().any(|d| d.reference == reference)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Dependency>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}
