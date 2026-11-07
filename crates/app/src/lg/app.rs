//! The window's contents: toolbar, the current page's view, info panel, status bar.
//! Knows nothing about winit or the GPU; the host in `mod.rs` drives it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use libgui::*;
use libgui_keymap::{Chord, Keymap};
use libfilepicker::{PickOptions, Picked};
use viewer_core::{Document, FileSource, Unreadable};

use libviewer::Host;
use super::views::{self, Action, ViewState};
use crate::formats::registry;
use crate::loading::{Job, Loaded, Target};
use libviewer::human_bytes;

/// One level of navigation: a file, or an entry opened from a container.
/// One way to show a page: a facet of its document, or the file read by a loader.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewChoice {
    Facet(usize),
    Loader(&'static str),
}

pub struct Page {
    pub id: u64,
    pub src: FileSource,
    pub doc: Document,
    pub loader: &'static str,
    pub attempts: Vec<(&'static str, String)>,
    pub load_time: Duration,
    pub view: ViewState,
    /// The picked file or folder this page reads through, kept as long as the page is
    /// open (see [`Picked`]): a root page owns it, pages opened from it share it.
    /// Dropped with the page on Back or when a new file replaces it, never earlier.
    access: Option<Rc<Picked>>,
}

/// An answer from the file picker, and what it was for.
struct Pick {
    picked: Picked,
    /// Open Folder… on a model whose neighbours were blocked: reopen this model
    /// from inside the picked folder, if it is in there.
    reopen: Option<PathBuf>,
}

/// How wide the window is, which decides where the chrome goes
/// (docs/design/responsive-ui.md §1). Width only: a narrow desktop window gets the phone layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WidthClass {
    /// Under 600: a phone upright, a narrow window.
    Compact,
    /// 600–1023: a phone sideways, a tablet upright, half a desktop screen.
    Medium,
    /// 1024 and up.
    Expanded,
}

impl WidthClass {
    const MEDIUM: f32 = 600.0;
    const EXPANDED: f32 = 1024.0;
    /// Switching down waits this far below a threshold, so dragging a window
    /// edge across one doesn't flicker between layouts.
    const HYSTERESIS: f32 = 24.0;

    /// The class for `width`, given the class shown last frame.
    pub fn for_width(width: f32, last: Option<WidthClass>) -> WidthClass {
        let up = |w: f32| {
            if w >= Self::EXPANDED {
                WidthClass::Expanded
            } else if w >= Self::MEDIUM {
                WidthClass::Medium
            } else {
                WidthClass::Compact
            }
        };
        let fresh = up(width);
        match last {
            // Growing switches at the threshold; shrinking only once clear of it.
            Some(last) if fresh < last && up(width + Self::HYSTERESIS) >= last => last,
            _ => fresh,
        }
    }
}

/// What the keyboard can do, bound per platform by [`ViewerApp::keymap`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Open,
    Back,
    ToggleInfo,
}

/// Below this height (a phone sideways) the status bar goes, to leave the content room.
const SHORT: f32 = 480.0;
/// The info panel's width, beside the content or as a drawer over it.
const INFO_WIDTH: f32 = 300.0;

pub struct ViewerApp {
    width_class: Option<WidthClass>,
    stack: Vec<Page>,
    job: Option<Job>,
    error: Option<String>,
    next_page_id: u64,
    pub dark: bool,
    pub show_info: bool,
    /// Kept clear of the notch, status bar and home indicator (zero on desktops).
    pub safe_area: Insets,
    /// Leave out what changes run to run (timings), so screenshots can be compared.
    pub stable: bool,
    /// New window title, for the host to apply.
    pub title: Option<String>,
    /// Textures of closed pages, for the host to free.
    pub retired: Vec<TextureId>,
    /// Where the file picker leaves its answer, which can come after `pick` returns.
    picked: Rc<RefCell<Option<Pick>>>,
    /// Access for the root page the current job will open, handed to that page when
    /// it loads. A failed load drops it and leaves the pages on screen as they were.
    pending_access: Option<Rc<Picked>>,
    keys: Keymap<Command>,
}

impl ViewerApp {
    pub fn new(job: Option<Job>, dark: bool) -> Self {
        Self {
            width_class: None,
            stack: Vec::new(),
            job,
            error: None,
            show_info: false,
            safe_area: Insets::all(0.0),
            stable: false,
            next_page_id: 0,
            dark,
            title: None,
            retired: Vec::new(),
            picked: Rc::default(),
            pending_access: None,
            keys: Self::keymap(),
        }
    }

