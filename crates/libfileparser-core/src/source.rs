use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Files up to this size are read into memory when opened, rather than mapped: cheap,
/// and safe from the file changing underneath.
pub const COPY_BELOW: u64 = 16 << 20;

/// How much of a file [`FileSource::header`] holds.
const HEADER: usize = 8192;

enum Data {
    #[cfg(feature = "mmap")]
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
    /// A range of another file's bytes, sharing them.
    Part(Arc<Data>, std::ops::Range<usize>),
    /// A file read by position, never mapped (`FileSource::open_live`).
    Positional(Positional),
}

struct Positional {
    file: File,
    /// The size when opened. A file that grows later is read to here; one that
    /// shrinks reads short.
    len: u64,
    head: OnceLock<Vec<u8>>,
    /// The whole file, read the first time someone asks for all of it at once.
    whole: OnceLock<Vec<u8>>,
}

impl Positional {
    fn new(file: File, len: u64) -> Self {
        Self { file, len, head: OnceLock::new(), whole: OnceLock::new() }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let want = buf.len().min(self.len.saturating_sub(offset) as usize);
        let buf = &mut buf[..want];
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::read_at(&self.file, buf, offset)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::FileExt::seek_read(&self.file, buf, offset)
        }
        #[cfg(not(any(unix, windows)))]
        {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = &self.file;
            f.seek(SeekFrom::Start(offset))?;
            f.read(buf)
        }
    }

    /// Fills as much of `buf` as the file has from `offset`; short only at its end.
    fn read_full(&self, mut offset: u64, buf: &mut [u8]) -> usize {
        let mut done = 0;
        while done < buf.len() {
            match self.read_at(offset, &mut buf[done..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    done += n;
                    offset += n as u64;
                }
            }
        }
        done
    }

    fn read_range(&self, offset: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0; len];
        let n = self.read_full(offset, &mut buf);
        buf.truncate(n);
        buf
    }
}

impl Data {
    fn len(&self) -> u64 {
        match self {
            Data::Positional(p) => p.len,
            Data::Part(_, range) => range.len() as u64,
            other => other.as_ref().len() as u64,
        }
    }

    fn header(&self) -> &[u8] {
        match self {
            Data::Positional(p) => p.head.get_or_init(|| p.read_range(0, HEADER)),
            Data::Part(of, range) => {
                let head = of.header();
                if range.end <= head.len() {
                    &head[range.clone()]
                } else {
                    let b = &(**of).as_ref()[range.clone()];
                    &b[..b.len().min(HEADER)]
                }
            }
            other => {
                let b = other.as_ref();
                &b[..b.len().min(HEADER)]
            }
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> usize {
        match self {
            Data::Positional(p) => p.read_full(offset, buf),
            Data::Part(of, range) => {
                let start = range.start as u64 + offset.min(range.len() as u64);
                let n = buf.len().min((range.end as u64).saturating_sub(start) as usize);
                of.read_at(start, &mut buf[..n])
            }
            other => {
                let b = other.as_ref();
                let start = (offset as usize).min(b.len());
                let n = buf.len().min(b.len() - start);
                buf[..n].copy_from_slice(&b[start..start + n]);
                n
            }
        }
    }
}

impl AsRef<[u8]> for Data {
    fn as_ref(&self) -> &[u8] {
        match self {
            #[cfg(feature = "mmap")]
            Data::Mapped(m) => m,
            Data::Owned(v) => v,
            Data::Part(of, range) => &(**of).as_ref()[range.clone()],
            Data::Positional(p) => p.whole.get_or_init(|| p.read_range(0, p.len as usize)),
        }
    }
}

/// The bytes of a file plus its name. Cheap to clone.
///
/// [`FileSource::open`] reads a small file into memory and maps a large one, so
/// opening a multi-GB file costs nothing until pages are touched. A mapped file that
/// another process truncates while it is open faults the process (SIGBUS), which no
/// Rust code can catch: open files that may change, live logs above all, with
/// [`FileSource::open_live`], which reads by position instead. Readers that can read
/// by range ([`FileSource::read_at`]) never hold such a file whole.
#[derive(Clone)]
pub struct FileSource {
    name: Arc<str>,
    path: Option<Arc<Path>>,
    data: Arc<Data>,
    folder: bool,
}

impl FileSource {
    /// Opens a file: read into memory below [`COPY_BELOW`], else mapped (or, without
    /// the `mmap` feature, read by position).
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let data = if len < COPY_BELOW {
            let mut bytes = Vec::with_capacity(len as usize);
            std::io::Read::read_to_end(&mut &file, &mut bytes)?;
            Data::Owned(bytes)
        } else {
            Self::large(file, len)?
        };
        Ok(Self::on_disk(path, data))
    }

    /// Opens a file that may change while open, a log being written or rotated: read
    /// by position, never mapped, so its shrinking reads short instead of faulting.
    /// It is read as of its size now. A reader that asks for all of its bytes at once
    /// ([`FileSource::bytes`]) gets a copy, made once.
    pub fn open_live(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self::on_disk(path, Data::Positional(Positional::new(file, len))))
    }

