//! Tables and containers, read a row at a time as they scroll into view, and again
//! every frame they're drawn: a live table (Textures going from decoding to decoded)
//! shows what it holds now, and a table of a million rows costs what's on screen.

use std::cell::RefCell;
use std::sync::Arc;

use libfileparser_core::{Container, Entry, EntryKind, TableSource};
use libgui::*;

/// A table's state: its columns (made when it is first shown) and libgui's.
pub struct TableView {
    state: TableState,
}

impl TableView {
    pub fn for_table(t: &dyn TableSource) -> Self {
        let columns = t.info().columns;
        Self { state: TableState::new(columns.iter().map(|c| Column::new(c).width(160.0).unsortable())) }
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
    let rows = t.info().rows.known() as usize;
    let cache = RefCell::new(RowCache { row: None });
    ui.table("table", &mut view.state, rows, |ui, row, col| {
        let cells = cache.borrow_mut().get(row, || t.rows(row as u64..row as u64 + 1).ok().and_then(|mut r| r.pop()).unwrap_or_default());
        ui.label(cells.get(col).map_or("", String::as_str));
    });
}

/// A container's entries; returns the one clicked, to open as a file of its own.
pub fn container(ui: &mut Ui, c: &Arc<dyn Container>, view: &mut TableView) -> Option<u64> {
    let count = c.entry_count().known() as usize;
    let cache: RefCell<RowCache<Option<Entry>>> = RefCell::new(RowCache { row: None });
    let entry = |row: usize| cache.borrow_mut().get(row, || c.entries(row as u64..row as u64 + 1).ok().and_then(|mut e| e.pop()));
    let r = ui.table("entries", &mut view.state, count, |ui, row, col| {
        let Some(e) = entry(row) else { return };
        let folder = e.kind == EntryKind::Folder;
        match col {
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
