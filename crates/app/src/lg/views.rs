//! Renderers, one per content model, drawn with libgui. They never know which file
//! format they show.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use libgui::*;
use viewer_core::{Band, Container, Content, Next, Pixels, Raster, SceneDoc, Table, TextDoc, TreeNode};

use super::{SceneGpu, Textures};
use super::app::Page;

/// Longer lines are cut for display (minified files would otherwise stall layout).
const MAX_LINE_BYTES: usize = 8 * 1024;
const MONO_SIZE: f32 = 13.0;
const LINE_HEIGHT: f32 = 18.0;

/// Something a view asks the app to do.
pub enum Action {
    OpenEntry(Arc<dyn Container>, usize),
}

/// Per-page renderer state that libgui leaves to us.
pub enum ViewState {
    None,
    Raster(RasterState),
    Markdown(libgui_markdown::Markdown),
    Tree(TreeView),
    Table(TableState),
    Scene(Box<SceneView>),
    Pages(Box<super::pages::PagesView>),
}

impl ViewState {
    pub fn new(content: &Content) -> Self {
        match content {
            Content::Raster(_) => Self::Raster(RasterState::default()),
            Content::Markdown(md) => Self::Markdown(libgui_markdown::Markdown::parse(md)),
            Content::Tree(_) => Self::Tree(TreeView::default()),
            Content::Table(t) => Self::Table(TableState::new(
                // Rows can be wider than the header (flexible CSV); give those cells a column too.
                (0..t.columns).map(|c| Column::new(t.headers.get(c).map_or("", String::as_str)).width(160.0).unsortable()),
            )),
            Content::Container(c) => {
                let mut columns = vec![
                    Column::new("Name").width(320.0).grow(1.0).unsortable(),
                    Column::new("Size").width(110.0).align(Align::End).unsortable(),
                ];
                // Only archives pack their entries; a folder's column would stay empty.
                if c.entries().iter().any(|e| e.compressed_size.is_some()) {
                    columns.push(Column::new("Packed").width(110.0).align(Align::End).unsortable());
                }
                Self::Table(TableState::new(columns))
            }
            Content::Scene(_) => Self::Scene(Box::default()),
            Content::Pages(_) => Self::Pages(Box::default()),
            _ => Self::None,
        }
    }

    /// GPU textures this page owns, to free when it is closed.
    pub fn textures(&self) -> Vec<TextureId> {
        match self {
            Self::Raster(st) => st.tiles.iter().map(|(t, _)| *t).collect(),
            Self::Scene(st) => st.gpu.texture().into_iter().collect(),
            Self::Pages(st) => st.textures(),
            _ => Vec::new(),
        }
    }
}

pub fn show(ui: &mut Ui, page: &mut Page, tex: &mut dyn Textures, dark: bool) -> Option<Action> {
    // State (scroll offsets, expanded nodes) is keyed by page, so going back restores it.
    ui.with_key(page.id, |ui| match (&mut page.doc.content, &mut page.view) {
        (Content::Raster(r), ViewState::Raster(st)) => {
            raster(ui, r, st, tex);
            None
        }
        (Content::Text(t), _) => {
            text(ui, t, dark);
            None
        }
        (Content::Markdown(_), ViewState::Markdown(md)) => {
            ui.scroll_area("markdown", |ui| {
                ui.container(Layout::column().height(Size::Fit).padding(Insets::xy(24.0, 16.0)), Frame::none(), |ui| {
                    md.show(ui, "md");
                });
            });
            None
        }
        (Content::Tree(root), ViewState::Tree(tv)) => {
            tree(ui, root, tv);
            None
        }
        (Content::Table(t), ViewState::Table(st)) => {
            table(ui, t, st);
            None
        }
        (Content::Container(c), ViewState::Table(st)) => container(ui, c.as_ref(), st).map(|i| Action::OpenEntry(c.clone(), i)),
        (Content::Hex(src), _) => {
            hex(ui, src.bytes());
            None
        }
        (Content::Scene(doc), ViewState::Scene(st)) => {
            scene(ui, doc, st, tex);
            None
        }
        (Content::Pages(src), ViewState::Pages(st)) => {
            super::pages::show(ui, src, st, tex);
            None
        }
        _ => unreachable!("every page gets the view state its content needs"),
    })
}

