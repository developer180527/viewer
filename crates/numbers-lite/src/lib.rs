//! Apple Numbers spreadsheets (`.numbers`), as Numbers 3 (2013) and later write them.
//!
//! A document is a zip (or, inside one, an `Index.zip`) of `Index/**/*.iwa` files. Each
//! is Snappy-compressed chunks holding *archives*: objects with an identifier and a
//! type, written as Protocol Buffers, which name each other by identifier. The ones a
//! reader of cells needs:
//!
//! ```text
//! Document (1) ── sheets ─▶ Sheet (2: its name) ── drawables ─▶ TableInfo (6000)
//!   ── model ─▶ TableModel (6001: name, rows, columns, header rows)
//!        ├─ tiles ─▶ Tile (6002): 256 rows each; a row is a buffer of cells and an
//!        │           offset into it per column
//!        ├─ strings ─▶ DataList (6005): text by key
//!        └─ rich text ─▶ DataList ─▶ payload (6218) ─▶ Storage (2001): its text
//! ```
//!
//! A sheet in Numbers is a canvas holding any number of tables, so a [`Sheet`] has
//! [`Table`]s. Cell values are read (numbers, text, dates, durations, booleans, errors);
//! formulas' results are, not their text. Styles, charts and images are ignored.
//!
//! Not read: documents from iWork '09 and before (a zip of `index.xml`), cells stored
//! only in the layout before Numbers 5.1 (counted in [`Table::unread_rows`]), and
//! encrypted documents ([`Error::Encrypted`]).

use std::collections::HashMap;
use std::fmt;
use std::io::{Cursor, Read};

