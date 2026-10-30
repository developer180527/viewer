//! Renderers, one per content model. They never know which file format they show.

mod container;
mod hex;
mod raster;
mod table;
mod text;
mod tree;

use std::sync::Arc;

use eframe::egui;
use viewer_core::{Container, Content};

use crate::app::Page;

/// Something a view asks the app to do.
pub enum Action {
    OpenEntry(Arc<dyn Container>, usize),
}

/// Per-page renderer state that egui can't keep for us.
pub enum ViewState {
    None,
    Raster(raster::RasterState),
}

impl ViewState {
    pub fn new(content: &Content) -> Self {
        match content {
            Content::Raster(_) => Self::Raster(Default::default()),
            _ => Self::None,
        }
    }
}

pub fn show(ui: &mut egui::Ui, page: &mut Page, md_cache: &mut egui_commonmark::CommonMarkCache) -> Option<Action> {
    // Scroll positions etc. are keyed by page, so going back restores them.
    ui.push_id(page.id, |ui| match (&page.doc.content, &mut page.view) {
        (Content::Raster(r), ViewState::Raster(state)) => {
            raster::show(ui, r, state);
            None
        }
        (Content::Text(t), _) => {
            text::show(ui, t);
            None
        }
        (Content::Markdown(md), _) => {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(860.0));
                egui_commonmark::CommonMarkViewer::new().show(ui, md_cache, md);
            });
            None
        }
        (Content::Tree(root), _) => {
            tree::show(ui, root);
            None
        }
        (Content::Table(t), _) => {
            table::show(ui, t);
            None
        }
        (Content::Container(c), _) => container::show(ui, c.as_ref()).map(|i| Action::OpenEntry(c.clone(), i)),
        (Content::Hex(src), _) => {
            hex::show(ui, src.bytes());
            None
        }
        (Content::Raster(_), _) => unreachable!("raster pages always get raster state"),
    })
    .inner
}