fn rgb([r, g, b]: [u8; 3]) -> Color {
    Color::rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
}

/// One row of monospaced text in a virtual list: a gutter, then `text` in `spans`.
fn mono_row(ui: &mut Ui, i: usize, gutter: &str, text: &str, spans: Vec<TextSpan>, tail: Option<&str>) {
    let mono = ui.font_for(FontRole::Mono);
    let pal = ui.theme.palette;
    let gutter = ui.frame_text(gutter);
    let text = ui.frame_text(text);
    let tail = tail.map(|t| ui.frame_text(t));
    let id = ui.make_id(("row", i));
    ui.add_leaf(id, Layout::leaf(Size::Grow(1.0), Size::Fixed(LINE_HEIGHT)), Vec2::ZERO, false, move |p, r| {
        // The painter is shared by every paint closure, so put the font back after.
        let body = std::mem::replace(&mut p.font, mono);
        let em = p.measure(MONO_SIZE, "0");
        let y = r.y + ((r.h - em.y) / 2.0).round();
        let gw = p.measure(MONO_SIZE, gutter).x + em.x * 2.0;
        p.text(Vec2::new(r.x, y), MONO_SIZE, pal.text_faint, gutter);
        p.text_spans(Vec2::new(r.x + gw, y), MONO_SIZE, pal.text, text, &spans);
        if let Some(tail) = tail {
            let x = r.x + gw + p.measure(MONO_SIZE, text).x;
            p.text(Vec2::new(x, y), MONO_SIZE, pal.text_faint, tail);
        }
        p.font = body;
    });
}

/// Virtualised text view: only visible lines are shaped, so huge files scroll freely.
fn text(ui: &mut Ui, doc: &TextDoc, dark: bool) {
    let digits = doc.line_count().to_string().len();
    let theme = if dark { 0 } else { 1 };
    let opts = ListOptions { padding: Insets::xy(8.0, 4.0), ..ListOptions::new(LINE_HEIGHT) };
    ui.virtual_list_with("text", doc.line_count(), opts, |ui, i| {
        let raw = doc.raw_line(i);
        let full = String::from_utf8_lossy(&raw[..raw.len().min(MAX_LINE_BYTES)]);
        let line = full.trim_end_matches(['\n', '\r']);
        let mut spans = Vec::new();
        if let Some(h) = doc.highlights.as_ref().filter(|_| raw.len() <= MAX_LINE_BYTES) {
            for s in &h[theme][i] {
                let end = s.range.end.min(line.len());
                if s.range.start < end {
                    spans.push(TextSpan::new(s.range.start..end, rgb(s.color)));
                }
            }
        }
        let tail = (raw.len() > MAX_LINE_BYTES).then(|| format!("  … ({} more bytes)", raw.len() - MAX_LINE_BYTES));
        mono_row(ui, i, &format!("{:>digits$}", i + 1), line, spans, tail.as_deref());
    });
}

