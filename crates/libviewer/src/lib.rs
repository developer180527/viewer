//! Views for libfileparser's content models, drawn with libgui (docs/design/libviewer.md):
//! any app shows any facet of any file with them, never knowing its format.
//!
//! | Content | View |
//! |---|---|
//! | `Text` | [`text`]: numbered lines, read as they scroll into view, coloured by an app's [`Highlights`] |
//! | `Hex` | [`hex_file`]: offset, bytes, printable text |
//! | `Tree` | [`tree`]: a node's children read when it is first expanded |
//! | `Table` | [`table`]: the visible rows, read every frame (a live table shows what it holds now) |
//! | `Container` | [`container`]: entries, the clicked one to open as a file |
//! | `Raster` | [`raster`]: tiles on the GPU; an image in bands decoded on a thread, two bands ahead |
//! | `Scene` | [`scene`]: RendererX, orbit, pan, zoom |
//! | `Pages` | [`pages`]: every page in a vertical scroll, rendered in tiles on a thread, only where the window looks |
//! | `Markdown` | [`markdown`]: formatted, from its source lines |
//!
//! [`View`] keeps one content's view state between frames (scroll, what's expanded,
//! the GPU tiles) and [`show`] draws any content with the right view. The app is the
//! [`Host`]: where images go, how 3D is drawn, how the window is woken.
//!
//! ```ignore
//! let content = doc.facet(i, &cx)?;          // libfileparser
//! let mut view = View::new(&content);         // once per page
//! if let Some(Action::OpenEntry(c, i)) = libviewer::show(ui, &content, &mut view, host, None, dark) {
//!     let file = c.open(i)?;                  // a file inside: open it again
//! }
//! ```

mod host;
mod markdown;
mod pages;
mod raster;
mod scene;
mod table;
mod text;
mod tree;

use std::sync::Arc;

use libfileparser_core::{Container, Content};
use libgui::{TextureId, Ui};

pub use host::{Frame3d, Host, SceneGpu, Waker, draw_scene};
pub use markdown::{MarkdownView, markdown};
pub use pages::{PagesView, pages};
pub use raster::{RasterView, raster};
pub use scene::{SceneView, scene};
pub use table::{TableView, container, human_bytes, table};
#[allow(deprecated)]
pub use text::hex;
pub use text::{Highlights, hex_file, text};
pub use tree::{TreeView, tree};

/// Something a view asks the app to do.
#[non_exhaustive]
pub enum Action {
    /// Open entry `index` of a container as a file of its own.
    OpenEntry(Arc<dyn Container>, u64),
}

/// One content's view state, kept between frames.
#[non_exhaustive]
pub enum View {
    None,
    Raster(RasterView),
    Tree(TreeView),
    Table(TableView),
    Scene(Box<SceneView>),
    Pages(Box<PagesView>),
    Markdown(Box<MarkdownView>),
}

impl View {
    pub fn new(content: &Content) -> Self {
        match content {
            Content::Raster(_) => Self::Raster(RasterView::default()),
            Content::Tree(_) => Self::Tree(TreeView::default()),
            Content::Table(t) => Self::Table(TableView::for_table(t.as_ref())),
            Content::Container(c) => Self::Table(TableView::for_container(c.as_ref())),
            Content::Scene(_) => Self::Scene(Box::default()),
            Content::Pages(_) => Self::Pages(Box::default()),
            Content::Markdown(t) => Self::Markdown(Box::new(MarkdownView::new(t.as_ref()))),
            _ => Self::None,
        }
    }

    /// GPU textures this view owns, to free with [`Host::free`] when its page closes.
    pub fn textures(&self) -> Vec<TextureId> {
        match self {
            Self::Raster(st) => st.textures(),
            Self::Scene(st) => st.textures(),
            Self::Pages(st) => st.textures(),
            _ => Vec::new(),
        }
    }
}

/// Draws `content` with its view. `view` must be [`View::new`] of the same content.
/// `highlights` colours text.
pub fn show(ui: &mut Ui, content: &Content, view: &mut View, host: &mut dyn Host, highlights: Option<&dyn Highlights>, dark: bool) -> Option<Action> {
    match (content, view) {
        (Content::Text(t), _) => text(ui, t.as_ref(), highlights, dark),
        (Content::Hex(src), _) => hex_file(ui, src),
        (Content::Tree(t), View::Tree(v)) => tree(ui, t.as_ref(), v),
        (Content::Table(t), View::Table(v)) => table(ui, t.as_ref(), v),
        (Content::Container(c), View::Table(v)) => return container(ui, c, v).map(|i| Action::OpenEntry(c.clone(), i)),
        (Content::Raster(r), View::Raster(v)) => raster(ui, r, v, host),
        (Content::Scene(s), View::Scene(v)) => scene(ui, s, v, host),
        (Content::Pages(p), View::Pages(v)) => pages(ui, p, v, host),
        (Content::Markdown(_), View::Markdown(v)) => markdown(ui, v),
        (Content::Scene(_) | Content::Raster(_) | Content::Tree(_) | Content::Table(_) | Content::Container(_) | Content::Pages(_) | Content::Markdown(_), _) => {
            ui.label_muted("This view was made for other content.");
        }
        _ => {
            ui.label_muted("No view for this content yet.");
        }
    }
    None
}

/// What a content model is called, for an app's status line ("Tree view").
pub fn model_name(content: &Content) -> &'static str {
    match content {
        Content::Raster(_) => "Raster",
        Content::Text(_) => "Text",
        Content::Tree(_) => "Tree",
        Content::Table(_) => "Table",
        Content::Container(_) => "Container",
        Content::Hex(_) => "Hex",
        Content::Scene(_) => "3D scene",
        Content::Pages(_) => "Pages",
        Content::Markdown(_) => "Rich text",
        _ => "Other",
    }
}