mod proto;
mod snappy;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    NotNumbers,
    /// Protected by a password.
    Encrypted,
    Zip(String),
    /// A document whose parts hold no document object.
    Damaged(&'static str),
    TooLarge { what: &'static str, value: u64, max: u64 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotNumbers => write!(f, "not a Numbers spreadsheet"),
            Error::Encrypted => write!(f, "protected by a password"),
            Error::Zip(e) => write!(f, "zip: {e}"),
            Error::Damaged(what) => write!(f, "damaged: {what}"),
            Error::TooLarge { what, value, max } => write!(f, "{what}: {value}, more than the {max} allowed"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Limits {
    /// Cells with something in them.
    pub cells: u64,
    /// Bytes of text in cells.
    pub text: u64,
    /// Most bytes the document's parts may decompress to, together.
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
    /// A number, percentage or currency amount.
    Number(f64),
    Text(String),
    Bool(bool),
    /// A date and time, `2024-03-01T09:30:00`: Numbers keeps no time zone.
    Date(String),
    /// A length of time, in seconds.
    Duration(f64),
    /// A formula that failed.
    Error,
}

/// A cell, at its 0-based row and column in its table.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Table {
    pub name: String,
    pub rows: u32,
    pub columns: u32,
    /// How many of its first rows and columns are headers.
    pub header_rows: u32,
    pub header_columns: u32,
    pub cells: Vec<Cell>,
    /// Rows stored only as Numbers wrote them before 5.1, which aren't read.
    pub unread_rows: u32,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Sheet {
    pub name: String,
    pub tables: Vec<Table>,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    /// Parts that couldn't be decompressed and were passed over: what they held (some
    /// rows of a table, perhaps) is missing.
    pub unread_parts: u32,
}

/// The archive types kept: the rest of a document (styles, drawings, the calculation
/// engine) is passed over as it's decompressed.
const DOCUMENT: u32 = 1;
const SHEET: u32 = 2;
const STORAGE: u32 = 2001;
const TABLE_INFO: u32 = 6000;
const TABLE_MODEL: u32 = 6001;
const TILE: u32 = 6002;
const DATA_LIST: u32 = 6005;
const RICH_TEXT: u32 = 6218;

/// Every kept object of a document, by identifier: its type and its message.
type Objects = HashMap<u64, (u32, Vec<u8>)>;

/// The archives of one decompressed IWA file, added to `objects`. Each is a length, an
/// ArchiveInfo (its identifier; a MessageInfo per message: type and length), then the
/// messages; the first message is the object, any others patches to it.
fn archives(mut b: &[u8], objects: &mut Objects) {
    while !b.is_empty() {
        let Some(info) = proto::varint(&mut b).and_then(|n| usize::try_from(n).ok()).and_then(|n| b.split_at_checked(n)) else { return };
        b = info.1;
        let id = proto::number(info.0, 1);
        let mut first = true;
        for message in proto::all(info.0, 2) {
            let Some(len) = proto::number(message, 3).and_then(|n| usize::try_from(n).ok()) else { return };
            let Some((body, rest)) = b.split_at_checked(len) else { return };
            b = rest;
            let kind = proto::number(message, 1).and_then(|k| u32::try_from(k).ok()).unwrap_or(0);
            if let (true, Some(id), DOCUMENT | SHEET | STORAGE | TABLE_INFO | TABLE_MODEL | TILE | DATA_LIST | RICH_TEXT) = (first, id, kind) {
                objects.entry(id).or_insert_with(|| (kind, body.to_vec()));
            }
            first = false;
        }
    }
}

/// A number as Numbers stores it: 128-bit decimal floating point, a 113-bit whole
/// number times a power of ten, as the nearest double.
fn decimal(b: &[u8; 16]) -> f64 {
    let exponent = (b[15] & 0x7f) as i32 * 128 + (b[14] >> 1) as i32 - 0x1820;
    let whole = b[..14].iter().rev().fold((b[14] & 1) as u128, |whole, byte| whole * 256 + *byte as u128);
    // Dividing by a power of ten is exact where multiplying by its inverse isn't
    // (0.3 is 3/10, not 3 × 0.1000000000000000055…).
    let value = if exponent >= 1 { whole as f64 * 10f64.powi(exponent) } else { whole as f64 / 10f64.powi(-exponent) };
    if b[15] & 0x80 != 0 { -value } else { value }
}

/// Seconds since 2001-01-01 as a date and time.
fn date(seconds: f64) -> Option<String> {
    // Years 1 to 9999.
    if !(-63_113_904_000.0..252_423_993_600.0).contains(&seconds) {
        return None;
    }
    let total = seconds.floor() as i64 + 978_307_200;
    let (days, of_day) = (total.div_euclid(86_400), total.rem_euclid(86_400));
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, month) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some(format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}", of_day / 3600, of_day / 60 % 60, of_day % 60))
}

/// A table's text, by the keys its cells hold.
#[derive(Default)]
struct Texts {
    strings: HashMap<u32, String>,
    rich: HashMap<u32, String>,
}

/// One cell from a row's buffer (the layout since Numbers 5.1, version 5): a version, a
/// type, flags saying which fields follow, then those fields in the flags' order.
/// `None` for an empty cell (a style with no value) or one of another version.
fn cell(b: &[u8], texts: &Texts) -> Option<Value> {
    let (version, kind) = (*b.first()?, *b.get(1)?);
    if version != 5 {
        return None;
    }
    let flags = u32::from_le_bytes(b.get(8..12)?.try_into().unwrap());
    let mut at = 12;
    let mut field = |bit: u32, len: usize| -> Option<&[u8]> {
        if flags & bit == 0 {
            return None;
        }
        let f = b.get(at..at + len);
        at += len;
        f
    };
    let decimal = field(1, 16).map(|f| decimal(f.try_into().unwrap()));
    let double = field(2, 8).map(|f| f64::from_le_bytes(f.try_into().unwrap()));
    let seconds = field(4, 8).map(|f| f64::from_le_bytes(f.try_into().unwrap()));
    let string = field(8, 4).map(|f| u32::from_le_bytes(f.try_into().unwrap()));
    let rich = field(16, 4).map(|f| u32::from_le_bytes(f.try_into().unwrap()));
    Some(match kind {
        // Number and currency.
        2 | 10 => Value::Number(decimal.or(double)?),
        3 => Value::Text(texts.strings.get(&string?)?.clone()),
        5 => seconds.and_then(date).map(Value::Date)?,
        6 => Value::Bool(double? > 0.0),
        7 => Value::Duration(double?),
        8 => Value::Error,
        9 => Value::Text(texts.rich.get(&rich?)?.clone()),
        _ => return None,
    })
}

/// What's counted against the limits while a document is read.
struct Budget<'a> {
    limits: &'a Limits,
    cells: u64,
    text: u64,
}

impl Budget<'_> {
    fn add(&mut self, value: &Value) -> Result<()> {
        self.cells += 1;
        if self.cells > self.limits.cells {
            return Err(Error::TooLarge { what: "cells", value: self.cells, max: self.limits.cells });
        }
        if let Value::Text(s) | Value::Date(s) = value {
            self.text = self.text.saturating_add(s.len() as u64);
            if self.text > self.limits.text {
                return Err(Error::TooLarge { what: "bytes of cell text", value: self.text, max: self.limits.text });
            }
        }
        Ok(())
    }
}

