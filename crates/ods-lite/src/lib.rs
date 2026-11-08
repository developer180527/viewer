//! Reads OpenDocument spreadsheets: ODS (a zip) and FODS (the same XML, flat).
//!
//! ODS files compress runs with repeat counts, and nearly every one repeats empty rows
//! to the sheet's edge, so only cells that hold something are kept. A value repeated
//! over a million rows is real work, and counts against [`Limits::cells`].
//!
//! ```no_run
//! let bytes = std::fs::read("book.ods")?;
//! for sheet in ods_lite::read(&bytes, &ods_lite::Limits::default())?.sheets {
//!     println!("{}: {} cells", sheet.name, sheet.cells.len());
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::fmt;
use std::io::{Cursor, Read};

use quick_xml::events::{BytesStart, Event};

/// Why a file couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    NotOds,
    /// Protected by a password.
    Encrypted,
    Zip(String),
    Xml(String),
    TooLarge { what: &'static str, value: u64, max: u64 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotOds => write!(f, "not an OpenDocument spreadsheet"),
            Error::Encrypted => write!(f, "protected by a password"),
            Error::Zip(e) => write!(f, "zip: {e}"),
            Error::Xml(e) => write!(f, "XML: {e}"),
            Error::TooLarge { what, value, max } => write!(f, "{what}: {value}, more than the {max} allowed"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Limits {
    /// Cells with something in them, repeats counted.
    pub cells: u64,
    /// Bytes of text in cells (values and formulas), repeats counted: a few bytes of
    /// `<text:s text:c="65535"/>` are 64 KiB of spaces.
    pub text: u64,
    /// Most bytes `content.xml` may unzip to.
    pub part_bytes: u64,
    pub unpack_ratio: u64,
    pub unpack_always: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { cells: 1 << 27, text: 1 << 30, part_bytes: 2 << 30, unpack_ratio: 200, unpack_always: 64 << 20 }
    }
}

impl Limits {
    pub fn with_cells(mut self, max: u64) -> Self {
        self.cells = max;
        self
    }

    pub fn with_text(mut self, max: u64) -> Self {
        self.text = max;
        self
    }

    pub fn with_part_bytes(mut self, max: u64) -> Self {
        self.part_bytes = max;
        self
    }

    pub fn with_unpack(mut self, ratio: u64, always: u64) -> Self {
        self.unpack_ratio = ratio;
        self.unpack_always = always;
        self
    }
}

/// What a cell holds.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// A float, percentage or currency.
    Number(f64),
    Text(String),
    Bool(bool),
    /// As written: ISO 8601.
    Date(String),
    /// As written: `PT12H30M`.
    Time(String),
    /// An error a formula evaluated to, as shown: `#DIV/0!`.
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    /// `None` for a formula with no saved result.
    pub value: Option<Value>,
    pub formula: Option<String>,
}

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Sheet {
    pub name: String,
    /// In file order: by row, then column.
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    /// Named ranges and expressions: (name, what it refers to).
    pub names: Vec<(String, String)>,
}

/// Reads an `.ods` (zip).
pub fn read(bytes: &[u8], limits: &Limits) -> Result<Workbook> {
    let zip_err = |e: zip::result::ZipError| Error::Zip(e.to_string());
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(zip_err)?;
    if let Ok(mut manifest) = zip.by_name("META-INF/manifest.xml") {
        let mut text = Vec::new();
        let _ = (&mut manifest).take(1 << 24).read_to_end(&mut text);
        if text.windows(14).any(|w| w == b"encryption-dat") {
            return Err(Error::Encrypted);
        }
    }
    let cap = limits.part_bytes.min(limits.unpack_always.max((bytes.len() as u64).saturating_mul(limits.unpack_ratio)));
    let file = zip.by_name("content.xml").map_err(|_| Error::NotOds)?;
    if file.size() > cap {
        return Err(Error::TooLarge { what: "content.xml bytes", value: file.size(), max: cap });
    }
    parse(std::io::BufReader::new(file.take(cap)), limits)
}

/// Reads a flat `.fods`.
pub fn read_flat(xml: &[u8], limits: &Limits) -> Result<Workbook> {
    parse(xml, limits)
}

fn attr(e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    let a = e.attributes().flatten().find(|a| a.key.as_ref() == name)?;
    Some(a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_or_else(|_| String::from_utf8_lossy(&a.value).into_owned(), |v| v.into_owned()))
}

fn count(e: &BytesStart<'_>, name: &[u8]) -> u64 {
    attr(e, name).and_then(|v| v.parse().ok()).unwrap_or(1u64).max(1)
}

