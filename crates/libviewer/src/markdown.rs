//! Markdown, formatted (libgui's Markdown widget), from a `Content::Markdown`'s lines.

use libfileparser_core::TextSource;
use libgui::*;

/// A Markdown document parsed once for its view.
pub struct MarkdownView {
    md: libgui_markdown::Markdown,
}

/// More than this is shown as its first part, with a note: the widget lays out the
/// whole document every frame.
const MAX_LINES: u64 = 50_000;

impl MarkdownView {
    pub fn new(src: &dyn TextSource) -> Self {
        let total = src.info().lines.known();
        let lines = src.lines(0..total.min(MAX_LINES)).unwrap_or_default();
        let mut text = String::new();
        for l in &lines {
            text.push_str(&String::from_utf8_lossy(l));
            text.push('\n');
        }
        if total > MAX_LINES {
            text.push_str(&format!("\n---\n\n*Shown up to line {MAX_LINES} of {total}; the Source view has it all.*\n"));
        }
        Self { md: libgui_markdown::Markdown::parse(&text) }
    }
}

/// Draws Markdown formatted, in a scroll.
pub fn markdown(ui: &mut Ui, v: &mut MarkdownView) {
    ui.scroll_area("markdown", |ui| {
        ui.container(Layout::column().height(Size::Fit).padding(Insets::xy(24.0, 16.0)), Frame::none(), |ui| {
            v.md.show(ui, "md");
        });
    });
}
