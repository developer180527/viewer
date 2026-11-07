//! Archives and folders as containers (docs/design/libfileparser.md §2): zip, tar (also
//! gzipped), a gzipped file, and folders on disk. Entries are listed when the archive
//! opens and read when one is opened, as a file of its own that goes through detection
//! again, so a PNG inside a tar inside a gzip just works.
//!
//! The same listing is the generic **Package** facet the front door gives a file that
//! is a zip whatever else reads it (a USDZ, a 3MF): [`package`].
//!
//! Safe on hostile archives: an entry inflates only as far as the context's limits
//! allow for its packed size (a decompression bomb stops there), entry counts are
//! checked before they're listed, and a folder listing neither follows symlinks nor
//! runs past [`MAX_FOLDER_FILES`].
//!
//! The API is the shape every libfileparser category has: [`readers`], [`detect`],
//! [`open`] and [`open_with`]; and [`open_folder`] for a folder.

use std::io::{Cursor, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use libfileparser_core::{
    Category, Confidence, Container, Content, Count, Detection, Document, Entry, EntryKind, FileSource, FormatInfo, Limit, Limits,
    MemoryUse, ParseContext, ParseError, Reader, ReaderInfo, Result, Role,
};

/// Every format this category reads, in the order ties are broken: a `.tar.gz` is a
/// tar before it's a gzip.
pub fn readers() -> &'static [&'static dyn Reader] {
    &[&ZipReader, &TarReader, &GzipReader]
}

pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
    libfileparser_core::detect(readers(), header, name)
}

pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open(readers(), src, cx)
}

pub fn open_with(id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open_with(readers(), id, src, cx)
}

fn document(id: &'static str, format: &str, src: &FileSource, container: Arc<dyn Container>, entries: &[Entry]) -> Document {
    let files = entries.iter().filter(|e| e.kind == EntryKind::File).count();
    let total: u64 = entries.iter().map(|e| e.size).sum();
    Document::new(FormatInfo::new(id, format, Category::Archives))
        .with_facet(Role::Primary, "Files", Content::Container(container))
        .with_meta("Files", files)
        .with_meta("Uncompressed size", format!("{total} bytes"))
        .with_file_facets(src, None)
}

/// Most reserved up front from a size hint. Hints come from the archive itself, so a
/// tiny crafted file could otherwise make us allocate the whole cap at once.
const MAX_PREALLOC: u64 = 64 << 20;

/// Inflates `reader`, refusing past what `packed` bytes may become within `limits`.
fn read_capped(reader: impl Read, hint: u64, packed: u64, limits: &Limits) -> Result<Vec<u8>> {
    let cap = limits.unpack_cap(packed);
    let mut out = Vec::with_capacity(hint.min(MAX_PREALLOC).min(cap) as usize);
    reader.take(cap + 1).read_to_end(&mut out).map_err(ParseError::corrupt)?;
    limits.check_unpack(packed, out.len() as u64)?;
    Ok(out)
}

fn slice<T: Clone>(all: &[T], range: Range<u64>) -> Vec<T> {
    let end = range.end.min(all.len() as u64) as usize;
    let start = (range.start as usize).min(end);
    all[start..end].to_vec()
}

fn entry_at<T>(all: &[T], index: u64) -> Result<&T> {
    all.get(index as usize).ok_or_else(|| ParseError::invalid(format!("no entry {index}")))
}

// ---------------------------------------------------------------- zip

pub struct ZipReader;

static ZIP: ReaderInfo = ReaderInfo::new("zip", "Zip archive", Category::Archives)
    .extensions(&["zip", "jar", "apk", "cbz", "epub", "docx", "xlsx", "pptx", "odt"])
    .mime_types(&["application/zip"]);

pub fn is_zip(header: &[u8]) -> bool {
    header.starts_with(b"PK\x03\x04") || header.starts_with(b"PK\x05\x06")
}

