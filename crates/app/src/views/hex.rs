//! Classic hex dump: offset, 16 bytes, ASCII. Virtualised over the memory map.

use std::fmt::Write;

use eframe::egui::{self, TextFormat, TextStyle, text::LayoutJob};

const PER_ROW: usize = 16;

pub fn show(ui: &mut egui::Ui, bytes: &[u8]) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let row_height = ui.text_style_height(&TextStyle::Monospace);
    let rows = bytes.len().div_ceil(PER_ROW);
    let offset_width = if bytes.len() > u32::MAX as usize { 12 } else { 8 };
    let (weak, normal, strong) = (ui.visuals().weak_text_color(), ui.visuals().text_color(), ui.visuals().strong_text_color());

    if bytes.is_empty() {
        ui.weak("Empty file");
        return;
    }
    egui::ScrollArea::both().auto_shrink(false).show_rows(ui, row_height, rows, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut hex = String::with_capacity(PER_ROW * 3 + 2);
        let mut ascii = String::with_capacity(PER_ROW);
        for row in range {
            let start = row * PER_ROW;
            let chunk = &bytes[start..(start + PER_ROW).min(bytes.len())];
            hex.clear();
            ascii.clear();
            for (i, b) in chunk.iter().enumerate() {
                let _ = write!(hex, "{b:02x} ");
                if i == 7 {
                    hex.push(' ');
                }
                ascii.push(if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '.' });
            }
            let mut job = LayoutJob::default();
            let fmt = |color| TextFormat { font_id: font.clone(), color, ..Default::default() };
            job.append(&format!("{start:0offset_width$x}  "), 0.0, fmt(weak));
            job.append(&format!("{hex:<50}"), 0.0, fmt(normal));
            job.append(&format!(" {ascii}"), 0.0, fmt(strong));
            ui.add(egui::Label::new(job).extend());
        }
    });
}
