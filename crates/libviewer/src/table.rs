//! Tables and containers, read a row at a time as they scroll into view, and again
//! every frame they're drawn: a live table (Textures going from decoding to decoded)
//! shows what it holds now, and a table of a million rows costs what's on screen.

use std::cell::RefCell;
use std::sync::Arc;

use libfileparser_core::find::Spot;
use libfileparser_core::{Cell, Container, Entry, EntryKind, TableSource};
use libgui::*;

use crate::find::{Search, matching};

/// A table's state: its columns (made when it is first shown) and libgui's.
pub struct TableView {
    state: TableState,
}

impl TableView {
    pub fn for_table(t: &dyn TableSource) -> Self {
        let columns = t.info().columns;
        // Numbers right-aligned, so their digits line up.
        Self {
            state: TableState::new(columns.iter().map(|c| {
                let column = Column::new(c.name.as_str()).width(160.0).unsortable();
                if c.kind.is_numeric() { column.align(Align::End) } else { column }
            })),
        }
    }

    /// Name and size, and Packed when the entries listed so far say how they were packed.
    pub fn for_container(c: &dyn Container) -> Self {
        let mut columns = vec![Column::new("Name").width(320.0).grow(1.0).unsortable(), Column::new("Size").width(110.0).align(Align::End).unsortable()];
        // Only archives pack their entries; a folder's column would stay empty.
        if c.entries(0..100).unwrap_or_default().iter().any(|e| e.packed.is_some()) {
            columns.push(Column::new("Packed").width(110.0).align(Align::End).unsortable());
        }
        Self { state: TableState::new(columns) }
    }
}

/// One row, read once per frame however many of its cells are drawn.
struct RowCache<T> {
    row: Option<(usize, T)>,
}

impl<T: Clone> RowCache<T> {
    fn get(&mut self, row: usize, read: impl FnOnce() -> T) -> T {
        match &self.row {
            Some((r, v)) if *r == row => v.clone(),
            _ => {
                let v = read();
                self.row = Some((row, v.clone()));
                v
            }
        }
    }
}

pub fn table(ui: &mut Ui, t: &dyn TableSource, view: &mut TableView) {
    table_found(ui, t, view, None);
}

/// The table's options: the current match's row selected, and brought into view
/// (with a row either side), its column too, when it has just moved.
fn options(ui: &mut Ui, rows: usize, search: Option<&Search>) -> TableOptions {
    let current = search.and_then(Search::current_spot);
    let reveal = search.and_then(|s| s.reveal_row(ui, rows, 1, Spot::index));
    let reveal_column = match (&current, reveal) {
        (Some(Spot::Cell { column, .. }), Some(_)) => Some(*column),
        _ => None,
    };
    TableOptions { selected: current.map(|s| s.index() as usize), reveal, reveal_column, ..TableOptions::themed(&ui.theme) }
}

/// Text of a matched cell: the current match's in the accent colour, the others in the
/// warning one.
fn found_label(ui: &mut Ui, text: &str, current: bool) {
    let pal = ui.theme.palette;
    let size = ui.theme.metrics.font_size;
    ui.text_with(text, size, if current { pal.accent } else { pal.warning });
}

/// Whether the cell at `row`, `col` is a match, and the current one.
fn cell_match(found: &Option<(std::sync::MutexGuard<'_, crate::find::Found>, Option<usize>)>, row: usize, col: Option<usize>) -> Option<bool> {
    let (f, at) = found.as_ref()?;
    matching(&f.spots, row as u64, Spot::index).find(|&k| match &f.spots[k] {
        Spot::Cell { column, .. } => Some(*column) == col,
        _ => true,
    }).map(|k| *at == Some(k))
}

/// [`table`], with `search`'s matching cells marked and the current one's row selected.
pub fn table_found(ui: &mut Ui, t: &dyn TableSource, view: &mut TableView, search: Option<&Search>) {
    let rows = t.info().rows.known() as usize;
    let cache = RefCell::new(RowCache { row: None });
    let opts = options(ui, rows, search);
    let found = search.map(|s| (s.found(), s.current()));
    ui.table_with("table", &mut view.state, rows, opts, |ui, row, col| {
        let cells = cache.borrow_mut().get(row, || t.rows(row as u64..row as u64 + 1).ok().and_then(|mut r| r.pop()).unwrap_or_default());
        match cells.get(col) {
            // Null isn't empty text: shown, but quietly.
            Some(Cell::Null) => ui.label_muted("null"),
            Some(Cell::Error(why)) => ui.label_muted(&format!("⚠ {why}")),
            Some(cell) => match cell_match(&found, row, Some(col)) {
                Some(current) => found_label(ui, &cell.to_string(), current),
                None => ui.label(&cell.to_string()),
            },
            None => {}
        }
    });
}

/// A container's entries; returns the one clicked, to open as a file of its own.
pub fn container(ui: &mut Ui, c: &Arc<dyn Container>, view: &mut TableView) -> Option<u64> {
    container_found(ui, c, view, None)
}

/// [`container`], with `search`'s matching entries marked and the current one selected.
pub fn container_found(ui: &mut Ui, c: &Arc<dyn Container>, view: &mut TableView, search: Option<&Search>) -> Option<u64> {
    let count = c.entry_count().known() as usize;
    let cache: RefCell<RowCache<Option<Entry>>> = RefCell::new(RowCache { row: None });
    let entry = |row: usize| cache.borrow_mut().get(row, || c.entries(row as u64..row as u64 + 1).ok().and_then(|mut e| e.pop()));
    let opts = options(ui, count, search);
    let found = search.map(|s| (s.found(), s.current()));
    let r = ui.table_with("entries", &mut view.state, count, opts, |ui, row, col| {
        let Some(e) = entry(row) else { return };
        let folder = e.kind == EntryKind::Folder;
        let hit = if col == 0 { cell_match(&found, row, None) } else { None };
        match col {
            0 if hit.is_some() => found_label(ui, &if folder { format!("{}/", e.path.trim_end_matches('/')) } else { e.path.clone() }, hit == Some(true)),
            0 if folder => ui.label_muted(&format!("{}/", e.path.trim_end_matches('/'))),
            0 => ui.label(&e.path),
            1 if !folder => ui.label(&human_bytes(e.size)),
            2 => {
                if let Some(z) = e.packed.filter(|_| !folder) {
                    ui.label_muted(&human_bytes(z));
                }
            }
            _ => {}
        }
    });
    r.clicked_row.filter(|&i| entry(i).is_some_and(|e| e.kind != EntryKind::Folder)).map(|i| i as u64)
}

/// "476.4 KiB".
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}