fn hex(ui: &mut Ui, bytes: &[u8]) {
    const PER_ROW: usize = 16;
    let rows = bytes.len().div_ceil(PER_ROW);
    let opts = ListOptions { padding: Insets::xy(8.0, 4.0), ..ListOptions::new(LINE_HEIGHT) };
    let faint = ui.theme.palette.text_muted;
    ui.virtual_list_with("hex", rows, opts, |ui, i| {
        let chunk = &bytes[i * PER_ROW..((i + 1) * PER_ROW).min(bytes.len())];
        let mut line = String::with_capacity(80);
        for k in 0..PER_ROW {
            match chunk.get(k) {
                Some(b) => line.push_str(&format!("{b:02x} ")),
                None => line.push_str("   "),
            }
            if k == 7 {
                line.push(' ');
            }
        }
        let ascii_start = line.len() + 1;
        line.push(' ');
        line.extend(chunk.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' }));
        let spans = vec![TextSpan::new(ascii_start..line.len(), faint)];
        mono_row(ui, i, &format!("{:08x}", i * PER_ROW), &line, spans, None);
    });
}

fn table(ui: &mut Ui, t: &Table, st: &mut TableState) {
    ui.table("table", st, t.rows.len(), |ui, row, col| {
        ui.label(t.rows[row].get(col).map_or("", String::as_str));
    });
}

fn container(ui: &mut Ui, c: &dyn Container, st: &mut TableState) -> Option<usize> {
    let entries = c.entries();
    let r = ui.table("entries", st, entries.len(), |ui, row, col| {
        let e = &entries[row];
        match col {
            0 if e.is_dir => ui.label_muted(&format!("{}/", e.path.trim_end_matches('/'))),
            0 => ui.label(&e.path),
            1 if !e.is_dir => ui.label(&crate::util::human_bytes(e.size)),
            2 => {
                if let Some(z) = e.compressed_size.filter(|_| !e.is_dir) {
                    ui.label_muted(&crate::util::human_bytes(z));
                }
            }
            _ => {}
        }
    });
    r.clicked_row.filter(|&i| !entries[i].is_dir)
}

// --- Tree ---------------------------------------------------------------------

/// libgui's tree asks for nodes by a `Copy` key and never stores the shape, so we
/// hand out a small integer per node the first time its parent is expanded.
pub struct TreeView {
    state: TreeState<u32>,
    arena: RefCell<Arena>,
}

impl Default for TreeView {
    fn default() -> Self {
        Self { state: TreeState::new(), arena: RefCell::new(Arena { paths: vec![Vec::new()], index: HashMap::new() }) }
    }
}

/// Key `k` is the node at child-index path `paths[k]` from the root; key 0 is the root.
struct Arena {
    paths: Vec<Vec<u32>>,
    index: HashMap<(u32, u32), u32>,
}

struct Source<'a> {
    root: &'a TreeNode,
    arena: &'a RefCell<Arena>,
}

impl<'a> Source<'a> {
    fn node(&self, key: u32) -> &'a TreeNode {
        let arena = self.arena.borrow();
        arena.paths[key as usize].iter().fold(self.root, |n, &c| &n.children[c as usize])
    }

    fn push_children(&self, parent: u32, out: &mut Vec<u32>) {
        let count = self.node(parent).children.len() as u32;
        let mut arena = self.arena.borrow_mut();
        for i in 0..count {
            let next = arena.paths.len() as u32;
            let key = *arena.index.entry((parent, i)).or_insert(next);
            if key == next {
                let mut path = arena.paths[parent as usize].clone();
                path.push(i);
                arena.paths.push(path);
            }
            out.push(key);
        }
    }
}

impl TreeSource for Source<'_> {
    type Key = u32;

    fn roots(&self, out: &mut Vec<u32>) {
        self.push_children(0, out);
    }

    fn children(&self, node: u32, out: &mut Vec<u32>) {
        self.push_children(node, out);
    }

    fn has_children(&self, node: u32) -> bool {
        !self.node(node).children.is_empty()
    }

    fn label(&self, node: u32) -> Cow<'_, str> {
        let n = self.node(node);
        match (n.key.is_empty(), n.value.is_empty()) {
            (_, true) => Cow::Borrowed(&n.key),
            (true, false) => Cow::Borrowed(&n.value),
            (false, false) => Cow::Owned(format!("{}: {}", n.key, n.value)),
        }
    }
}

fn tree(ui: &mut Ui, root: &TreeNode, tv: &mut TreeView) {
    let src = Source { root, arena: &tv.arena };
    ui.container(Layout::column().padding(Insets::xy(8.0, 4.0)), Frame::none(), |ui| {
        ui.tree_view("tree", &mut tv.state, &src);
    });
}

// --- Raster -------------------------------------------------------------------

/// Images go to the GPU in tiles of this side, a few per frame (`UPLOAD_BUDGET`):
/// staging memory stays small, the first tiles show sooner, and once the last is up
/// the decoded copy is dropped.
const TILE: u32 = 2048;
const UPLOAD_BUDGET: usize = 48 << 20;

#[derive(Default)]
pub struct RasterState {
    tiles: Vec<(TextureId, Rect)>,
    /// Tiles uploaded so far, in row-major order (whole images).
    uploaded: usize,
    /// Rows on the GPU so far (streamed images), and why the stream stopped early.
    rows: u32,
    error: Option<String>,
    /// The stream wakes the window when a band arrives.
    wakes: bool,
    /// `None` means "fit to window".
    zoom: Option<f32>,
    offset: Vec2,
}

