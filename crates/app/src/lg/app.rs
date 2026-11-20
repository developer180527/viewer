//! The window's contents: toolbar, the current page's view, info panel, status bar.
//! Knows nothing about winit or the GPU; the host in `mod.rs` drives it. What's open
//! and what can be done with it is `viewer-core`'s [`Session`]; this draws it, and
//! maps buttons, keys and drops onto its commands.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use libgui::*;
use libgui_keymap::{Chord, Keymap};
use libfilepicker::{PickOptions, Picked};
use viewer_core::{GrantId, Info, Notice, Session, Status, human_bytes};

use libviewer::Host;
use viewer_surface::views::{self, Action, ViewState};

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
    session: Session,
    /// Each open page's view state (scroll, zoom, textures), by its id.
    views: HashMap<u64, ViewState>,
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
    /// What the user picked, kept while the session's pages read through it: each
    /// holds the platform's access (see [`Picked`]), dropped here, on this thread.
    grants: HashMap<GrantId, Picked>,
    keys: Keymap<Command>,
}

impl ViewerApp {
    pub fn new(session: Session, dark: bool) -> Self {
        Self {
            width_class: None,
            session,
            views: HashMap::new(),
            show_info: false,
            safe_area: Insets::all(0.0),
            stable: false,
            dark,
            title: None,
            retired: Vec::new(),
            picked: Rc::default(),
            grants: HashMap::new(),
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
        self.session.shown()
    }

    pub fn error(&self) -> Option<&str> {
        self.session.error()
    }

    pub fn loading(&self) -> bool {
        self.session.loading().is_some()
    }

    fn open_dialog(&mut self) {
        let slot = Rc::clone(&self.picked);
        libfilepicker::pick(move |picked| deliver(&slot, picked, None));
    }

    /// Open Folder…: asks for the folder `model` is in (the picker opens there, so it
    /// is one tap), then reopens the model from inside it.
    fn open_folder_for(&mut self, model: PathBuf) {
        let slot = Rc::clone(&self.picked);
        let opts = PickOptions { folders: true, start_in: Session::open_folder_for(&model) };
        libfilepicker::pick_with(opts, move |picked| deliver(&slot, picked, Some(model)));
    }

    /// Frees the view state (and its textures) of pages the session closed, and
    /// makes it for pages it opened.
    fn sync_views(&mut self) {
        for id in self.session.take_closed() {
            if let Some(view) = self.views.remove(&id) {
                self.retired.extend(view.textures());
            }
        }
        for page in self.session.pages() {
            self.views.entry(page.id).or_insert_with(|| ViewState::new(&page.doc.content));
        }
        if let Some(t) = self.session.take_title() {
            self.title = Some(t);
        }
        for id in self.session.take_released() {
            self.grants.remove(&id);
        }
    }

    fn back(&mut self) {
        if self.session.back() {
            self.sync_views();
        }
    }

    /// Starts loading a file the picker chose, or takes in a finished load. Returns
    /// true when a file started or finished loading (successfully or not).
    pub fn poll_job(&mut self) -> bool {
        let pick = self.picked.borrow_mut().take();
        if let Some(Pick { picked, reopen }) = pick {
            let id = self.session.open_granted(picked.path(), reopen, None);
            self.grants.insert(id, picked);
            return true;
        }
        let done = self.session.poll();
        if done {
            self.sync_views();
        }
        done
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
                self.session.open_path(path);
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
            if (class != WidthClass::Compact && screen.y >= SHORT) || self.loading() {
                self.status_bar(ui);
            }
        });
        if let Some(Action::OpenEntry(container, index)) = action {
            self.session.open_entry(container, index);
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
            if self.session.can_go_back() && ui.button("‹ Back").clicked {
                self.back();
            }
            ui.label(self.session.current().map_or("Viewer", |p| p.src.name()));
            ui.flex();
            let mut pick = None;
            // Middle dots, not "⋯": Inter lacks that, and drawing it would load a system font.
            ui.menu_button("···", |ui| {
                if ui.menu_item("Open…").clicked {
                    self.open_dialog();
                }
                if self.session.current().is_some() {
                    if ui.menu_item(if self.show_info { "Hide info" } else { "Show info" }).clicked {
                        self.show_info = !self.show_info;
                    }
                    let (choices, current) = self.session.view_choices();
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
                self.session.view(choice);
            }
        });
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
            if self.session.can_go_back() {
                let back = ui.button("Back");
                ui.tooltip(&back, "Backspace");
                if back.clicked {
                    self.back();
                }
            }
            // Medium keeps the last two levels: the current file stays readable.
            let keep = if class == WidthClass::Medium { 2 } else { usize::MAX };
            let pages = self.session.pages();
            let skip = pages.len().saturating_sub(keep);
            let mut crumbs: Vec<&str> = pages[skip..].iter().map(|p| p.src.name()).collect();
            if skip > 0 {
                crumbs.insert(0, "…");
            }
            ui.label(&crumbs.join("  ›  "));
            ui.flex();

            let (choices, current) = self.session.view_choices();
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
                    self.session.view(choices[selected].0);
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
            match self.session.status() {
                Status::Loading(name) => {
                    ui.progress("loading", None);
                    ui.label(&format!("Loading {name}…"));
                }
                Status::Shown { format, size, model, load_time } => {
                    ui.label(&format);
                    ui.label_muted("·");
                    if let Some(size) = size {
                        ui.label(&human_bytes(size));
                        ui.label_muted("·");
                    }
                    ui.label(&format!("{model} view"));
                    ui.flex();
                    if !self.stable {
                        ui.label_muted(&format!("decoded in {:.1} ms", load_time.as_secs_f64() * 1000.0));
                    }
                }
                Status::Empty => ui.label_muted("No file open"),
            }
        });
    }

    fn info_panel(&self, ui: &mut Ui) {
        let items = self.session.info();
        if items.is_empty() {
            ui.label_muted("No file open");
            return;
        }
        let opts = ScrollOptions { padding: Insets::all(12.0), gap: 4.0, ..ScrollOptions::new(Size::Grow(1.0)) };
        ui.scroll_area_with("info", opts, |ui| {
            for item in &items {
                match item {
                    Info::Row(k, v) => {
                        ui.label_muted(k);
                        ui.paragraph(v);
                        ui.space(4.0);
                    }
                    Info::Section(title, text) => {
                        ui.space(8.0);
                        ui.section(title);
                        for line in text.lines() {
                            ui.paragraph(line);
                        }
                    }
                }
            }
        });
    }

    fn central(&mut self, ui: &mut Ui, tex: &mut dyn Host) -> Option<Action> {
        if let Some(err) = self.session.error() {
            let danger = ui.theme.palette.danger;
            ui.container(Layout::row().padding(Insets::all(8.0)), Frame::none(), |ui| {
                ui.text_with(&format!("Couldn't open: {err}"), 14.0, danger);
            });
        }
        if let Some(model) = self.notices(ui) {
            self.open_folder_for(model);
        }
        let dark = self.dark;
        let page = self.session.current();
        match page.and_then(|p| self.views.get_mut(&p.id).map(|v| (p, v))) {
            Some((page, view)) => views::show(ui, page, view, tex, dark),
            None => {
                if !self.loading() {
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
    /// The session's notices over the content: that a fallback reader is showing the
    /// file, or that files it names can't be read, with the fix. Returns the model to
    /// reopen when that notice's Open Folder… is pressed; the model stays usable
    /// underneath.
    fn notices(&self, ui: &mut Ui) -> Option<PathBuf> {
        let pal = ui.theme.palette;
        let frame = Frame { fill: pal.surface, border: pal.warning, border_width: 1.0, ..Frame::none() };
        let mut reopen = None;
        for notice in self.session.notices() {
            match notice {
                Notice::Fallback(message) => {
                    ui.container_id(Id::from_name("fallback notice"), Layout::row().padding(Insets::xy(12.0, 8.0)), frame, |ui| {
                        ui.paragraph(&message);
                    });
                }
                Notice::Blocked { message, reopen: model } => {
                    ui.container_id(Id::from_name("blocked banner"), Layout::row().padding(Insets::xy(12.0, 8.0)).gap(12.0).align(Align::Start, Align::Center), frame, |ui| {
                        ui.container(Layout::column().width(Size::Grow(1.0)), Frame::none(), |ui| {
                            ui.paragraph(&message);
                        });
                        if ui.button("Open Folder…").clicked {
                            reopen = Some(model);
                        }
                    });
                }
            }
        }
        reopen
    }
}

/// Hands a picker's answer to the app and wakes it (the answer can come after the
/// frame that asked, on phones). A cancelled pick changes nothing.
fn deliver(slot: &Rc<RefCell<Option<Pick>>>, picked: Option<Picked>, reopen: Option<PathBuf>) {
    let Some(picked) = picked else { return };
    *slot.borrow_mut() = Some(Pick { picked, reopen });
    if let Some(wake) = super::WAKE.get() {
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::WidthClass::{self, *};

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
