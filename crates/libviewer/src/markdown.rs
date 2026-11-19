//! Formatted text (`Content::Flow`), drawn with libgui's Markdown widget: the blocks
//! are written out as Markdown, which the widget lays out (headings, lists, tables,
//! code, quotes, emphasis, links).
//!
//! **Images.** An image that is a paragraph of its own is drawn: its file comes from
//! the flow ([`FlowSource::resource`]), is opened by the host's
//! [`Host::image_opener`], decoded and scaled on a thread of the view's own (to
//! [`MAX_SIDE`] at most, band by band, so a huge photo never sits in memory whole),
//! uploaded a few per frame, and drawn at its size or the column's width. Until then,
//! and when it can't be read, its alt text shows; so does an image inside a line of
//! text.
//!
//! Bold, italic and bold italic are drawn in the `Ui`'s faces for those [`FontRole`]s,
//! which the app registers; one not registered falls back to the body face, and the
//! marks don't show. Viewer registers Inter's the first time formatted text is drawn.
//! Underline, superscript and subscript have no Markdown, so they show as plain text.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

use libfileparser_core::{Block, FlowSource, Inline, Pixels, Raster};
use libgui::*;

use crate::Host;

/// An image's longest side once decoded for the page, in pixels: sharp at twice a
/// wide column's width, and at most 16 MB of texture.
pub const MAX_SIDE: u32 = 2048;
/// Images a document shows; past these, their alt text.
const MAX_IMAGES: usize = 1000;
/// Decoded image bytes uploaded per frame, so a page of photos stays smooth.
const UPLOAD_BUDGET: usize = 32 << 20;

/// An image decoded for the page: its URL in the Markdown, its size and pixels.
type Decoded = (String, u32, u32, Vec<u8>);

/// A document's formatted text, prepared once for its view, and its images.
pub struct FlowView {
    md: libgui_markdown::Markdown,
    source: Arc<dyn FlowSource>,
    /// Each image's URL in the Markdown, and the flow's reference it stands for.
    refs: Vec<(String, String)>,
    /// Images on the GPU: their texture and size in pixels.
    textures: HashMap<String, (TextureId, Vec2)>,
    /// Decoded images from the thread, once it's started; `None` when it's done.
    stream: Option<Receiver<Decoded>>,
    started: bool,
}

/// More blocks than this show as the first part, with a note: the widget lays out the
/// whole document every frame.
const MAX_BLOCKS: u64 = 20_000;

