use std::ops::Range;
use std::sync::Arc;

use crate::{FileSource, Result};

/// The result of loading a file: one content model plus descriptive metadata.
pub struct Document {
    /// Human-readable format name, e.g. "PNG image".
    pub format: String,
    pub content: Content,
    /// Key/value pairs shown in the info panel.
    pub metadata: Vec<(String, String)>,
    /// Files this one refers to (textures, layers, buffers) that couldn't be read.
    /// Shared with the loader's background work, so it can grow after loading.
    pub unreadable: Unreadables,
    /// For a file read through libfileparser: its document, whose facets the page can
    /// switch between without reading the file again, and the one shown.
    pub facets: Option<Facets>,
}

/// A libfileparser document behind a Viewer document, and which of its facets is shown.
#[derive(Clone)]
pub struct Facets {
    pub doc: std::sync::Arc<libfileparser_core::Document>,
    pub shown: usize,
}

impl Document {
    pub fn new(format: impl Into<String>, content: Content) -> Self {
        Self {
            format: format.into(),
            content,
            metadata: Vec::new(),
            unreadable: Unreadables::default(),
            facets: None,
        }
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        self.metadata.push((key.into(), value.to_string()));
        self
    }
}

pub use libfileparser_core::Unreadable;

/// One file a document couldn't read: its name as the document gave it, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreadableFile {
    pub name: String,
    pub reason: Unreadable,
}

/// The files a document couldn't read. A view of a libfileparser document's
/// [`Dependencies`](libfileparser_core::Dependencies), so files found unreadable by
/// background work (texture decoding) appear as they are found. Cheap to clone.
#[derive(Clone, Default, Debug)]
pub struct Unreadables(libfileparser_core::Dependencies);

impl Unreadables {
    pub fn from_dependencies(deps: libfileparser_core::Dependencies) -> Self {
        Self(deps)
    }

    /// Records `name` as unreadable for `reason`.
    pub fn add(&self, name: impl Into<String>, reason: Unreadable) {
        self.0.record(name, reason.into());
    }

    /// Records a failed read of `path` from its error, under the path's file name.
    pub fn add_io(&self, path: &std::path::Path, e: &std::io::Error) {
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        self.add(name, Unreadable::of(e));
    }

    pub fn list(&self) -> Vec<UnreadableFile> {
        use libfileparser_core::DependencyStatus;
        self.0
            .list()
            .into_iter()
            .filter_map(|d| match d.status {
                DependencyStatus::Blocked => Some(UnreadableFile { name: d.reference, reason: Unreadable::Blocked }),
                DependencyStatus::Missing => Some(UnreadableFile { name: d.reference, reason: Unreadable::Missing }),
                DependencyStatus::Outside => Some(UnreadableFile { name: d.reference, reason: Unreadable::Outside }),
                _ => None,
            })
            .collect()
    }

    /// The names that failed for `reason`.
    pub fn names(&self, reason: Unreadable) -> Vec<String> {
        self.0.with_status(reason.into())
    }

    pub fn count(&self, reason: Unreadable) -> usize {
        self.0.count(reason.into())
    }
}

/// The content models. Every format maps onto one of these; see PLAN.md §1.
pub enum Content {
    Raster(Raster),
    Text(TextDoc),
    Markdown(String),
    Tree(TreeNode),
    Table(Table),
    Container(Arc<dyn Container>),
    /// Raw bytes of the source file.
    Hex(FileSource),
    /// A 3D model: meshes, materials and nodes, built without a GPU.
    Scene(SceneDoc),
    /// Pages laid out one after another and rendered on demand (PDF).
    Pages(std::sync::Arc<dyn crate::PageSource>),
}

impl Content {
    pub fn model_name(&self) -> &'static str {
        match self {
            Content::Raster(_) => "Raster",
            Content::Text(_) => "Text",
            Content::Markdown(_) => "Rich text",
            Content::Tree(_) => "Tree",
            Content::Table(_) => "Table",
            Content::Container(_) => "Container",
            Content::Hex(_) => "Hex",
            Content::Scene(_) => "3D scene",
            Content::Pages(_) => "Pages",
        }
    }
}

/// A 3D model. The scene is shared with the renderer, which follows it as it fills in
/// and frees the CPU copy of what it has uploaded (docs/design/3d-renderer.md §11.1).
pub struct SceneDoc {
    pub scene: scene_data::CpuScene,
    /// Textures still being decoded on background threads; they arrive in `scene`
    /// one by one.
    pub textures_pending: Arc<std::sync::atomic::AtomicUsize>,
}

/// An image in straight (non-premultiplied) RGBA8: whole, or arriving in bands of
/// rows from a decoder thread (big images, so the whole never sits in memory).
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
}

pub enum Pixels {
    /// Every row, `width * height * 4` bytes.
    Full(Vec<u8>),
    /// Rows still coming, top to bottom.
    Bands(BandStream),
    /// The view has what it needs (e.g. on the GPU) and dropped the CPU copy.
    Released,
}

impl Raster {
    pub fn full(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self { width, height, pixels: Pixels::Full(rgba) }
    }

    /// The pixels when they are all here.
    pub fn rgba(&self) -> Option<&[u8]> {
        match &self.pixels {
            Pixels::Full(p) => Some(p),
            _ => None,
        }
    }

