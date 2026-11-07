//! The `Reader` trait every format implements, and choosing among readers
//! (docs/design/libfileparser.md §4).

use std::path::Path;

use crate::{Document, FileSource, Model, ParseContext, ParseError, Result};

/// The categories of file, one library each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Category {
    Images,
    ThreeD,
    Docs,
    Data,
    Text,
    Archives,
    Media,
}

/// How sure a reader is that a file is its format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Confidence {
    None,
    /// The name's extension matches.
    Extension,
    /// The content looks right, without a signature to prove it.
    Content,
    /// A signature (magic number) matches.
    Magic,
    /// A signature and the name both say this format: a format inside a generic
    /// container (a USDZ, a DOCX) outranks the container's own reader.
    Certain,
}

/// What `detect` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Detection {
    pub confidence: Confidence,
    /// For formats written as text: their language, so a Source facet can be read
    /// as it ("usd", "json", "obj").
    pub language: Option<&'static str>,
}

impl Detection {
    pub const NONE: Detection = Detection { confidence: Confidence::None, language: None };

    pub fn new(confidence: Confidence) -> Self {
        Self { confidence, language: None }
    }

    pub fn with_language(mut self, language: &'static str) -> Self {
        self.language = Some(language);
        self
    }

    pub fn is_match(&self) -> bool {
        self.confidence > Confidence::None
    }
}

/// What a reader reads, for listings and detection by name.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct ReaderInfo {
    /// Short and stable, used to choose a reader by name: "gltf", "usd".
    pub id: &'static str,
    /// For people: "glTF".
    pub name: &'static str,
    pub category: Category,
    /// The formats it owns, by extension: lower case, without the dot.
    pub extensions: &'static [&'static str],
    pub mime_types: &'static [&'static str],
    /// Formats another category owns that this reader also reads, from its own angle
    /// (`-data` reads an SVG as an XML tree; docs/design/libfileparser.md §2.1). Used
    /// alone, the reader opens them; in the front door the owner opens them, and this
    /// reader's primary facet is added as a [`Role::Structure`](crate::Role) facet.
    pub also_reads: &'static [&'static str],
    /// The model of that facet.
    pub also_reads_model: Model,
}

impl ReaderInfo {
    pub const fn new(id: &'static str, name: &'static str, category: Category) -> Self {
        Self { id, name, category, extensions: &[], mime_types: &[], also_reads: &[], also_reads_model: Model::Tree }
    }

    pub const fn extensions(mut self, extensions: &'static [&'static str]) -> Self {
        self.extensions = extensions;
        self
    }

    pub const fn mime_types(mut self, mime_types: &'static [&'static str]) -> Self {
        self.mime_types = mime_types;
        self
    }

    /// Formats owned elsewhere that this reader also reads, as a facet of `model`.
    pub const fn also_reads(mut self, extensions: &'static [&'static str], model: Model) -> Self {
        self.also_reads = extensions;
        self.also_reads_model = model;
        self
    }

    /// Whether `name` ends in one of the extensions this reader owns.
    pub fn matches_extension(&self, name: &str) -> bool {
        has_extension(self.extensions, name)
    }

    /// Whether `name` ends in one of the extensions this reader only also reads.
    pub fn matches_also_reads(&self, name: &str) -> bool {
        has_extension(self.also_reads, name)
    }
}

fn has_extension(extensions: &[&str], name: &str) -> bool {
    Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// One format. Implement it to add a format to a category (docs/design/libfileparser.md §6).
pub trait Reader: Sync {
    fn info(&self) -> &ReaderInfo;

    /// Whether `header` (a file's first bytes, at most a few KiB) and `name` look like
    /// this format. Must not allocate, and must not panic on any input.
    fn detect(&self, header: &[u8], name: &str) -> Detection;

    /// Reads `src` into a document. Builds only what the primary facet needs; other
    /// facets are made on demand. Other files only through `cx.files` (best through
    /// [`Dependencies::resolve`](crate::Dependencies::resolve)), background work only through
    /// `cx.spawn`, progress only through `cx.notify`; every size the file claims is
    /// checked against `cx.limits` before it is allocated.
    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document>;
}

/// The readers in `readers` that fit, best first. Ties keep the list's order.
pub fn detect<'a>(readers: &[&'a dyn Reader], header: &[u8], name: &str) -> Vec<(&'a dyn Reader, Detection)> {
    let mut found: Vec<_> = readers.iter().map(|&r| (r, r.detect(header, name))).filter(|(_, d)| d.is_match()).collect();
    found.sort_by_key(|(_, d)| std::cmp::Reverse(d.confidence));
    found
}

/// Opens `src` with the best reader in `readers` that succeeds. A reader that says
/// the file isn't its format, or finds it damaged, makes way for the next; any other
/// error is final. A panic in a reader counts as damage, not a crash.
pub fn open(readers: &[&dyn Reader], src: &FileSource, cx: &ParseContext) -> Result<Document> {
    cx.cancel.check()?;
    let mut last = ParseError::NotThisFormat;
    for (reader, _) in detect(readers, src.header(), src.name()) {
        match open_guarded(reader, src, cx) {
            Ok(doc) => return Ok(doc),
            Err(e) if e.try_next_reader() => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Opens `src` with the reader whose id is `id`, whether or not it detects the file.
pub fn open_with(readers: &[&dyn Reader], id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    cx.cancel.check()?;
    let reader = readers.iter().find(|r| r.info().id == id).ok_or(ParseError::NotThisFormat)?;
    open_guarded(*reader, src, cx)
}

/// Runs `reader.open`, turning a panic into [`ParseError::Corrupt`].
/// The document gets the context's provided facets ([`Document::with_provided_facets`]).
pub fn open_guarded(reader: &dyn Reader, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    guard(reader.info().id, || reader.open(src, cx)).map(|doc| doc.with_provided_facets(cx))
}

/// Runs `f`, turning a panic in it into [`ParseError::Corrupt`] that names `what`: no
/// panic in format code crosses the library's boundary (docs/design/libfileparser.md §7).
pub(crate) fn guard<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|payload| Err(ParseError::Corrupt(format!("{what} panicked: {}", panic_message(&*payload)))))
}

pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}
