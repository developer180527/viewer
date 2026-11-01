//! The window's contents: toolbar, the current page's view, info panel, status bar.
//! Knows nothing about winit or the GPU; the host in `mod.rs` drives it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use libgui::*;
use viewer_core::{Document, FileSource};

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

pub struct ViewerApp {
    stack: Vec<Page>,
    job: Option<Job>,
    error: Option<String>,
    show_info: bool,
    next_page_id: u64,
    pub dark: bool,
    /// New window title, for the host to apply.
    pub title: Option<String>,
    /// Textures of closed pages, for the host to free.
    pub retired: Vec<TextureId>,
}

impl ViewerApp {
    pub fn new(job: Option<Job>, dark: bool) -> Self {
        Self {
            stack: Vec::new(),
            job,
            error: None,
            show_info: false,
            next_page_id: 0,
            dark,
            title: None,
            retired: Vec::new(),
        }
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
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.open_path(path);
        }
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

    /// Returns true when a load finished (successfully or not).
    pub fn poll_job(&mut self) -> bool {
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
        let root = Id::from_name("root");
        let mut action = None;
        ui.container_id(root, Layout::column(), Frame { fill: pal.bg_app, ..Frame::none() }, |ui| {
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

            self.toolbar(ui);
            ui.container(Layout::row().height(Size::Grow(1.0)), Frame::none(), |ui| {
                ui.container_id(Id::from_name("central"), Layout::column(), Frame { clip: true, ..Frame::none() }, |ui| {
                    action = self.central(ui, tex);
                });
                if self.show_info {
                    let frame = Frame { fill: pal.bg_panel, border: pal.border, border_width: 1.0, ..Frame::none() };
                    ui.container_id(Id::from_name("info"), Layout::column().width(Size::Fixed(300.0)), frame, |ui| {
                        self.info_panel(ui);
                    });
                }
            });
            self.status_bar(ui);
        });
        if let Some(Action::OpenEntry(container, index)) = action {
            self.open_entry(container, index);
        }

        // Global shortcuts last, so focused widgets get the first chance at a chord.
        let cmd = if cfg!(target_os = "macos") { Shortcut::plain(Key::O).logo() } else { Shortcut::plain(Key::O).ctrl() };
        if ui.consume_shortcut(cmd) {
            self.open_dialog();
        }
        if ui.consume_shortcut(Shortcut::plain(Key::ArrowLeft).alt()) || ui.consume_shortcut(Shortcut::plain(Key::Backspace)) {
            self.back();
        }
        if ui.consume_shortcut(Shortcut::plain(Key::I)) {
            self.show_info = !self.show_info;
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let pal = ui.theme.palette;
        let layout = Layout::row().padding(Insets::xy(8.0, 6.0)).gap(8.0);
        let frame = Frame { fill: pal.bg_panel, border: pal.border, border_width: 1.0, ..Frame::none() };
        ui.container_id(Id::from_name("toolbar"), layout, frame, |ui| {
            let open = ui.button("Open…");
            ui.tooltip(&open, if cfg!(target_os = "macos") { "Open a file (⌘O)" } else { "Open a file (Ctrl+O)" });
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
            let crumbs = self.stack.iter().map(|p| p.src.name()).collect::<Vec<_>>().join("  ›  ");
            ui.label(&crumbs);
            ui.flex();

            if let Some(page) = self.stack.last() {
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
                let names: Vec<&str> = ids.iter().map(|id| registry().get(id).map_or(*id, |l| l.name())).collect();
                let mut selected = ids.iter().position(|id| *id == current).expect("current loader is listed");
                ui.label_muted("View as");
                ui.container(Layout::row().width(Size::Fixed(200.0)), Frame::none(), |ui| {
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
                ui.label_muted(&format!("decoded in {:.1} ms", page.load_time.as_secs_f64() * 1000.0));
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
                        let hint = if cfg!(target_os = "macos") { "Drop a file here, or press ⌘O" } else { "Drop a file here, or press Ctrl+O" };
                        ui.text_with(hint, 18.0, muted);
                    });
                }
                None
            }
        }
    }
}