/// Uploads the next tiles of a whole image within the budget. Returns true when all
/// are on the GPU.
fn upload(tex: &mut dyn Textures, width: u32, height: u32, rgba: &[u8], st: &mut RasterState) -> bool {
    let tile = TILE.min(tex.max_side().max(256));
    let (cols, rows) = (width.div_ceil(tile), height.div_ceil(tile));
    let total = (cols * rows) as usize;
    let mut spent = 0;
    while st.uploaded < total && (spent == 0 || spent < UPLOAD_BUDGET) {
        let i = st.uploaded as u32;
        let (tx, ty) = ((i % cols) * tile, (i / cols) * tile);
        let (w, h) = (tile.min(width - tx), tile.min(height - ty));
        let mut buf = Vec::with_capacity((w * h * 4) as usize);
        for y in ty..ty + h {
            let row = ((y * width + tx) * 4) as usize;
            buf.extend_from_slice(&rgba[row..row + (w * 4) as usize]);
        }
        let id = tex.upload(w, h, &buf);
        st.tiles.push((id, Rect::new(tx as f32, ty as f32, w as f32, h as f32)));
        st.uploaded += 1;
        spent += buf.len();
    }
    st.uploaded == total
}

/// Uploads one streamed band as a row of tiles, `TILE` wide and the band's height.
fn upload_band(tex: &mut dyn Textures, width: u32, band: &Band, st: &mut RasterState) {
    let tile = TILE.min(tex.max_side().max(256));
    let rows = (band.rgba.len() / (width as usize * 4).max(1)) as u32;
    for tx in (0..width).step_by(tile as usize) {
        let w = tile.min(width - tx);
        let mut buf = Vec::with_capacity((w * rows * 4) as usize);
        for y in 0..rows {
            let row = ((y * width + tx) * 4) as usize;
            buf.extend_from_slice(&band.rgba[row..row + (w * 4) as usize]);
        }
        let id = tex.upload(w, rows, &buf);
        st.tiles.push((id, Rect::new(tx as f32, band.y as f32, w as f32, rows as f32)));
    }
    st.rows = st.rows.max(band.y + rows);
}

/// Moves pixels to the GPU: a whole image a budget's worth of tiles per frame, a
/// streamed one band by band as the decoder delivers them. Either way the CPU copy
/// is dropped once everything is uploaded.
fn feed(ui: &mut Ui, r: &mut Raster, st: &mut RasterState, tex: &mut dyn Textures) {
    let (width, height) = (r.width, r.height);
    let release = match &r.pixels {
        Pixels::Full(rgba) if width > 0 && height > 0 => {
            let done = upload(tex, width, height, rgba, st);
            if !done {
                ui.request_repaint();
            }
            done
        }
        Pixels::Bands(stream) => {
            if !std::mem::replace(&mut st.wakes, true) {
                stream.set_wake(|| {
                    if let Some(wake) = crate::loading::WAKE.get() {
                        wake();
                    }
                });
            }
            // Screenshots wait for the whole image; windows take what has arrived.
            let block = tex.settles();
            let mut spent = 0;
            loop {
                match if block { stream.wait_next() } else { stream.try_next() } {
                    Next::Band(band) => {
                        upload_band(tex, width, &band, st);
                        spent += band.rgba.len();
                        if !block && spent >= UPLOAD_BUDGET {
                            ui.request_repaint();
                            break false;
                        }
                    }
                    // The next band wakes the window.
                    Next::Pending => break false,
                    Next::Done => break true,
                    Next::Failed(e) => {
                        st.error = Some(e);
                        break true;
                    }
                }
            }
        }
        _ => false,
    };
    if release {
        // Everything is on the GPU (or the stream ended): no CPU copy is kept.
        r.pixels = Pixels::Released;
        tex.uploads_done();
    }
}