impl FlowView {
    pub fn new(src: Arc<dyn FlowSource>) -> Self {
        let total = src.block_count().known();
        let (blocks, error) = match src.blocks(0..total.min(MAX_BLOCKS)) {
            Ok(b) => (b, None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
        let mut text = String::new();
        let mut refs = Vec::new();
        for b in &blocks {
            block(b, "", &mut text, &mut refs);
        }
        if let Some(e) = error {
            text.push_str(&format!("*This document couldn't be read: {e}*\n"));
        } else if total > MAX_BLOCKS {
            text.push_str(&format!("\n---\n\n*Shown up to block {MAX_BLOCKS} of {total}.*\n"));
        }
        refs.truncate(MAX_IMAGES);
        Self { md: libgui_markdown::Markdown::parse(&text), source: src, refs, textures: HashMap::new(), stream: None, started: false }
    }

    /// GPU textures this view owns, to free when its page closes.
    pub fn textures(&self) -> Vec<TextureId> {
        self.textures.values().map(|(t, _)| *t).collect()
    }

    /// Starts decoding the images, once, and uploads what's decoded within the budget.
    fn load_images(&mut self, host: &mut dyn Host) {
        if !self.started {
            self.started = true;
            let Some(open) = host.image_opener() else { return };
            if self.refs.is_empty() {
                return;
            }
            let (tx, rx) = sync_channel::<Decoded>(2);
            let (source, refs, wake) = (self.source.clone(), self.refs.clone(), host.waker());
            let spawned = std::thread::Builder::new().name("document images".into()).spawn(move || {
                for (url, reference) in refs {
                    let decoded = source.resource(&reference).ok().and_then(|file| open(&file)).and_then(|r| scaled(&r, MAX_SIDE));
                    if let Some((w, h, rgba)) = decoded {
                        // The view gone (its page closed): stop.
                        if tx.send((url, w, h, rgba)).is_err() {
                            return;
                        }
                        wake();
                    }
                }
                wake();
            });
            if spawned.is_ok() {
                self.stream = Some(rx);
            }
        }
        let Some(stream) = &self.stream else { return };
        let mut spent = 0;
        while spent < UPLOAD_BUDGET {
            // A screenshot waits for every image; a window takes what's ready.
            let next = if host.settles() { stream.recv().map_err(|_| TryRecvError::Disconnected) } else { stream.try_recv() };
            match next {
                Ok((url, w, h, rgba)) => {
                    spent += rgba.len();
                    let id = host.upload(w, h, &rgba);
                    self.textures.insert(url, (id, Vec2::new(w as f32, h as f32)));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.stream = None;
                    break;
                }
            }
        }
    }
}

/// `r` scaled down to `max` pixels on its longest side (never up), averaging the
/// pixels each one covers; band by band when it decodes in bands, so only the result
/// is held whole.
fn scaled(r: &Raster, max: u32) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h) = (r.width(), r.height());
    if w == 0 || h == 0 {
        return None;
    }
    let k = (max as f64 / w.max(h) as f64).min(1.0);
    let (ow, oh) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
    if (ow, oh) == (w, h) {
        return r.decode().ok().map(|rgba| (w, h, rgba));
    }
    let mut sum = vec![0u32; (ow * oh * 4) as usize];
    let mut count = vec![0u32; (ow * oh) as usize];
    let col: Vec<u32> = (0..w).map(|x| (x as u64 * ow as u64 / w as u64) as u32).collect();
    let mut add = |y: u32, row: &[u8]| {
        let oy = (y as u64 * oh as u64 / h as u64) as u32;
        for x in 0..w as usize {
            let o = (oy * ow + col[x]) as usize;
            for c in 0..4 {
                sum[o * 4 + c] += row[x * 4 + c] as u32;
            }
            count[o] += 1;
        }
    };
    let stride = w as usize * 4;
    match r.pixels() {
        Pixels::Full(rgba) => rgba.chunks_exact(stride).enumerate().for_each(|(y, row)| add(y as u32, row)),
        Pixels::Bands(source) => {
            for band in source.bands().ok()? {
                let band = band.ok()?;
                band.rgba.chunks_exact(stride).enumerate().for_each(|(k, row)| add(band.y + k as u32, row));
            }
        }
        _ => r.decode().ok()?.chunks_exact(stride).enumerate().for_each(|(y, row)| add(y as u32, row)),
    }
    let rgba = sum.as_chunks::<4>().0.iter().zip(&count).flat_map(|(s, &n)| s.iter().map(move |v| (v / n.max(1)) as u8)).collect();
    Some((ow, oh, rgba))
}

/// Padding around the text, in points.
const PADDING: Insets = Insets::xy(24.0, 16.0);

/// Draws formatted text, in a scroll, with its images as they arrive.
pub fn flow(ui: &mut Ui, v: &mut FlowView, host: &mut dyn Host) {
    v.load_images(host);
    // The column's width, from the last frame's layout (the window's, at first): an
    // image wider than it is drawn at its width.
    let id = ui.make_id("flow-column");
    let column = ui.rect_of(id).map_or(ui.screen_size().x, |r| r.w) - PADDING.left - PADDING.right;
    let ppp = host.pixels_per_point().max(1.0);
    let textures = &v.textures;
    let image = |url: &str| -> Option<(TextureId, Vec2)> {
        let &(tex, px) = textures.get(url)?;
        // A pixel a point on a standard screen; on a dense one, sharp rather than tiny,
        // for images decoded at a denser size than the page (photos), as browsers do.
        let natural = if px.x > MAX_SIDE as f32 / 2.0 { px * (1.0 / ppp) } else { px };
        let k = (column.max(16.0) / natural.x).min(1.0);
        Some((tex, natural * k))
    };
    let opts = libgui_markdown::MarkdownOptions { image: Some(&image), ..Default::default() };
    ui.scroll_area("flow", |ui| {
        ui.container_id(id, Layout::column().height(Size::Fit).padding(PADDING), Frame::none(), |ui| {
            v.md.show_with(ui, "flow", &opts);
        });
    });
}

