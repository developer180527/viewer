//! `viewer-core` for Swift: the session, its commands, and what's shown, through
//! UniFFI (docs/design/apple-ui.md §3). A shell drives a [`ViewerSession`] as the libgui
//! app drives `viewer_core::Session`: commands in, values out, and a [`Waker`] it
//! implements, called from a loader thread when [`ViewerSession::poll`] has something.
//!
//! **Content crosses by range**, as in Rust: a text's line count, then the lines asked
//! for; a table's columns and row count, then rows; a container's entries. SwiftUI's
//! lazy lists ask for what's on screen, so a million-row table costs the visible rows.
//! Pages, images, 3D and formatted text are drawn by libviewer's renderers in a Metal
//! view: a [`ViewerCanvas`] (`canvas.rs`) on the shell's `CAMetalLayer`, for content
//! of [`ContentKind::Drawn`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use libfileparser_core::{Content as Model, EntryKind};
use viewer_core::{Hooks, Info, Notice, Session, Status};
use viewer_model::Content;

mod canvas;
pub use canvas::{AppRequest, CanvasError, CursorShape, Frame, MouseButton, TouchPhase, ViewerCanvas};

uniffi::setup_scaffolding!();

/// Implemented by the shell: wake the UI, which then calls [`ViewerSession::poll`].
/// Called from any thread.
#[uniffi::export(with_foreign)]
pub trait Waker: Send + Sync {
    fn wake(&self);
}

/// A status line.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum StatusLine {
    Loading { name: String },
    Shown { format: String, size: Option<u64>, model: String, load_ms: f64 },
    Empty,
}

/// An item of the info panel.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum InfoItem {
    Row { label: String, value: String },
    Section { title: String, text: String },
}

/// One line over the content.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum NoticeItem {
    Fallback { message: String },
    /// `folder` is where the picker should open; picking it (or one above) and
    /// passing `reopen` to [`ViewerSession::open_granted`] fixes it.
    Blocked { message: String, reopen: String, folder: Option<String> },
}

/// The current page's view-as choices, and which is shown.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ViewChoices {
    pub names: Vec<String>,
    pub current: u32,
}

/// What kind of content the current page has: which native view shows it.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentKind {
    /// Lines, by range: [`ViewerSession::text_lines`].
    Text,
    /// Columns and rows, by range: [`ViewerSession::table_rows`].
    Table,
    /// Entries, by range, each openable: [`ViewerSession::entries`].
    Container,
    /// Drawn by libviewer's renderers (pages, images, 3D, formatted text, trees, hex):
    /// the Metal view, a [`ViewerCanvas`].
    Drawn,
    /// Nothing open.
    None,
}

/// An entry of a container (a zip's file, a folder's child).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct EntryItem {
    pub path: String,
    pub size: u64,
    pub folder: bool,
}

/// A recently opened file, newest first.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct RecentItem {
    pub path: String,
    pub name: String,
    /// Seconds since 1970.
    pub opened: u64,
    /// The token given when it was opened (a security-scoped bookmark); empty if none.
    pub bookmark: Vec<u8>,
}

/// Light or dark.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeChoice {
    /// As the system is.
    System,
    Light,
    Dark,
}

/// Which interface the Apple apps draw.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterfaceChoice {
    /// SwiftUI's.
    Native,
    /// libgui's, the libgui app's interface in the same window.
    Libgui,
}

/// The user's settings, the same in every shell (viewer-core's).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct AppSettings {
    pub theme: ThemeChoice,
    /// At launch with no file named, open the last one again.
    pub reopen_last: bool,
    pub interface: InterfaceChoice,
}

/// The app's session: what's open, and what can be done with it. Thread-safe; its
/// calls are quick (opening runs on loader threads).
#[derive(uniffi::Object)]
pub struct ViewerSession {
    inner: Mutex<Session>,
    /// The shell's, for the canvas too: decoded textures and compiled shaders wake it.
    waker: Arc<dyn Waker>,
}

impl ViewerSession {
    fn session(&self) -> MutexGuard<'_, Session> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current page's content, borrowed under the lock, for the range calls.
    fn with_content<R>(&self, f: impl FnOnce(Option<&Content>) -> R) -> R {
        let s = self.session();
        f(s.current().map(|p| &p.doc.content))
    }
}

#[uniffi::export]
impl ViewerSession {
    #[uniffi::constructor]
    pub fn new(waker: Arc<dyn Waker>) -> Arc<Self> {
        let w = waker.clone();
        let hooks = Hooks::new(Arc::new(move || w.wake()));
        Arc::new(Self { inner: Mutex::new(Session::new(hooks)), waker })
    }

