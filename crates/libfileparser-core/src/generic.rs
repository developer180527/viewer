//! The facets every file can have, whatever reads it (docs/design/libfileparser.md
//! §2): **Source**, its text by line when it is text, and **Raw**, its bytes; and
//! [`FileParts`], files inside a file (a GLB's chunks, a USDZ's layers, a `.blend`'s
//! packed images) as a container. Every category adds Source and Raw through
//! [`crate::Document::with_file_facets`], so an app using one category alone gets them,
//! and the front door adds them only where a category didn't.

use std::ops::Range;
use std::sync::Arc;

use crate::{Container, Content, Count, Entry, FacetInfo, FileSource, Limit, LineText, MemoryUse, Model, ParseError, Result, Role};

/// Whether bytes (a file's first few KB) are text: no NUL, and UTF-8 apart from a
/// character cut off at the end.
pub fn is_text(header: &[u8]) -> bool {
    if header.is_empty() || header.contains(&0) {
        return false;
    }
    match std::str::from_utf8(header) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && header.len() - e.valid_up_to() < 4,
    }
}

impl LineText {
    /// As [`LineText::new`], once its line index (one offset per line, counted first)
    /// is known to fit `limits`.
    pub fn within(source: FileSource, language: Option<&str>, limits: &crate::Limits) -> Result<Self> {
        let lines = source.bytes().iter().filter(|&&b| b == b'\n').count() as u64 + 1;
        let bytes = lines.saturating_mul(size_of::<usize>() as u64);
        limits.check(Limit::Allocation, bytes)?;
        limits.check(Limit::Memory, bytes)?;
        Ok(Self::new(source, language))
    }
}

impl crate::Document {
    /// Adds the Source facet when `src` is text and the document has none (in
    /// `language`: the reader's `Detection::language`), then the Raw facet when it has
    /// none. Both are made only when asked for. A category's documents end with these.
    pub fn with_file_facets(self, src: &FileSource, language: Option<&str>) -> Self {
        self.with_source(src, language).with_raw(src)
    }

    /// The Source facet: `src`'s text by line, in `language`, when it is text and the
    /// document has no Source facet yet.
    pub fn with_source(self, src: &FileSource, language: Option<&str>) -> Self {
        let primary_is_text = self.facets().next().is_some_and(|f| f.model == Model::Text);
        if src.is_folder() || primary_is_text || self.has_role(Role::Source) || !is_text(src.header()) {
            return self;
        }
        let (from, language) = (src.clone(), language.map(str::to_owned));
        self.with_lazy_facet(FacetInfo::new(Role::Source, "Source", Model::Text), move |cx| {
            Ok(Content::Text(Arc::new(LineText::within(from.clone(), language.as_deref(), &cx.limits)?)))
        })
    }

    /// The Raw facet, `src`'s bytes, when the document has none.
    pub fn with_raw(self, src: &FileSource) -> Self {
        if src.is_folder() || self.has_role(Role::Raw) {
            return self;
        }
        self.with_facet(Role::Raw, "Raw", Content::Hex(src.clone()))
    }

    /// Whether any facet has `role`.
    pub fn has_role(&self, role: Role) -> bool {
        self.facets().any(|f| f.role == role)
    }
}

/// Files inside a file, each a range of its bytes, as a container: an entry opens as
/// a view of the file (`FileSource::part`), never a copy. Entries are listed once, by
/// path (`images/wood.png`), in the order given.
pub struct FileParts {
    src: FileSource,
    entries: Vec<(String, Range<usize>)>,
}

impl FileParts {
    /// Ranges past the end of `src` are cut to it.
    pub fn new(src: &FileSource, entries: Vec<(String, Range<usize>)>) -> Self {
        Self { src: src.clone(), entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Container for FileParts {
    fn entry_count(&self) -> Count {
        Count::Exact(self.entries.len() as u64)
    }

    fn entries(&self, range: Range<u64>) -> Result<Vec<Entry>> {
        let end = range.end.min(self.entries.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(self.entries[start..end].iter().map(|(p, r)| Entry::file(p.clone(), r.len() as u64)).collect())
    }

    fn open(&self, index: u64) -> Result<FileSource> {
        let (path, range) = self.entries.get(index as usize).ok_or_else(|| ParseError::invalid(format!("no entry {index}")))?;
        Ok(self.src.part(path.rsplit('/').next().unwrap_or(path), range.clone()))
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.entries.iter().map(|(p, _)| p.len() as u64 + 40).sum())
    }
}

/// Adds facets to documents of some formats without changing their readers
/// (docs/design/libfileparser.md §6): a game engine's "Physics" inspector for USD. An
/// app registers providers on its [`crate::ParseContext`]; every document a reader opens
/// through `-core`'s `open`, `open_with` or `open_guarded` gets their facets after its
/// own, made only when asked for. A provider that fails or panics fails its facet
/// alone.
pub trait FacetProvider: Send + Sync {
    /// The reader ids it adds facets for (`["usd"]`); empty for every format.
    fn formats(&self) -> &[&str];
    /// What it would add to `doc`, cheaply: nothing is made yet.
    fn facets(&self, doc: &crate::Document) -> Vec<FacetInfo>;
    /// The content of `facet`, one of those [`Self::facets`] gave for `doc`.
    fn make(&self, doc: &crate::Document, facet: &FacetInfo, cx: &crate::ParseContext) -> Result<Content>;
}
