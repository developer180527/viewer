use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

enum Data {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl AsRef<[u8]> for Data {
    fn as_ref(&self) -> &[u8] {
        match self {
            Data::Mapped(m) => m,
            Data::Owned(v) => v,
        }
    }
}

/// The bytes of a file plus its name. Cheap to clone; disk files are memory-mapped
/// so opening a multi-GB file costs nothing until pages are touched.
#[derive(Clone)]
pub struct FileSource {
    name: Arc<str>,
    path: Option<Arc<Path>>,
    data: Arc<Data>,
    folder: bool,
}

impl FileSource {
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        // Mapping a zero-length file fails on some platforms.
        let data = if len == 0 {
            Data::Owned(Vec::new())
        } else {
            // SAFETY: the map is read-only. If another process truncates the file while
            // it is open we may fault; every mmap-based viewer accepts this trade-off.
            Data::Mapped(unsafe { memmap2::Mmap::map(&file)? })
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self {
            name: name.into(),
            path: Some(path.into()),
            data: Arc::new(data),
            folder: false,
        })
    }

    /// A folder on disk: a name and a path, no bytes. Its contents are listed by
    /// `viewer_fmt_archive::open_folder`, not by format detection.
    pub fn folder(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self { name: name.into(), path: Some(path.into()), data: Arc::new(Data::Owned(Vec::new())), folder: true }
    }

    /// An in-memory file, e.g. an entry extracted from an archive.
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            name: name.into().into(),
            path: None,
            data: Arc::new(Data::Owned(bytes)),
            folder: false,
        }
    }

    pub fn is_folder(&self) -> bool {
        self.folder
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn path_buf(&self) -> Option<PathBuf> {
        self.path.as_deref().map(Path::to_path_buf)
    }

    pub fn bytes(&self) -> &[u8] {
        match &*self.data {
            Data::Mapped(m) => m,
            Data::Owned(v) => v,
        }
    }

    /// The bytes as a shared handle that keeps the mapping alive, for consumers that
    /// outlive this `FileSource` and must not copy (3D scenes borrow vertex data
    /// straight from the mapped file).
    pub fn shared_bytes(&self) -> Arc<dyn AsRef<[u8]> + Send + Sync> {
        self.data.clone()
    }

    pub fn len(&self) -> u64 {
        self.bytes().len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.bytes().is_empty()
    }

    /// The first bytes, enough for any magic-number check.
    pub fn header(&self) -> &[u8] {
        let b = self.bytes();
        &b[..b.len().min(8192)]
    }

    /// Lower-cased extension of the file name, if any.
    pub fn ext(&self) -> Option<String> {
        Path::new(&*self.name)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
    }
}

impl fmt::Debug for FileSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSource")
            .field("name", &self.name)
            .field("len", &self.len())
            .finish()
    }
}
