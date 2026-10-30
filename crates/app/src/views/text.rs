//! Virtualised text view: only visible lines are laid out, so huge files scroll freely.

use eframe::egui::{self, Color32, FontId, TextFormat, TextStyle, text::LayoutJob};
use viewer_core::TextDoc;

/// Longer lines are cut for display (minified files would otherwise stall layout).
const MAX_LINE_BYTES: usize = 8 * 1024;

pub fn show(ui: &mut egui::Ui, doc: &TextDoc) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let row_height = ui.text_style_height(&TextStyle::Monospace);
    let gutter = doc.line_count().to_string().len();
    let theme = if ui.visuals().dark_mode { 0 } else { 1 };
    let weak = ui.visuals().weak_text_color();
    let normal = ui.visuals().text_color();

    egui::ScrollArea::both().auto_shrink(false).show_rows(ui, row_height, doc.line_count(), |ui, rows| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in rows {
            let mut job = LayoutJob::default();
            job.append(&format!("{:>gutter$}  ", i + 1), 0.0, format(&font, weak, false));
            let raw = doc.raw_line(i);
            let full = String::from_utf8_lossy(&raw[..raw.len().min(MAX_LINE_BYTES)]);
            let line = full.trim_end_matches(['\n', '\r']);
            match doc.highlights.as_ref().filter(|_| raw.len() <= MAX_LINE_BYTES) {
                Some(h) => {
                    for span in &h[theme][i] {
                        let end = span.range.end.min(line.len());
                        if span.range.start >= end {
                            continue;
                        }
                        let [r, g, b] = span.color;
                        // egui ships no bold monospace face, so `span.bold` is ignored.
                        job.append(&line[span.range.start..end], 0.0, format(&font, Color32::from_rgb(r, g, b), span.italic));
                    }
                }
                None => {
                    job.append(line, 0.0, format(&font, normal, false));
                    if raw.len() > MAX_LINE_BYTES {
                        job.append(&format!("  … ({} more bytes)", raw.len() - MAX_LINE_BYTES), 0.0, format(&font, weak, true));
                    }
                }
            }
            ui.add(egui::Label::new(job).extend());
        }
    });
}

fn format(font: &FontId, color: Color32, italics: bool) -> TextFormat {
    TextFormat { font_id: font.clone(), color, italics, ..Default::default() }
}
