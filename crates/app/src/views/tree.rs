//! Collapsible tree. Large child lists are grouped in chunks so expanding a
//! 100 000-element array stays instant.

use eframe::egui::{self, Color32, Id, RichText};
use viewer_core::TreeNode;

const CHUNK: usize = 200;

pub fn show(ui: &mut egui::Ui, root: &TreeNode) {
    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        let id = ui.scope_id().with("tree");
        node(ui, root, id, true);
    });
}

fn node(ui: &mut egui::Ui, n: &TreeNode, id: Id, open: bool) {
    if n.children.is_empty() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(format!("{}:", n.key)).strong());
            ui.label(RichText::new(&n.value).monospace().color(value_color(ui, &n.value)));
        });
        return;
    }
    let header = RichText::new(format!("{}  {}", n.key, n.value));
    egui::CollapsingHeader::new(header).id_salt(id).default_open(open).show(ui, |ui| {
        if n.children.len() <= CHUNK {
            for (i, c) in n.children.iter().enumerate() {
                node(ui, c, id.with(i), false);
            }
            return;
        }
        for (ci, chunk) in n.children.chunks(CHUNK).enumerate() {
            let first = ci * CHUNK;
            let label = format!("[{} … {}]", first, first + chunk.len() - 1);
            egui::CollapsingHeader::new(label).id_salt(id.with(("chunk", ci))).show(ui, |ui| {
                for (i, c) in chunk.iter().enumerate() {
                    node(ui, c, id.with(first + i), false);
                }
            });
        }
    });
}

fn value_color(ui: &egui::Ui, v: &str) -> Color32 {
    let dark = ui.visuals().dark_mode;
    match v.chars().next() {
        Some('"') if dark => Color32::from_rgb(152, 195, 121),
        Some('"') => Color32::from_rgb(34, 134, 58),
        Some('0'..='9' | '-') if dark => Color32::from_rgb(209, 154, 102),
        Some('0'..='9' | '-') => Color32::from_rgb(0, 92, 197),
        _ if dark => Color32::from_rgb(198, 120, 221),
        _ => Color32::from_rgb(111, 66, 193),
    }
}
