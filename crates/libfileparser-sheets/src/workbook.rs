//! A workbook read by calamine: each sheet a table, kept as only the cells it has.

use std::io::Cursor;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use calamine::{Data, DataRef, Reader as _, SheetType, SheetVisible, Sheets};
use libfileparser_core::{
    Category, Cell, Column, Content, Count, Document, FacetInfo, FileSource, FormatInfo, Limit, MemoryUse, Model, OwnedTable, ParseContext, ParseError, Result, Role, TableInfo, TableSource,
};

/// The file's bytes, shared rather than copied, as calamine's reader.
#[derive(Clone)]
pub struct Shared(pub Arc<dyn AsRef<[u8]> + Send + Sync>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

type Book = Sheets<Cursor<Shared>>;

/// `A`, `B`… `Z`, `AA`…
pub fn column_name(mut c: u32) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

pub fn number(v: f64) -> Cell {
    // A spreadsheet stores every number as a double; 3.0 is the 3 the user typed.
    if v.fract() == 0.0 && v.abs() < 9e15 { Cell::Int(v as i64) } else { Cell::Float(v) }
}

fn date(d: &calamine::ExcelDateTime) -> Cell {
    if d.is_duration() {
        return Cell::Date(match d.as_duration() {
            Some(t) => format!("{}:{:02}:{:02}", t.num_hours(), t.num_minutes() % 60, t.num_seconds() % 60),
            None => d.as_f64().to_string(),
        });
    }
    match d.as_datetime() {
        Some(t) if t.time() == Default::default() => Cell::Date(t.date().to_string()),
        Some(t) => Cell::Date(t.to_string().replacen(' ', "T", 1)),
        None => number(d.as_f64()),
    }
}

fn cell(d: &Data) -> Option<Cell> {
    Some(match d {
        Data::Empty => return None,
        Data::Int(v) => Cell::Int(*v),
        Data::Float(v) => number(*v),
        Data::String(s) => Cell::Text(s.clone()),
        Data::Bool(b) => Cell::Bool(*b),
        Data::DateTime(t) => date(t),
        Data::DateTimeIso(s) | Data::DurationIso(s) => Cell::Date(s.clone()),
        Data::Error(e) => Cell::Error(e.to_string()),
    })
}

fn cell_ref(d: &DataRef<'_>) -> Option<Cell> {
    Some(match d {
        DataRef::Empty => return None,
        DataRef::Int(v) => Cell::Int(*v),
        DataRef::Float(v) => number(*v),
        DataRef::String(s) => Cell::Text(s.clone()),
        DataRef::SharedString(s) => Cell::Text(s.to_string()),
        DataRef::Bool(b) => Cell::Bool(*b),
        DataRef::DateTime(t) => date(t),
        DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => Cell::Date(s.clone()),
        DataRef::Error(e) => Cell::Error(e.to_string()),
    })
}

/// One sheet, only the cells it has: a sheet can name its last cell a million rows down
/// and hold ten.
pub struct Sheet {
    columns: Vec<Column>,
    rows: u64,
    /// By row, then column.
    cells: Vec<(u32, u32, Cell)>,
    heap: u64,
}

/// Columns A to XFD: as wide as Excel, LibreOffice and Numbers go. A file naming a cell
/// further right is damaged, and every row read is laid out this wide.
pub(crate) const MAX_COLUMNS: u32 = 16_384;

impl Sheet {
    pub fn new(mut cells: Vec<(u32, u32, Cell)>, cx: &ParseContext) -> Result<Self> {
        cells.sort_by_key(|c| (c.0, c.1));
        let rows = cells.last().map_or(0, |c| c.0 as u64 + 1);
        let width = cells.iter().map(|c| c.1).max().map_or(0, |c| c.saturating_add(1));
        if width > MAX_COLUMNS {
            return Err(ParseError::corrupt(format!("a cell in column {}, past the last column ({})", width, column_name(MAX_COLUMNS - 1))));
        }
        cx.limits.check(Limit::Rows, rows)?;
        let text: u64 = cells.iter().map(|c| if let Cell::Text(s) = &c.2 { s.len() as u64 } else { 0 }).sum();
        let heap = cells.len() as u64 * size_of::<(u32, u32, Cell)>() as u64 + text;
        cx.limits.check(Limit::Memory, heap)?;
        Ok(Self { columns: (0..width).map(|c| Column::new(column_name(c))).collect(), rows, cells, heap })
    }
}

impl TableSource for Sheet {
    fn info(&self) -> TableInfo {
        TableInfo::new(self.columns.clone(), Count::Exact(self.rows))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<Cell>>> {
        let end = range.end.min(self.rows);
        let width = self.columns.len();
        let mut out = vec![vec![Cell::Null; width]; end.saturating_sub(range.start) as usize];
        let from = self.cells.partition_point(|c| (c.0 as u64) < range.start);
        for (r, c, v) in self.cells[from..].iter().take_while(|c| (c.0 as u64) < end) {
            out[(*r as u64 - range.start) as usize][*c as usize] = v.clone();
        }
        Ok(out)
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.heap)
    }
}

fn error(e: impl std::fmt::Display) -> ParseError {
    let text = e.to_string();
    if text.to_ascii_lowercase().contains("password") {
        ParseError::unsupported("This workbook is protected by a password.")
    } else {
        ParseError::corrupt(text)
    }
}

/// Every so often: is anyone still waiting for this?
fn tick(n: &mut u32, cx: &ParseContext) -> Result<()> {
    *n = n.wrapping_add(1);
    if *n & 0xfff == 0 { cx.cancel.check() } else { Ok(()) }
}

fn read_cells(book: &mut Book, name: &str, cx: &ParseContext) -> Result<Vec<(u32, u32, Cell)>> {
    let mut out = Vec::new();
    let mut n = 0;
    match book {
        // Cell by cell: nothing here is sized by what the file claims.
        Sheets::Xlsx(x) => {
            let mut r = x.worksheet_cells_reader(name).map_err(error)?;
            while let Some(c) = r.next_cell().map_err(error)? {
                tick(&mut n, cx)?;
                if let Some(v) = cell_ref(c.get_value()) {
                    out.push((c.get_position().0, c.get_position().1, v));
                }
            }
        }
        Sheets::Xlsb(x) => {
            let mut r = x.worksheet_cells_reader(name).map_err(error)?;
            while let Some(c) = r.next_cell().map_err(error)? {
                tick(&mut n, cx)?;
                if let Some(v) = cell_ref(c.get_value()) {
                    out.push((c.get_position().0, c.get_position().1, v));
                }
            }
        }
        // Measured by guard.rs before these were opened.
        book => {
            let range = book.worksheet_range(name).map_err(error)?;
            let (top, left) = range.start().unwrap_or((0, 0));
            for (r, c, v) in range.used_cells() {
                tick(&mut n, cx)?;
                if let Some(v) = cell(v) {
                    out.push((top + r as u32, left + c as u32, v));
                }
            }
        }
    }
    Ok(out)
}

fn read_formulas(book: &mut Book, name: &str, out: &mut Vec<Vec<Cell>>, cx: &ParseContext) -> Result<()> {
    let mut add = |r: u32, c: u32, f: String| out.push(vec![Cell::Text(name.to_string()), Cell::Text(format!("{}{}", column_name(c), r + 1)), Cell::Text(f)]);
    let mut n = 0;
    match book {
        Sheets::Xlsx(x) => {
            let mut r = x.worksheet_cells_reader(name).map_err(error)?;
            while let Some(c) = r.next_formula().map_err(error)? {
                tick(&mut n, cx)?;
                if !c.get_value().is_empty() {
                    add(c.get_position().0, c.get_position().1, c.get_value().clone());
                }
            }
        }
        Sheets::Xlsb(x) => {
            let mut r = x.worksheet_cells_reader(name).map_err(error)?;
            while let Some(c) = r.next_formula().map_err(error)? {
                tick(&mut n, cx)?;
                if !c.get_value().is_empty() {
                    add(c.get_position().0, c.get_position().1, c.get_value().clone());
                }
            }
        }
        book => {
            let range = book.worksheet_formula(name).map_err(error)?;
            let (top, left) = range.start().unwrap_or((0, 0));
            for (r, c, f) in range.used_cells() {
                tick(&mut n, cx)?;
                if !f.is_empty() {
                    add(top + r as u32, left + c as u32, f.clone());
                }
            }
        }
    }
    Ok(())
}

/// Runs calamine. It indexes and subtracts without checking on some damaged files; a
/// panic there is that file being damaged. Sheets and formulas read later, for a lazy
/// facet, go through here too.
fn calm<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(ParseError::corrupt("the workbook's structure is damaged")))
}

