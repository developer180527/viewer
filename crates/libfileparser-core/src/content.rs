//! Content models: the shape of a facet's data (docs/design/libfileparser.md §3).
//!
//! Anything that can be large is a source read by range, so opening stays cheap
//! however big the file. Each source trait has a simple owned implementation here for
//! readers that build their content up front.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rendererx_scene::CpuScene;

use crate::{FileSource, ParseError, Result};

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
            Content::Raster(r) => MemoryUse::heap(r.rgba.len() as u64),
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

/// An image in straight (non-premultiplied) RGBA8.
#[derive(Clone)]
pub struct Raster {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
}

impl Raster {
    /// `rgba` must hold exactly `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self> {
        if rgba.len() as u64 != width as u64 * height as u64 * 4 {
            return Err(ParseError::corrupt(format!("{} bytes for a {width} × {height} image", rgba.len())));
        }
        Ok(Self { width, height, rgba: rgba.into() })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

// ---------------------------------------------------------------- text

/// Text read by line.
pub trait TextSource: Send + Sync {
    fn line_count(&self) -> usize;
    /// Line `i` without its terminator; empty past the end.
    fn line(&self, i: usize) -> &[u8];
    /// The language it's written in, if known ("usd", "json", "rust").
    fn language(&self) -> Option<&str> {
        None
    }
    fn encoding(&self) -> &str {
        "UTF-8"
    }
    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
}

/// Text indexed by line over a file's bytes, which stay where they are (usually
/// mapped): a 2 GB log costs only its line index.
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
    fn line_count(&self) -> usize {
        self.starts.len()
    }

    fn line(&self, i: usize) -> &[u8] {
        let bytes = self.source.bytes();
        let Some(&start) = self.starts.get(i) else { return &[] };
        let end = self.starts.get(i + 1).copied().unwrap_or(bytes.len());
        let mut line = &bytes[start..end];
        while let [rest @ .., b'\n' | b'\r'] = line {
            line = rest;
        }
        line
    }

    fn language(&self) -> Option<&str> {
        self.language.as_deref()
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TreeNode {
    /// The key or name: "Cube", "materials", "[3]".
    pub label: String,
    /// A value for leaves, or a summary for branches ("{3}", "Mesh").
    pub value: String,
    pub has_children: bool,
}

/// A tree read one level at a time.
pub trait TreeSource: Send + Sync {
    /// The children of `parent`, or the top level for `None`.
    fn children(&self, parent: Option<NodeId>) -> Vec<NodeId>;
    fn node(&self, id: NodeId) -> TreeNode;
    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
}

/// A tree built up front.
#[derive(Default)]
pub struct OwnedTree {
    nodes: Vec<(TreeNode, Vec<NodeId>)>,
    roots: Vec<NodeId>,
}

impl OwnedTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node under `parent` (or at the top) and returns its id.
    pub fn add(&mut self, parent: Option<NodeId>, label: impl Into<String>, value: impl Into<String>) -> NodeId {
        let id = NodeId(self.nodes.len() as u64);
        self.nodes.push((TreeNode { label: label.into(), value: value.into(), has_children: false }, Vec::new()));
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

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

impl TreeSource for OwnedTree {
    fn children(&self, parent: Option<NodeId>) -> Vec<NodeId> {
        match parent {
            Some(p) => self.nodes.get(p.0 as usize).map(|(_, c)| c.clone()).unwrap_or_default(),
            None => self.roots.clone(),
        }
    }

    fn node(&self, id: NodeId) -> TreeNode {
        self.nodes.get(id.0 as usize).map(|(n, _)| n.clone()).unwrap_or_default()
    }

    fn memory(&self) -> MemoryUse {
        let text: usize = self.nodes.iter().map(|(n, c)| n.label.len() + n.value.len() + c.len() * 8).sum();
        MemoryUse::heap((text + self.nodes.len() * size_of::<(TreeNode, Vec<NodeId>)>()) as u64)
    }
}

// ---------------------------------------------------------------- table

/// How many rows a table has, when counting all of them would mean reading the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RowCount {
    Exact(usize),
    /// At least this many; more may follow.
    AtLeast(usize),
}

/// A table read by row range.
pub trait TableSource: Send + Sync {
    fn columns(&self) -> Vec<String>;
    fn row_count(&self) -> RowCount;
    /// The cells of rows in `range` (fewer past the end).
    fn rows(&self, range: Range<usize>) -> Vec<Vec<String>>;
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
    fn columns(&self) -> Vec<String> {
        self.columns.clone()
    }

    fn row_count(&self) -> RowCount {
        RowCount::Exact(self.rows.len())
    }

    fn rows(&self, range: Range<usize>) -> Vec<Vec<String>> {
        let end = range.end.min(self.rows.len());
        self.rows.get(range.start.min(end)..end).map(<[_]>::to_vec).unwrap_or_default()
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

/// Files inside a file: a zip's entries, a GLB's chunks, a USDZ's layers.
pub trait Container: Send + Sync {
    fn entries(&self) -> &[Entry];
    /// The bytes of entry `index`, as a file the front door can open like any other.
    fn open(&self, index: usize) -> Result<FileSource>;
    fn memory(&self) -> MemoryUse {
        MemoryUse::default()
    }
}

// ---------------------------------------------------------------- scene

/// A 3D scene: `rendererx-scene` data (meshes, materials, textures, nodes), built
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