/// An image `reference` as a Markdown URL, kept in `refs` (once) to be loaded.
fn image_url(reference: &str, refs: &mut Vec<(String, String)>) -> String {
    let url = reference.replace('%', "%25").replace(' ', "%20").replace('(', "%28").replace(')', "%29").replace('<', "%3C").replace('>', "%3E");
    if !refs.iter().any(|(u, _)| *u == url) {
        refs.push((url.clone(), reference.to_string()));
    }
    url
}

/// `b` as Markdown, each line after the first starting with `indent`; the images it
/// names into `refs`.
fn block(b: &Block, indent: &str, out: &mut String, refs: &mut Vec<(String, String)>) {
    let line = |out: &mut String, s: &str| {
        out.push_str(indent);
        out.push_str(s);
        out.push('\n');
    };
    match b {
        Block::Heading { level, text } => line(out, &format!("{} {}", "#".repeat(*level as usize), inlines(text, refs))),
        Block::Paragraph(text) => line(out, &inlines(text, refs)),
        Block::List { ordered, start, items } => {
            for (n, item) in items.iter().enumerate() {
                let marker = if *ordered { format!("{}. ", start + n as u64) } else { "- ".to_string() };
                let inner = format!("{indent}{}", " ".repeat(marker.len()));
                let mut body = String::new();
                for (k, ib) in item.iter().enumerate() {
                    block(ib, if k == 0 { "" } else { &inner }, &mut body, refs);
                }
                out.push_str(indent);
                out.push_str(&marker);
                out.push_str(body.trim());
                out.push('\n');
            }
        }
        Block::Quote(bs) => {
            let mut inner = String::new();
            for q in bs {
                block(q, "", &mut inner, refs);
            }
            for l in inner.lines() {
                line(out, &format!("> {l}"));
            }
        }
        Block::Code { language, text } => {
            line(out, &format!("```{}", language.as_deref().unwrap_or("")));
            for l in text.lines() {
                line(out, l);
            }
            line(out, "```");
        }
        Block::Table { header_rows, rows } => {
            let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
            let mut row = |cells: &[Vec<Inline>]| {
                let mut r = String::from("|");
                for c in 0..cols {
                    r.push(' ');
                    r.push_str(&cells.get(c).map(|x| cell(&inlines(x, refs))).unwrap_or_default());
                    r.push_str(" |");
                }
                r
            };
            // Markdown tables need a header row: the first row, or an empty one.
            let body = if *header_rows > 0 {
                line(out, &row(&rows[0]));
                &rows[1..]
            } else {
                line(out, &row(&[]));
                &rows[..]
            };
            line(out, &format!("|{}", " --- |".repeat(cols)));
            for r in body {
                line(out, &row(r));
            }
        }
        Block::Image { reference, alt } => {
            let url = image_url(reference, refs);
            line(out, &format!("![{}]({url})", escape(alt)));
        }
        Block::Rule | Block::Break => line(out, "---"),
        Block::Raw { text, .. } => {
            line(out, "```");
            for l in text.lines() {
                line(out, l);
            }
            line(out, "```");
        }
        _ => {}
    }
    out.push('\n');
}

/// Inline Markdown made safe for a table cell: on one line, every `|` escaped once
/// (text's are already; code's and links' aren't).
fn cell(md: &str) -> String {
    let mut s = String::with_capacity(md.len());
    let mut escaped = false;
    for c in md.chars() {
        match c {
            '\n' => s.push(' '),
            '|' if !escaped => s.push_str("\\|"),
            _ => s.push(c),
        }
        escaped = c == '\\' && !escaped;
    }
    s
}

