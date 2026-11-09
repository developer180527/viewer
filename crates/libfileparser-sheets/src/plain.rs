//! The workbooks our own crates read: ODS and flat FODS (`ods-lite`), SYLK, DIF, bare
//! BIFF streams (`biff-lite`: Excel 2 to 5) and XML Spreadsheet 2003 (`xmlss-lite`).

use std::sync::Arc;

use libfileparser_core::{Category, Cell, Confidence, Content, Detection, Document, FileSource, FormatInfo, Limit, OwnedTable, ParseContext, ParseError, Reader, ReaderInfo, Result, Role};

use crate::workbook::{Sheet, column_name, number};

/// One sheet with no formula table or second sheet to speak of, as a document.
fn one_sheet(src: &FileSource, reader: &'static str, format: &'static str, cells: Vec<(u32, u32, Cell)>, formulas: Vec<Vec<Cell>>, cx: &ParseContext) -> Result<Document> {
    let doc = Document::new(FormatInfo::new(reader, format, Category::Sheets)).with_facet(Role::Primary, "Sheet", Content::Table(Arc::new(Sheet::new(cells, cx)?)));
    Ok(with_formulas(doc, formulas).with_meta("Sheets", 1).with_file_facets(src, None))
}

fn with_formulas(doc: Document, rows: Vec<Vec<Cell>>) -> Document {
    if rows.is_empty() {
        return doc;
    }
    let table = OwnedTable::text(vec!["Sheet".into(), "Cell".into(), "Formula".into()], Vec::new()).with_cells(rows);
    doc.with_facet(Role::Inspector, "Formulas", Content::Table(Arc::new(table)))
}

fn formula_row(sheet: &str, row: u32, col: u32, formula: String) -> Vec<Cell> {
    vec![Cell::Text(sheet.to_string()), Cell::Text(format!("{}{}", column_name(col), row + 1)), Cell::Text(formula)]
}

fn ods_cell(v: ods_lite::Value) -> Cell {
    use ods_lite::Value as V;
    match v {
        V::Number(n) => number(n),
        V::Text(s) => Cell::Text(s),
        V::Bool(b) => Cell::Bool(b),
        V::Date(s) | V::Time(s) => Cell::Date(s),
        V::Error(s) => Cell::Error(s),
        _ => Cell::Null,
    }
}

fn ods_limits(cx: &ParseContext) -> ods_lite::Limits {
    let l = &cx.limits;
    ods_lite::Limits::default().with_cells(l.memory / 64).with_text(l.memory).with_part_bytes(l.allocation).with_unpack(l.unpack_ratio, l.unpack_always)
}

fn ods_error(e: ods_lite::Error) -> ParseError {
    match e {
        ods_lite::Error::NotOds => ParseError::NotThisFormat,
        ods_lite::Error::Encrypted => ParseError::unsupported("This workbook is protected by a password."),
        ods_lite::Error::TooLarge { value, max, .. } => ParseError::too_large(Limit::Memory, value, max),
        e => ParseError::corrupt(e),
    }
}

/// Sheets of cells as a document: the first the primary table, the others after it,
/// then the formulas and defined names.
type Cells = Vec<(u32, u32, Cell)>;

fn many(src: &FileSource, reader: &'static str, format: impl Into<String>, sheets: Vec<(String, Cells)>, formulas: Vec<Vec<Cell>>, names: Vec<(String, String)>, cx: &ParseContext) -> Result<Document> {
    let count = sheets.len();
    let mut sheets = sheets.into_iter();
    let (name, cells) = sheets.next().ok_or_else(|| ParseError::corrupt("a workbook with no sheets"))?;
    let mut doc = Document::new(FormatInfo::new(reader, format, Category::Sheets)).with_facet(Role::Primary, name, Content::Table(Arc::new(Sheet::new(cells, cx)?)));
    for (name, cells) in sheets {
        cx.cancel.check()?;
        doc = doc.with_facet(Role::Structure, name, Content::Table(Arc::new(Sheet::new(cells, cx)?)));
    }
    doc = with_formulas(doc, formulas);
    if !names.is_empty() {
        let rows = names.into_iter().map(|(n, r)| vec![n, r]).collect();
        doc = doc.with_facet(Role::Inspector, "Defined names", Content::Table(Arc::new(OwnedTable::text(vec!["Name".into(), "Refers to".into()], rows))));
    }
    Ok(doc.with_meta("Sheets", count).with_file_facets(src, None))
}

