//! Any document into pages or formatted text (docs/design/libfileparser.md §2):
//! - **pages:** PDF and XPS (MuPDF), DjVu, comics (CBZ, CBT, CB7, CBR);
//! - **books and office documents:** EPUB, MOBI/AZW, FictionBook, DOCX, ODT, RTF, DOC,
//!   HWP, and slides (PPTX, PPT, ODP);
//! - **the web and mail:** HTML, MHTML, EML, mbox, Outlook MSG;
//! - **plain-text markup:** Markdown, Org, reStructuredText, AsciiDoc, Djot, Typst,
//!   Textile, Jupyter notebooks, thirteen wiki dialects, man pages, LaTeX, Texinfo,
//!   POD, Fountain, DocBook and TEI.
//!
//! Every formatted-text reader builds through one builder (`build`), most through
//! one HTML pipeline (`html`), so nesting and limits are the same everywhere. Nothing
//! a document contains is run or fetched.
//!
//! **PDF is MuPDF, and only MuPDF.** With the `mupdf` feature (AGPL), PDFs open as
//! pages rendered by MuPDF built from source. Without it, a PDF is still recognised and
//! named, and refused with [`ParseError::Unsupported`]. Any other engine is the app's
//! to wire in: it implements [`Reader`] with id `"pdf"` (using [`pdf::document`] so its
//! documents look the same) and registers it, as `libfileparser-pdfkit` does for
//! Apple's PDFKit.
//!
//! The API is the shape every libfileparser category has: [`readers`], [`detect`],
//! [`open`] and [`open_with`].

mod books;
mod build;
mod comics;
mod djvu;
mod docx;
mod epub;
mod html;
mod office;
mod latex;
mod lite;
mod mail;
mod markup;
mod notebook;
mod markdown;
mod odt;
mod org;
mod package;
mod pages;
pub mod pdf;
mod roff;
mod pod;
mod rtf;
mod wiki;
mod xmldoc;
mod xps;

use libfileparser_core::{Bytes, Detection, Document, FileSource, Limit, ParseContext, Reader, Result};

pub use books::{HwpReader, MobiReader};
pub use comics::ComicReader;
pub use djvu::DjvuReader;
pub use docx::DocxReader;
pub use epub::EpubReader;
pub use html::HtmlReader;
pub use latex::{LatexReader, TexinfoReader};
pub use mail::{EmlReader, MboxReader, MhtReader, MsgReader};
pub use markdown::MarkdownReader;
pub use notebook::NotebookReader;
pub use markup::{AsciidocReader, DjotReader, RstReader, TextileReader, TypstReader};
pub use odt::OdtReader;
pub use office::{DocReader, PptReader, PptxReader};
pub use org::OrgReader;
pub use roff::ManReader;
pub use pod::{FountainReader, PodReader};
pub use rtf::RtfReader;
pub use pdf::PdfReader;
pub use wiki::{CreoleReader, DokuWikiReader, GemtextReader, HaddockReader, JiraReader, MediaWikiReader, MuseReader, TWikiReader, TikiWikiReader, Txt2tagsReader, VimwikiReader, XWikiReader, ZimWikiReader};
pub use xmldoc::{DocBookReader, Fb2Reader, OdpReader, TeiReader};
pub use xps::XpsReader;

/// Every format this category reads, in the order ties are broken.
pub fn readers() -> &'static [&'static dyn Reader] {
    &[&PdfReader, &XpsReader, &DjvuReader, &ComicReader, &EpubReader, &DocxReader, &PptxReader, &PptReader, &DocReader, &OdtReader, &OdpReader, &HwpReader, &MobiReader, &RtfReader, &OrgReader, &MarkdownReader, &RstReader, &DjotReader, &AsciidocReader, &TypstReader, &TextileReader, &NotebookReader, &MediaWikiReader, &CreoleReader, &DokuWikiReader, &JiraReader, &Txt2tagsReader, &MuseReader, &HaddockReader, &XWikiReader, &TikiWikiReader, &TWikiReader, &ZimWikiReader, &VimwikiReader, &GemtextReader, &ManReader, &LatexReader, &TexinfoReader, &PodReader, &FountainReader, &DocBookReader, &TeiReader, &Fb2Reader, &MsgReader, &MboxReader, &MhtReader, &EmlReader, &HtmlReader]
}

/// The readers that fit a file, best first.
pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
    libfileparser_core::detect(readers(), header, name)
}

/// Opens `src` with the best reader that succeeds.
pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open(readers(), src, cx)
}

/// Opens `src` with the reader whose id is `id`.
pub fn open_with(id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open_with(readers(), id, src, cx)
}

/// All of `src`'s bytes, for formats parsed whole: at most `cx.limits.allocation`.
pub(crate) fn whole(src: &FileSource, cx: &ParseContext) -> Result<Bytes> {
    cx.limits.check(Limit::Allocation, src.len())?;
    Ok(src.range(0, src.len() as usize))
}