fn raster(ui: &mut Ui, r: &mut Raster, st: &mut RasterState, tex: &mut dyn Textures) {
    feed(ui, r, st, tex);
    let progress = match (&r.pixels, &st.error) {
        (_, Some(e)) => Some(format!("  ·  stopped at row {} of {}: {e}", st.rows, r.height)),
        (Pixels::Bands(_), None) => Some(format!("  ·  decoding {}%", st.rows as u64 * 100 / r.height.max(1) as u64)),
        _ => None,
    };
    let id = ui.make_id("raster");
    let resp = ui.interact_drag(id);
    let rect = resp.rect; // last frame's: one frame of latency, as libgui documents
    let size = Vec2::new(r.width as f32, r.height as f32);
    let fit = move |rect: Rect| if rect.w > 0.0 { (rect.w / size.x).min(rect.h / size.y).min(1.0) } else { 1.0 };
    let zoom = st.zoom.unwrap_or(fit(rect));

    let mut new_zoom = zoom;
    if resp.hovered {
        new_zoom *= (resp.scroll.y / 300.0).exp();
        if ui.key_pressed(Key::Num0) {
            st.zoom = None;
            st.offset = Vec2::ZERO;
        }
        if ui.key_pressed(Key::Num1) {
            new_zoom = 1.0;
        }
        if ui.key_pressed(Key::Equal) {
            new_zoom *= 1.25;
        }
        if ui.key_pressed(Key::Minus) {
            new_zoom /= 1.25;
        }
    }
    if resp.double_clicked {
        if st.zoom.is_some() {
            st.zoom = None;
            st.offset = Vec2::ZERO;
        } else {
            new_zoom = 1.0;
        }
    }
    let new_zoom = new_zoom.clamp(0.01, 64.0);
    if new_zoom != zoom {
        // Keep the image point under the cursor fixed while zooming.
        let anchor = if resp.hovered { resp.mouse_pos } else { rect.center() };
        let image_pt = (anchor - rect.center() - st.offset) * (1.0 / zoom);
        st.offset = anchor - rect.center() - image_pt * new_zoom;
        st.zoom = Some(new_zoom);
    }
    if resp.active && resp.drag_delta != Vec2::ZERO {
        st.offset += resp.drag_delta;
        st.zoom.get_or_insert(zoom);
    }

    let (tiles, zoom, offset) = (st.tiles.clone(), st.zoom, st.offset);
    let (w, h) = (r.width, r.height);
    let bg = ui.theme.palette.bg_inset;
    let faint = ui.theme.palette.text_muted;
    let frame = Frame { fill: bg, clip: true, ..Frame::none() };
    ui.container(Layout::column(), frame, |ui| {
        ui.add_leaf(id, Layout::leaf(Size::Grow(1.0), Size::Grow(1.0)), Vec2::ZERO, true, move |p, rect| {
            let zoom = zoom.unwrap_or(fit(rect));
            let origin = rect.center() + offset - size * (zoom / 2.0);
            for (t, tile) in &tiles {
                let screen = Rect::new(origin.x + tile.x * zoom, origin.y + tile.y * zoom, tile.w * zoom, tile.h * zoom);
                if screen.intersect(&rect).is_some() {
                    p.image_with_alpha(screen, *t, [0.0, 0.0, 1.0, 1.0], 0.0, Color::WHITE, ImageAlpha::Straight);
                }
            }
            let label = format!("{:.0}%  ·  {w} × {h}{}", zoom * 100.0, progress.as_deref().unwrap_or(""));
            p.text(Vec2::new(rect.x + 8.0, rect.y + rect.h - 22.0), 12.0, faint, label);
        });
    });
}

// ---------------------------------------------------------------- 3D

#[derive(Default)]
pub struct SceneView {
    gpu: SceneGpu,
    controls: Option<rendererx::OrbitControls>,
    /// The user moved the camera, so don't reframe when loading finishes.
    moved: bool,
    /// Framed on the finished model's measured bounds (not the loader's early hint).
    framed_final: bool,
    /// The scene wakes the window when it changes (textures arriving).
    wakes: bool,
    /// Everything, textures included, reached the GPU.
    settled: bool,
}