    // ---------------------------------------------------------------- commands

    /// Opens a file or folder as the new root.
    pub fn open_path(&self, path: String) {
        self.session().open_path(PathBuf::from(path));
    }

    /// Opens what the user picked; with `reopen` inside it, that file instead. Returns
    /// the grant's number: keep the platform's access (a security-scoped URL) until
    /// [`Self::take_released`] returns it. `bookmark`, the platform's token for opening
    /// `picked` again, is kept with it in the recent list.
    pub fn open_granted(&self, picked: String, reopen: Option<String>, bookmark: Option<Vec<u8>>) -> u64 {
        self.session().open_granted(&PathBuf::from(picked), reopen.map(PathBuf::from), bookmark)
    }

    // ---------------------------------------------------------------- recent files

    /// Keeps the recent list and the settings in `dir` (the app's Application Support
    /// folder), loading them now.
    pub fn use_data_dir(&self, dir: String) {
        self.session().use_data_dir(&PathBuf::from(dir));
    }

    pub fn settings(&self) -> AppSettings {
        let s = self.session().settings().clone();
        let theme = match s.theme {
            viewer_core::Theme::System => ThemeChoice::System,
            viewer_core::Theme::Light => ThemeChoice::Light,
            viewer_core::Theme::Dark => ThemeChoice::Dark,
        };
        let interface = match s.interface {
            viewer_core::Interface::Native => InterfaceChoice::Native,
            viewer_core::Interface::Libgui => InterfaceChoice::Libgui,
        };
        AppSettings { theme, reopen_last: s.reopen_last, interface }
    }

    /// Changes the settings (saved by [`Self::save`]).
    pub fn set_settings(&self, settings: AppSettings) {
        let theme = match settings.theme {
            ThemeChoice::System => viewer_core::Theme::System,
            ThemeChoice::Light => viewer_core::Theme::Light,
            ThemeChoice::Dark => viewer_core::Theme::Dark,
        };
        let interface = match settings.interface {
            InterfaceChoice::Native => viewer_core::Interface::Native,
            InterfaceChoice::Libgui => viewer_core::Interface::Libgui,
        };
        self.session().set_settings(viewer_core::Settings { theme, reopen_last: settings.reopen_last, interface });
    }

    pub fn recents(&self) -> Vec<RecentItem> {
        self.session()
            .recents()
            .items()
            .iter()
            .map(|r| RecentItem { path: r.path.display().to_string(), name: r.name(), opened: r.opened, bookmark: r.bookmark.clone() })
            .collect()
    }

    pub fn forget_recent(&self, path: String) {
        self.session().recents_mut().remove(&PathBuf::from(path));
    }

    pub fn clear_recents(&self) {
        self.session().recents_mut().clear();
    }

    /// Writes what changed: the recent list (and the places in it) and the settings.
    /// Returns why not.
    pub fn save(&self) -> Option<String> {
        self.session().save().err().map(|e| e.to_string())
    }

    /// Grants no page reads through any more: stop accessing them.
    pub fn take_released(&self) -> Vec<u64> {
        self.session().take_released()
    }

    /// Opens entry `index` of the current page's container on top of it.
    pub fn open_entry(&self, index: u64) {
        let container = self.with_content(|c| match c {
            Some(Content::Model(Model::Container(c))) => Some(c.clone()),
            _ => None,
        });
        if let Some(c) = container {
            self.session().open_entry(c, index);
        }
    }

    /// Shows the current page as choice `index` of [`Self::view_choices`].
    pub fn choose_view(&self, index: u32) {
        let mut s = self.session();
        let (choices, _) = s.view_choices();
        if let Some((choice, _)) = choices.get(index as usize) {
            s.view(*choice);
        }
    }

    pub fn back(&self) -> bool {
        self.session().back()
    }

    /// Takes in a finished open; `true` when one finished, so the UI updates.
    pub fn poll(&self) -> bool {
        self.session().poll()
    }

    // ---------------------------------------------------------------- what's shown

    /// The pages' names, root first: a breadcrumb, a window title.
    pub fn breadcrumbs(&self) -> Vec<String> {
        self.session().pages().iter().map(|p| p.src.name().to_string()).collect()
    }

    /// The current page's id, stable for its life: a key for its view's state.
    pub fn page_id(&self) -> Option<u64> {
        self.session().current().map(|p| p.id)
    }

    pub fn can_go_back(&self) -> bool {
        self.session().can_go_back()
    }

    pub fn error(&self) -> Option<String> {
        self.session().error().map(str::to_string)
    }