/// A workbook read by `ods-lite`, every sheet a table.
fn workbook(src: &FileSource, book: ods_lite::Workbook, reader: &'static str, format: &'static str, cx: &ParseContext) -> Result<Document> {
    let mut formulas = Vec::new();
    let mut sheets = Vec::with_capacity(book.sheets.len());
    for sheet in book.sheets {
        cx.cancel.check()?;
        let mut cells = Vec::with_capacity(sheet.cells.len());
        for c in sheet.cells {
            if let Some(f) = c.formula {
                formulas.push(formula_row(&sheet.name, c.row, c.col, f));
            }
            if let Some(v) = c.value {
                cells.push((c.row, c.col, ods_cell(v)));
            }
        }
        sheets.push((sheet.name, cells));
    }
    many(src, reader, format, sheets, formulas, book.names, cx)
}

/// A bare BIFF stream (no compound file around it): how Excel 2, 3 and 4 saved every
/// file. Opened by the XLS reader, as such files are named `.xls`.
pub(crate) fn biff(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    cx.cancel.check()?;
    let max = cx.limits.memory / 64;
    let book = biff_lite::read(src.bytes(), max).map_err(|e| match e {
        biff_lite::Error::NotBiff => ParseError::NotThisFormat,
        biff_lite::Error::Encrypted => crate::locked(),
        biff_lite::Error::TooManyCells(max) => ParseError::too_large(Limit::Memory, max.saturating_add(1).saturating_mul(64), max.saturating_mul(64)),
        e => ParseError::corrupt(e),
    })?;
    let format = match book.biff {
        2 => "Excel 2 worksheet",
        3 => "Excel 3 worksheet",
        4 => "Excel 4 worksheet",
        5 => "Excel 5 workbook (bare stream)",
        _ => "Excel 97 workbook (bare stream)",
    };
    let sheets = book
        .sheets
        .into_iter()
        .map(|s| {
            use biff_lite::Value as V;
            let cells = s
                .cells
                .into_iter()
                .filter_map(|c| {
                    let v = match c.value {
                        V::Number(n) => number(n),
                        V::Text(t) => Cell::Text(t),
                        V::Bool(b) => Cell::Bool(b),
                        V::Error(e) => Cell::Error(e),
                        _ => return None,
                    };
                    Some((c.row, c.col, v))
                })
                .collect();
            (s.name, cells)
        })
        .collect();
    many(src, "xls", format, sheets, Vec::new(), Vec::new(), cx)
}

static ODS: ReaderInfo = ReaderInfo::new("ods", "OpenDocument spreadsheet", Category::Sheets).extensions(&["ods", "ots"]).mime_types(&["application/vnd.oasis.opendocument.spreadsheet"]);

pub struct OdsReader;

const MIMETYPE: &[u8] = b"mimetypeapplication/vnd.oasis.opendocument.spreadsheet";
const PK: &[u8] = b"PK\x03\x04";

impl Reader for OdsReader {
    fn info(&self) -> &ReaderInfo {
        &ODS
    }

    /// Certain from its `mimetype` part, stored first; a zip named `.ods` is tried.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if header.starts_with(PK) && header.get(30..30 + MIMETYPE.len()) == Some(MIMETYPE) {
            Detection::new(Confidence::Certain)
        } else if header.starts_with(PK) && ODS.matches_extension(name) {
            Detection::new(Confidence::Magic)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !src.header().starts_with(PK) {
            return Err(ParseError::NotThisFormat);
        }
        let book = ods_lite::read(src.bytes(), &ods_limits(cx)).map_err(ods_error)?;
        workbook(src, book, "ods", "OpenDocument spreadsheet", cx)
    }
}

static FODS: ReaderInfo = ReaderInfo::new("fods", "Flat OpenDocument spreadsheet", Category::Sheets).extensions(&["fods"]).mime_types(&["application/vnd.oasis.opendocument.spreadsheet-flat-xml"]);

pub struct FodsReader;

impl Reader for FodsReader {
    fn info(&self) -> &ReaderInfo {
        &FODS
    }

    /// By name: it must outrank the XML reader. By content when its root says it's one.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        let mime = b"application/vnd.oasis.opendocument.spreadsheet";
        let says = header.windows(mime.len()).any(|w| w == mime);
        if FODS.matches_extension(name) || says { Detection::new(Confidence::Certain).with_language("xml") } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let book = ods_lite::read_flat(src.bytes(), &ods_limits(cx)).map_err(ods_error)?;
        workbook(src, book, "fods", "Flat OpenDocument spreadsheet", cx)
    }
}

static SYLK: ReaderInfo = ReaderInfo::new("sylk", "SYLK spreadsheet", Category::Sheets).extensions(&["slk", "sylk"]);