    fn keymap() -> Keymap<Command> {
        let mut keys = Keymap::for_current_platform();
        keys.bind(Chord::primary(Key::O), Command::Open)
            .bind(Chord::key(Key::ArrowLeft).alt(), Command::Back)
            .bind(Key::Backspace, Command::Back)
            .bind(Key::I, Command::ToggleInfo);
        keys
    }

    /// The file (or the error opening it) is what is on screen, not a spinner.
    pub fn shown(&self) -> bool {
        (!self.stack.is_empty() || self.error.is_some()) && self.job.is_none()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn loading(&self) -> bool {
        self.job.is_some()
    }

    fn open_path(&mut self, path: PathBuf) {
        self.job = Some(Job::open_path(path, None));
        self.pending_access = None;
    }

    fn open_dialog(&mut self) {
        let slot = Rc::clone(&self.picked);
        libfilepicker::pick(move |picked| deliver(&slot, picked, None));
    }

    /// Open Folder…: asks for the folder `model` is in (the picker opens there, so it
    /// is one tap), then reopens the model from inside it.
    fn open_folder_for(&mut self, model: PathBuf) {
        let slot = Rc::clone(&self.picked);
        let opts = PickOptions { folders: true, start_in: model.parent().map(Path::to_path_buf) };
        libfilepicker::pick_with(opts, move |picked| deliver(&slot, picked, Some(model)));
    }

    fn open_entry(&mut self, container: Arc<dyn libfileparser_core::Container>, index: u64) {
        let name = container.entries(index..index + 1).ok().and_then(|mut e| e.pop()).map_or_else(|| format!("entry {index}"), |e| e.path);
        self.job = Some(Job::spawn(name, Target::Push, None, move || container.open(index).map_err(|e| anyhow::anyhow!("{e}"))));
        self.pending_access = None;
    }

    /// Shows `choice` for the current page: another of its document's facets, or the
    /// file read by another loader.
    fn view(&mut self, choice: ViewChoice) {
        match choice {
            ViewChoice::Loader(id) => self.view_as(id),
            ViewChoice::Facet(index) => {
                let Some(page) = self.stack.last() else { return };
                let Some(facets) = page.doc.facets.clone() else { return };
                self.job = Some(Job::facet(page.src.clone(), facets, index, page.loader));
                self.pending_access = None;
            }
        }
    }

    fn view_as(&mut self, loader: &'static str) {
        let Some(page) = self.stack.last() else { return };
        let src = page.src.clone();
        self.job = Some(Job::spawn(src.name().into(), Target::ReplaceTop, Some(loader.into()), move || Ok(src)));
        self.pending_access = None;
    }

    fn close_page(&mut self, page: Page) {
        self.retired.extend(page.view.textures());
    }

    fn back(&mut self) {
        if self.stack.len() > 1 {
            let page = self.stack.pop().unwrap();
            self.close_page(page);
            self.error = None;
        }
    }

    /// Starts loading a file the picker chose, or takes in a finished load. Returns
    /// true when a file started or finished loading (successfully or not).
    pub fn poll_job(&mut self) -> bool {
        let pick = self.picked.borrow_mut().take();
        if let Some(Pick { picked, reopen }) = pick {
            let access = Rc::new(picked);
            // Granted the model's folder (or one above it): open the model itself, now
            // readable with its neighbours. Any other folder lists as usual.
            let path = match reopen {
                Some(model) if inside(&model, access.path()) => model,
                _ => access.path().to_path_buf(),
            };
            self.open_path(path);
            self.pending_access = Some(access);
            return true;
        }
        let Some(job) = &self.job else { return false };
        let Some(result) = job.poll() else { return false };
        let target = job.target;
        self.job = None;
        // A root page owns what was picked for it; others read through the page they
        // were opened from (or replace).
        let access = match target {
            Target::Root => self.pending_access.take(),
            Target::Push | Target::ReplaceTop => self.stack.last().and_then(|p| p.access.clone()),
        };
        self.pending_access = None;
        match result {
            Ok(Loaded { src, doc, loader, attempts, load_time }) => {
                self.error = None;
                let view = ViewState::new(&doc.content);
                let page = Page { id: self.next_page_id, src, doc, loader, attempts, load_time, view, access };
                self.next_page_id += 1;
                match target {
                    Target::Root => {
                        for old in std::mem::take(&mut self.stack) {
                            self.close_page(old);
                        }
                        self.stack.push(page);
                    }
                    Target::Push => self.stack.push(page),
                    Target::ReplaceTop => {
                        if let Some(old) = self.stack.pop() {
                            self.close_page(old);
                        }
                        self.stack.push(page);
                    }
                }
                self.title = Some(self.stack.iter().map(|p| p.src.name()).collect::<Vec<_>>().join(" › "));
            }
            Err(e) => self.error = Some(e),
        }
        true
    }

    pub fn ui(&mut self, ui: &mut Ui, tex: &mut dyn Host) {
        let pal = ui.theme.palette;
        let safe = self.safe_area;
        // Layout works in the safe rect; the backgrounds still fill the screen.
        let screen = ui.screen_size() - Vec2::new(safe.left + safe.right, safe.top + safe.bottom);
        let class = WidthClass::for_width(screen.x, self.width_class);
        self.width_class = Some(class);
        let root = Id::from_name("root");
        let mut action = None;
        ui.container_id(root, Layout::column().padding(safe), Frame { fill: pal.bg_app, ..Frame::none() }, |ui| {
            let drop = ui.drop_zone(&[libgui_winit::FILES]);
            if let Some(payload) = drop.dropped
                && let Some(path) = payload.take::<Vec<PathBuf>>().ok().and_then(|p| p.into_iter().next())
            {
                self.open_path(path);
            }
            if drop.hovered {
                let rect = drop.rect;
                ui.overlay(move |p| {
                    p.rect(rect, Color::BLACK.with_alpha(0.6), 0.0);
                    p.text_centered(rect, 24.0, Color::WHITE, "Drop to open");
                });
            }

            if class == WidthClass::Compact {
                self.top_bar(ui);
            } else {
                self.toolbar(ui, class);
            }
            let panel = Frame { fill: pal.bg_panel, border: pal.border, border_width: 1.0, ..Frame::none() };
            let central = Id::from_name("central");
            ui.container(Layout::row().height(Size::Grow(1.0)), Frame::none(), |ui| {
                ui.container_id(central, Layout::column(), Frame { clip: true, ..Frame::none() }, |ui| {
                    action = self.central(ui, tex);
                });
                // Expanded: beside the content, which narrows to make room.
                if self.show_info && class == WidthClass::Expanded {
                    ui.container_id(Id::from_name("info"), Layout::column().width(Size::Fixed(INFO_WIDTH)), panel, |ui| {
                        self.info_panel(ui);
                    });
                }
            });
            // Narrower: a drawer over the content's right edge (all of it on compact),
            // placed from last frame's layout like any layer.
            if self.show_info
                && class != WidthClass::Expanded
                && let Some(area) = ui.rect_of(central)
            {
                let w = if class == WidthClass::Compact { area.w } else { INFO_WIDTH.min(area.w) };
                let rect = Rect { x: area.right() - w, y: area.y, w, h: area.h };
                ui.layer(Id::from_name("info drawer"), rect, panel, |ui| self.info_panel(ui));
                if ui.key_pressed(Key::Escape) {
                    self.show_info = false;
                }
            }
            // Compact and short windows keep the status bar only to show loading.
            if (class != WidthClass::Compact && screen.y >= SHORT) || self.job.is_some() {
                self.status_bar(ui);
            }
        });
        if let Some(Action::OpenEntry(container, index)) = action {
            self.open_entry(container, index);
        }

        // Global shortcuts last, so focused widgets get the first chance at a chord.
        if self.keys.triggered(ui, Command::Open) {
            self.open_dialog();
        }
        if self.keys.triggered(ui, Command::Back) {
            self.back();
        }
        if self.keys.triggered(ui, Command::ToggleInfo) {
            self.show_info = !self.show_info;
        }
    }

    /// The compact top bar: Back, the current file's name, and a "···" menu with
    /// everything the full toolbar has.
    fn top_bar(&mut self, ui: &mut Ui) {
        let pal = ui.theme.palette;
        let layout = Layout::row().padding(Insets::xy(8.0, 6.0)).gap(8.0);
        let frame = Frame { fill: pal.bg_panel, border: pal.border, border_width: 1.0, ..Frame::none() };
        ui.container_id(Id::from_name("toolbar"), layout, frame, |ui| {
            if self.stack.len() > 1 && ui.button("‹ Back").clicked {
                self.back();
            }
            ui.label(self.stack.last().map_or("Viewer", |p| p.src.name()));
            ui.flex();
            let mut pick = None;
            // Middle dots, not "⋯": Inter lacks that, and drawing it would load a system font.
            ui.menu_button("···", |ui| {
                if ui.menu_item("Open…").clicked {
                    self.open_dialog();
                }
                if self.stack.last().is_some() {
                    if ui.menu_item(if self.show_info { "Hide info" } else { "Show info" }).clicked {
                        self.show_info = !self.show_info;
                    }
                    let (choices, current) = self.view_as_choices();
                    if !choices.is_empty() {
                        ui.menu_separator();
                    }
                    for (i, (choice, name)) in choices.into_iter().enumerate() {
                        let label = format!("View as {name}");
                        if ui.menu_item_ex(&label, None, i != current).clicked {
                            pick = Some(choice);
                        }
                    }
                }
            });
            if let Some(choice) = pick {
                self.view(choice);
            }
        });
    }

    /// What the current page can be shown as, with names, and which it is shown as:
    /// first its document's facets when libfileparser read it (Scene, Stage, Layers…,
    /// Source, Raw), then the other loaders that claim the file.
    fn view_as_choices(&self) -> (Vec<(ViewChoice, String)>, usize) {
        let Some(page) = self.stack.last() else { return (Vec::new(), 0) };
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
        let has = |role: libfileparser_core::Role| page.doc.facets.as_ref().is_some_and(|f| f.doc.facets().any(|x| x.role == role));
        let mut ids: Vec<&'static str> = registry().candidates(&page.src).iter().map(|(_, l)| l.id()).collect();
        // Text is always worth offering, even for files that don't sniff as text.
        if !ids.contains(&"text") {
            ids.push("text");
        }
        // A loader forced with --view-as may not claim the file; list it anyway,
        // or the combo would fall back to entry 0 and re-open the file every frame.
        if !ids.contains(&page.loader) {
            ids.push(page.loader);
        }
        for id in ids {
            // The document's own facets stand for the loader that read it, its Source
            // for plain text and its Raw for hex.
            let shown_by_facets = page.doc.facets.is_some()
                && (id == page.loader || (id == "text" && has(libfileparser_core::Role::Source)) || (id == "hex" && has(libfileparser_core::Role::Raw)));
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

    fn toolbar(&mut self, ui: &mut Ui, class: WidthClass) {
        let pal = ui.theme.palette;
        let layout = Layout::row().padding(Insets::xy(8.0, 6.0)).gap(8.0);
        let frame = Frame { fill: pal.bg_panel, border: pal.border, border_width: 1.0, ..Frame::none() };
        ui.container_id(Id::from_name("toolbar"), layout, frame, |ui| {
            let open = ui.button("Open…");
            ui.tooltip(&open, &format!("Open a file ({})", self.keys.label(Command::Open)));
            if open.clicked {
                self.open_dialog();
            }
            if self.stack.len() > 1 {
                let back = ui.button("Back");
                ui.tooltip(&back, "Backspace");
                if back.clicked {
                    self.back();
                }
            }
            // Medium keeps the last two levels: the current file stays readable.
            let keep = if class == WidthClass::Medium { 2 } else { usize::MAX };
            let skip = self.stack.len().saturating_sub(keep);
            let mut crumbs: Vec<&str> = self.stack[skip..].iter().map(|p| p.src.name()).collect();
            if skip > 0 {
                crumbs.insert(0, "…");
            }
            ui.label(&crumbs.join("  ›  "));
            ui.flex();

            let (choices, current) = self.view_as_choices();
            if !choices.is_empty() {
                let names: Vec<&str> = choices.iter().map(|(_, n)| n.as_str()).collect();
                let mut selected = current;
                if class == WidthClass::Expanded {
                    ui.label_muted("View as");
                }
                let combo = if class == WidthClass::Expanded { 200.0 } else { 140.0 };
                ui.container(Layout::row().width(Size::Fixed(combo)), Frame::none(), |ui| {
                    ui.combo("view_as", &mut selected, &names);
                });
                if selected != current {
                    self.view(choices[selected].0);
                }
            }
            ui.container(Layout::row().width(Size::Fit), Frame::none(), |ui| {
                let info = ui.toggle("Info", &mut self.show_info);
                ui.tooltip(&info, "I");
            });
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        let pal = ui.theme.palette;
        let layout = Layout::row().padding(Insets::xy(8.0, 4.0)).gap(12.0);
        let frame = Frame { fill: pal.bg_panel, border: pal.border, border_width: 1.0, ..Frame::none() };
        ui.container_id(Id::from_name("status"), layout, frame, |ui| {
            if let Some(job) = &self.job {
                ui.progress("loading", None);
                ui.label(&format!("Loading {}…", job.name));
            } else if let Some(page) = self.stack.last() {
                ui.label(&page.doc.format);
                ui.label_muted("·");
                if !page.src.is_folder() {
                    ui.label(&human_bytes(page.src.len()));
                    ui.label_muted("·");
                }
                ui.label(&format!("{} view", page.doc.content.model_name()));
                ui.flex();
                if !self.stable {
                    ui.label_muted(&format!("decoded in {:.1} ms", page.load_time.as_secs_f64() * 1000.0));
                }
            } else {
                ui.label_muted("No file open");
            }
        });
    }

    fn info_panel(&self, ui: &mut Ui) {
        let Some(page) = self.stack.last() else {
            ui.label_muted("No file open");
            return;
        };
        let opts = ScrollOptions { padding: Insets::all(12.0), gap: 4.0, ..ScrollOptions::new(Size::Grow(1.0)) };
        ui.scroll_area_with("info", opts, |ui| {
            let row = |ui: &mut Ui, k: &str, v: &str| {
                ui.label_muted(k);
                ui.paragraph(v);
                ui.space(4.0);
            };
            row(ui, "Name", page.src.name());
            if let Some(p) = page.src.path() {
                row(ui, "Path", &p.display().to_string());
            }
            if !page.src.is_folder() {
                row(ui, "Size", &format!("{} ({} bytes)", human_bytes(page.src.len()), page.src.len()));
            }
            row(ui, "Format", &page.doc.format);
            row(ui, "Loader", page.loader);
            for (k, v) in &page.doc.metadata {
                row(ui, k, v);
            }
            // Files the document refers to but couldn't read. Blocked ones also get the
            // banner over the content; missing ones only this quiet note, since no
            // permission brings them back.
            for (reason, title) in [
                (Unreadable::Missing, "Not found"),
                (Unreadable::Blocked, "Not allowed to read"),
                // Not looked for: the model points outside its folder, which Viewer never reads.
                (Unreadable::Outside, "Outside the model's folder"),
            ] {
                let names = page.doc.unreadable.names(reason);
                if !names.is_empty() {
                    ui.space(8.0);
                    ui.section(&format!("{title} ({})", names.len()));
                    ui.paragraph(&name_list(&names, 12));
                }
            }
            if !page.attempts.is_empty() {
                ui.space(8.0);
                ui.section("Loaders that failed first");
                for (id, err) in &page.attempts {
                    ui.paragraph(&format!("{id}: {err}"));
                }
            }
        });
    }

    fn central(&mut self, ui: &mut Ui, tex: &mut dyn Host) -> Option<Action> {
        if let Some(err) = &self.error {
            let danger = ui.theme.palette.danger;
            ui.container(Layout::row().padding(Insets::all(8.0)), Frame::none(), |ui| {
                ui.text_with(&format!("Couldn't open: {err}"), 14.0, danger);
            });
        }
        if let Some(model) = self.blocked_banner(ui) {
            self.open_folder_for(model);
        }
        let dark = self.dark;
        match self.stack.last_mut() {
            Some(page) => views::show(ui, page, tex, dark),
            None => {
                if self.job.is_none() {
                    let muted = ui.theme.palette.text_muted;
                    ui.container(Layout::column().align(Align::Center, Align::Center), Frame::none(), |ui| {
                        let hint = format!("Drop a file here, or press {}", self.keys.label(Command::Open));
                        ui.text_with(&hint, 18.0, muted);
                    });
                }
                None
            }
        }
    }
}

impl ViewerApp {
    /// When the page's file names files Viewer isn't allowed to read (on iOS: the user
    /// picked the model alone, so its textures are out of reach), a banner over the
    /// content says so and offers the fix. Returns the model to reopen when its
    /// Open Folder… is pressed. The model stays usable underneath.
    fn blocked_banner(&self, ui: &mut Ui) -> Option<PathBuf> {
        let page = self.stack.last()?;
        let blocked = page.doc.unreadable.count(Unreadable::Blocked);
        let model = page.src.path().filter(|_| blocked > 0 && !page.src.is_folder())?.to_path_buf();
        let pal = ui.theme.palette;
        let files = if blocked == 1 { "1 file isn't shown.".to_string() } else { format!("{blocked} files aren't shown.") };
        let why = if cfg!(target_os = "ios") {
            "iOS only lets Viewer open the file you picked, not the files next to it. Open its folder to show them."
        } else {
            "Viewer isn't allowed to read the folder this file is in. Open the folder to grant it."
        };
        let frame = Frame { fill: pal.surface, border: pal.warning, border_width: 1.0, ..Frame::none() };
        let mut open = false;
        ui.container_id(Id::from_name("blocked banner"), Layout::row().padding(Insets::xy(12.0, 8.0)).gap(12.0).align(Align::Start, Align::Center), frame, |ui| {
            ui.container(Layout::column().width(Size::Grow(1.0)), Frame::none(), |ui| {
                ui.paragraph(&format!("{files} {why}"));
            });
            open = ui.button("Open Folder…").clicked;
        });
        open.then_some(model)
    }
}

/// Hands a picker's answer to the app and wakes it (the answer can come after the
/// frame that asked, on phones). A cancelled pick changes nothing.
fn deliver(slot: &Rc<RefCell<Option<Pick>>>, picked: Option<Picked>, reopen: Option<PathBuf>) {
    let Some(picked) = picked else { return };
    *slot.borrow_mut() = Some(Pick { picked, reopen });
    if let Some(wake) = crate::loading::WAKE.get() {
        wake();
    }
}

/// `path` is `folder` or somewhere under it. Compared as real paths: iOS mixes
/// `/var/…` and `/private/var/…` for the same place, and a picked folder may be
/// reached through a link.
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

#[cfg(test)]
mod tests {
    use super::WidthClass::{self, *};

    #[test]
    fn a_model_is_inside_its_own_folder_and_its_parents() {
        let dir = std::env::temp_dir().join(format!("viewer-inside-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("car/textures")).unwrap();
        std::fs::write(dir.join("car/car.usdz"), b"").unwrap();
        let model = dir.join("car/car.usdz");
        assert!(super::inside(&model, &dir.join("car")));
        assert!(super::inside(&model, &dir), "a parent folder grants it too");
        assert!(!super::inside(&model, &dir.join("car/textures")), "a folder inside the model's doesn't");
        assert!(!super::inside(&model, &std::env::temp_dir().join("elsewhere")));
        // Through a link to the same place (iOS's /var and /private/var).
        #[cfg(unix)]
        {
            let link = std::env::temp_dir().join(format!("viewer-inside-link-{}", std::process::id()));
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&dir, &link).unwrap();
            assert!(super::inside(&link.join("car/car.usdz"), &dir.join("car")));
            std::fs::remove_file(link).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn long_name_lists_are_cut() {
        let names: Vec<String> = (1..=5).map(|i| format!("t{i}.png")).collect();
        assert_eq!(super::name_list(&names, 3), "t1.png, t2.png, t3.png and 2 more");
        assert_eq!(super::name_list(&names[..2], 3), "t1.png, t2.png");
    }

    #[test]
    fn width_classes() {
        assert_eq!(WidthClass::for_width(390.0, None), Compact);
        assert_eq!(WidthClass::for_width(600.0, None), Medium);
        assert_eq!(WidthClass::for_width(1023.0, None), Medium);
        assert_eq!(WidthClass::for_width(1024.0, None), Expanded);
    }

    #[test]
    fn shrinking_waits_past_the_threshold() {
        // Growing switches at the threshold.
        assert_eq!(WidthClass::for_width(600.0, Some(Compact)), Medium);
        // Shrinking holds until 24 px below it.
        assert_eq!(WidthClass::for_width(590.0, Some(Medium)), Medium);
        assert_eq!(WidthClass::for_width(576.0, Some(Medium)), Medium);
        assert_eq!(WidthClass::for_width(575.0, Some(Medium)), Compact);
        assert_eq!(WidthClass::for_width(1010.0, Some(Expanded)), Expanded);
        assert_eq!(WidthClass::for_width(990.0, Some(Expanded)), Medium);
        // A big jump goes straight to the right class.
        assert_eq!(WidthClass::for_width(320.0, Some(Expanded)), Compact);
    }
}
