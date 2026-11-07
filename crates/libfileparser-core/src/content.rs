//! Content models: the shape of a facet's data (docs/design/libfileparser.md §3).
//!
//! Anything that can be large is a source read by range, so opening stays cheap
//! however big the file. Each source trait has a simple owned implementation here for
//! readers that build their content up front.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use scene_data::CpuScene;

use crate::{Bytes, FileSource, ParseError, Result};

/// One facet's data. Cheap to clone: every variant shares its data.
#[derive(Clone)]
#[non_exhaustive]
pub enum Content {
    Raster(Raster),
    Text(Arc<dyn TextSource>),
    Tree(Arc<dyn TreeSource>),
    Table(Arc<dyn TableSource>),
    Container(Arc<dyn Container>),
    Scene(SceneDoc),
    /// The bytes of a file, shown raw.
    Hex(FileSource),
}

/// Which content model a facet uses, known before the facet is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Model {
    Raster,
    Text,
    Tree,
    Table,
    Container,
    Scene,
    Hex,
}

impl Content {
    pub fn model(&self) -> Model {
        match self {
            Content::Raster(_) => Model::Raster,
            Content::Text(_) => Model::Text,
            Content::Tree(_) => Model::Tree,
            Content::Table(_) => Model::Table,
            Content::Container(_) => Model::Container,
            Content::Scene(_) => Model::Scene,
            Content::Hex(_) => Model::Hex,
        }
    }

    /// What this content holds.
    pub fn memory(&self) -> MemoryUse {
        match self {
            Content::Raster(r) => match &r.pixels {
                Pixels::Full(p) => MemoryUse::heap(p.len() as u64),
                Pixels::Bands(s) => s.memory(),
            },
            Content::Text(t) => t.memory(),
            Content::Tree(t) => t.memory(),
            Content::Table(t) => t.memory(),
            Content::Container(c) => c.memory(),
            Content::Scene(s) => MemoryUse::heap(s.scene.read().cpu_bytes()),
            Content::Hex(src) => MemoryUse::mapped(src.len()),
        }
    }
}

/// Bytes a document holds: on the heap, and mapped from files (which the OS can
/// page out and back in).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryUse {
    pub heap: u64,
    pub mapped: u64,
}

impl MemoryUse {
    pub fn heap(bytes: u64) -> Self {
        Self { heap: bytes, mapped: 0 }
    }

    pub fn mapped(bytes: u64) -> Self {
        Self { heap: 0, mapped: bytes }
    }

    pub fn total(&self) -> u64 {
        self.heap + self.mapped
    }
}

impl std::ops::Add for MemoryUse {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self { heap: self.heap + o.heap, mapped: self.mapped + o.mapped }
    }
}

// ---------------------------------------------------------------- raster

/// An image in straight (non-premultiplied) RGBA8: its size, then its pixels, either
/// already decoded or decoded band by band when asked.
#[derive(Clone)]
pub struct Raster {
    width: u32,
    height: u32,
    pixels: Pixels,
}

/// A raster's pixels.
#[derive(Clone)]
#[non_exhaustive]
pub enum Pixels {
    /// Every row: `width * height * 4` bytes.
    Full(Arc<[u8]>),
    /// Decoded when asked for, a band of rows at a time: a document holds only how to
    /// decode the image (its file and format), never the image, and an app decides
    /// where and how much to decode (a thread streaming bands to the GPU, or the whole
    /// image at once).
    Bands(Arc<dyn BandSource>),
}

/// Rows `y..y + rgba.len() / (width * 4)` of an image, as RGBA8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Band {
    pub y: u32,
    pub rgba: Vec<u8>,
}

/// An image's bands, top to bottom, each decoded when it's asked for.
pub type Bands = Box<dyn Iterator<Item = Result<Band>> + Send>;

/// Decodes an image band by band, on the calling thread: the library starts no thread
/// of its own (an app that wants decoding off its UI thread pulls from one of its own).
pub trait BandSource: Send + Sync {
    /// The bands from the top. Each call starts again, so the image can be shown twice
    /// without being kept; an error ends them.
    fn bands(&self) -> Result<Bands>;
    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
}

