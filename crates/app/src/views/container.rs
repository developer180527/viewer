//! Archive listing. Clicking a file opens it on top of the navigation stack.

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use viewer_core::Container;

use crate::util::human_bytes;

/// Returns the index of the entry the user opened.
pub fn show(ui: &mut egui::Ui, c: &dyn Container) -> Option<usize> {
    let entries = c.entries();
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    let mut opened = None;
    TableBuilder::new(ui)
        .striped(true)
        .auto_shrink(false)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::remainder().clip(true))
        .column(Column::auto().at_least(80.0))
        .column(Column::auto().at_least(80.0))
        .header(row_height, |mut h| {
            h.col(|ui| {
                ui.strong("Name");
            });
            h.col(|ui| {
                ui.strong("Size");
            });
            h.col(|ui| {
                ui.strong("Packed");
            });
        })
        .body(|body| {
            body.rows(row_height, entries.len(), |mut row| {
                let i = row.index();
                let e = &entries[i];
                row.col(|ui| {
                    if e.is_dir {
                        ui.weak(format!("📁 {}", e.path));
                    } else if ui.link(format!("📄 {}", e.path)).clicked() {
                        opened = Some(i);
                    }
                });
                row.col(|ui| {
                    if !e.is_dir {
                        ui.label(human_bytes(e.size));
                    }
                });
                row.col(|ui| {
                    if let Some(p) = e.compressed_size.filter(|_| !e.is_dir) {
                        ui.weak(human_bytes(p));
                    }
                });
            });
        });
    opened
}
