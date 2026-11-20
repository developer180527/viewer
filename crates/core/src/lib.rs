//! Viewer without its UI: what's open, what can be done with it, and what each part of
//! the screen shows, as data (docs/design/apple-ui.md §2). **A shell presents; the
//! session decides**, so the libgui shell and the SwiftUI shell can only look
//! different, not behave differently.
//!
//! A [`Session`] holds the open pages, newest last: a file, an entry opened from a
//! container, the same file viewed another way. Opening runs on a loader thread
//! ([`Session::poll`] takes in what finished); the [`Hooks`] say how to wake the shell
//! when it does. What a shell draws comes back as values: the [`Session::take_title`], the
//! [`Session::view_choices`], a [`Notice`] over the content, the [`Session::info`]
//! panel's items, the [`Session::status`] line.
//!
//! What stays in a shell: layout, menus and toolbars, keys and gestures (mapped onto
//! the session's commands), the platform's file picker (whose answer comes in as a
//! [`Session::open_granted`]), and each page's view state, which a shell keys by
//! [`Page::id`] and frees for the ids [`Session::take_closed`] returns.

mod loading;
pub mod recents;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libfileparser_core::{Container, Role};
use viewer_formats::registry;
use viewer_model::{Document, FileSource, Unreadable};

use loading::{Job, Loaded, Target};
pub use recents::{Place, Recent, Recents};

/// Wakes the shell from a loader thread: it should call [`Session::poll`].
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Startup tracing: told "file mapped", "document decoded by …".
pub type Trace = Arc<dyn Fn(&str) + Send + Sync>;

/// How the session reaches its shell.
#[derive(Clone)]
pub struct Hooks {
    pub wake: Waker,
    /// Startup tracing: "file mapped", "document decoded by …".
    pub trace: Option<Trace>,
}

impl Hooks {
    pub fn new(wake: Waker) -> Self {
        Self { wake, trace: None }
    }

    pub fn with_trace(mut self, trace: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.trace = Some(Arc::new(trace));
        self
    }
}

/// A file or folder the user picked, by the number [`Session::open_granted`] gave it.
/// The platform's token for it (a security-scoped bookmark, which Apple platforms
/// release on the thread that took it) stays with the shell, which drops it when
/// [`Session::take_released`] returns its number: when no page reads through it.
pub type GrantId = u64;

/// A grant while pages read through it: the last clone dropped releases it.
#[derive(Clone)]
struct Grant {
    _token: Arc<GrantToken>,
}

struct GrantToken {
    id: GrantId,
    released: Arc<Mutex<Vec<GrantId>>>,
}

impl Drop for GrantToken {
    fn drop(&mut self) {
        self.released.lock().unwrap_or_else(|e| e.into_inner()).push(self.id);
    }
}

/// One way to show the current page: a facet of its document, or the file read by a
/// loader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewChoice {
    Facet(usize),
    Loader(&'static str),
}

/// One level of navigation: a file, or an entry opened from a container.
pub struct Page {
    /// Stable for the page's life: a shell's key for its view state.
    pub id: u64,
    pub src: FileSource,
    pub doc: Document,
    /// The loader that read it.
    pub loader: &'static str,
    /// Loaders that failed before it, and why.
    pub attempts: Vec<(&'static str, String)>,
    pub load_time: Duration,
    /// What the page reads through: a root page owns its grant, pages opened from it
    /// share it, and it's dropped with the last of them.
    grant: Option<Grant>,
}

/// One line over the content, when there's something to say about how it's shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// The reader for this kind of file couldn't read it, and a more general one is
    /// showing it instead (a damaged workbook as hex).
    Fallback(String),
    /// The file names files the app isn't allowed to read (on iOS: the user picked the
    /// model alone). `reopen` is the file to open again from inside its folder, which
    /// [`Session::open_folder_for`]'s grant then allows.
    Blocked { message: String, reopen: PathBuf },
}

/// An item of the info panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Info {
    /// A label and its value.
    Row(String, String),
    /// A titled paragraph after the rows.
    Section(String, String),
}

/// The status line.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Loading(String),
    Shown {
        format: String,
        /// `None` for a folder.
        size: Option<u64>,
        /// The content model's name ("Rich text", "3D scene").
        model: String,
        load_time: Duration,
    },
    Empty,
}

