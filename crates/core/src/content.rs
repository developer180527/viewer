use std::ops::Range;


use crate::FileSource;

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

/// What a page shows: one of libfileparser's content models (shown by libviewer's
/// views), or one of Viewer's own that libfileparser has no model for yet.
pub enum Content {
    /// Raster, Text, Tree, Table, Container, Hex or Scene: libfileparser's models.
    Model(libfileparser_core::Content),
    /// Text with Viewer's syntax highlighting (its text loader).
    Text(TextDoc),
}

impl Content {
    pub fn model_name(&self) -> &'static str {
        match self {
            Content::Model(c) => match c {
                libfileparser_core::Content::Raster(_) => "Raster",
                libfileparser_core::Content::Text(_) => "Text",
                libfileparser_core::Content::Tree(_) => "Tree",
                libfileparser_core::Content::Table(_) => "Table",
                libfileparser_core::Content::Container(_) => "Container",
                libfileparser_core::Content::Hex(_) => "Hex",
                libfileparser_core::Content::Scene(_) => "3D scene",
                libfileparser_core::Content::Pages(_) => "Pages",
                libfileparser_core::Content::Flow(_) => "Rich text",
                _ => "Other",
            },
            Content::Text(_) => "Text",
        }
    }

    /// A file's bytes, shown raw.
    pub fn hex(src: FileSource) -> Self {
        Content::Model(libfileparser_core::Content::Hex(src))
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


/// A highlighted text read as libfileparser's text, so libviewer's text view shows it.
impl libfileparser_core::TextSource for TextDoc {
    fn info(&self) -> libfileparser_core::TextInfo {
        libfileparser_core::TextInfo::new(libfileparser_core::Count::Exact(self.line_count() as u64))
    }

    fn lines(&self, range: Range<u64>) -> libfileparser_core::Result<Vec<libfileparser_core::Bytes>> {
        let end = range.end.min(self.line_count() as u64) as usize;
        let start = (range.start as usize).min(end);
        let shared = self.source.shared_bytes();
        Ok((start..end)
            .map(|i| {
                let from = self.line_starts[i];
                libfileparser_core::Bytes::slice_of(shared.clone(), from..from + self.line(i).len())
            })
            .collect())
    }

    fn memory(&self) -> libfileparser_core::MemoryUse {
        libfileparser_core::MemoryUse::heap((self.line_starts.len() * size_of::<usize>()) as u64)
    }
}
