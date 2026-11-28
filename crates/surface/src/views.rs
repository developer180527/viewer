//! A page's content on screen: libviewer's views for libfileparser's models, and
//! Viewer's highlighted text.

use libgui::*;
use viewer_core::Page;
use viewer_model::{Content, TextDoc};

pub use libviewer::Action;
use libviewer::{Host, Search, Waker};

/// Per-page view state that libgui leaves to us.
pub enum ViewState {
    Model(Box<libviewer::View>),
    None,
}

impl ViewState {
    pub fn new(content: &Content) -> Self {
        match content {
            Content::Model(c) => Self::Model(Box::new(libviewer::View::new(c))),
            Content::Text(_) => Self::None,
        }
    }

    /// Where the reader is, for views that can come back to it.
    pub fn place(&self) -> Option<libviewer::PagesPlace> {
        match self {
            Self::Model(v) => v.place(),
            Self::None => None,
        }
    }

    pub fn restore(&mut self, place: libviewer::PagesPlace) {
        if let Self::Model(v) = self {
            v.restore(place);
        }
    }

    /// Frees what the view can make again (a paged document's tiles).
    pub fn trim(&mut self, host: &mut dyn Host) {
        if let Self::Model(v) = self {
            v.trim(host);
        }
    }

    /// GPU textures this page owns, to free when it is closed.
    pub fn textures(&self) -> Vec<TextureId> {
        match self {
            Self::Model(v) => v.textures(),
            Self::None => Vec::new(),
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

/// A search of what `page` shows, for `text`; `None` when the text is empty or the
/// page has nothing to search (an image, a 3D scene).
pub fn search(page: &Page, text: &str, wake: Waker) -> Option<Search> {
    match &page.doc.content {
        Content::Model(c) => Search::content(c, text, wake),
        Content::Text(_) => {
            let doc = page.doc.clone();
            Search::run(text, wake, move |q, cancel, found| match &doc.content {
                Content::Text(t) => libfileparser_core::find::text(t, q, cancel, found),
                Content::Model(_) => Ok(()),
            })
        }
    }
}

pub fn show(ui: &mut Ui, page: &Page, view: &mut ViewState, host: &mut dyn Host, search: Option<&Search>, dark: bool) -> Option<Action> {
    // State (scroll offsets, expanded nodes) is keyed by page, so going back restores it.
    ui.with_key(page.id, |ui| match (&page.doc.content, view) {
        (Content::Model(c), ViewState::Model(view)) => {
            if matches!(c, libfileparser_core::Content::Flow(_)) {
                crate::fonts::ensure_rich_faces(ui);
            }
            libviewer::show_found(ui, c, view, host, None, search, dark)
        }
        (Content::Text(t), _) => {
            libviewer::text_found(ui, t, Some(&Highlighted(t)), search, dark);
            None
        }
        _ => unreachable!("every page gets the view state its content needs"),
    })
}