/// What's open, and opening.
pub struct Session {
    pages: Vec<Page>,
    job: Option<Job>,
    error: Option<String>,
    next_page_id: u64,
    /// The grant for the root page the current job will open, handed to that page
    /// when it loads. A failed load drops it and leaves the pages as they were.
    pending: Option<Grant>,
    closed: Vec<u64>,
    title: Option<String>,
    hooks: Hooks,
    next_grant: GrantId,
    released: Arc<Mutex<Vec<GrantId>>>,
    recents: Recents,
    /// The shell's token for the root page being opened, for the recent list.
    pending_bookmark: Option<Vec<u8>>,
}

impl Session {
    pub fn new(hooks: Hooks) -> Self {
        Self { pages: Vec::new(), job: None, error: None, next_page_id: 0, pending: None, closed: Vec::new(), title: None, hooks, next_grant: 0, released: Arc::default(), recents: Recents::in_memory(), pending_bookmark: None }
    }

    // ---------------------------------------------------------------- commands

    /// Opens `path` (a file or a folder) as the new root, replacing the pages once it
    /// has loaded.
    pub fn open_path(&mut self, path: PathBuf) {
        self.open_path_as(path, None);
    }

    /// As [`Self::open_path`], read by the loader with id `view_as`, or shown as the
    /// document's facet of that name ("Stage", "Raw").
    pub fn open_path_as(&mut self, path: PathBuf, view_as: Option<String>) {
        self.job = Some(Job::open_path(path, view_as, &self.hooks));
        self.pending = None;
        self.pending_bookmark = None;
    }

    /// Opens what the user picked, `picked`. With `reopen` (the file
    /// [`Notice::Blocked`] named) inside it, that file opens again, its neighbours now
    /// readable; otherwise what was picked opens. Returns the grant's number: the shell
    /// keeps the platform's token under it until [`Self::take_released`] gives it back.
    /// `bookmark`, the shell's token for opening `picked` again later, goes with the
    /// file into the recent list.
    pub fn open_granted(&mut self, picked: &Path, reopen: Option<PathBuf>, bookmark: Option<Vec<u8>>) -> GrantId {
        let path = match reopen {
            Some(model) if inside(&model, picked) => model,
            _ => picked.to_path_buf(),
        };
        self.open_path(path);
        self.pending_bookmark = bookmark;
        let id = self.next_grant;
        self.next_grant += 1;
        self.pending = Some(Grant { _token: Arc::new(GrantToken { id, released: self.released.clone() }) });
        id
    }

