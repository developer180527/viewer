//! Documents shared between the sessions of one app's windows: a file open in two
//! windows is read and decoded once, and both pages show the same [`Document`].
//!
//! Only weak references are kept, so a document lives exactly as long as some page
//! shows it, and the cache never holds one alive on its own. A file is the same file
//! when its canonical path, length and modification time are; changed on disk, it's
//! read again.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::SystemTime;

use viewer_model::{Document, FileSource};

/// Which document: the file as it was when read, and how it was asked to be shown.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Key {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    view_as: Option<String>,
}

impl Key {
    /// `None` for what isn't a plain file on disk (a folder's listing is cheap to
    /// read again, and changes without its own time changing).
    pub fn of(path: &Path, view_as: Option<&str>) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let path = std::fs::canonicalize(path).ok()?;
        Some(Self { path, len: meta.len(), modified: meta.modified().ok(), view_as: view_as.map(str::to_ascii_lowercase) })
    }
}

/// What a page opened from a cached document gets.
pub(crate) struct Hit {
    pub src: FileSource,
    pub doc: Arc<Document>,
    pub loader: &'static str,
    pub attempts: Vec<(&'static str, String)>,
}

struct Entry {
    key: Key,
    /// The source the document was read from. It's what the document reads through,
    /// so it costs nothing extra while the document lives; dead entries are dropped
    /// whenever the cache is used.
    src: FileSource,
    doc: Weak<Document>,
    loader: &'static str,
    attempts: Vec<(&'static str, String)>,
}

/// The cache, cheap to clone; clones share it.
#[derive(Clone, Default)]
pub(crate) struct Documents(Arc<Mutex<Vec<Entry>>>);

impl Documents {
    fn entries(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        let mut e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        e.retain(|e| e.doc.strong_count() > 0);
        e
    }

    /// The document some page already shows for `key`.
    pub fn get(&self, key: &Key) -> Option<Hit> {
        self.entries().iter().find(|e| &e.key == key).and_then(|e| {
            Some(Hit { src: e.src.clone(), doc: e.doc.upgrade()?, loader: e.loader, attempts: e.attempts.clone() })
        })
    }

    /// Notes a document just read for `key`, for the next window that opens it.
    pub fn put(&self, key: Key, src: &FileSource, doc: &Arc<Document>, loader: &'static str, attempts: &[(&'static str, String)]) {
        let mut entries = self.entries();
        entries.retain(|e| e.key != key);
        entries.push(Entry { key, src: src.clone(), doc: Arc::downgrade(doc), loader, attempts: attempts.to_vec() });
    }

    /// Drops what no page shows any more.
    pub fn prune(&self) {
        drop(self.entries());
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_lives_only_as_long_as_a_page_shows_it() {
        let dir = std::env::temp_dir().join(format!("viewer-docs-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "hello").unwrap();
        let docs = Documents::default();
        let key = Key::of(&path, None).unwrap();
        let src = FileSource::open(&path).unwrap();
        let doc = Arc::new(Document::new("Text", viewer_model::Content::hex(src.clone())));
        docs.put(key.clone(), &src, &doc, "hex", &[]);
        assert!(Arc::ptr_eq(&docs.get(&key).unwrap().doc, &doc));
        // Another way of showing it is another document.
        assert!(docs.get(&Key::of(&path, Some("text")).unwrap()).is_none());
        drop(doc);
        assert!(docs.get(&key).is_none());
        assert_eq!(docs.len(), 0);
        // Changed on disk, it's another file.
        std::fs::write(&path, "hello, again").unwrap();
        assert_ne!(Key::of(&path, None).unwrap(), key);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
