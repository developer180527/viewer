//! Archive containers. Entries are opened lazily and re-enter format detection, so
//! a PNG inside a tar inside a gzip just works.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use viewer_core::{Confidence, Container, Content, Document, Entry, FileSource, FormatLoader, Result};

pub static LOADERS: &[&dyn FormatLoader] = &[&ZipLoader, &TarLoader, &GzipLoader];

/// Refuse to inflate a single entry beyond this (decompression bombs).
const MAX_ENTRY_SIZE: u64 = 2 << 30;

fn document(format: &str, container: Arc<dyn Container>) -> Document {
    let files = container.entries().iter().filter(|e| !e.is_dir).count();
    let total: u64 = container.entries().iter().map(|e| e.size).sum();
    Document::new(format, Content::Container(container))
        .with_meta("Files", files)
        .with_meta("Uncompressed size", format!("{total} bytes"))
}

/// Most we reserve up front from a size hint. Hints come from the archive itself, so
/// a tiny crafted file could otherwise make us allocate `MAX_ENTRY_SIZE` at once.
const MAX_PREALLOC: u64 = 64 << 20;

/// Inflates `reader`, refusing past what `packed` bytes may reasonably become
/// (`viewer_core::limits::unpack_cap`): a decompression bomb stops at the cap rather
/// than at [`MAX_ENTRY_SIZE`].
fn read_capped(reader: impl Read, hint: u64, packed: u64) -> Result<Vec<u8>> {
    let cap = viewer_core::limits::unpack_cap(packed, MAX_ENTRY_SIZE);
    let mut out = Vec::with_capacity(hint.min(MAX_PREALLOC).min(cap) as usize);
    reader.take(cap + 1).read_to_end(&mut out)?;
    if out.len() as u64 > cap {
        bail!("inflates past {cap} bytes from {packed}; refusing to extract (a decompression bomb?)");
    }
    Ok(out)
}

// ---------------------------------------------------------------- zip

pub struct ZipLoader;