/// A data list's entries: key and entry.
fn entries(objects: &Objects, list: Option<u64>) -> impl Iterator<Item = (u32, &[u8])> {
    let body = list.and_then(|id| objects.get(&id)).filter(|o| o.0 == DATA_LIST).map_or(&[][..], |o| &o.1[..]);
    proto::all(body, 3).filter_map(|e| Some((u32::try_from(proto::number(e, 1)?).ok()?, e)))
}

fn table(objects: &Objects, model: &[u8], budget: &mut Budget) -> Result<Table> {
    let count = |n: u32| proto::number(model, n).map_or(0, |v| v.min(u32::MAX as u64) as u32);
    let mut t = Table { name: proto::text(model, 8).unwrap_or_default(), rows: count(6), columns: count(7), header_rows: count(10), header_columns: count(11), ..Table::default() };
    let Some(store) = proto::bytes(model, 4) else { return Ok(t) };
    let mut texts = Texts::default();
    for (key, entry) in entries(objects, proto::reference(store, 4)) {
        texts.strings.insert(key, proto::text(entry, 3).unwrap_or_default());
    }
    // Rich text: the entry names a payload, the payload a storage, whose text it is.
    for (key, entry) in entries(objects, proto::reference(store, 17)) {
        let storage = proto::reference(entry, 9).and_then(|id| objects.get(&id)).filter(|o| o.0 == RICH_TEXT).and_then(|o| proto::reference(&o.1, 1)).and_then(|id| objects.get(&id)).filter(|o| o.0 == STORAGE);
        if let Some((_, storage)) = storage {
            texts.rich.insert(key, proto::all(storage, 3).map(String::from_utf8_lossy).collect());
        }
    }
    let Some(tiles) = proto::bytes(store, 3) else { return Ok(t) };
    let tile_rows = proto::number(tiles, 2).filter(|n| (1..=1 << 16).contains(n)).unwrap_or(256) as u32;
    for tile in proto::all(tiles, 1) {
        let first_row = (proto::number(tile, 1).unwrap_or(0).min(u32::MAX as u64) as u32).saturating_mul(tile_rows);
        let Some((_, body)) = proto::reference(tile, 2).and_then(|id| objects.get(&id)).filter(|o| o.0 == TILE) else { continue };
        for row in proto::all(body, 5) {
            let index = first_row.saturating_add(proto::number(row, 1).unwrap_or(0).min(u32::MAX as u64) as u32);
            let (Some(buffer), Some(offsets)) = (proto::bytes(row, 6), proto::bytes(row, 7)) else {
                t.unread_rows += u32::from(proto::bytes(row, 3).is_some_and(|b| !b.is_empty()));
                continue;
            };
            // An offset per column, 0xFFFF where there's no cell; in units of four
            // bytes when the row is wide.
            let scale = if proto::number(row, 8).unwrap_or(0) != 0 { 4 } else { 1 };
            for (col, offset) in offsets.as_chunks::<2>().0.iter().map(|o| u16::from_le_bytes(*o)).enumerate() {
                if offset == u16::MAX {
                    continue;
                }
                if let Some(value) = buffer.get(offset as usize * scale..).and_then(|b| cell(b, &texts)) {
                    budget.add(&value)?;
                    t.cells.push(Cell { row: index, col: col as u32, value });
                }
            }
        }
    }
    Ok(t)
}