impl Raster {
    /// `rgba` must hold exactly `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self> {
        if rgba.len() as u64 != width as u64 * height as u64 * 4 {
            return Err(ParseError::corrupt(format!("{} bytes for a {width} × {height} image", rgba.len())));
        }
        Ok(Self { width, height, pixels: Pixels::Full(rgba.into()) })
    }

    /// An image decoded by `source` when asked for.
    pub fn from_bands(width: u32, height: u32, source: impl BandSource + 'static) -> Self {
        Self { width, height, pixels: Pixels::Bands(Arc::new(source)) }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &Pixels {
        &self.pixels
    }

    /// The pixels, when they are already decoded.
    pub fn rgba(&self) -> Option<&[u8]> {
        match &self.pixels {
            Pixels::Full(p) => Some(p),
            Pixels::Bands(_) => None,
        }
    }

    /// Every pixel at once: decoded now when the raster is in bands. A band that runs
    /// past the bottom, or a source that stops early, is an error.
    pub fn decode(&self) -> Result<Vec<u8>> {
        let source = match &self.pixels {
            Pixels::Full(p) => return Ok(p.to_vec()),
            Pixels::Bands(s) => s,
        };
        let row = self.width as usize * 4;
        let mut rgba = vec![0u8; row * self.height as usize];
        let mut filled = 0usize;
        for band in source.bands()? {
            let band = band?;
            let start = band.y as usize * row;
            let end = start.checked_add(band.rgba.len()).filter(|&e| e <= rgba.len()).ok_or_else(|| ParseError::corrupt("a band runs past the image"))?;
            rgba[start..end].copy_from_slice(&band.rgba);
            filled = filled.max(end);
        }
        if filled < rgba.len() {
            return Err(ParseError::corrupt(format!("the image ended at row {} of {}", filled / row.max(1), self.height)));
        }
        Ok(rgba)
    }
}

// ---------------------------------------------------------------- counts

/// How many items a source has. Counting may mean reading the whole file (lines of a
/// huge log, rows of a huge CSV), so a source may give a lower bound first and the
/// exact count later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Count {
    Exact(u64),
    /// At least this many; more may follow.
    AtLeast(u64),
}

impl Count {
    /// The count, or its lower bound.
    pub fn known(&self) -> u64 {
        match *self {
            Count::Exact(n) | Count::AtLeast(n) => n,
        }
    }

    pub fn is_exact(&self) -> bool {
        matches!(self, Count::Exact(_))
    }
}

impl fmt::Display for Count {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Count::Exact(n) => write!(f, "{n}"),
            Count::AtLeast(n) => write!(f, "{n}+"),
        }
    }
}

/// `range` cut to `0..len`: sources return fewer items past the end, never an error.
fn clamp(range: Range<u64>, len: u64) -> Range<usize> {
    let end = range.end.min(len);
    range.start.min(end) as usize..end as usize
}

// ---------------------------------------------------------------- text

/// What a text is, known without reading its lines.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TextInfo {
    pub lines: Count,
    pub encoding: String,
    /// The language it's written in, if known ("usd", "json", "rust").
    pub language: Option<String>,
}

impl TextInfo {
    pub fn new(lines: Count) -> Self {
        Self { lines, encoding: "UTF-8".into(), language: None }
    }

    pub fn with_encoding(mut self, encoding: impl Into<String>) -> Self {
        self.encoding = encoding.into();
        self
    }

    pub fn with_language(mut self, language: Option<&str>) -> Self {
        self.language = language.map(str::to_owned);
        self
    }
}

/// Text read by line.
pub trait TextSource: Send + Sync {
    fn info(&self) -> TextInfo;
    /// Lines in `range`, without their terminators; fewer past the end.
    fn lines(&self, range: Range<u64>) -> Result<Vec<Bytes>>;
    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
}

/// Text indexed by line over a file's bytes, which stay where they are (usually
/// mapped): a 2 GB log costs only its line index, and lines are handed out as views
/// into the file, not copies.
pub struct LineText {
    source: FileSource,
    starts: Vec<usize>,
    language: Option<String>,
}

impl LineText {
    pub fn new(source: FileSource, language: Option<&str>) -> Self {
        let bytes = source.bytes();
        let mut starts = vec![0];
        starts.extend(bytes.iter().enumerate().filter(|&(_, &b)| b == b'\n').map(|(i, _)| i + 1).filter(|&s| s < bytes.len()));
        if bytes.is_empty() {
            starts.clear();
        }
        Self { source, starts, language: language.map(str::to_owned) }
    }
}