/// Opens a workbook whose bytes passed `guard`'s checks.
pub fn open(src: &FileSource, bytes: Shared, reader: &'static str, format: &'static str, cx: &ParseContext) -> Result<Document> {
    calm(|| open_workbook(src, bytes, reader, format, cx))
}

fn open_workbook(src: &FileSource, bytes: Shared, reader: &'static str, format: &'static str, cx: &ParseContext) -> Result<Document> {
    let rs = Cursor::new(bytes);
    // By the format we already know: calamine's own detection hides why a file failed.
    let book: Book = match reader {
        "xlsx" => Sheets::Xlsx(calamine::Xlsx::new(rs).map_err(error)?),
        "xlsb" => Sheets::Xlsb(calamine::Xlsb::new(rs).map_err(error)?),
        _ => Sheets::Xls(calamine::Xls::new(rs).map_err(error)?),
    };
    let sheets: Vec<(String, bool)> =
        book.sheets_metadata().iter().filter(|s| matches!(s.typ, SheetType::WorkSheet | SheetType::MacroSheet)).map(|s| (s.name.clone(), s.visible == SheetVisible::Visible)).collect();
    let names = book.defined_names().to_vec();
    let book = Arc::new(Mutex::new(book));

    // A workbook opens on its first visible sheet; hidden ones come after the others.
    let mut order: Vec<&(String, bool)> = sheets.iter().collect();
    order.sort_by_key(|s| !s.1);
    let Some((first, _)) = order.first().map(|s| (s.0.clone(), s.1)) else {
        return Err(ParseError::corrupt("a workbook with no sheets"));
    };
    let table = Sheet::new(read_cells(&mut book.lock().unwrap_or_else(|e| e.into_inner()), &first, cx)?, cx)?;
    let mut doc = Document::new(FormatInfo::new(reader, format, Category::Sheets)).with_facet(Role::Primary, first.clone(), Content::Table(Arc::new(table)));
    for (name, visible) in order.iter().skip(1) {
        let label = if *visible { name.clone() } else { format!("{name} (hidden)") };
        let (book, name) = (book.clone(), name.clone());
        doc = doc.with_lazy_facet(FacetInfo::new(Role::Structure, label, Model::Table), move |cx| {
            let cells = calm(|| read_cells(&mut book.lock().unwrap_or_else(|e| e.into_inner()), &name, cx))?;
            Ok(Content::Table(Arc::new(Sheet::new(cells, cx)?)))
        });
    }
    let all: Vec<String> = sheets.iter().map(|s| s.0.clone()).collect();
    let from = book.clone();
    doc = doc.with_lazy_facet(FacetInfo::new(Role::Inspector, "Formulas", Model::Table), move |cx| {
        let mut rows = Vec::new();
        for name in &all {
            cx.cancel.check()?;
            // A sheet whose formulas can't be read shouldn't cost the others.
            let _ = calm(|| read_formulas(&mut from.lock().unwrap_or_else(|e| e.into_inner()), name, &mut rows, cx));
        }
        Ok(Content::Table(Arc::new(OwnedTable::text(vec!["Sheet".into(), "Cell".into(), "Formula".into()], Vec::new()).with_cells(rows))))
    });
    if !names.is_empty() {
        let rows = names.into_iter().map(|(n, r)| vec![n, r]).collect();
        doc = doc.with_facet(Role::Inspector, "Defined names", Content::Table(Arc::new(OwnedTable::text(vec!["Name".into(), "Refers to".into()], rows))));
    }
    let hidden = sheets.iter().filter(|s| !s.1).count();
    doc = doc.with_meta("Sheets", sheets.len());
    if hidden > 0 {
        doc = doc.with_meta("Hidden sheets", hidden);
    }
    Ok(doc.with_file_facets(src, None))
}
