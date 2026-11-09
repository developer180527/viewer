//! Workbooks (docs/design/libfileparser.md §2.1): every sheet a table of typed cells,
//! the workbook's formulas and defined names as tables of their own. XLSX, XLSB and XLS
//! are read by calamine; ODS, FODS, SYLK, DIF, XML Spreadsheet 2003 and the bare BIFF of
//! Excel 2 to 5 by our own crates.
//!
//! A new format is one more [`Reader`] and one more line in [`readers`].

use libfileparser_core::{Category, Confidence, Detection, Document, FileSource, ParseContext, ParseError, Reader, ReaderInfo, Result};

mod guard;
mod plain;
mod workbook;

pub use plain::{DifReader, FodsReader, OdsReader, SylkReader, XmlssReader};

use workbook::Shared;

const PK: &[u8] = b"PK\x03\x04";
const OLE: &[u8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";

/// Every reader here, in the order they're tried.
pub fn readers() -> &'static [&'static dyn Reader] {
    &[&XlsxReader, &XlsbReader, &XlsReader, &OdsReader, &FodsReader, &SylkReader, &DifReader, &XmlssReader]
}

/// Whether an OLE file holds an encrypted Office package: how a password-protected
/// XLSX or XLSB is stored.
fn encrypted(bytes: &[u8]) -> bool {
    let name: Vec<u8> = "EncryptedPackage".encode_utf16().flat_map(u16::to_le_bytes).collect();
    bytes.windows(name.len()).any(|w| w == name)
}

fn shared(src: &FileSource, cx: &ParseContext) -> Result<Shared> {
    cx.cancel.check()?;
    Ok(Shared(src.shared_bytes()))
}

pub(crate) fn locked() -> ParseError {
    ParseError::unsupported("This workbook is protected by a password.")
}

/// Opens an Office Open XML workbook (XLSX or XLSB).
fn open_ooxml(src: &FileSource, cx: &ParseContext, reader: &'static str, format: &'static str) -> Result<Document> {
    let bytes = shared(src, cx)?;
    if src.header().starts_with(OLE) {
        return Err(if encrypted(bytes.as_ref()) { locked() } else { ParseError::NotThisFormat });
    }
    if !src.header().starts_with(PK) {
        return Err(ParseError::NotThisFormat);
    }
    guard::zip_size(&bytes, &cx.limits)?;
    workbook::open(src, bytes, reader, format, cx)
}

static XLSX: ReaderInfo = ReaderInfo::new("xlsx", "Excel workbook", Category::Sheets)
    .extensions(&["xlsx", "xlsm", "xltx", "xltm", "xlam"])
    .mime_types(&["application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"]);

pub struct XlsxReader;

impl Reader for XlsxReader {
    fn info(&self) -> &ReaderInfo {
        &XLSX
    }

    /// A zip (or, password-protected, an OLE file) named as a workbook: certain, so it
    /// outranks the zip reader.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if (header.starts_with(PK) || header.starts_with(OLE)) && XLSX.matches_extension(name) { Detection::new(Confidence::Certain) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        open_ooxml(src, cx, "xlsx", "Excel workbook")
    }
}

static XLSB: ReaderInfo = ReaderInfo::new("xlsb", "Excel binary workbook", Category::Sheets).extensions(&["xlsb"]).mime_types(&["application/vnd.ms-excel.sheet.binary.macroenabled.12"]);

pub struct XlsbReader;

impl Reader for XlsbReader {
    fn info(&self) -> &ReaderInfo {
        &XLSB
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if (header.starts_with(PK) || header.starts_with(OLE)) && XLSB.matches_extension(name) { Detection::new(Confidence::Certain) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        open_ooxml(src, cx, "xlsb", "Excel binary workbook")
    }
}

static XLS: ReaderInfo = ReaderInfo::new("xls", "Excel 97–2003 workbook", Category::Sheets).extensions(&["xls", "xla", "xlt", "xlw"]).mime_types(&["application/vnd.ms-excel"]);

pub struct XlsReader;

impl Reader for XlsReader {
    fn info(&self) -> &ReaderInfo {
        &XLS
    }

    /// A compound file named as a workbook; or a bare BIFF stream, as Excel 2 to 4 saved
    /// every file, certain when named as one.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        let named = XLS.matches_extension(name);
        if header.starts_with(OLE) && named || biff_lite::detect(header) && named {
            Detection::new(Confidence::Certain)
        } else if biff_lite::detect(header) {
            Detection::new(Confidence::Magic)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        if biff_lite::detect(src.header()) {
            return plain::biff(src, cx);
        }
        let bytes = shared(src, cx)?;
        if !src.header().starts_with(OLE) {
            return Err(ParseError::NotThisFormat);
        }
        guard::xls_extent(&bytes, &cx.limits)?;
        workbook::open(src, bytes, "xls", "Excel 97–2003 workbook", cx)
    }
}