/// Reads a `.numbers` document's sheets and their tables.
pub fn read(bytes: &[u8], limits: &Limits) -> Result<Workbook> {
    if !bytes.starts_with(b"PK") {
        return Err(Error::NotNumbers);
    }
    let cap = limits.part_bytes.min(limits.unpack_always.max((bytes.len() as u64).saturating_mul(limits.unpack_ratio)));
    let mut unpacked = 0u64;
    let mut objects = Objects::new();
    let mut unread_parts = 0;
    let found = load(bytes, cap, &mut unpacked, &mut objects, &mut unread_parts, true)?;
    if !found {
        return Err(Error::NotNumbers);
    }
    let document = objects.values().find(|o| o.0 == DOCUMENT).ok_or(Error::Damaged("no document among its parts"))?;
    let mut budget = Budget { limits, cells: 0, text: 0 };
    let mut book = Workbook { unread_parts, ..Workbook::default() };
    for sheet in proto::all(&document.1, 1).filter_map(|r| proto::number(r, 1)) {
        let Some((_, sheet)) = objects.get(&sheet).filter(|o| o.0 == SHEET) else { continue };
        let mut tables = Vec::new();
        // A sheet's drawables: tables, and charts, images and text boxes, which aren't.
        for drawable in proto::all(sheet, 2).filter_map(|r| proto::number(r, 1)) {
            let model = objects.get(&drawable).filter(|o| o.0 == TABLE_INFO).and_then(|o| proto::reference(&o.1, 2)).and_then(|id| objects.get(&id)).filter(|o| o.0 == TABLE_MODEL);
            if let Some((_, model)) = model {
                tables.push(table(&objects, model, &mut budget)?);
            }
        }
        book.sheets.push(Sheet { name: proto::text(sheet, 1).unwrap_or_default(), tables });
    }
    Ok(book)
}

/// One entry of a zip, unzipped, counted against what may be unpacked in all.
fn entry(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str, cap: u64, unpacked: &mut u64) -> Result<Vec<u8>> {
    let file = zip.by_name(name).map_err(|e| Error::Zip(e.to_string()))?;
    let room = cap.saturating_sub(*unpacked);
    // One byte past the room is read, to know there was more: an entry's stated size
    // isn't trusted.
    let mut out = Vec::new();
    file.take(room.saturating_add(1)).read_to_end(&mut out).map_err(|e| Error::Zip(e.to_string()))?;
    *unpacked += out.len() as u64;
    if *unpacked > cap {
        return Err(Error::TooLarge { what: "bytes unpacked", value: *unpacked, max: cap });
    }
    Ok(out)
}

/// Decompresses a document's IWA parts into `objects`: those of this zip, or of the
/// `Index.zip` inside it. Whether it held any.
fn load(bytes: &[u8], cap: u64, unpacked: &mut u64, objects: &mut Objects, unread: &mut u32, outer: bool) -> Result<bool> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| Error::Zip(e.to_string()))?;
    let names: Vec<String> = zip.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
    // The marks of a password: the key's parameters and a hint.
    if names.iter().any(|n| n == ".iwpv2" || n.ends_with("/.iwpv2")) {
        return Err(Error::Encrypted);
    }
    let mut found = false;
    for name in names.iter().filter(|n| n.ends_with(".iwa") && (n.starts_with("Index/") || n.contains("/Index/"))) {
        found = true;
        let packed = entry(&mut zip, name, cap, unpacked)?;
        let room = usize::try_from(cap.saturating_sub(*unpacked)).unwrap_or(usize::MAX);
        // A part that is damaged is passed over: Numbers itself writes one now and then
        // (an operation log) that isn't Snappy at all, and the rest still reads.
        match snappy::iwa(&packed, room) {
            Ok(part) => {
                *unpacked += part.len() as u64;
                archives(&part, objects);
            }
            Err(snappy::Stop::Full) => return Err(Error::TooLarge { what: "bytes unpacked", value: cap.saturating_add(1), max: cap }),
            Err(snappy::Stop::Damaged) => *unread += 1,
        }
    }
    // Numbers 3 and 4 kept the parts in a zip of their own inside the document.
    if !found
        && outer
        && let Some(inner) = names.iter().find(|n| *n == "Index.zip" || n.ends_with("/Index.zip"))
    {
        let inner = entry(&mut zip, inner, cap, unpacked)?;
        return load(&inner, cap, unpacked, objects, unread, false);
    }
    Ok(found)
}

#[cfg(test)]
mod tests;