impl TextSource for LineText {
    fn info(&self) -> TextInfo {
        TextInfo::new(Count::Exact(self.starts.len() as u64)).with_language(self.language.as_deref())
    }

    fn lines(&self, range: Range<u64>) -> Result<Vec<Bytes>> {
        let bytes = self.source.bytes();
        let shared = self.source.shared_bytes();
        Ok(clamp(range, self.starts.len() as u64)
            .map(|i| {
                let start = self.starts[i];
                let mut end = self.starts.get(i + 1).copied().unwrap_or(bytes.len());
                while end > start && matches!(bytes[end - 1], b'\n' | b'\r') {
                    end -= 1;
                }
                Bytes::slice_of(shared.clone(), start..end)
            })
            .collect())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap((self.starts.len() * size_of::<usize>()) as u64) + MemoryUse::mapped(self.source.len())
    }
}

// ---------------------------------------------------------------- tree

/// A node's handle within its tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);

/// What a tree shows for one node.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TreeNode {
    pub id: NodeId,
    /// The key or name: "Cube", "materials", `[3]`.
    pub label: String,
    /// A value for leaves, or a summary for branches ("{3}", "Mesh").
    pub value: String,
    pub has_children: bool,
}

impl TreeNode {
    pub fn new(id: NodeId, label: impl Into<String>, value: impl Into<String>, has_children: bool) -> Self {
        Self { id, label: label.into(), value: value.into(), has_children }
    }
}

/// A tree read one level, and one range of children, at a time: a node may have
/// 100,000 children (FBX records), and a view shows a screenful.
pub trait TreeSource: Send + Sync {
    /// How many children `parent` has, or how many top-level nodes for `None`.
    fn child_count(&self, parent: Option<NodeId>) -> Result<Count>;
    /// The children of `parent` (or the top level) in `range`; fewer past the end.
    /// An id this tree didn't hand out is [`ParseError::InvalidRequest`].
    fn children(&self, parent: Option<NodeId>, range: Range<u64>) -> Result<Vec<TreeNode>>;
    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
    /// The node a path names, in the tree's own path syntax (a USD prim path, a JSON
    /// pointer), so an app or a link can go straight to it. `None` when no node has that
    /// path, or the tree has no paths.
    fn find(&self, path: &str) -> Result<Option<NodeId>> {
        let _ = path;
        Ok(None)
    }
    /// A node's path, the inverse of [`Self::find`].
    fn path(&self, node: NodeId) -> Result<Option<String>> {
        let _ = node;
        Ok(None)
    }
}

/// A tree built up front. Nodes added with [`OwnedTree::add_at`] have a path.
#[derive(Default)]
pub struct OwnedTree {
    nodes: Vec<(TreeNode, Vec<NodeId>)>,
    roots: Vec<NodeId>,
    paths: std::collections::HashMap<String, NodeId>,
    path_of: std::collections::HashMap<NodeId, String>,
}

impl OwnedTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node under `parent` (or at the top) and returns its id.
    pub fn add(&mut self, parent: Option<NodeId>, label: impl Into<String>, value: impl Into<String>) -> NodeId {
        let id = NodeId(self.nodes.len() as u64);
        self.nodes.push((TreeNode::new(id, label, value, false), Vec::new()));
        match parent {
            Some(p) => {
                let (node, children) = &mut self.nodes[p.0 as usize];
                node.has_children = true;
                children.push(id);
            }
            None => self.roots.push(id),
        }
        id
    }

    /// Adds a node with a path that [`TreeSource::find`] finds it by. A path already
    /// taken keeps its first node.
    pub fn add_at(&mut self, parent: Option<NodeId>, label: impl Into<String>, value: impl Into<String>, path: impl Into<String>) -> NodeId {
        let id = self.add(parent, label, value);
        let path = path.into();
        if !self.paths.contains_key(&path) {
            self.paths.insert(path.clone(), id);
            self.path_of.insert(id, path);
        }
        id
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn ids(&self, parent: Option<NodeId>) -> Result<&[NodeId]> {
        match parent {
            None => Ok(&self.roots),
            Some(p) => self.nodes.get(p.0 as usize).map(|(_, c)| c.as_slice()).ok_or_else(|| ParseError::invalid(format!("no node {}", p.0))),
        }
    }
}

impl TreeSource for OwnedTree {
    fn child_count(&self, parent: Option<NodeId>) -> Result<Count> {
        Ok(Count::Exact(self.ids(parent)?.len() as u64))
    }

