//! A page's content on screen: libviewer's views for libfileparser's models, and
//! Viewer's own for what libfileparser has no model for yet (Markdown, PDF pages).

use libgui::*;
use viewer_core::{Content, TextDoc};

use super::app::Page;
pub use libviewer::Action;
use libviewer::Host;

/// Per-page view state that libgui leaves to us.
pub enum ViewState {
    Model(libviewer::View),
    Markdown(libgui_markdown::Markdown),
    Pages(Box<super::pages::PagesView>),
    None,
}

impl ViewState {
    pub fn new(content: &Content) -> Self {
        match content {
            Content::Model(c) => Self::Model(libviewer::View::new(c)),
            Content::Markdown(md) => Self::Markdown(libgui_markdown::Markdown::parse(md)),
            Content::Pages(_) => Self::Pages(Box::default()),
            Content::Text(_) => Self::None,
        }
    }

    /// GPU textures this page owns, to free when it is closed.
    pub fn textures(&self) -> Vec<TextureId> {
        match self {
            Self::Model(v) => v.textures(),
            Self::Pages(st) => st.textures(),
            _ => Vec::new(),
        }
    }
}

/// Viewer's highlighting of a text, for libviewer's text view.
struct Highlighted<'a>(&'a TextDoc);

impl libviewer::Highlights for Highlighted<'_> {
    fn spans(&self, line: usize, dark: bool) -> Vec<(std::ops::Range<usize>, [u8; 3])> {
        let Some(h) = &self.0.highlights else { return Vec::new() };
        h[if dark { 0 } else { 1 }].get(line).map_or_else(Vec::new, |spans| spans.iter().map(|s| (s.range.clone(), s.color)).collect())
    }
}

pub fn show(ui: &mut Ui, page: &mut Page, host: &mut dyn Host, dark: bool) -> Option<Action> {
    // State (scroll offsets, expanded nodes) is keyed by page, so going back restores it.
    ui.with_key(page.id, |ui| match (&page.doc.content, &mut page.view) {
        (Content::Model(c), ViewState::Model(view)) => libviewer::show(ui, c, view, host, None, dark),
        (Content::Text(t), _) => {
            libviewer::text(ui, t, Some(&Highlighted(t)), dark);
            None
        }
        (Content::Markdown(_), ViewState::Markdown(md)) => {
            ui.scroll_area("markdown", |ui| {
                ui.container(Layout::column().height(Size::Fit).padding(Insets::xy(24.0, 16.0)), Frame::none(), |ui| {
                    md.show(ui, "md");
                });
            });
            None
        }
        (Content::Pages(src), ViewState::Pages(st)) => {
            super::pages::show(ui, src, st, host);
            None
        }
        _ => unreachable!("every page gets the view state its content needs"),
    })
}