fn inlines(i: &[Inline], refs: &mut Vec<(String, String)>) -> String {
    let mut s = String::new();
    for x in merged(i) {
        match &x {
            Inline::Text(t) => s.push_str(&escape(t)),
            Inline::Emphasis(c) => s.push_str(&marked("*", &inlines(c, refs))),
            Inline::Strong(c) => s.push_str(&marked("**", &inlines(c, refs))),
            Inline::Strikethrough(c) => s.push_str(&marked("~~", &inlines(c, refs))),
            // No Markdown for these: the text alone.
            Inline::Underline(c) | Inline::Superscript(c) | Inline::Subscript(c) => s.push_str(&inlines(c, refs)),
            Inline::Code(c) => s.push_str(&format!("`{}`", c.replace('`', "'"))),
            Inline::Link { target, text } => s.push_str(&format!("[{}]({})", inlines(text, refs), target.replace(' ', "%20").replace(')', "%29"))),
            Inline::Image { reference, alt } => {
                let url = image_url(reference, refs);
                s.push_str(&format!("![{}]({url})", escape(alt)));
            }
            Inline::LineBreak => s.push_str("  \n"),
            Inline::Note(l) => s.push_str(&format!("[{}]", escape(l))),
            _ => {}
        }
    }
    s
}

/// Neighbouring runs of the same mark as one: word processors split a bold phrase
/// into runs wherever its formatting was touched, and `**a****b**` isn't bold in
/// Markdown.
fn merged(i: &[Inline]) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::with_capacity(i.len());
    for x in i {
        match (out.last_mut(), x) {
            (Some(Inline::Strong(a)), Inline::Strong(b)) | (Some(Inline::Emphasis(a)), Inline::Emphasis(b)) | (Some(Inline::Strikethrough(a)), Inline::Strikethrough(b)) => a.extend(b.iter().cloned()),
            _ => out.push(x.clone()),
        }
    }
    out
}

/// `inner` between `mark`s, its edge spaces outside them: Markdown doesn't close
/// `**bold **`, so a run ending in a space would show its asterisks.
fn marked(mark: &str, inner: &str) -> String {
    let body = inner.trim();
    if body.is_empty() {
        return inner.to_string();
    }
    let lead = &inner[..inner.len() - inner.trim_start().len()];
    let trail = &inner[inner.trim_end().len()..];
    format!("{lead}{mark}{body}{mark}{trail}")
}

/// Text with Markdown's own characters escaped, so a document's `*` stays a `*`.
fn escape(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    for c in t.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '#' | '<' | '>' | '|' | '~') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_become_markdown() {
        let t = |s: &str| vec![Inline::Text(s.into())];
        let mut out = String::new();
        for b in [
            Block::Heading { level: 2, text: t("Hi *there*") },
            Block::List { ordered: true, start: 3, items: vec![vec![Block::Paragraph(t("a"))], vec![Block::Paragraph(t("b"))]] },
            Block::Table { header_rows: 1, rows: vec![vec![t("x"), t("y")], vec![t("1"), t("2|3")]] },
        ] {
            block(&b, "", &mut out, &mut Vec::new());
        }
        assert!(out.contains("## Hi \\*there\\*"), "{out}");
        assert!(out.contains("3. a\n4. b"), "{out}");
        assert!(out.contains("| x | y |\n| --- | --- |\n| 1 | 2\\|3 |"), "{out}");
    }

    /// Word splits a bold phrase into runs anywhere: they read as one, with no
    /// asterisks left showing.
    #[test]
    fn split_runs_stay_marked() {
        let t = |s: &str| Inline::Text(s.into());
        let md = inlines(&[Inline::Strong(vec![t("12.04")]), Inline::Strong(vec![t("-13.04")]), t(" and "), Inline::Strong(vec![t("water ")]), t("partners")], &mut Vec::new());
        assert_eq!(md, "**12.04-13.04** and **water** partners");
        let doc = libgui_markdown::Markdown::parse(&md);
        let _ = doc;
        assert_eq!(marked("*", "  "), "  ");
    }
}