impl Reader for ZipReader {
    fn info(&self) -> &ReaderInfo {
        &ZIP
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if is_zip(header) {
            Detection::new(Confidence::Magic)
        } else if ZIP.matches_extension(name) {
            Detection::new(Confidence::Extension)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let container = ZipContainer::new(src, cx)?;
        let entries = container.entries.clone();
        Ok(document("zip", "Zip archive", src, Arc::new(container), &entries))
    }
}

/// A zip's entries as the Package facet, for a file another reader read (a USDZ):
/// `None` when it isn't a zip.
pub fn package(src: &FileSource, cx: &ParseContext) -> Result<Option<Content>> {
    if !is_zip(src.header()) {
        return Ok(None);
    }
    Ok(Some(Content::Container(Arc::new(ZipContainer::new(src, cx)?))))
}

struct ZipContainer {
    src: FileSource,
    entries: Vec<Entry>,
    limits: Limits,
}

impl ZipContainer {
    fn new(src: &FileSource, cx: &ParseContext) -> Result<Self> {
        let mut archive = zip::ZipArchive::new(Cursor::new(src.bytes())).map_err(|e| ParseError::corrupt(format!("zip: {e}")))?;
        cx.limits.check(Limit::Entries, archive.len() as u64)?;
        let entries = (0..archive.len())
            .map(|i| {
                let f = archive.by_index_raw(i).map_err(|e| ParseError::corrupt(format!("zip entry {i}: {e}")))?;
                let name = f.name().map_err(|e| ParseError::corrupt(format!("zip entry {i}: {e}")))?.into_owned();
                Ok(if f.is_dir() { Entry::folder(name) } else { Entry::file(name, f.size()).with_packed(f.compressed_size()) })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { src: src.clone(), entries, limits: cx.limits.clone() })
    }
}

impl Container for ZipContainer {
    fn entry_count(&self) -> Count {
        Count::Exact(self.entries.len() as u64)
    }

    fn entries(&self, range: Range<u64>) -> Result<Vec<Entry>> {
        Ok(slice(&self.entries, range))
    }

    fn open(&self, index: u64) -> Result<FileSource> {
        entry_at(&self.entries, index)?;
        let mut archive = zip::ZipArchive::new(Cursor::new(self.src.bytes())).map_err(ParseError::corrupt)?;
        let file = archive.by_index(index as usize).map_err(|e| ParseError::corrupt(format!("zip entry {index}: {e}")))?;
        let (size, packed) = (file.size(), file.compressed_size());
        let name = file.name().map_err(ParseError::corrupt)?.into_owned();
        Ok(FileSource::from_bytes(name, read_capped(file, size, packed, &self.limits)?))
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.entries.iter().map(|e| e.path.len() as u64 + 48).sum())
    }
}

// ---------------------------------------------------------------- tar

pub struct TarReader;

static TAR: ReaderInfo = ReaderInfo::new("tar", "Tar archive", Category::Archives).extensions(&["tar", "tgz"]).mime_types(&["application/x-tar"]);

fn is_tar(header: &[u8]) -> bool {
    header.get(257..262) == Some(b"ustar")
}

fn is_gzip(header: &[u8]) -> bool {
    header.starts_with(&[0x1f, 0x8b])
}

impl Reader for TarReader {
    fn info(&self) -> &ReaderInfo {
        &TAR
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        let lower = name.to_ascii_lowercase();
        if is_tar(header) {
            Detection::new(Confidence::Magic)
        } else if is_gzip(header) && (lower.ends_with(".tar.gz") || lower.ends_with(".tgz")) {
            // Beats the plain gzip reader, so a .tar.gz opens as one archive.
            Detection::new(Confidence::Certain)
        } else if lower.ends_with(".tar") {
            Detection::new(Confidence::Extension)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let (data, format) = if is_gzip(src.header()) {
            let raw = read_capped(flate2::read::MultiGzDecoder::new(src.bytes()), src.len().saturating_mul(4), src.len(), &cx.limits)?;
            (FileSource::from_bytes(src.name(), raw), "Gzipped tar archive")
        } else {
            (src.clone(), "Tar archive")
        };
        let mut entries = Vec::new();
        let mut ranges = Vec::new();
        let mut archive = tar::Archive::new(data.bytes());
        for entry in archive.entries().map_err(ParseError::corrupt)? {
            let entry = entry.map_err(|e| ParseError::corrupt(format!("tar: {e}")))?;
            if entries.len() as u64 >= cx.limits.entries {
                return Err(ParseError::too_large(Limit::Entries, entries.len() as u64 + 1, cx.limits.entries));
            }
            let path = entry.path().map_err(ParseError::corrupt)?.to_string_lossy().into_owned();
            let start = entry.raw_file_position() as usize;
            let size = entry.size();
            entries.push(if entry.header().entry_type().is_dir() { Entry::folder(path) } else { Entry::file(path, size) });
            ranges.push(start..start.saturating_add(size as usize));
        }
        let listed = entries.clone();
        Ok(document("tar", format, src, Arc::new(TarContainer { data, entries, ranges }), &listed))
    }
}

/// The (inflated) tar's bytes; each entry is a view of them.
struct TarContainer {
    data: FileSource,
    entries: Vec<Entry>,
    ranges: Vec<Range<usize>>,
}

impl Container for TarContainer {
    fn entry_count(&self) -> Count {
        Count::Exact(self.entries.len() as u64)
    }