    #[cfg(feature = "mmap")]
    fn large(file: File, _: u64) -> std::io::Result<Data> {
        // SAFETY: the map is read-only. A file truncated while mapped faults the
        // process; `open_live` is for files that may be (see the type's docs).
        Ok(Data::Mapped(unsafe { memmap2::Mmap::map(&file)? }))
    }

    #[cfg(not(feature = "mmap"))]
    fn large(file: File, len: u64) -> std::io::Result<Data> {
        Ok(Data::Positional(Positional::new(file, len)))
    }

    fn on_disk(path: &Path, data: Data) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self { name: name.into(), path: Some(path.into()), data: Arc::new(data), folder: false }
    }

    /// Whether the file is read by position ([`FileSource::open_live`]): a reader
    /// that can should then read ranges ([`FileSource::read_at`]), not [`FileSource::bytes`].
    pub fn is_positional(&self) -> bool {
        let mut d = &*self.data;
        while let Data::Part(of, _) = d {
            d = of;
        }
        matches!(d, Data::Positional(_))
    }

    /// Reads from `offset` into `buf`; how many bytes were read, fewer only at the
    /// end of the file. Works on every source; on one opened with `open_live`,
    /// without reading the rest of the file.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> usize {
        self.data.read_at(offset, buf)
    }

    /// `len` bytes from `offset` (fewer at the end), as shared bytes: a view of the
    /// file where it is in memory, read by position where it isn't.
    pub fn range(&self, offset: u64, len: usize) -> crate::Bytes {
        if self.is_positional() {
            let mut buf = vec![0; len.min(self.len().saturating_sub(offset) as usize)];
            let n = self.read_at(offset, &mut buf);
            buf.truncate(n);
            buf.into()
        } else {
            let start = offset as usize;
            crate::Bytes::slice_of(self.shared_bytes(), start..start.saturating_add(len))
        }
    }

    /// A folder on disk: a name and a path, no bytes. Its contents are listed by the
    /// archives category, not by format detection.
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
        (*self.data).as_ref()
    }

    /// `range` of this file's bytes as a file of its own named `name`, sharing the
    /// bytes rather than copying them: a GLB's JSON chunk, an image inside a model.
    /// The range is clamped to the file.
    pub fn part(&self, name: impl Into<String>, range: std::ops::Range<usize>) -> Self {
        let len = self.len() as usize;
        let end = range.end.min(len);
        let range = range.start.min(end)..end;
        Self { name: name.into().into(), path: None, data: Arc::new(Data::Part(self.data.clone(), range)), folder: false }
    }

    /// The bytes as a shared handle that keeps the mapping alive, for consumers that
    /// outlive this `FileSource` and must not copy (3D scenes borrow vertex data
    /// straight from the mapped file).
    pub fn shared_bytes(&self) -> Arc<dyn AsRef<[u8]> + Send + Sync> {
        self.data.clone()
    }

    pub fn len(&self) -> u64 {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The first bytes (8 KiB), enough for any magic-number check. Never reads the
    /// whole of a file opened with `open_live`.
    pub fn header(&self) -> &[u8] {
        self.data.header()
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

#[cfg(test)]
mod tests {
    use super::FileSource;

    #[test]
    fn a_part_shares_its_file_and_is_clamped() {
        let file = FileSource::from_bytes("model.glb", b"glTF....{\"asset\":{}}rest".to_vec());
        let json = file.part("model.json", 8..20);
        assert_eq!((json.name(), json.bytes()), ("model.json", &b"{\"asset\":{}}"[..]));
        assert!(json.path().is_none());
        // Of a part, and past the end: clamped, never a panic.
        assert_eq!(json.part("x", 1..100).bytes(), b"\"asset\":{}}");
        assert!(file.part("y", 50..60).is_empty());
    }

    #[test]
    fn a_live_file_is_read_by_position_and_survives_shrinking() {
        let dir = std::env::temp_dir().join(format!("lfp-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.log");
        std::fs::write(&path, b"one\ntwo\nthree\n").unwrap();
        let live = FileSource::open_live(&path).unwrap();
        assert!(live.is_positional());
        assert_eq!((live.len(), live.header()), (14, &b"one\ntwo\nthree\n"[..]));
        let mut buf = [0; 3];
        assert_eq!((live.read_at(4, &mut buf), &buf), (3, b"two"));
        assert_eq!(&*live.range(8, 100), b"three\n");
        // Rotated away under us: reads come back short, nothing faults.
        std::fs::write(&path, b"").unwrap();
        assert_eq!(live.read_at(4, &mut buf), 0);
        assert!(live.part("p", 0..3).is_positional());
        // A small file is copied when opened: changing it later changes nothing.
        std::fs::write(&path, b"abc").unwrap();
        let copied = FileSource::open(&path).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(copied.bytes(), b"abc");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