    pub fn view_choices(&self) -> ViewChoices {
        let (choices, current) = self.session().view_choices();
        ViewChoices { names: choices.into_iter().map(|(_, n)| n).collect(), current: current as u32 }
    }

    pub fn notices(&self) -> Vec<NoticeItem> {
        self.session()
            .notices()
            .into_iter()
            .map(|n| match n {
                Notice::Fallback(message) => NoticeItem::Fallback { message },
                Notice::Blocked { message, reopen } => {
                    let folder = Session::open_folder_for(&reopen).map(|f| f.display().to_string());
                    NoticeItem::Blocked { message, reopen: reopen.display().to_string(), folder }
                }
            })
            .collect()
    }

    pub fn info(&self) -> Vec<InfoItem> {
        self.session()
            .info()
            .into_iter()
            .map(|i| match i {
                Info::Row(label, value) => InfoItem::Row { label, value },
                Info::Section(title, text) => InfoItem::Section { title, text },
            })
            .collect()
    }

    pub fn status(&self) -> StatusLine {
        match self.session().status() {
            Status::Loading(name) => StatusLine::Loading { name },
            Status::Shown { format, size, model, load_time } => StatusLine::Shown { format, size, model, load_ms: load_time.as_secs_f64() * 1000.0 },
            Status::Empty => StatusLine::Empty,
        }
    }

    // ---------------------------------------------------------------- content by range

    pub fn content_kind(&self) -> ContentKind {
        self.with_content(|c| match c {
            None => ContentKind::None,
            Some(Content::Text(_) | Content::Model(Model::Text(_))) => ContentKind::Text,
            Some(Content::Model(Model::Table(_))) => ContentKind::Table,
            Some(Content::Model(Model::Container(_))) => ContentKind::Container,
            Some(_) => ContentKind::Drawn,
        })
    }

    /// Lines in the current text (a lower bound while a big file is still counted).
    pub fn text_line_count(&self) -> u64 {
        self.with_content(|c| match c {
            Some(Content::Text(t)) => t.line_count() as u64,
            Some(Content::Model(Model::Text(t))) => t.info().lines.known(),
            _ => 0,
        })
    }

    /// Lines `start..start + count` of the current text; fewer past the end.
    pub fn text_lines(&self, start: u64, count: u64) -> Vec<String> {
        self.with_content(|c| match c {
            Some(Content::Text(t)) => {
                let end = (start + count).min(t.line_count() as u64);
                (start..end).map(|i| String::from_utf8_lossy(t.line(i as usize)).into_owned()).collect()
            }
            Some(Content::Model(Model::Text(t))) => t.lines(start..start + count).map(|ls| ls.iter().map(|l| String::from_utf8_lossy(l).trim_end_matches(['\n', '\r']).to_string()).collect()).unwrap_or_default(),
            _ => Vec::new(),
        })
    }

    pub fn table_columns(&self) -> Vec<String> {
        self.with_content(|c| match c {
            Some(Content::Model(Model::Table(t))) => t.info().columns.iter().map(|c| c.name.clone()).collect(),
            _ => Vec::new(),
        })
    }

    pub fn table_row_count(&self) -> u64 {
        self.with_content(|c| match c {
            Some(Content::Model(Model::Table(t))) => t.info().rows.known(),
            _ => 0,
        })
    }

    /// Rows `start..start + count`, each cell as text; fewer past the end.
    pub fn table_rows(&self, start: u64, count: u64) -> Vec<Vec<String>> {
        self.with_content(|c| match c {
            Some(Content::Model(Model::Table(t))) => t.rows(start..start + count).map(|rows| rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect()).unwrap_or_default(),
            _ => Vec::new(),
        })
    }

    pub fn entry_count(&self) -> u64 {
        self.with_content(|c| match c {
            Some(Content::Model(Model::Container(c))) => c.entry_count().known(),
            _ => 0,
        })
    }

    /// Entries `start..start + count` of the current container; fewer past the end.
    pub fn entries(&self, start: u64, count: u64) -> Vec<EntryItem> {
        self.with_content(|c| match c {
            Some(Content::Model(Model::Container(c))) => {
                c.entries(start..start + count).map(|es| es.into_iter().map(|e| EntryItem { path: e.path, size: e.size, folder: e.kind == EntryKind::Folder }).collect()).unwrap_or_default()
            }
            _ => Vec::new(),
        })
    }
}

/// The session's helpers, for Swift: "1.5 KiB".
#[uniffi::export]
pub fn human_bytes(bytes: u64) -> String {
    viewer_core::human_bytes(bytes)
}