    fn entries(&self, range: Range<u64>) -> Result<Vec<Entry>> {
        Ok(slice(&self.entries, range))
    }

    fn open(&self, index: u64) -> Result<FileSource> {
        let entry = entry_at(&self.entries, index)?;
        let range = self.ranges[index as usize].clone();
        if range.end > self.data.bytes().len() {
            return Err(ParseError::corrupt(format!("{} runs past the end of the archive", entry.path)));
        }
        Ok(self.data.part(entry.path.clone(), range))
    }

    fn memory(&self) -> MemoryUse {
        let listing = MemoryUse::heap(self.entries.iter().map(|e| e.path.len() as u64 + 64).sum());
        // An inflated tar is on the heap; a plain one is the mapped file.
        listing + if self.data.path().is_none() { MemoryUse::heap(self.data.len()) } else { MemoryUse::mapped(self.data.len()) }
    }
}

// ---------------------------------------------------------------- gzip

/// A single gzipped file, as a container of one entry.
pub struct GzipReader;

static GZIP: ReaderInfo = ReaderInfo::new("gzip", "Gzip", Category::Archives).extensions(&["gz"]).mime_types(&["application/gzip"]);

impl Reader for GzipReader {
    fn info(&self) -> &ReaderInfo {
        &GZIP
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if is_gzip(header) {
            Detection::new(Confidence::Magic)
        } else if GZIP.matches_extension(name) {
            Detection::new(Confidence::Extension)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let raw = read_capped(flate2::read::MultiGzDecoder::new(src.bytes()), src.len().saturating_mul(4), src.len(), &cx.limits)?;
        let inner = src.name().strip_suffix(".gz").or_else(|| src.name().strip_suffix(".GZ")).unwrap_or("decompressed").to_owned();
        let entries = vec![Entry::file(inner.clone(), raw.len() as u64).with_packed(src.len())];
        let inner = FileSource::from_bytes(inner, raw);
        Ok(document("gzip", "Gzip", src, Arc::new(GzipContainer { inner, entries: entries.clone() }), &entries))
    }
}

struct GzipContainer {
    inner: FileSource,
    entries: Vec<Entry>,
}

impl Container for GzipContainer {
    fn entry_count(&self) -> Count {
        Count::Exact(1)
    }