    fn children(&self, parent: Option<NodeId>, range: Range<u64>) -> Result<Vec<TreeNode>> {
        let ids = self.ids(parent)?;
        Ok(ids[clamp(range, ids.len() as u64)].iter().map(|id| self.nodes[id.0 as usize].0.clone()).collect())
    }

    fn memory(&self) -> MemoryUse {
        let text: usize = self.nodes.iter().map(|(n, c)| n.label.len() + n.value.len() + c.len() * 8).sum();
        let paths: usize = self.paths.keys().map(|p| 2 * p.len() + 48).sum();
        MemoryUse::heap((text + paths + self.nodes.len() * size_of::<(TreeNode, Vec<NodeId>)>()) as u64)
    }

    fn find(&self, path: &str) -> Result<Option<NodeId>> {
        Ok(self.paths.get(path).copied())
    }

    fn path(&self, node: NodeId) -> Result<Option<String>> {
        Ok(self.path_of.get(&node).cloned())
    }
}

// ---------------------------------------------------------------- table

/// What a table is, known without reading its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TableInfo {
    pub columns: Vec<String>,
    pub rows: Count,
}

impl TableInfo {
    pub fn new(columns: Vec<String>, rows: Count) -> Self {
        Self { columns, rows }
    }
}

/// A table read by row range.
pub trait TableSource: Send + Sync {
    fn info(&self) -> TableInfo;
    /// The cells of rows in `range`; fewer past the end.
    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<String>>>;
    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
}

/// A table built up front.
pub struct OwnedTable {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl OwnedTable {
    pub fn new(columns: Vec<String>, rows: Vec<Vec<String>>) -> Self {
        Self { columns, rows }
    }
}

impl TableSource for OwnedTable {
    fn info(&self) -> TableInfo {
        TableInfo::new(self.columns.clone(), Count::Exact(self.rows.len() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<String>>> {
        Ok(self.rows[clamp(range, self.rows.len() as u64)].to_vec())
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.rows.iter().flatten().chain(&self.columns).map(|c| c.len() as u64 + 24).sum())
    }
}

// ---------------------------------------------------------------- container

/// Whether an entry is a file or a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EntryKind {
    File,
    Folder,
}

/// One item inside a container.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Entry {
    /// From the container's root, separated by `/`.
    pub path: String,
    pub size: u64,
    /// Compressed size, where the container compresses.
    pub packed: Option<u64>,
    pub kind: EntryKind,
}

impl Entry {
    pub fn file(path: impl Into<String>, size: u64) -> Self {
        Self { path: path.into(), size, packed: None, kind: EntryKind::File }
    }

    pub fn folder(path: impl Into<String>) -> Self {
        Self { path: path.into(), size: 0, packed: None, kind: EntryKind::Folder }
    }

    pub fn with_packed(mut self, packed: u64) -> Self {
        self.packed = Some(packed);
        self
    }
}

/// Files inside a file: a zip's entries, a GLB's chunks, a USDZ's layers. Read by
/// range, like every source: an archive may hold a million entries.
pub trait Container: Send + Sync {
    fn entry_count(&self) -> Count;
    /// Entries in `range`; fewer past the end.
    fn entries(&self, range: Range<u64>) -> Result<Vec<Entry>>;
    /// Entry `index` as a file the front door can open like any other. An index past
    /// the end is [`ParseError::InvalidRequest`].
    fn open(&self, index: u64) -> Result<FileSource>;
    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
}

// ---------------------------------------------------------------- scene

/// A 3D scene: `scene-data` data (meshes, materials, textures, nodes), built
/// without a GPU. Textures may still be arriving from background work.
#[derive(Clone)]
pub struct SceneDoc {
    scene: CpuScene,
    pending: Arc<AtomicUsize>,
}

impl SceneDoc {
    /// `pending` counts textures still being decoded; it reaches zero when all have
    /// arrived (or failed).
    pub fn new(scene: CpuScene, pending: Arc<AtomicUsize>) -> Self {
        Self { scene, pending }
    }

    pub fn scene(&self) -> &CpuScene {
        &self.scene
    }

    pub fn textures_pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    /// The shared count behind [`SceneDoc::textures_pending`], for hosts that keep
    /// their own handle to it.
    pub fn pending_counter(&self) -> Arc<AtomicUsize> {
        self.pending.clone()
    }
}
