//! Every libfileparser category together (docs/design/libfileparser.md §8), in the
//! same shape as each of them: [`readers`], [`detect`], [`open`], [`open_with`].
//!
//! What it adds is only what no single category can do:
//!
//! 1. **Detection across categories:** every enabled reader, best first.
//! 2. **The best reading that opens;** on `NotThisFormat` or `Corrupt`, the next.
//! 3. **Owners first** (docs/design/libfileparser.md §2.1): a reader that only *also
//!    reads* a format (`-data` reading an SVG as XML) opens it only when no owner
//!    does; otherwise its reading is added as a Structure facet, made when asked for.
//! 4. **The generic facets,** after the reading's own: **Source** (the file as text, in
//!    the language its reader named) when it is text, **Package** (its entries) when
//!    it is a zip another reader read (a USDZ), and **Raw** (its bytes) always.
//!    Generic facets are made only when asked for.
//!
//! It has no policy of its own: it takes the app's [`ParseContext`] like any category,
//! with `-core`'s defaults unless the app gives its own. Categories are features
//! (`images`, `3d`, `data`, `text`, `archives`, all on by default); an app that wants one kind of file
//! takes that category alone.
//!
//! ```ignore
//! let cx = ParseContext::new().with_files(FolderFiles::new());
//! let doc = libfileparser::open(&FileSource::open("car.usdz")?, &cx)?;
//! for facet in doc.facets() { /* Scene, Stage, Layers, …, Package, Raw */ }
//! let stage = doc.facet(1, &cx)?; // made now, on first use
//! ```

use std::sync::{Arc, OnceLock};

pub use libfileparser_core::*;

/// Every enabled reader, in the order ties are broken: the categories that know a
/// format best first, plain text last (nearly anything readable is also text).
pub fn readers() -> &'static [&'static dyn Reader] {
    Library::default_ref().readers()
}

/// The readers that fit a file, best first.
pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
    Library::default_ref().detect(header, name)
}

/// A reader that was tried and didn't read the file, and why.
#[derive(Debug)]
#[non_exhaustive]
pub struct Attempt {
    pub reader: &'static str,
    pub error: ParseError,
}

/// Opens `src` with the best reading that succeeds, with the generic facets added. A
/// folder is listed (`archives`).
pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    Library::default_ref().open(src, cx)
}

/// As [`open`], also saying which readers were tried before the one that read it (or
/// all of them, when none did), for an app's info panel.
pub fn open_reporting(src: &FileSource, cx: &ParseContext) -> (Result<Document>, Vec<Attempt>) {
    Library::default_ref().open_reporting(src, cx)
}

/// Opens `src` with the reader whose id is `id`, whether or not it detects the file,
/// with the generic facets added.
pub fn open_with(id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    Library::default_ref().open_with(id, src, cx)
}

/// The enabled readers, plus an app's own: a format added to every category's
/// detection without forking one, or a built-in reader replaced by id
/// (docs/design/libfileparser.md §6). The free functions are [`Library::new`]'s.
///
/// ```ignore
/// let lib = libfileparser::Library::new()
///     .reader(&ThreeMfReader)            // a 3D format of the app's own
///     .replace("fbx", &InHouseFbxReader); // instead of ufbx
/// let doc = lib.open(&src, &cx)?;
/// ```
///
/// Facets an app adds to a format go on its `ParseContext` (`with_facet_provider`), so
/// they reach a category used alone too.
#[derive(Clone)]
pub struct Library {
    readers: Vec<&'static dyn Reader>,
}

impl Default for Library {
    fn default() -> Self {
        Self::new()
    }
}

impl Library {
    /// Every enabled category's readers.
    pub fn new() -> Self {
        #[allow(unused_mut)]
        let mut all: Vec<&'static dyn Reader> = Vec::new();
        #[cfg(feature = "images")]
        all.extend(libfileparser_images::readers());
        #[cfg(feature = "3d")]
        all.extend(libfileparser_3d::readers());
        #[cfg(feature = "archives")]
        all.extend(libfileparser_archives::readers());
        #[cfg(feature = "data")]
        all.extend(libfileparser_data::readers());
        #[cfg(feature = "text")]
        all.extend(libfileparser_text::readers());
        Self { readers: all }
    }