    fn entries(&self, range: Range<u64>) -> Result<Vec<Entry>> {
        Ok(slice(&self.entries, range))
    }

    fn open(&self, index: u64) -> Result<FileSource> {
        entry_at(&self.entries, index)?;
        Ok(self.inner.clone())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.inner.len())
    }
}

// ---------------------------------------------------------------- folder

/// Most files a folder listing holds: past this, a folder is someone's whole disk.
pub const MAX_FOLDER_FILES: usize = 50_000;

/// A folder on disk as a container: every file under it, by path relative to it.
/// Entries open from their real paths, so a model inside still finds the textures next
/// to it. Hidden files (`.DS_Store`, `.git`) are left out, and symlinks are not
/// followed, so a link to `/` or a loop cannot run the listing away.
///
/// Only the folder itself must be readable. Subfolders that can't be read (a
/// privacy-protected folder on macOS, one outside an iOS grant) are skipped and
/// counted, and a folder with more than [`MAX_FOLDER_FILES`] files (or the context's
/// items limit, if lower) lists the first ones, so one corner of a big tree never hides
/// the rest.
pub fn open_folder(root: &Path, cx: &ParseContext) -> Result<Document> {
    let max = MAX_FOLDER_FILES.min(cx.limits.entries.min(usize::MAX as u64) as usize);
    let mut entries = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut dirs = vec![PathBuf::new()];
    'walk: while let Some(rel) = dirs.pop() {
        cx.cancel.check()?;
        let listing = match std::fs::read_dir(root.join(&rel)) {
            Ok(listing) => listing,
            Err(e) if rel.as_os_str().is_empty() => return Err(ParseError::corrupt(format!("reading {}: {e}", root.display()))),
            Err(_) => {
                skipped.push(slash_path(&rel));
                continue;
            }
        };
        for item in listing {
            // One unreadable item (a broken entry, a racing delete) doesn't end the listing.
            let Ok(item) = item else { continue };
            let name = item.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(kind) = item.file_type() else { continue };
            let path = rel.join(&name);
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() {
                if entries.len() == max {
                    truncated = true;
                    break 'walk;
                }
                let size = item.metadata().map_or(0, |m| m.len());
                entries.push(Entry::file(slash_path(&path), size));
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    skipped.sort();
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let files = entries.len();
    let container = FolderContainer { root: root.into(), entries };
    let mut doc = Document::new(FormatInfo::new("folder", "Folder", Category::Archives))
        .with_facet(Role::Primary, "Files", Content::Container(Arc::new(container)))
        .with_meta("Files", if truncated { format!("{files} (the first {max}; open a folder inside it for the rest)") } else { files.to_string() })
        .with_meta("Total size", format!("{total} bytes"));
    if !skipped.is_empty() {
        let shown: Vec<&str> = skipped.iter().take(5).map(String::as_str).collect();
        let more = if skipped.len() > shown.len() { format!(" and {} more", skipped.len() - shown.len()) } else { String::new() };
        doc = doc.with_meta("Not readable", format!("{} folder(s) skipped: {}{more}", skipped.len(), shown.join(", ")));
    }
    Ok(doc)
}

/// A relative path with `/` separators, as entries are named.
fn slash_path(p: &Path) -> String {
    p.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

struct FolderContainer {
    root: PathBuf,
    entries: Vec<Entry>,
}

impl Container for FolderContainer {
    fn entry_count(&self) -> Count {
        Count::Exact(self.entries.len() as u64)
    }

    fn entries(&self, range: Range<u64>) -> Result<Vec<Entry>> {
        Ok(slice(&self.entries, range))
    }

    fn open(&self, index: u64) -> Result<FileSource> {
        let path = self.root.join(&entry_at(&self.entries, index)?.path);
        FileSource::open(&path).map_err(|e| ParseError::corrupt(format!("{}: {e}", path.display())))
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.entries.iter().map(|e| e.path.len() as u64 + 48).sum())
    }
}