/// The cell being read.
#[derive(Default)]
struct Open {
    repeat: u64,
    kind: Option<String>,
    number: Option<f64>,
    date: Option<String>,
    time: Option<String>,
    boolean: Option<bool>,
    string: Option<String>,
    formula: Option<String>,
    text: String,
    paragraphs: u32,
    error: bool,
}

impl Open {
    fn new(e: &BytesStart<'_>) -> Self {
        Self {
            repeat: count(e, b"table:number-columns-repeated"),
            kind: attr(e, b"office:value-type"),
            number: attr(e, b"office:value").and_then(|v| v.parse().ok()),
            date: attr(e, b"office:date-value"),
            time: attr(e, b"office:time-value"),
            boolean: attr(e, b"office:boolean-value").map(|v| v == "true"),
            string: attr(e, b"office:string-value"),
            formula: attr(e, b"table:formula"),
            error: attr(e, b"calcext:value-type").as_deref() == Some("error"),
            ..Default::default()
        }
    }

    fn value(&mut self) -> Option<Value> {
        let text = std::mem::take(&mut self.text);
        if self.error {
            return Some(Value::Error(text));
        }
        Some(match self.kind.as_deref() {
            Some("float" | "percentage" | "currency") => Value::Number(self.number?),
            Some("date") => Value::Date(self.date.take()?),
            Some("time") => Value::Time(self.time.take()?),
            Some("boolean") => Value::Bool(self.boolean?),
            Some("string") => Value::Text(self.string.take().unwrap_or(text)),
            _ if text.is_empty() => return None,
            _ => Value::Text(text),
        })
    }
}

/// Bytes of text a cell holds.
fn text_bytes(value: &Option<Value>, formula: &Option<String>) -> u64 {
    let v = match value {
        Some(Value::Text(s) | Value::Date(s) | Value::Time(s) | Value::Error(s)) => s.len(),
        _ => 0,
    };
    (v + formula.as_ref().map_or(0, String::len)) as u64
}