    /// Waits for a streamed image's remaining bands and assembles the whole (for
    /// views that need every pixel at once).
    pub fn make_full(&mut self) -> std::result::Result<(), String> {
        let Pixels::Bands(stream) = &self.pixels else { return Ok(()) };
        let mut rgba = vec![0u8; self.width as usize * self.height as usize * 4];
        let row = self.width as usize * 4;
        loop {
            match stream.wait_next() {
                Next::Band(b) => {
                    let start = b.y as usize * row;
                    let end = (start + b.rgba.len()).min(rgba.len());
                    rgba[start..end].copy_from_slice(&b.rgba[..end - start]);
                }
                Next::Done | Next::Pending => break,
                Next::Failed(e) => return Err(e),
            }
        }
        self.pixels = Pixels::Full(rgba);
        Ok(())
    }
}

/// Rows `y..y + rgba.len() / (width * 4)` of an image.
pub struct Band {
    pub y: u32,
    pub rgba: Vec<u8>,
}

pub enum Next {
    Band(Band),
    /// More is coming; not decoded yet.
    Pending,
    /// Every band has been delivered.
    Done,
    /// The decoder stopped: what came before is all there is.
    Failed(String),
}

type Wake = Arc<std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

/// Bands from a decoder thread. At most two wait at a time: a view that isn't
/// consuming them (hidden window) makes the decoder wait, so memory stays bounded,
/// and dropping the stream (closing the page) stops the decoder.
pub struct BandStream {
    rx: std::sync::Mutex<std::sync::mpsc::Receiver<std::result::Result<Band, String>>>,
    wake: Wake,
}

impl BandStream {
    /// Runs `produce` on a new thread. It hands each band to `emit`, which returns
    /// false once nobody is listening (stop then); an error ends the stream.
    pub fn spawn(produce: impl FnOnce(&mut dyn FnMut(Band) -> bool) -> std::result::Result<(), String> + Send + 'static) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        let wake: Wake = Arc::default();
        let notify = wake.clone();
        let ping = move || {
            let f = notify.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(f) = f {
                f();
            }
        };
        std::thread::Builder::new()
            .name("image decode".into())
            .spawn(move || {
                let mut emit = |band: Band| {
                    let ok = tx.send(Ok(band)).is_ok();
                    ping();
                    ok
                };
                if let Err(e) = produce(&mut emit) {
                    let _ = tx.send(Err(e));
                }
                drop(tx);
                ping();
            })
            .expect("spawning the image decoder");
        Self { rx: std::sync::Mutex::new(rx), wake }
    }

    /// Called (from the decoder thread) whenever a band arrives or the stream ends.
    pub fn set_wake(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
    }

    pub fn try_next(&self) -> Next {
        use std::sync::mpsc::TryRecvError;
        match self.rx.lock().unwrap_or_else(|e| e.into_inner()).try_recv() {
            Ok(Ok(b)) => Next::Band(b),
            Ok(Err(e)) => Next::Failed(e),
            Err(TryRecvError::Empty) => Next::Pending,
            Err(TryRecvError::Disconnected) => Next::Done,
        }
    }

    pub fn wait_next(&self) -> Next {
        match self.rx.lock().unwrap_or_else(|e| e.into_inner()).recv() {
            Ok(Ok(b)) => Next::Band(b),
            Ok(Err(e)) => Next::Failed(e),
            Err(_) => Next::Done,
        }
    }
}

/// A text file indexed by line. The bytes stay in the (usually memory-mapped)
/// source, so a 2 GB log costs only its line index.
pub struct TextDoc {
    pub source: FileSource,
    /// Byte offset of the start of each line.
    pub line_starts: Vec<usize>,
    /// Name of the detected syntax, if any.
    pub syntax: Option<String>,
    /// Pre-computed highlighting, one theme per entry (dark first, then light). Span
    /// ranges index into `String::from_utf8_lossy(raw_line(i))`.
    pub highlights: Option<[Vec<Vec<Span>>; 2]>,
}

impl TextDoc {
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Bytes of line `i` including its line terminator.
    pub fn raw_line(&self, i: usize) -> &[u8] {
        let bytes = self.source.bytes();
        let start = self.line_starts[i];
        let end = self.line_starts.get(i + 1).copied().unwrap_or(bytes.len());
        &bytes[start..end]
    }

    /// Bytes of line `i` without its line terminator.
    pub fn line(&self, i: usize) -> &[u8] {
        let mut line = self.raw_line(i);
        while let [rest @ .., b'\n' | b'\r'] = line {
            line = rest;
        }
        line
    }
}

/// A highlighted byte range within one line.
#[derive(Clone, Debug)]
pub struct Span {
    pub range: Range<usize>,
    pub color: [u8; 3],
    pub bold: bool,
    pub italic: bool,
}

/// A node in a hierarchical document (JSON, TOML, ...).
#[derive(Debug, Default)]
pub struct TreeNode {
    pub key: String,
    /// Displayed value for leaves, or a summary such as "{3}" / "[10]" for branches.
    pub value: String,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(TreeNode::count).sum::<usize>()
    }
}

pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Widest row (or the header), computed once so views don't rescan every frame.
    pub columns: usize,
}

impl Table {
    pub fn new(headers: Vec<String>, rows: Vec<Vec<String>>) -> Self {
        let columns = rows.iter().map(Vec::len).chain([headers.len()]).max().unwrap_or(0);
        Self { headers, rows, columns }
    }
}

/// One item inside a container (archive entry, embedded file, ...).
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub compressed_size: Option<u64>,
    pub is_dir: bool,
}

/// Anything that holds other files. Opening an entry yields a new source that goes
/// through detection again, so containers nest arbitrarily (zip in tar in gz...).
pub trait Container: Send + Sync {
    fn entries(&self) -> &[Entry];
    fn open_entry(&self, index: usize) -> Result<FileSource>;
}