impl FormatLoader for ZipLoader {
    fn id(&self) -> &'static str {
        "zip"
    }

    fn name(&self) -> &'static str {
        "Zip archive"
    }

    fn probe(&self, header: &[u8], ext: Option<&str>, _: &str) -> Confidence {
        if header.starts_with(b"PK\x03\x04") || header.starts_with(b"PK\x05\x06") {
            Confidence::MAGIC
        } else if matches!(ext, Some("zip" | "jar" | "apk" | "cbz" | "epub" | "docx" | "xlsx" | "pptx" | "odt")) {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let mut archive = zip::ZipArchive::new(Cursor::new(src.bytes()))?;
        let entries = (0..archive.len())
            .map(|i| {
                let f = archive.by_index_raw(i)?;
                Ok(Entry {
                    path: f.name()?.into_owned(),
                    size: f.size(),
                    compressed_size: Some(f.compressed_size()),
                    is_dir: f.is_dir(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(document("Zip archive", Arc::new(ZipContainer { src: src.clone(), entries })))
    }
}

struct ZipContainer {
    src: FileSource,
    entries: Vec<Entry>,
}

impl Container for ZipContainer {
    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn open_entry(&self, index: usize) -> Result<FileSource> {
        let mut archive = zip::ZipArchive::new(Cursor::new(self.src.bytes()))?;
        let file = archive.by_index(index)?;
        let (size, packed) = (file.size(), file.compressed_size());
        let name = file.name()?.into_owned();
        Ok(FileSource::from_bytes(name, read_capped(file, size, packed)?))
    }
}

// ---------------------------------------------------------------- tar

pub struct TarLoader;

fn is_tar(header: &[u8]) -> bool {
    header.get(257..262) == Some(b"ustar")
}

impl FormatLoader for TarLoader {
    fn id(&self) -> &'static str {
        "tar"
    }

    fn name(&self) -> &'static str {
        "Tar archive"
    }

    fn probe(&self, header: &[u8], ext: Option<&str>, name: &str) -> Confidence {
        let name = name.to_ascii_lowercase();
        if is_tar(header) {
            Confidence::MAGIC
        } else if header.starts_with(&[0x1f, 0x8b]) && (name.ends_with(".tar.gz") || ext == Some("tgz")) {
            // Beats the plain gzip loader so .tar.gz opens as one archive.
            Confidence(110)
        } else if ext == Some("tar") {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let (data, format) = if src.bytes().starts_with(&[0x1f, 0x8b]) {
            let raw = read_capped(flate2::read::MultiGzDecoder::new(src.bytes()), src.len() * 4, src.len())?;
            (FileSource::from_bytes(src.name(), raw), "Gzipped tar archive")
        } else {
            (src.clone(), "Tar archive")
        };

        let mut entries = Vec::new();
        let mut offsets = Vec::new();
        let mut archive = tar::Archive::new(data.bytes());
        for entry in archive.entries()? {
            let entry = entry?;
            let header = entry.header();
            entries.push(Entry {
                path: entry.path()?.to_string_lossy().into_owned(),
                size: entry.size(),
                compressed_size: None,
                is_dir: header.entry_type().is_dir(),
            });
            offsets.push(entry.raw_file_position() as usize);
        }
        Ok(document(format, Arc::new(TarContainer { data, entries, offsets })))
    }
}

/// Holds the (decompressed) tar bytes; entries are plain slices of it.
struct TarContainer {
    data: FileSource,
    entries: Vec<Entry>,
    offsets: Vec<usize>,
}

impl Container for TarContainer {
    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn open_entry(&self, index: usize) -> Result<FileSource> {
        let entry = &self.entries[index];
        let start = self.offsets[index];
        let bytes = self
            .data
            .bytes()
            .get(start..start + entry.size as usize)
            .context("entry extends past end of archive")?;
        Ok(FileSource::from_bytes(entry.path.clone(), bytes.to_vec()))
    }
}

// ---------------------------------------------------------------- gzip

/// A single gzipped file, shown as a container with one entry.
pub struct GzipLoader;

impl FormatLoader for GzipLoader {
    fn id(&self) -> &'static str {
        "gzip"
    }

    fn name(&self) -> &'static str {
        "Gzip"
    }

    fn probe(&self, header: &[u8], _: Option<&str>, _: &str) -> Confidence {
        if header.starts_with(&[0x1f, 0x8b]) { Confidence::MAGIC } else { Confidence::NONE }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let raw = read_capped(flate2::read::MultiGzDecoder::new(src.bytes()), src.len() * 4, src.len())?;
        let inner = src
            .name()
            .strip_suffix(".gz")
            .or_else(|| src.name().strip_suffix(".GZ"))
            .unwrap_or("decompressed")
            .to_owned();
        let entries = vec![Entry {
            path: inner.clone(),
            size: raw.len() as u64,
            compressed_size: Some(src.len()),
            is_dir: false,
        }];
        let inner = FileSource::from_bytes(inner, raw);
        Ok(document("Gzip", Arc::new(GzipContainer { inner, entries })))
    }
}

struct GzipContainer {
    inner: FileSource,
    entries: Vec<Entry>,
}

impl Container for GzipContainer {
    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn open_entry(&self, _: usize) -> Result<FileSource> {
        Ok(self.inner.clone())
    }
}

// ---------------------------------------------------------------- folder

/// Most files a folder listing holds: past this, a folder is someone's whole disk.
const MAX_FOLDER_FILES: usize = 50_000;

/// A folder on disk as a container: every file under it, by path relative to it.
/// Entries open from their real paths, so a model inside still finds the textures
/// next to it. Hidden files (`.DS_Store`, `.git`) are left out, and symlinks are not
/// followed, so a link to `/` or a loop cannot run the listing away.
///
/// Only the folder itself must be readable. Subfolders that can't be read (a
/// privacy-protected folder on macOS, one outside an iOS grant) are skipped and
/// counted, and a folder with more than [`MAX_FOLDER_FILES`] files lists the first
/// ones, so one corner of a big tree never hides the rest.
pub fn open_folder(root: &Path) -> Result<Document> {
    let mut entries = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut dirs = vec![PathBuf::new()];
    'walk: while let Some(rel) = dirs.pop() {
        let listing = match std::fs::read_dir(root.join(&rel)) {
            Ok(listing) => listing,
            Err(e) if rel.as_os_str().is_empty() => return Err(e).with_context(|| format!("reading {}", root.display())),
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
                if entries.len() == MAX_FOLDER_FILES {
                    truncated = true;
                    break 'walk;
                }
                let size = item.metadata().map_or(0, |m| m.len());
                entries.push(Entry { path: slash_path(&path), size, compressed_size: None, is_dir: false });
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    skipped.sort();
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let files = entries.len();
    let mut doc = Document::new("Folder", Content::Container(Arc::new(FolderContainer { root: root.into(), entries })))
        .with_meta("Files", if truncated { format!("{files} (the first {MAX_FOLDER_FILES}; open a folder inside it for the rest)") } else { files.to_string() })
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
    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn open_entry(&self, index: usize) -> Result<FileSource> {
        let path = self.root.join(&self.entries[index].path);
        FileSource::open(&path).with_context(|| path.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn container(doc: Document) -> Arc<dyn Container> {
        match doc.content {
            Content::Container(c) => c,
            _ => panic!("not a container"),
        }
    }

    #[test]
    fn zip_round_trip() {
        let mut buf = Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        w.start_file("dir/hello.txt", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b"hello").unwrap();
        w.finish().unwrap();

        let src = FileSource::from_bytes("a.zip", buf.into_inner());
        let c = container(ZipLoader.open(&src).unwrap());
        assert_eq!(c.entries()[0].path, "dir/hello.txt");
        assert_eq!(c.open_entry(0).unwrap().bytes(), b"hello");
    }

    #[test]
    fn tar_gz_round_trip() {
        let mut tar = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(3);
        h.set_cksum();
        tar.append_data(&mut h, "a/b.txt", &b"abc"[..]).unwrap();
        let tar = tar.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();

        let src = FileSource::from_bytes("x.tar.gz", gz.finish().unwrap());
        assert!(TarLoader.probe(src.header(), src.ext().as_deref(), src.name()) > GzipLoader.probe(src.header(), None, ""));
        let c = container(TarLoader.open(&src).unwrap());
        assert_eq!(c.entries()[0].path, "a/b.txt");
        assert_eq!(c.open_entry(0).unwrap().bytes(), b"abc");
    }

    #[test]
    fn folder_lists_files_under_it_and_opens_them_from_disk() {
        let root = std::env::temp_dir().join(format!("viewer-folder-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("textures")).unwrap();
        std::fs::write(root.join("model.obj"), b"v 0 0 0").unwrap();
        std::fs::write(root.join("textures/albedo.png"), b"png").unwrap();
        std::fs::write(root.join(".DS_Store"), b"junk").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), b"ref").unwrap();

        let c = container(open_folder(&root).unwrap());
        let paths: Vec<&str> = c.entries().iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["model.obj", "textures/albedo.png"]);
        let src = c.open_entry(1).unwrap();
        assert_eq!(src.bytes(), b"png");
        // A real path, so a loader can find the files next to it.
        assert_eq!(src.path(), Some(root.join("textures/albedo.png").as_path()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_subfolders_are_skipped_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("viewer-folder-locked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("locked")).unwrap();
        std::fs::write(root.join("model.obj"), b"v 0 0 0").unwrap();
        std::fs::write(root.join("locked/secret.png"), b"x").unwrap();
        std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let doc = open_folder(&root);
        std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let doc = doc.unwrap();
        let note = doc.metadata.iter().find(|(k, _)| k == "Not readable").map(|(_, v)| v.clone());
        let c = container(doc);
        let paths: Vec<&str> = c.entries().iter().map(|e| e.path.as_str()).collect();
        // Root runs this as root on some CI images, where nothing is unreadable.
        if paths.len() == 1 {
            assert_eq!(paths, ["model.obj"]);
            assert!(note.unwrap().contains("locked"));
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