    /// Grants no page reads through any more (closed, replaced, or a load that failed):
    /// the shell drops their tokens.
    pub fn take_released(&mut self) -> Vec<GrantId> {
        std::mem::take(&mut *self.released.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// The folder to ask the user for, for [`Notice::Blocked`]'s file: its own.
    pub fn open_folder_for(model: &Path) -> Option<PathBuf> {
        model.parent().map(Path::to_path_buf)
    }

    /// Opens entry `index` of `container` on top of the current page.
    pub fn open_entry(&mut self, container: Arc<dyn Container>, index: u64) {
        let name = container.entries(index..index + 1).ok().and_then(|mut e| e.pop()).map_or_else(|| format!("entry {index}"), |e| e.path);
        self.job = Some(Job::spawn(name, Target::Push, None, &self.hooks, move || container.open(index).map_err(|e| anyhow::anyhow!("{e}"))));
        self.pending = None;
    }

    /// Shows the current page as `choice`: another of its document's facets, or the
    /// file read by another loader. It replaces the page once loaded.
    pub fn view(&mut self, choice: ViewChoice) {
        let Some(page) = self.pages.last() else { return };
        match choice {
            ViewChoice::Loader(id) => {
                let src = page.src.clone();
                self.job = Some(Job::spawn(src.name().into(), Target::ReplaceTop, Some(id.into()), &self.hooks, move || Ok(src)));
            }
            ViewChoice::Facet(index) => {
                let Some(facets) = page.doc.facets.clone() else { return };
                self.job = Some(Job::facet(page.src.clone(), facets, index, page.loader, &self.hooks));
            }
        }
        self.pending = None;
    }

    /// Back to the page below; `false` at the root.
    pub fn back(&mut self) -> bool {
        if self.pages.len() < 2 {
            return false;
        }
        if let Some(page) = self.pages.pop() {
            self.closed.push(page.id);
        }
        self.error = None;
        true
    }

    /// Takes in a finished load. `true` when one finished (opened or failed), so the
    /// shell redraws.
    pub fn poll(&mut self) -> bool {
        let Some(job) = &self.job else { return false };
        let Some(result) = job.poll() else { return false };
        let target = job.target;
        self.job = None;
        // A root page owns what was picked for it; others read through the page they
        // were opened from (or replace).
        let bookmark = self.pending_bookmark.take();
        let grant = match target {
            Target::Root => self.pending.take(),
            Target::Push | Target::ReplaceTop => self.pages.last().and_then(|p| p.grant.clone()),
        };
        self.pending = None;
        match result {
            Ok(Loaded { src, doc, loader, attempts, load_time }) => {
                self.error = None;
                let page = Page { id: self.next_page_id, src, doc, loader, attempts, load_time, grant };
                self.next_page_id += 1;
                match target {
                    Target::Root => {
                        if let Some(path) = page.src.path() {
                            self.recents.note(path, bookmark);
                        }
                        let old = std::mem::take(&mut self.pages);
                        self.closed.extend(old.iter().map(|p| p.id));
                    }
                    Target::Push => {}
                    Target::ReplaceTop => {
                        if let Some(old) = self.pages.pop() {
                            self.closed.push(old.id);
                        }
                    }
                }
                self.pages.push(page);
                self.title = Some(self.pages.iter().map(|p| p.src.name()).collect::<Vec<_>>().join(" › "));
            }
            Err(e) => self.error = Some(e),
        }
        true
    }

    // ---------------------------------------------------------------- recent files

    /// The recent list files are noted in as they open: kept in memory until a shell
    /// sets one loaded from its file.
    pub fn set_recents(&mut self, recents: Recents) {
        self.recents = recents;
    }

    pub fn recents(&self) -> &Recents {
        &self.recents
    }

    pub fn recents_mut(&mut self) -> &mut Recents {
        &mut self.recents
    }

    /// The file a page's place is remembered under: a root page's own file. Entries
    /// inside containers and other views aren't remembered.
    fn place_key(&self, page_id: u64) -> Option<&Path> {
        let root = self.pages.first().filter(|p| p.id == page_id && self.pages.len() == 1)?;
        root.src.path()
    }

    /// Where the reader was in this page's file when it was last open.
    pub fn place(&self, page_id: u64) -> Option<Place> {
        self.recents.get(self.place_key(page_id)?)?.place
    }

    /// Where the reader is in this page now.
    pub fn set_place(&mut self, page_id: u64, place: Place) {
        if let Some(path) = self.place_key(page_id).map(Path::to_path_buf) {
            self.recents.set_place(&path, place);
        }
    }

    // ---------------------------------------------------------------- what's shown

    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    pub fn current(&self) -> Option<&Page> {
        self.pages.last()
    }

    pub fn can_go_back(&self) -> bool {
        self.pages.len() > 1
    }

    /// What's being opened, by name.
    pub fn loading(&self) -> Option<&str> {
        self.job.as_ref().map(|j| j.name.as_str())
    }

    /// Why the last open failed. The pages stay as they were.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The file (or the error opening it) is what's on screen, not a spinner.
    pub fn shown(&self) -> bool {
        (!self.pages.is_empty() || self.error.is_some()) && self.job.is_none()
    }

    /// The window's new title ("bundle.zip › data.json"), once, when it changed.
    pub fn take_title(&mut self) -> Option<String> {
        self.title.take()
    }

    /// Ids of pages closed since the last call: their view state can go.
    pub fn take_closed(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.closed)
    }

    /// What the current page can be shown as, with names, and which it is shown as:
    /// first its document's facets when libfileparser read it (Scene, Stage, Layers…,
    /// Source, Raw), then the other loaders that claim the file.
    pub fn view_choices(&self) -> (Vec<(ViewChoice, String)>, usize) {
        let Some(page) = self.pages.last() else { return (Vec::new(), 0) };
        // A folder is a list of files, not bytes: there is nothing else to view it as.
        if page.src.is_folder() {
            return (Vec::new(), 0);
        }
        let mut choices: Vec<(ViewChoice, String)> = Vec::new();
        let mut current = None;
        if let Some(facets) = &page.doc.facets {
            for (i, f) in facets.doc.facets().enumerate() {
                choices.push((ViewChoice::Facet(i), f.name.clone()));
            }
            current = Some(facets.shown);
        }
        let has = |role: Role| page.doc.facets.as_ref().is_some_and(|f| f.doc.facets().any(|x| x.role == role));
        let mut ids: Vec<&'static str> = registry().candidates(&page.src).iter().map(|(_, l)| l.id()).collect();
        // Text is always worth offering, even for files that don't sniff as text.
        if !ids.contains(&"text") {
            ids.push("text");
        }
        // A loader forced with --view-as may not claim the file; list it anyway, or the
        // choice would fall back to entry 0 and re-open the file every frame.
        if !ids.contains(&page.loader) {
            ids.push(page.loader);
        }
        for id in ids {
            // The document's own facets stand for the loader that read it, its Source
            // for plain text and its Raw for hex.
            let shown_by_facets = page.doc.facets.is_some() && (id == page.loader || (id == "text" && has(Role::Source)) || (id == "hex" && has(Role::Raw)));
            if shown_by_facets {
                continue;
            }
            if current.is_none() && id == page.loader {
                current = Some(choices.len());
            }
            choices.push((ViewChoice::Loader(id), registry().get(id).map_or(id, |l| l.name()).to_string()));
        }
        (choices, current.unwrap_or(0))
    }

    /// What to say over the current page's content, if anything: files it can't
    /// reach (with the fix), or that it's shown by a fallback reader.
    pub fn notices(&self) -> Vec<Notice> {
        let Some(page) = self.pages.last() else { return Vec::new() };
        let mut out = Vec::new();
        if let Some((id, why)) = page.attempts.first() {
            let name = |id: &str| registry().get(id).map_or_else(|| id.to_string(), |l| l.name().to_string());
            let why = why.trim_start_matches("damaged or not understood: ").trim_end_matches('.');
            out.push(Notice::Fallback(format!("Couldn't read this as {}: {why}. Showing it as {} instead.", name(id), name(page.loader))));
        }
        let blocked = page.doc.unreadable.count(Unreadable::Blocked);
        if let Some(model) = page.src.path().filter(|_| blocked > 0 && !page.src.is_folder()) {
            let files = if blocked == 1 { "1 file isn't shown.".to_string() } else { format!("{blocked} files aren't shown.") };
            let why = if cfg!(target_os = "ios") {
                "iOS only lets Viewer open the file you picked, not the files next to it. Open its folder to show them."
            } else {
                "Viewer isn't allowed to read the folder this file is in. Open the folder to grant it."
            };
            out.push(Notice::Blocked { message: format!("{files} {why}"), reopen: model.to_path_buf() });
        }
        out
    }

    /// The info panel: the current page's name, path, size, format, loader and
    /// metadata, then the files it couldn't read and the loaders that failed first.
    pub fn info(&self) -> Vec<Info> {
        let Some(page) = self.pages.last() else { return Vec::new() };
        let row = |k: &str, v: String| Info::Row(k.to_string(), v);
        let mut out = vec![row("Name", page.src.name().to_string())];
        if let Some(p) = page.src.path() {
            out.push(row("Path", p.display().to_string()));
        }
        if !page.src.is_folder() {
            out.push(row("Size", format!("{} ({} bytes)", human_bytes(page.src.len()), page.src.len())));
        }
        out.push(row("Format", page.doc.format.clone()));
        out.push(row("Loader", page.loader.to_string()));
        out.extend(page.doc.metadata.iter().map(|(k, v)| row(k, v.clone())));
        // Files the document refers to but couldn't read. Blocked ones also get a
        // notice over the content; missing ones only this, since no permission brings
        // them back.
        for (reason, title) in [
            (Unreadable::Missing, "Not found"),
            (Unreadable::Blocked, "Not allowed to read"),
            // Not looked for: the model points outside its folder, which Viewer never reads.
            (Unreadable::Outside, "Outside the model's folder"),
        ] {
            let names = page.doc.unreadable.names(reason);
            if !names.is_empty() {
                out.push(Info::Section(format!("{title} ({})", names.len()), name_list(&names, 12)));
            }
        }
        if !page.attempts.is_empty() {
            let failed = page.attempts.iter().map(|(id, err)| format!("{id}: {err}")).collect::<Vec<_>>().join("\n");
            out.push(Info::Section("Loaders that failed first".into(), failed));
        }
        out
    }

    pub fn status(&self) -> Status {
        if let Some(name) = self.loading() {
            return Status::Loading(name.to_string());
        }
        match self.pages.last() {
            Some(page) => Status::Shown {
                format: page.doc.format.clone(),
                size: (!page.src.is_folder()).then(|| page.src.len()),
                model: page.doc.content.model_name().to_string(),
                load_time: page.load_time,
            },
            None => Status::Empty,
        }
    }
}

/// `bytes` for people: "1.5 KiB".
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[unit]) }
}

/// `path` is `folder` or somewhere under it. Compared as real paths: iOS mixes `/var/…`
/// and `/private/var/…` for the same place, and a picked folder may be reached through
/// a link.
fn inside(path: &Path, folder: &Path) -> bool {
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    real(path).starts_with(real(folder))
}

/// Up to `max` names, then how many more.
fn name_list(names: &[String], max: usize) -> String {
    let shown = names.iter().take(max).map(String::as_str).collect::<Vec<_>>().join(", ");
    match names.len().saturating_sub(max) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}
