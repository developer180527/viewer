//! Other files a document uses (textures, buffers, layers, `.mtl` files), and whether
//! each could be read (docs/design/document-model.md §5).

use std::fmt;
use std::sync::{Arc, Mutex};

use crate::{Count, FileSource, Limit, MemoryUse, ParseContext, TableInfo, TableSource};

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

impl DependencyStatus {
    /// For people, as a Dependencies table shows it.
    pub fn label(self) -> &'static str {
        match self {
            DependencyStatus::Read => "read",
            DependencyStatus::Blocked => "not allowed to read",
            DependencyStatus::Missing => "missing",
            DependencyStatus::Outside => "outside the allowed folder",
        }
    }
}

/// One referenced file: its name as the document gives it, what it's for, where it
/// was found, and its status.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Dependency {
    pub reference: String,
    pub status: DependencyStatus,
    /// What the document uses it for, in the format's words ("texture", "buffer",
    /// "layer", "material library"); empty when the reader didn't say.
    pub kind: String,
    /// Where it was read from, relative to the document, when that differs from
    /// `reference` or the reader looked in several places; `None` unless read.
    pub found: Option<String>,
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
                self.record_found(reference, reference);
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
                    self.record_found(reference, c.as_ref());
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
        self.update(reference.into(), status, None);
    }

    /// Says what the document uses `reference` for ("texture", "layer"), once it has
    /// been resolved or recorded. A reference not in the list is ignored.
    pub fn set_kind(&self, reference: &str, kind: &str) {
        if let Some(d) = self.lock().iter_mut().find(|d| d.reference == reference) {
            d.kind = kind.to_string();
        }
    }

    /// The list as a table (file, used as, found at, status) that always shows the
    /// list as it is now: background work that adds to it shows up.
    pub fn table(&self) -> DependencyTable {
        DependencyTable(self.clone())
    }

    fn record_found(&self, reference: &str, found: &str) {
        self.update(reference.to_string(), DependencyStatus::Read, Some(found.to_string()));
    }

    fn update(&self, reference: String, status: DependencyStatus, found: Option<String>) {
        let found = if status == DependencyStatus::Read { found } else { None };
        let mut list = self.lock();
        match list.iter_mut().find(|d| d.reference == reference) {
            Some(d) => {
                d.status = status;
                if found.is_some() || status != DependencyStatus::Read {
                    d.found = found;
                }
            }
            None => list.push(Dependency { reference, status, kind: String::new(), found }),
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

/// A document's dependencies as a table, from [`Dependencies::table`].
pub struct DependencyTable(Dependencies);

impl DependencyTable {
    const COLUMNS: [&str; 4] = ["File", "Used as", "Found at", "Status"];
}

impl TableSource for DependencyTable {
    fn info(&self) -> TableInfo {
        TableInfo::new(Self::COLUMNS.map(String::from).to_vec(), Count::Exact(self.0.len() as u64))
    }

    fn rows(&self, range: std::ops::Range<u64>) -> crate::Result<Vec<Vec<String>>> {
        let list = self.0.lock();
        let (start, end) = (range.start.min(list.len() as u64) as usize, range.end.min(list.len() as u64) as usize);
        Ok(list[start..end.max(start)]
            .iter()
            .map(|d| {
                let found = d.found.clone().filter(|f| *f != d.reference).unwrap_or_default();
                vec![d.reference.clone(), d.kind.clone(), found, d.status.label().to_string()]
            })
            .collect())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
}
