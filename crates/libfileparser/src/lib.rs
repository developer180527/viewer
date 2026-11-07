//! Every libfileparser category together (docs/design/libfileparser.md §8), in the
//! same shape as each of them: [`readers`], [`detect`], [`open`], [`open_with`].
//!
//! What it adds is only what no single category can do:
//!
//! 1. **Detection across categories:** every enabled reader, best first.
//! 2. **The best reading that opens;** on `NotThisFormat` or `Corrupt`, the next.
//! 3. **The generic facets,** after the reading's own: **Source** (the file as text, in
//!    the language its reader named) when it is text, **Package** (its entries) when
//!    it is a zip another reader read (a USDZ), and **Raw** (its bytes) always.
//!    Generic facets are made only when asked for.
//!
//! It has no policy of its own: it takes the app's [`ParseContext`] like any category,
//! with `-core`'s defaults unless the app gives its own. Categories are features
//! (`images`, `3d`, `text`, `archives`, all on by default); an app that wants one kind of file
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
    static ALL: OnceLock<Vec<&'static dyn Reader>> = OnceLock::new();
    ALL.get_or_init(|| {
        #[allow(unused_mut)]
        let mut all: Vec<&'static dyn Reader> = Vec::new();
        #[cfg(feature = "images")]
        all.extend(libfileparser_images::readers());
        #[cfg(feature = "3d")]
        all.extend(libfileparser_3d::readers());
        #[cfg(feature = "archives")]
        all.extend(libfileparser_archives::readers());
        #[cfg(feature = "text")]
        all.extend(libfileparser_text::readers());
        all
    })
}

/// The readers that fit a file, best first.
pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
    libfileparser_core::detect(readers(), header, name)
}

/// A reader that was tried and didn't read the file, and why.
#[derive(Debug)]
pub struct Attempt {
    pub reader: &'static str,
    pub error: ParseError,
}

/// Opens `src` with the best reading that succeeds, with the generic facets added. A
/// folder is listed (`archives`).
pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    open_reporting(src, cx).0
}

/// As [`open`], also saying which readers were tried before the one that read it (or
/// all of them, when none did), for an app's info panel.
pub fn open_reporting(src: &FileSource, cx: &ParseContext) -> (Result<Document>, Vec<Attempt>) {
    let mut attempts = Vec::new();
    if let Err(e) = cx.cancel.check() {
        return (Err(e), attempts);
    }
    if src.is_folder() {
        return (open_folder(src, cx), attempts);
    }
    for (reader, detection) in detect(src.header(), src.name()) {
        match open_guarded(reader, src, cx) {
            Ok(doc) => return (Ok(with_generic_facets(doc, src, detection.language)), attempts),
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

/// Opens `src` with the reader whose id is `id`, whether or not it detects the file,
/// with the generic facets added.
pub fn open_with(id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let doc = libfileparser_core::open_with(readers(), id, src, cx)?;
    let reader = readers().iter().find(|r| r.info().id == id);
    let language = reader.and_then(|r| r.detect(src.header(), src.name()).language);
    Ok(with_generic_facets(doc, src, language))
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
#[cfg_attr(not(all(feature = "text", feature = "archives")), allow(unused_variables, unused_mut))]
fn with_generic_facets(mut doc: Document, src: &FileSource, language: Option<&'static str>) -> Document {
    let has = |doc: &Document, role: Role| doc.facets().any(|f| f.role == role);
    #[cfg(feature = "text")]
    {
        let primary_is_text = doc.facets().next().is_some_and(|f| f.model == Model::Text);
        if !has(&doc, Role::Source) && !primary_is_text && libfileparser_text::is_text(src.header()) {
            let from = src.clone();
            doc = doc.with_lazy_facet(FacetInfo::new(Role::Source, "Source", Model::Text), move |cx| {
                libfileparser_text::source(&from, language, cx)?.ok_or_else(|| ParseError::invalid("not text"))
            });
        }
    }
    #[cfg(feature = "archives")]
    if !has(&doc, Role::Package) && doc.format().category != Category::Archives && libfileparser_archives::is_zip(src.header()) {
        let from = src.clone();
        doc = doc.with_lazy_facet(FacetInfo::new(Role::Package, "Package", Model::Container), move |cx| {
            libfileparser_archives::package(&from, cx)?.ok_or_else(|| ParseError::invalid("not a zip"))
        });
    }
    if !src.is_folder() {
        doc = doc.with_facet(Role::Raw, "Raw", Content::Hex(src.clone()));
    }
    doc
}

/// The reader called `id`, among the enabled ones.
pub fn reader(id: &str) -> Option<&'static dyn Reader> {
    readers().iter().copied().find(|r| r.info().id == id)
}

/// Opens entry `index` of a container facet as a document of its own, through the
/// same detection: a PNG in a zip in a USDZ.
pub fn open_entry(container: &Arc<dyn Container>, index: u64, cx: &ParseContext) -> Result<Document> {
    let src = container.open(index)?;
    open(&src, cx)
}