    fn default_ref() -> &'static Library {
        static DEFAULT: OnceLock<Library> = OnceLock::new();
        DEFAULT.get_or_init(Library::new)
    }

    /// Adds `reader`, ahead of plain text (so it wins a tie with "this is text") and
    /// behind the built-in readers of its category.
    pub fn reader(mut self, reader: &'static dyn Reader) -> Self {
        let at = self.readers.iter().position(|r| r.info().category == Category::Text).unwrap_or(self.readers.len());
        self.readers.insert(at, reader);
        self
    }

    /// Puts `reader` where the reader `id` was. Unknown ids add it as [`Self::reader`]
    /// does.
    pub fn replace(mut self, id: &str, reader: &'static dyn Reader) -> Self {
        match self.readers.iter().position(|r| r.info().id == id) {
            Some(i) => {
                self.readers[i] = reader;
                self
            }
            None => self.reader(reader),
        }
    }

    /// Its readers, in the order ties are broken.
    pub fn readers(&self) -> &[&'static dyn Reader] {
        &self.readers
    }

    /// The readers that fit a file, best first: owners of its format before readers
    /// that only also read it, then by confidence.
    pub fn detect(&self, header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
        let mut found = libfileparser_core::detect(&self.readers, header, name);
        // Stable: confidence order is kept within each group.
        found.sort_by_key(|(r, _)| r.info().matches_also_reads(name));
        found
    }

    pub fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        self.open_reporting(src, cx).0
    }

    pub fn open_reporting(&self, src: &FileSource, cx: &ParseContext) -> (Result<Document>, Vec<Attempt>) {
        let mut attempts = Vec::new();
        if let Err(e) = cx.cancel.check() {
            return (Err(e), attempts);
        }
        if src.is_folder() {
            return (open_folder(src, cx), attempts);
        }
        for (reader, detection) in self.detect(src.header(), src.name()) {
            match open_guarded(reader, src, cx) {
                Ok(doc) => {
                    let doc = self.with_other_readings(doc, reader, src, cx);
                    return (Ok(with_generic_facets(doc, src, detection.language)), attempts);
                }
                Err(e) if e.try_next_reader() => attempts.push(Attempt { reader: reader.info().id, error: e }),
                Err(e) => return (Err(e), attempts),
            }
        }
        // As a category's `open`: the last reader's reason.
        let last = match attempts.last().map(|a| &a.error) {
            Some(ParseError::Corrupt(why)) => ParseError::Corrupt(why.clone()),
            _ => ParseError::NotThisFormat,
        };
        (Err(last), attempts)
    }

    pub fn open_with(&self, id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        let doc = libfileparser_core::open_with(&self.readers, id, src, cx)?;
        let reader = self.readers.iter().copied().find(|r| r.info().id == id);
        let language = reader.and_then(|r| r.detect(src.header(), src.name()).language);
        let doc = match reader {
            Some(r) => self.with_other_readings(doc, r, src, cx),
            None => doc,
        };
        Ok(with_generic_facets(doc, src, language))
    }

    /// Adds the readings of readers that also read this format (§2.1), each as a
    /// Structure facet named after the reader ("XML"), before Source and Raw.
    fn with_other_readings(&self, doc: Document, opened: &'static dyn Reader, src: &FileSource, cx: &ParseContext) -> Document {
        let others: Vec<&'static dyn Reader> = self
            .detect(src.header(), src.name())
            .into_iter()
            .map(|(r, _)| r)
            .filter(|r| r.info().id != opened.info().id && r.info().matches_also_reads(src.name()))
            .collect();
        if others.is_empty() {
            return doc;
        }
        let mut with = cx.clone();
        with.facet_providers = others.into_iter().map(|reader| Arc::new(OtherReading { reader, src: src.clone() }) as Arc<dyn FacetProvider>).collect();
        doc.with_provided_facets(&with)
    }

    /// The reader called `id`.
    pub fn get(&self, id: &str) -> Option<&'static dyn Reader> {
        self.readers.iter().copied().find(|r| r.info().id == id)
    }
}

/// Another category's reading of a file, as one facet: its primary one.
struct OtherReading {
    reader: &'static dyn Reader,
    src: FileSource,
}

impl FacetProvider for OtherReading {
    fn formats(&self) -> &[&str] {
        &[]
    }

    fn facets(&self, _: &Document) -> Vec<FacetInfo> {
        let info = self.reader.info();
        vec![FacetInfo::new(Role::Structure, info.name, info.also_reads_model)]
    }

    fn make(&self, _: &Document, _: &FacetInfo, cx: &ParseContext) -> Result<Content> {
        open_guarded(self.reader, &self.src, cx)?.facet(0, cx)
    }
}

/// A document of the generic facets alone: for a file no reader reads, its bytes (and
/// its text, when it is text).
pub fn open_raw(src: &FileSource) -> Document {
    let doc = Document::new(FormatInfo::new("raw", "File", Category::Data)).with_meta("Size", format!("{} bytes", src.len()));
    with_generic_facets(doc, src, None)
}

#[cfg(feature = "archives")]
fn open_folder(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let path = src.path().ok_or_else(|| ParseError::invalid("a folder without a path"))?;
    libfileparser_archives::open_folder(path, cx)
}

#[cfg(not(feature = "archives"))]
fn open_folder(_: &FileSource, _: &ParseContext) -> Result<Document> {
    Err(ParseError::NotThisFormat)
}

/// Adds Source, Package and Raw after the reading's own facets, each only where the
/// reading has none of its own and the file is that kind of thing.
/// Categories add Source and Raw themselves (`Document::with_file_facets`); this adds
/// them to a document that has none, and Package to a zip no category listed.
#[cfg_attr(not(feature = "archives"), allow(unused_mut))]
fn with_generic_facets(mut doc: Document, src: &FileSource, language: Option<&'static str>) -> Document {
    // The reader's language, else the one `-text` finds from the name or first line.
    #[cfg(feature = "text")]
    let language = language.or_else(|| libfileparser_text::language(src.name(), src.header()));
    doc = doc.with_source(src, language);
    #[cfg(feature = "archives")]
    if !doc.has_role(Role::Package) && doc.format().category != Category::Archives && libfileparser_archives::is_zip(src.header()) {
        let from = src.clone();
        doc = doc.with_lazy_facet(FacetInfo::new(Role::Package, "Package", Model::Container), move |cx| {
            libfileparser_archives::package(&from, cx)?.ok_or_else(|| ParseError::invalid("not a zip"))
        });
    }
    doc.with_raw(src)
}

/// The reader called `id`, among the enabled ones.
pub fn reader(id: &str) -> Option<&'static dyn Reader> {
    Library::default_ref().get(id)
}

/// Opens entry `index` of a container facet as a document of its own, through the
/// same detection: a PNG in a zip in a USDZ.
pub fn open_entry(container: &Arc<dyn Container>, index: u64, cx: &ParseContext) -> Result<Document> {
    let src = container.open(index)?;
    open(&src, cx)
}
