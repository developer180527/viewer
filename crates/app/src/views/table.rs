//! Virtualised table: only visible rows are built.

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use viewer_core::Table;

pub fn show(ui: &mut egui::Ui, t: &Table) {
    let cols = t.columns;
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    egui::ScrollArea::horizontal().auto_shrink(false).show(ui, |ui| {
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .auto_shrink(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::auto().at_least(32.0))
            .columns(Column::initial(140.0).at_least(40.0).clip(true), cols)
            .header(row_height + 2.0, |mut h| {
                h.col(|ui| {
                    ui.weak("#");
                });
                for c in 0..cols {
                    h.col(|ui| {
                        ui.strong(t.headers.get(c).map_or("", String::as_str));
                    });
                }
            })
            .body(|body| {
                body.rows(row_height, t.rows.len(), |mut row| {
                    let cells = &t.rows[row.index()];
                    let n = row.index() + 1;
                    row.col(|ui| {
                        ui.weak(n.to_string());
                    });
                    for c in 0..cols {
                        row.col(|ui| {
                            ui.add(egui::Label::new(cells.get(c).map_or("", String::as_str)).truncate());
                        });
                    }
                });
            });
    });
}
