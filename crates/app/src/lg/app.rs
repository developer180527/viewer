//! The window's contents: toolbar, the current page's view, info panel, status bar.
//! Knows nothing about winit or the GPU; the host in `mod.rs` drives it.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use libgui::*;
use libgui_keymap::{Chord, Keymap};
use viewer_core::{Document, FileSource};
use libfilepicker::Picked;

use super::Textures;
use super::views::{self, Action, ViewState};
use crate::formats::registry;
use crate::loading::{Job, Loaded, Target};
use crate::util::human_bytes;

/// One level of navigation: a file, or an entry opened from a container.
pub struct Page {
    pub id: u64,
    pub src: FileSource,
    pub doc: Document,
    pub loader: &'static str,
    pub attempts: Vec<(&'static str, String)>,
    pub load_time: Duration,
    pub view: ViewState,
}

/// How wide the window is, which decides where the chrome goes
/// (docs/responsive-ui.md §1). Width only: a narrow desktop window gets the phone layout.
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
    picked: Rc<RefCell<Option<Picked>>>,
    /// The file last picked, held so it stays readable (see [`Picked`]).
    reading: Option<Picked>,
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
            reading: None,
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
    }

    fn open_dialog(&mut self) {
        let slot = Rc::clone(&self.picked);
        libfilepicker::pick(move |picked| {
            if picked.is_some() {
                *slot.borrow_mut() = picked;
                if let Some(wake) = crate::loading::WAKE.get() {
                    wake();
                }
            }
        });
    }

    fn open_entry(&mut self, container: Arc<dyn viewer_core::Container>, index: usize) {
        let name = container.entries()[index].path.clone();
        self.job = Some(Job::spawn(name, Target::Push, None, move || container.open_entry(index)));
    }

    fn view_as(&mut self, loader: &'static str) {
        let Some(page) = self.stack.last() else { return };
        let src = page.src.clone();
        self.job = Some(Job::spawn(src.name().into(), Target::ReplaceTop, Some(loader.into()), move || Ok(src)));
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
        let picked = self.picked.borrow_mut().take();
        if let Some(picked) = picked {
            self.open_path(picked.path().to_path_buf());
            self.reading = Some(picked);
            return true;
        }
        let Some(job) = &self.job else { return false };
        let Some(result) = job.poll() else { return false };
        let target = job.target;
        self.job = None;
        match result {
            Ok(Loaded { src, doc, loader, attempts, load_time }) => {
                self.error = None;
                let view = ViewState::new(&doc.content);
                let page = Page { id: self.next_page_id, src, doc, loader, attempts, load_time, view };
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

    pub fn ui(&mut self, ui: &mut Ui, tex: &mut dyn Textures) {
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
                    ui.menu_separator();
                    let (ids, current) = self.view_as_choices();
                    for id in ids {
                        let name = registry().get(id).map_or(id, |l| l.name());
                        let label = format!("View as {name}");
                        if ui.menu_item_ex(&label, None, id != current).clicked {
                            pick = Some(id);
                        }
                    }
                }
            });
            if let Some(id) = pick {
                self.view_as(id);
            }
        });
    }

    /// The loaders the current page can be shown with, and the one it is shown with.
    fn view_as_choices(&self) -> (Vec<&'static str>, &'static str) {
        let Some(page) = self.stack.last() else { return (Vec::new(), "") };
        let current = page.loader;
        let mut ids: Vec<&'static str> = registry().candidates(&page.src).iter().map(|(_, l)| l.id()).collect();
        // Text is always worth offering, even for files that don't sniff as text.
        if !ids.contains(&"text") {
            ids.push("text");
        }
        // A loader forced with --view-as may not claim the file; list it anyway,
        // or the combo would fall back to entry 0 and re-open the file every frame.
        if !ids.contains(&current) {
            ids.push(current);
        }
        (ids, current)
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

            if !self.stack.is_empty() {
                let (ids, current) = self.view_as_choices();
                let names: Vec<&str> = ids.iter().map(|id| registry().get(id).map_or(*id, |l| l.name())).collect();
                let mut selected = ids.iter().position(|id| *id == current).expect("current loader is listed");
                if class == WidthClass::Expanded {
                    ui.label_muted("View as");
                }
                let combo = if class == WidthClass::Expanded { 200.0 } else { 140.0 };
                ui.container(Layout::row().width(Size::Fixed(combo)), Frame::none(), |ui| {
                    ui.combo("view_as", &mut selected, &names);
                });
                if ids[selected] != current {
                    self.view_as(ids[selected]);
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
                ui.label(&human_bytes(page.src.len()));
                ui.label_muted("·");
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
            row(ui, "Size", &format!("{} ({} bytes)", human_bytes(page.src.len()), page.src.len()));
            row(ui, "Format", &page.doc.format);
            row(ui, "Loader", page.loader);
            for (k, v) in &page.doc.metadata {
                row(ui, k, v);
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

    fn central(&mut self, ui: &mut Ui, tex: &mut dyn Textures) -> Option<Action> {
        if let Some(err) = &self.error {
            let danger = ui.theme.palette.danger;
            ui.container(Layout::row().padding(Insets::all(8.0)), Frame::none(), |ui| {
                ui.text_with(&format!("Couldn't open: {err}"), 14.0, danger);
            });
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