/// A 3D model drawn by RendererX into a texture. Drag to orbit, Shift-drag (or
/// two-finger pan) to pan, scroll or pinch to zoom, double-click or 0 to reframe.
fn scene(ui: &mut Ui, doc: &SceneDoc, st: &mut SceneView, tex: &mut dyn Textures) {
    if !std::mem::replace(&mut st.wakes, true) {
        doc.scene.set_on_change(|| {
            if let Some(wake) = crate::loading::WAKE.get() {
                wake();
            }
        });
    }
    let pending = || doc.textures_pending.load(std::sync::atomic::Ordering::Acquire);
    if tex.settles() {
        // Screenshots show the finished model; decoding is bounded, but don't hang.
        let start = std::time::Instant::now();
        while pending() > 0 && start.elapsed().as_secs() < 60 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    let id = ui.make_id("scene");
    let resp = ui.interact_drag(id);
    let rect = resp.rect; // last frame's: one frame of latency, as for images
    let aspect = if rect.h > 0.0 { rect.w / rect.h } else { 1.0 };

    let (bounds, loading) = {
        let read = doc.scene.read();
        (read.bounds(), read.geometry() == rendererx::scene_data::GeometryState::Loading)
    };
    let reset = resp.double_clicked || (resp.hovered && ui.key_pressed(Key::Num0));
    if let Some(b) = bounds {
        // Frame as soon as there are bounds, and once more on the finished model's
        // measured bounds unless the user has taken over the camera.
        if st.controls.is_none() || reset || (!st.moved && !st.framed_final && !loading) {
            st.controls = Some(rendererx::OrbitControls::framing(b, aspect));
            st.framed_final = !loading;
            st.moved &= !reset;
        }
    }
    if let Some(c) = st.controls.as_mut() {
        let h = rect.h.max(1.0);
        if resp.active && resp.drag_delta != Vec2::ZERO {
            if resp.modifiers.shift {
                c.pan(resp.drag_delta.x, resp.drag_delta.y, h);
            } else {
                c.orbit(resp.drag_delta.x, resp.drag_delta.y, h);
            }
            st.moved = true;
        }
        if resp.hovered && resp.scroll.y != 0.0 {
            c.zoom((resp.scroll.y / 300.0).exp());
            st.moved = true;
        }
        if resp.pinch != 0.0 {
            c.zoom(1.0 + resp.pinch);
            st.moved = true;
        }
        if resp.pan2 != Vec2::ZERO {
            c.pan(resp.pan2.x, resp.pan2.y, h);
            st.moved = true;
        }
    }

    let camera = st.controls.map(|c| c.camera()).unwrap_or_default();
    let pal = ui.theme.palette;
    let bg = [pal.bg_inset.r, pal.bg_inset.g, pal.bg_inset.b, 1.0];
    let scale = tex.pixels_per_point();
    let size = ((rect.w * scale).round() as u32, (rect.h * scale).round() as u32);
    let frame = if size.0 > 0 && size.1 > 0 {
        tex.render_scene(&mut st.gpu, &doc.scene, size, &camera, bg)
    } else {
        // The first frame has no rect yet; the next one will.
        super::Frame3d { busy: true, ..Default::default() }
    };
    if !frame.busy && pending() == 0 && frame.texture.is_some() && !std::mem::replace(&mut st.settled, true) {
        // All uploaded: let the host free the upload staging.
        tex.uploads_done();
    }
    if frame.busy {
        ui.request_repaint();
    } else if pending() > 0 {
        // Arriving textures wake the window, but a failed decode changes nothing; look
        // again now and then so the status line can't get stuck.
        ui.request_repaint_in(0.25);
    }

    let (texture, error) = (frame.texture, frame.error);
    let faint = pal.text_muted;
    let status = match (loading, pending()) {
        (true, _) => "Loading model…".to_string(),
        (false, n) if n > 0 => format!("Decoding textures… ({n} left)"),
        _ => "Drag to orbit · Shift-drag to pan · Scroll to zoom · Double-click to reframe".to_string(),
    };
    let frame_style = Frame { fill: pal.bg_inset, clip: true, ..Frame::none() };
    ui.container(Layout::column(), frame_style, |ui| {
        ui.add_leaf(id, Layout::leaf(Size::Grow(1.0), Size::Grow(1.0)), Vec2::ZERO, true, move |p, r| {
            match (texture, &error) {
                (Some(t), _) => p.image_with_alpha(r, t, [0.0, 0.0, 1.0, 1.0], 0.0, Color::WHITE, ImageAlpha::Straight),
                (None, Some(e)) => p.text_centered(r, 14.0, faint, e),
                (None, None) => {}
            }
            p.text(Vec2::new(r.x + 8.0, r.y + r.h - 22.0), 12.0, faint, status);
        });
    });
}
