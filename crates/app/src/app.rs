use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, RichText};
use viewer_core::{Document, FileSource};

use crate::loading::{Job, Loaded, Target, WAKE};
use crate::util::human_bytes;
use crate::views::{self, Action, ViewState};
use crate::{Args, formats::registry, startup};

const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const BACK: KeyboardShortcut = KeyboardShortcut::new(Modifiers::ALT, Key::ArrowLeft);

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
    md_cache: egui_commonmark::CommonMarkCache,
    // Automation (--benchmark / --screenshot).
    benchmark: bool,
    screenshot: Option<PathBuf>,
    content_frames: u32,
    first_frame_done: bool,
}

impl ViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args, job: Option<Job>) -> Self {
        let ctx = cc.egui_ctx.clone();
        WAKE.get_or_init(|| Box::new(move || ctx.request_repaint()));
        Self {
            stack: Vec::new(),
            job,
            error: None,
            show_info: false,
            next_page_id: 0,
            md_cache: Default::default(),
            benchmark: args.benchmark,
            screenshot: args.screenshot,
            content_frames: 0,
            first_frame_done: false,
        }
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

    fn back(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
            self.error = None;
        }
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else { return };
        let Some(result) = job.poll() else { return };
        let target = job.target;
        self.job = None;
        match result {
            Ok(Loaded { src, doc, loader, attempts, load_time }) => {
                self.error = None;
                let view = ViewState::new(&doc.content);
                let page = Page { id: self.next_page_id, src, doc, loader, attempts, load_time, view };
                self.next_page_id += 1;
                match target {
                    Target::Root => self.stack = vec![page],
                    Target::Push => self.stack.push(page),
                    Target::ReplaceTop => {
                        self.stack.pop();
                        self.stack.push(page);
                    }
                }
                let title = self.stack.iter().map(|p| p.src.name()).collect::<Vec<_>>().join(" › ");
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped {
            self.open_path(path);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&OPEN)) {
            self.open_dialog();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&BACK) || i.consume_key(Modifiers::NONE, Key::Backspace)) {
            self.back();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::I)) {
            self.show_info = !self.show_info;
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("📂 Open…").on_hover_text("Open a file (⌘/Ctrl+O)").clicked() {
                self.open_dialog();
            }
            if self.stack.len() > 1 && ui.button("⬅ Back").on_hover_text("Backspace").clicked() {
                self.back();
            }
            let crumbs = self.stack.iter().map(|p| p.src.name()).collect::<Vec<_>>().join("  ›  ");
            ui.label(RichText::new(crumbs).strong());

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.show_info, "ℹ Info").on_hover_text("I");
                if let Some(page) = self.stack.last() {
                    let current = page.loader;
                    let candidates = registry().candidates(&page.src);
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt("view_as")
                        .selected_text(registry().get(current).map_or(current, |l| l.name()))
                        .show_ui(ui, |ui| {
                            for (_, l) in &candidates {
                                if ui.selectable_label(l.id() == current, l.name()).clicked() {
                                    chosen = Some(l.id());
                                }
                            }
                            // Text is always worth offering, even for files that don't sniff as text.
                            if !candidates.iter().any(|(_, l)| l.id() == "text")
                                && ui.selectable_label(current == "text", "Text").clicked()
                            {
                                chosen = Some("text");
                            }
                        });
                    ui.label("View as");
                    if let Some(id) = chosen.filter(|id| *id != current) {
                        self.view_as(id);
                    }
                }
            });
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                ui.spinner();
                ui.label(format!("Loading {}…", job.name));
            } else if let Some(page) = self.stack.last() {
                ui.label(&page.doc.format);
                ui.separator();
                ui.label(human_bytes(page.src.len()));
                ui.separator();
                ui.label(format!("{} view", page.doc.content.model_name()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("decoded in {:.1} ms", page.load_time.as_secs_f64() * 1000.0));
                });
            } else {
                ui.weak("No file open");
            }
        });
    }

    fn info_panel(&self, ui: &mut egui::Ui) {
        let Some(page) = self.stack.last() else {
            ui.weak("No file open");
            return;
        };
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("info").num_columns(2).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.weak(k);
                    ui.add(egui::Label::new(v).wrap());
                    ui.end_row();
                };
                row("Name", page.src.name().into());
                if let Some(p) = page.src.path() {
                    row("Path", p.display().to_string());
                }
                row("Size", format!("{} ({} bytes)", human_bytes(page.src.len()), page.src.len()));
                row("Format", page.doc.format.clone());
                row("Loader", page.loader.into());
                for (k, v) in &page.doc.metadata {
                    row(k, v.clone());
                }
            });
            if !page.attempts.is_empty() {
                ui.add_space(12.0);
                ui.strong("Loaders that failed first");
                for (id, err) in &page.attempts {
                    ui.label(format!("{id}: {err}"));
                }
            }
        });
    }

    fn central(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        if let Some(err) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, format!("Couldn't open: {err}"));
            ui.add_space(8.0);
        }
        match self.stack.last_mut() {
            Some(page) => views::show(ui, page, &mut self.md_cache),
            None => {
                if self.job.is_none() {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("Drop a file here, or press ⌘/Ctrl+O").size(18.0).weak());
                    });
                }
                None
            }
        }
    }

    /// --benchmark / --screenshot: act once the file has actually been drawn.
    fn automation(&mut self, ctx: &egui::Context) {
        if !self.benchmark && self.screenshot.is_none() {
            return;
        }
        let shown = (!self.stack.is_empty() || self.error.is_some()) && self.job.is_none();
        if !shown {
            return;
        }
        self.content_frames += 1;
        if self.content_frames == 1 {
            startup::mark("first frame showing the file");
            if let Some(err) = &self.error {
                eprintln!("viewer: {err}");
            }
        }
        // Give textures a couple of frames to upload before capturing or closing.
        if self.content_frames < 3 {
            ctx.request_repaint();
            return;
        }
        match self.screenshot.take() {
            None => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Some(path) => {
                let closer = ctx.clone();
                let done = egui::ScreenshotCallback::new(move |img| {
                    save_screenshot(&path, &img);
                    closer.send_viewport_cmd(egui::ViewportCommand::Close);
                    closer.request_repaint();
                });
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(done));
            }
        }
        ctx.request_repaint();
    }
}

fn save_screenshot(path: &std::path::Path, img: &egui::ColorImage) {
    let [w, h] = img.size;
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    crate::save_png(path, w as u32, h as u32, &rgba);
}

impl eframe::App for ViewerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.first_frame_done {
            startup::mark("first frame");
            self.first_frame_done = true;
        }
        self.poll_job(ctx);
        self.handle_input(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.show_info {
            egui::Panel::right("info").default_size(280.0).show(ui, |ui| self.info_panel(ui));
        }
        let action = egui::CentralPanel::default().show(ui, |ui| self.central(ui)).inner;
        if let Some(Action::OpenEntry(container, index)) = action {
            self.open_entry(container, index);
        }

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop")));
            let rect = ctx.content_rect();
            painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(160));
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Drop to open", egui::FontId::proportional(24.0), egui::Color32::WHITE);
        }

        self.automation(&ctx);
    }
}
