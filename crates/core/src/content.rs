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
}

impl Document {
    pub fn new(format: impl Into<String>, content: Content) -> Self {
        Self {
            format: format.into(),
            content,
            metadata: Vec::new(),
        }
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        self.metadata.push((key.into(), value.to_string()));
        self
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
        }
    }
}

/// A decoded image in straight (non-premultiplied) RGBA8.
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
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