fn parse<R: std::io::BufRead>(input: R, limits: &Limits) -> Result<Workbook> {
    let mut xml = quick_xml::Reader::from_reader(input);
    let mut buf = Vec::new();
    let mut book = Workbook::default();
    let mut total = 0u64;
    // Text in cells so far, and in the row being read (once).
    let (mut text_total, mut row_text) = (0u64, 0u64);
    let (mut row_at, mut row_repeat, mut col_at) = (0u64, 1u64, 0u64);
    // This row's cells, written out once its repeat is known to apply.
    let mut row: Vec<Cell> = Vec::new();
    let mut open: Option<Open> = None;
    // Inside something whose text isn't the cell's: a comment, a nested table, a shape.
    let mut skip = 0usize;
    let mut in_sheet = false;
    let mut saw_spreadsheet = false;

    let too_much_text = |value: u64| Error::TooLarge { what: "bytes of cell text", value, max: limits.text };
    let finish_cell = |c: &mut Open, row: &mut Vec<Cell>, r: u64, col_at: &mut u64, total: &mut u64, text_total: &mut u64, row_text: &mut u64| -> Result<()> {
        let value = c.value();
        let formula = c.formula.take();
        let n = c.repeat;
        let at = *col_at;
        *col_at = col_at.saturating_add(n);
        if value.is_none() && formula.is_none() {
            return Ok(());
        }
        *total = total.saturating_add(n);
        if *total > limits.cells {
            return Err(Error::TooLarge { what: "cells", value: *total, max: limits.cells });
        }
        let bytes = text_bytes(&value, &formula).saturating_mul(n);
        *text_total = text_total.saturating_add(bytes);
        *row_text = row_text.saturating_add(bytes);
        if *text_total > limits.text {
            return Err(too_much_text(*text_total));
        }
        for k in 0..n {
            let (Ok(col), Ok(row_index)) = (u32::try_from(at + k), u32::try_from(r)) else { break };
            row.push(Cell { row: row_index, col, value: value.clone(), formula: formula.clone() });
        }
        Ok(())
    };

    loop {
        let event = xml.read_event_into(&mut buf).map_err(|e| Error::Xml(e.to_string()))?;
        let empty = matches!(event, Event::Empty(_));
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let name = e.name();
                let name = name.as_ref();
                if skip > 0 {
                    skip += !empty as usize;
                } else {
                    match name {
                        b"office:spreadsheet" => saw_spreadsheet = true,
                        b"office:annotation" | b"draw:frame" | b"draw:custom-shape" | b"draw:g" => skip = !empty as usize,
                        b"table:table" if open.is_some() || in_sheet => skip = !empty as usize,
                        b"table:table" => {
                            in_sheet = !empty;
                            (row_at, row_repeat, col_at) = (0, 1, 0);
                            book.sheets.push(Sheet { name: attr(e, b"table:name").unwrap_or_default(), cells: Vec::new() });
                        }
                        b"table:table-row" if in_sheet => {
                            row_repeat = count(e, b"table:number-rows-repeated");
                            col_at = 0;
                            row_text = 0;
                            row.clear();
                            if empty {
                                row_at = row_at.saturating_add(row_repeat);
                            }
                        }
                        b"table:table-cell" | b"table:covered-table-cell" if in_sheet => {
                            let mut c = Open::new(e);
                            if empty {
                                finish_cell(&mut c, &mut row, row_at, &mut col_at, &mut total, &mut text_total, &mut row_text)?;
                            } else {
                                open = Some(c);
                            }
                        }
                        b"text:p" | b"text:h" => {
                            if let Some(c) = &mut open {
                                if c.paragraphs > 0 {
                                    c.text.push('\n');
                                }
                                c.paragraphs += 1;
                            }
                        }
                        b"text:s" => {
                            if let Some(c) = &mut open {
                                let n = count(e, b"text:c").min(1 << 16);
                                if text_total.saturating_add(c.text.len() as u64).saturating_add(n) > limits.text {
                                    return Err(too_much_text(text_total.saturating_add(c.text.len() as u64).saturating_add(n)));
                                }
                                c.text.extend(std::iter::repeat_n(' ', n as usize));
                            }
                        }
                        b"text:tab" => {
                            if let Some(c) = &mut open {
                                c.text.push('\t');
                            }
                        }
                        b"text:line-break" => {
                            if let Some(c) = &mut open {
                                c.text.push('\n');
                            }
                        }
                        b"table:named-range" => {
                            if let (Some(n), Some(r)) = (attr(e, b"table:name"), attr(e, b"table:cell-range-address")) {
                                book.names.push((n, r));
                            }
                        }
                        b"table:named-expression" => {
                            if let (Some(n), Some(r)) = (attr(e, b"table:name"), attr(e, b"table:expression")) {
                                book.names.push((n, r));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                let name = e.name();
                let name = name.as_ref();
                if skip > 0 {
                    skip -= 1;
                } else {
                    match name {
                        b"table:table-cell" | b"table:covered-table-cell" => {
                            if let Some(mut c) = open.take() {
                                finish_cell(&mut c, &mut row, row_at, &mut col_at, &mut total, &mut text_total, &mut row_text)?;
                            }
                        }
                        b"table:table-row" if in_sheet => {
                            let width = row.len() as u64;
                            // The first copy is already counted, cell by cell.
                            total = total.saturating_add(width.saturating_mul(row_repeat - 1));
                            if width > 0 && total > limits.cells {
                                return Err(Error::TooLarge { what: "cells", value: total, max: limits.cells });
                            }
                            text_total = text_total.saturating_add(row_text.saturating_mul(row_repeat - 1));
                            if text_total > limits.text {
                                return Err(too_much_text(text_total));
                            }
                            if let Some(sheet) = book.sheets.last_mut() {
                                for k in 0..if width == 0 { 0 } else { row_repeat } {
                                    for c in &row {
                                        let Some(r) = c.row.checked_add(k as u32) else { continue };
                                        sheet.cells.push(Cell { row: r, ..c.clone() });
                                    }
                                }
                            }
                            row.clear();
                            row_at = row_at.saturating_add(row_repeat);
                        }
                        b"table:table" => in_sheet = false,
                        _ => {}
                    }
                }
            }
            Event::Text(t) if skip == 0 => {
                if let Some(c) = &mut open
                    && c.paragraphs > 0
                {
                    c.text.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Event::GeneralRef(r) if skip == 0 => {
                if let Some(c) = &mut open
                    && c.paragraphs > 0
                {
                    match &*String::from_utf8_lossy(r.as_ref()) {
                        "amp" => c.text.push('&'),
                        "lt" => c.text.push('<'),
                        "gt" => c.text.push('>'),
                        "quot" => c.text.push('"'),
                        "apos" => c.text.push('\''),
                        n => {
                            let code = n.strip_prefix("#x").map(|h| u32::from_str_radix(h, 16)).or_else(|| n.strip_prefix('#').map(str::parse));
                            if let Some(ch) = code.and_then(|c| c.ok()).and_then(char::from_u32) {
                                c.text.push(ch);
                            }
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if !saw_spreadsheet {
        return Err(Error::NotOds);
    }
    Ok(book)
}
