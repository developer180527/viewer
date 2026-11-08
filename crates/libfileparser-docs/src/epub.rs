//! EPUB 2 and 3, read by rbook (the container, package, spine, table of contents and
//! resources), each chapter's XHTML by html5ever (`crate::html`): the chapters in
//! reading order as formatted text, the table of contents as a Structure tree, title,
//! author, language and publisher as metadata. Pure Rust, so it reads on every
//! platform, iOS included. A book whose text is encrypted (DRM) is refused plainly.

use std::io::Cursor;
use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Block, Category, Confidence, Content, Count, Detection, Document, FileSource, FlowSource, FormatInfo, MemoryUse, OwnedFlow, OwnedTree, ParseContext, ParseError,
    Reader, ReaderInfo, Result, Role,
};
use rbook::Epub;

use crate::build::Builder;

static INFO: ReaderInfo = ReaderInfo::new("epub", "EPUB", Category::Docs).extensions(&["epub"]).mime_types(&["application/epub+zip"]);

pub struct EpubReader;

/// The `mimetype` part, stored first, as the format requires.
fn has_mimetype(header: &[u8]) -> bool {
    header.starts_with(b"PK\x03\x04") && header.get(30..58) == Some(b"mimetypeapplication/epub+zip")
}

impl Reader for EpubReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// Certain from its `mimetype` part (it outranks the zip reader); a zip named
    /// `.epub` without one is still tried.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if has_mimetype(header) {
            Detection::new(Confidence::Certain)
        } else if header.starts_with(b"PK\x03\x04") && INFO.matches_extension(name) {
            Detection::new(Confidence::Magic)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !src.header().starts_with(b"PK\x03\x04") {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        let epub = Epub::read(Cursor::new(bytes.to_vec())).map_err(|e| ParseError::corrupt(format!("EPUB: {e}")))?;
        if encrypted(&epub) {
            return Err(ParseError::unsupported("This book is protected by DRM and can't be read"));
        }
        // Every chapter in reading order, a break between chapters.
        let mut b = Builder::new(cx);
        let mut chapters = 0usize;
        for data in epub.reader() {
            cx.cancel.check()?;
            let Ok(data) = data else { continue };
            if chapters > 0 {
                b.block(Block::Break)?;
            }
            let href = data.manifest_entry().href().decode().trim_start_matches('/').to_string();
            crate::html::parse(data.content(), &href, &mut b)?;
            chapters += 1;
        }
        let blocks = b.finish()?;
        let toc = toc(&epub);
        let meta = metadata(&epub);
        let flow = BookFlow { flow: OwnedFlow::new(blocks), epub, size: bytes.len() as u64 };
        let mut doc = Document::new(FormatInfo::new("epub", "EPUB book", Category::Docs)).with_facet(Role::Primary, "Book", Content::Flow(Arc::new(flow)));
        if let Some(tree) = toc {
            doc = doc.with_facet(Role::Structure, "Contents", Content::Tree(Arc::new(tree)));
        }
        for (k, v) in meta {
            doc = doc.with_meta(k, v);
        }
        Ok(doc.with_meta("Chapters", chapters).with_file_facets(src, None))
    }
}

/// Title, author, language and publisher.
fn metadata(epub: &Epub) -> Vec<(&'static str, String)> {
    let m = epub.metadata();
    let mut out = Vec::new();
    if let Some(t) = m.title() {
        out.push(("Title", t.value().to_string()));
    }
    let authors: Vec<String> = m.creators().map(|c| c.value().to_string()).collect();
    if !authors.is_empty() {
        out.push(("Author", authors.join(", ")));
    }
    if let Some(l) = m.language() {
        out.push(("Language", l.scheme().code().to_string()));
    }
    if let Some(p) = m.publishers().next() {
        out.push(("Publisher", p.value().to_string()));
    }
    out.retain(|(_, v)| !v.trim().is_empty());
    out
}

/// Whether `META-INF/encryption.xml` encrypts more than fonts (font obfuscation is
/// common and harmless; encrypted chapters are DRM).
fn encrypted(epub: &Epub) -> bool {
    let Ok(xml) = epub.read_resource_str("/META-INF/encryption.xml") else { return false };
    xml.match_indices("URI=\"").any(|(at, _)| {
        let uri = xml[at + 5..].split('"').next().unwrap_or("").to_ascii_lowercase();
        !(uri.ends_with(".otf") || uri.ends_with(".ttf") || uri.ends_with(".woff") || uri.ends_with(".woff2"))
    })
}

/// The table of contents as a tree of titles and their targets.
fn toc(epub: &Epub) -> Option<OwnedTree> {
    let root = epub.toc().contents()?;
    let mut tree = OwnedTree::new();
    // Each entry under its parent: the last node at each depth.
    let mut at: Vec<libfileparser_core::NodeId> = Vec::new();
    for entry in root.flatten() {
        let depth = entry.depth().max(1);
        at.truncate(depth - 1);
        let target = entry.href().map(|h| h.decode().into_owned()).unwrap_or_default();
        let id = tree.add(at.last().copied(), entry.label().split_whitespace().collect::<Vec<_>>().join(" "), target);
        at.push(id);
    }
    (!tree.is_empty()).then_some(tree)
}

/// A book's formatted text, its images read from the book when asked for.
struct BookFlow {
    flow: OwnedFlow,
    epub: Epub,
    size: u64,
}

impl FlowSource for BookFlow {
    fn block_count(&self) -> Count {
        self.flow.block_count()
    }

    fn blocks(&self, range: Range<u64>) -> Result<Vec<Block>> {
        self.flow.blocks(range)
    }

    fn resource(&self, reference: &str) -> Result<FileSource> {
        let path = format!("/{}", reference.trim_start_matches('/'));
        let data = self.epub.read_resource_bytes(path.as_str()).map_err(|e| ParseError::invalid(format!("{reference}: {e}")))?;
        Ok(FileSource::from_bytes(reference.rsplit('/').next().unwrap_or(reference).to_string(), data))
    }

    fn memory(&self) -> MemoryUse {
        self.flow.memory() + MemoryUse::heap(self.size)
    }
}