pub struct SylkReader;

impl Reader for SylkReader {
    fn info(&self) -> &ReaderInfo {
        &SYLK
    }

    fn detect(&self, header: &[u8], _: &str) -> Detection {
        if sylk_lite::detect(header) { Detection::new(Confidence::Magic) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let parsed = sylk_lite::read(src.bytes(), cx.limits.memory / 64).map_err(|e| match e {
            sylk_lite::Error::NotSylk => ParseError::NotThisFormat,
            e => ParseError::corrupt(e),
        })?;
        let (mut cells, mut formulas) = (Vec::new(), Vec::new());
        for c in parsed {
            if let Some(f) = c.formula {
                formulas.push(formula_row("Sheet", c.row, c.col, f));
            }
            use sylk_lite::Value as V;
            let value = match c.value {
                Some(V::Number(n)) => number(n),
                Some(V::Text(s)) => Cell::Text(s),
                Some(V::Bool(b)) => Cell::Bool(b),
                Some(V::Error(s)) => Cell::Error(s),
                _ => continue,
            };
            cells.push((c.row, c.col, value));
        }
        one_sheet(src, "sylk", "SYLK spreadsheet", cells, formulas, cx)
    }
}

static DIF: ReaderInfo = ReaderInfo::new("dif", "DIF spreadsheet", Category::Sheets).extensions(&["dif"]);

pub struct DifReader;

impl Reader for DifReader {
    fn info(&self) -> &ReaderInfo {
        &DIF
    }

    fn detect(&self, header: &[u8], _: &str) -> Detection {
        if dif_lite::detect(header) { Detection::new(Confidence::Magic) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let parsed = dif_lite::read(src.bytes(), cx.limits.memory / 64).map_err(|e| match e {
            dif_lite::Error::NotDif => ParseError::NotThisFormat,
            e => ParseError::corrupt(e),
        })?;
        let cells = parsed
            .into_iter()
            .map(|c| {
                use dif_lite::Value as V;
                let value = match c.value {
                    V::Number(n) => number(n),
                    V::Text(s) => Cell::Text(s),
                    V::Bool(b) => Cell::Bool(b),
                    V::Error(s) => Cell::Error(s),
                    _ => Cell::Null,
                };
                (c.row, c.col, value)
            })
            .collect();
        one_sheet(src, "dif", "DIF spreadsheet", cells, Vec::new(), cx)
    }
}

static XMLSS: ReaderInfo = ReaderInfo::new("xmlss", "XML Spreadsheet 2003", Category::Sheets).extensions(&["xml"]).mime_types(&["application/vnd.ms-excel"]);

pub struct XmlssReader;

impl Reader for XmlssReader {
    fn info(&self) -> &ReaderInfo {
        &XMLSS
    }

    /// By content, whatever the name (often `.xls`): certain, so it outranks the XML
    /// reader.
    fn detect(&self, header: &[u8], _: &str) -> Detection {
        if xmlss_lite::detect(header) { Detection::new(Confidence::Certain).with_language("xml") } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let max = cx.limits.memory / 64;
        let book = xmlss_lite::read(src.bytes(), max, cx.limits.memory).map_err(|e| match e {
            xmlss_lite::Error::NotXmlss => ParseError::NotThisFormat,
            xmlss_lite::Error::TooManyCells(max) => ParseError::too_large(Limit::Memory, max.saturating_add(1).saturating_mul(64), max.saturating_mul(64)),
            xmlss_lite::Error::TooMuchText(max) => ParseError::too_large(Limit::Memory, max.saturating_add(1), max),
            e => ParseError::corrupt(e),
        })?;
        let mut formulas = Vec::new();
        let sheets = book
            .sheets
            .into_iter()
            .map(|s| {
                let mut cells = Vec::with_capacity(s.cells.len());
                for c in s.cells {
                    if let Some(f) = c.formula {
                        formulas.push(formula_row(&s.name, c.row, c.col, f));
                    }
                    use xmlss_lite::Value as V;
                    let v = match c.value {
                        Some(V::Number(n)) => number(n),
                        Some(V::Text(t)) => Cell::Text(t),
                        Some(V::Bool(b)) => Cell::Bool(b),
                        Some(V::Date(d)) => Cell::Date(d),
                        Some(V::Error(e)) => Cell::Error(e),
                        _ => continue,
                    };
                    cells.push((c.row, c.col, v));
                }
                (s.name, cells)
            })
            .collect();
        many(src, "xmlss", "XML Spreadsheet 2003", sheets, formulas, book.names, cx)
    }
}
