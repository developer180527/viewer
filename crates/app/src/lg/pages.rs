//! The paged view (docs/pdf.md §7): every page one after another in a vertical
//! scroll, laid out from page sizes alone and rendered in tiles on a background
//! thread, only where the window looks.
//!
//! - **Layout** comes from sizes in points. Until the worker has measured a page, it
//!   is assumed to be the size of page 1; the scroll position is kept as a page and
//!   an offset into it, so pages above changing size never move what's on screen.
//! - **Tiles** are `TILE` pixels square at a render scale rounded to a quarter octave
//!   of the display scale, so zooming re-renders only when it changes enough, and
//!   whatever is cached at another scale is drawn (stretched) until the sharp tile
//!   arrives.
//! - **The worker** renders the tiles the view last asked for, nearest the middle of
//!   the window first, then the pages just above and below. A new request replaces
//!   the old, so tiles scrolled past are never rendered. In idle time it measures
//!   the remaining pages.
//! - **The cache** keeps rendered tiles on the GPU within `CACHE_BYTES`, dropping
//!   the ones drawn longest ago.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use libgui::*;
use viewer_core::{PageImage, PageRect, PageSize, PageSource};

use super::Textures;

/// A tile's side in pixels.
const TILE: u32 = 1024;
/// Space around and between pages, in points of the window.
const GAP: f32 = 12.0;
/// Room left and right of the widest page at fit width.
const MARGIN: f32 = 16.0;
/// Rendered tiles kept on the GPU (RGBA8).
const CACHE_BYTES: usize = if cfg!(any(target_os = "ios", target_os = "android")) { 96 << 20 } else { 256 << 20 };
/// Bytes uploaded per frame (at least one tile); the rest wait, so scrolling stays smooth.
const UPLOAD_BYTES: usize = if TOUCH { 4 << 20 } else { 16 << 20 };
/// Touch platforms: a released drag keeps going. (Trackpads send their own momentum.)
const TOUCH: bool = cfg!(any(target_os = "ios", target_os = "android"));
/// How fast a fling dies away (1/s), and the speed where it stops (points/s).
const FLING_FRICTION: f32 = 3.2;
const FLING_CUTOFF: f32 = 5.0;
/// A cheap whole-page preview renders first, this many steps coarser (two octaves):
/// a page scrolled to shows its content at once, then sharpens.
const PREVIEW_STEPS: i16 = 8;
/// Render scales are rounded to steps of a quarter octave.
const STEPS_PER_OCTAVE: f32 = 4.0;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 32.0;
/// Page sizes the worker measures per turn in idle time.
const SIZES_PER_TURN: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct TileKey {
    page: u32,
    /// The render scale's step: scale = 2^(step / STEPS_PER_OCTAVE).
    step: i16,
    tx: u16,
    ty: u16,
}

fn step_scale(step: i16) -> f32 {
    (step as f32 / STEPS_PER_OCTAVE).exp2()
}

fn step_for(scale: f32) -> i16 {
    (scale.log2() * STEPS_PER_OCTAVE).round().clamp(-60.0, 40.0) as i16
}

/// A tile's side in page points at `step`.
fn tile_points(step: i16) -> f32 {
    TILE as f32 / step_scale(step)
}

/// The part of a page `key` covers, in points, or `None` past the page's edge.
fn tile_region(key: TileKey, size: PageSize) -> Option<PageRect> {
    let side = tile_points(key.step);
    let (x, y) = (key.tx as f32 * side, key.ty as f32 * side);
    (x < size.width && y < size.height).then(|| PageRect { x, y, width: side.min(size.width - x), height: side.min(size.height - y) })
}

// ---------------------------------------------------------------- worker

#[derive(Default)]
struct Shared {
    /// What the view wants rendered, most important first.
    wanted: Vec<TileKey>,
    /// Rendered (or being rendered, or failed) and not since evicted: not asked again.
    done: HashSet<TileKey>,
    results: Vec<(TileKey, Result<PageImage, String>)>,
    /// Page sizes as the worker measures them, and how many are in.
    sizes: Vec<Option<Result<PageSize, String>>>,
    measured: usize,
    closed: bool,
}

/// The page-render thread for one document, with the state it shares with the view.
/// Dropping it stops the thread after the tile in hand.
struct Worker {
    shared: Arc<(Mutex<Shared>, Condvar)>,
}

impl Worker {
    fn spawn(src: Arc<dyn PageSource>) -> Self {
        let n = src.page_count();
        let shared = Arc::new((Mutex::new(Shared { sizes: vec![None; n], ..Default::default() }), Condvar::new()));
        let theirs = shared.clone();
        let spawned = std::thread::Builder::new().name("page render".into()).spawn(move || work(&src, &theirs));
        if spawned.is_err() {
            // No thread, no pages: say so on every page rather than spin.
            let mut s = lock(&shared.0);
            s.sizes.iter_mut().for_each(|e| *e = Some(Err("couldn't start the render thread".into())));
            s.measured = n;
        }
        Self { shared }
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        lock(&self.shared.0)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.lock().closed = true;
        self.shared.1.notify_all();
    }
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn wake() {
    if let Some(wake) = crate::loading::WAKE.get() {
        wake();
    }
}

enum Job {
    Tile(TileKey),
    Measure(usize),
}

fn work(src: &Arc<dyn PageSource>, shared: &(Mutex<Shared>, Condvar)) {
    let n = src.page_count();
    let mut next_size = 0;
    loop {
        let job = {
            let mut s = lock(&shared.0);
            loop {
                if s.closed {
                    return;
                }
                let next = s.wanted.iter().copied().find(|k| !s.done.contains(k));
                if let Some(key) = next {
                    s.done.insert(key);
                    break Job::Tile(key);
                }
                if next_size < n {
                    break Job::Measure(next_size);
                }
                s = shared.1.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        };
        match job {
            Job::Tile(key) => {
                let result = src
                    .page_size(key.page as usize)
                    .and_then(|size| {
                        let region = tile_region(key, size).ok_or_else(|| anyhow::anyhow!("tile outside the page"))?;
                        src.render(key.page as usize, step_scale(key.step), region)
                    })
                    .map_err(|e| format!("{e:#}"));
                lock(&shared.0).results.push((key, result));
            }
            Job::Measure(first) => {
                let last = (first + SIZES_PER_TURN).min(n);
                let sizes: Vec<_> = (first..last).map(|i| src.page_size(i).map_err(|e| format!("{e:#}"))).collect();
                let mut s = lock(&shared.0);
                for (i, size) in (first..last).zip(sizes) {
                    s.sizes[i] = Some(size);
                }
                s.measured = last;
                next_size = last;
            }
        }
        shared.1.notify_all();
        wake();
    }
}

// ---------------------------------------------------------------- layout

/// Page sizes and where each page starts, in points (gaps excluded).
#[derive(Default)]
struct Layout {
    sizes: Vec<PageSize>,
    /// `tops[i]`: the heights of the pages before `i`; `tops[n]` is them all.
    tops: Vec<f32>,
    widest: f32,
    /// How many of the worker's sizes this layout has taken in.
    measured: usize,
}

impl Layout {
    fn new(n: usize, guess: PageSize) -> Self {
        let mut l = Self { sizes: vec![guess; n], ..Default::default() };
        l.update();
        l
    }

    fn update(&mut self) {
        self.tops.clear();
        self.tops.push(0.0);
        let mut y = 0.0;
        for s in &self.sizes {
            y += s.height;
            self.tops.push(y);
        }
        self.widest = self.sizes.iter().map(|s| s.width).fold(1.0, f32::max);
    }

    fn len(&self) -> usize {
        self.sizes.len()
    }

    /// Page `i`'s top edge in window points from the top of the document at `zoom`.
    fn top(&self, i: usize, zoom: f32) -> f32 {
        self.tops[i] * zoom + i as f32 * GAP + GAP
    }

    fn height(&self, zoom: f32) -> f32 {
        self.top(self.len(), zoom)
    }

    /// The page at `y` (window points from the document's top): the last one
    /// starting at or above it.
    fn page_at(&self, y: f32, zoom: f32) -> usize {
        // Binary search for the first page starting below `y`.
        let n = self.len();
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.top(mid, zoom) <= y { lo = mid + 1 } else { hi = mid }
        }
        lo.saturating_sub(1).min(n.saturating_sub(1))
    }
}

// ---------------------------------------------------------------- view

struct Tile {
    tex: TextureId,
    bytes: usize,
    /// The frame it was last drawn in.
    used: u64,
}

pub struct PagesView {
    worker: Option<Worker>,
    layout: Layout,
    /// `None` is fit to width.
    zoom: Option<f32>,
    /// The scroll position: the page at the window's top and how far into it, in
    /// page points (negative in the gap above it).
    anchor: (usize, f32),
    /// Horizontal scroll in window points, when pages are wider than the window.
    x: f32,
    tiles: HashMap<TileKey, Tile>,
    /// Pages that failed to measure or render, with why.
    errors: HashMap<usize, String>,
    wanted: Vec<TileKey>,
    frame: u64,
    /// Why the document shows nothing at all.
    error: Option<String>,
    /// Scroll speed left from a flung touch drag, in window points/s.
    fling: Vec2,
}

impl Default for PagesView {
    fn default() -> Self {
        Self {
            worker: None,
            layout: Layout::default(),
            zoom: None,
            anchor: (0, -GAP),
            x: 0.0,
            tiles: HashMap::new(),
            errors: HashMap::new(),
            wanted: Vec::new(),
            frame: 0,
            error: None,
            fling: Vec2::ZERO,
        }
    }
}

impl PagesView {
    pub fn textures(&self) -> Vec<TextureId> {
        self.tiles.values().map(|t| t.tex).collect()
    }

    fn start(&mut self, src: &Arc<dyn PageSource>) {
        let n = src.page_count();
        if n == 0 {
            self.error = Some("This document has no pages.".into());
            return;
        }
        // Page 1's size lays out everything until the worker measures the rest.
        let guess = src.page_size(0).unwrap_or(PageSize { width: 612.0, height: 792.0 });
        self.layout = Layout::new(n, guess);
        self.worker = Some(Worker::spawn(src.clone()));
    }

    /// The zoom when the user hasn't chosen one: the widest page fills the window's
    /// width (phones, narrow windows), but on a wide window no bigger than 100% or a
    /// whole page in view, whichever is larger, so a portrait page isn't blown up.
    fn fit(&self, rect: Rect) -> f32 {
        let s = self.layout.sizes.first().copied().unwrap_or(PageSize { width: 1.0, height: 1.0 });
        let width = (rect.w - 2.0 * MARGIN) / self.layout.widest;
        let page = (rect.h - 2.0 * GAP) / s.height.max(1.0);
        width.min(page.max(1.0)).clamp(MIN_ZOOM, MAX_ZOOM)
    }

    fn top_y(&self, zoom: f32) -> f32 {
        let (page, off) = self.anchor;
        self.layout.top(page.min(self.layout.len()), zoom) + off * zoom
    }

    /// Scrolls so the window's top is at `y`, kept within the document.
    fn set_top_y(&mut self, y: f32, zoom: f32, view_h: f32) {
        let y = y.clamp(0.0, (self.layout.height(zoom) - view_h).max(0.0));
        let page = self.layout.page_at(y, zoom);
        self.anchor = (page, (y - self.layout.top(page, zoom)) / zoom);
    }

    /// Takes in page sizes the worker measured; returns true if any arrived.
    fn take_sizes(&mut self) -> bool {
        let Some(w) = &self.worker else { return false };
        let s = w.lock();
        if s.measured == self.layout.measured {
            return false;
        }
        for i in self.layout.measured..s.measured {
            match &s.sizes[i] {
                Some(Ok(size)) if size.width > 0.0 && size.height > 0.0 => self.layout.sizes[i] = *size,
                Some(Err(e)) => {
                    self.errors.insert(i, e.clone());
                }
                _ => {}
            }
        }
        self.layout.measured = s.measured;
        drop(s);
        self.layout.update();
        true
    }

    /// Uploads finished tiles: a few per frame, or all of them for a screenshot.
    fn take_tiles(&mut self, tex: &mut dyn Textures, all: bool) -> bool {
        let Some(w) = &self.worker else { return false };
        let mut s = w.lock();
        let mut take = 0;
        let mut bytes = 0;
        while take < s.results.len() && (all || take == 0 || bytes < UPLOAD_BYTES) {
            bytes += s.results[take].1.as_ref().map_or(0, |i| i.rgba.len());
            take += 1;
        }
        let results: Vec<_> = s.results.drain(..take).collect();
        let more = !s.results.is_empty();
        drop(s);
        for (key, result) in results {
            match result {
                Ok(img) => {
                    let id = tex.upload(img.width, img.height, &img.rgba);
                    if let Some(old) = self.tiles.insert(key, Tile { tex: id, bytes: img.rgba.len(), used: self.frame }) {
                        tex.free(old.tex);
                    }
                }
                Err(e) => {
                    self.errors.insert(key.page as usize, e);
                }
            }
        }
        more
    }

    /// Frees tiles until the cache fits its budget, before this frame draws: first
    /// those at scales no longer in use, then those drawn longest ago.
    fn evict(&mut self, tex: &mut dyn Textures, step: i16) {
        let mut total: usize = self.tiles.values().map(|t| t.bytes).sum();
        if total <= CACHE_BYTES {
            return;
        }
        let current = |k: &TileKey| k.step == step || k.step <= step - PREVIEW_STEPS;
        let mut old: Vec<_> = self.tiles.iter().map(|(k, t)| ((current(k), t.used), *k)).collect();
        old.sort_unstable_by_key(|(rank, _)| *rank);
        let mut forgotten = Vec::new();
        for (_, key) in old {
            if total <= CACHE_BYTES {
                break;
            }
            let t = self.tiles.remove(&key).unwrap();
            tex.free(t.tex);
            total -= t.bytes;
            forgotten.push(key);
        }
        if let Some(w) = &self.worker {
            let mut s = w.lock();
            for key in forgotten {
                s.done.remove(&key);
            }
        }
    }

    /// Waits (for a screenshot) until every tile in `keys` is rendered or failed.
    fn wait_for(&self, keys: &[TileKey]) {
        let Some(w) = &self.worker else { return };
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut s = w.lock();
        loop {
            let pending = keys.iter().any(|k| !self.tiles.contains_key(k) && !s.results.iter().any(|(r, _)| r == k));
            let now = Instant::now();
            if !pending || now >= deadline {
                return;
            }
            s = w.shared.1.wait_timeout(s, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

/// One page as placed in the window this frame.
struct Placed {
    index: usize,
    rect: Rect,
}

pub fn show(ui: &mut Ui, src: &Arc<dyn PageSource>, st: &mut PagesView, tex: &mut dyn Textures) {
    if st.worker.is_none() && st.error.is_none() {
        st.start(src);
    }
    if let Some(e) = &st.error {
        ui.label(e.as_str());
        return;
    }
    st.frame += 1;
    let settle = tex.settles();
    let ppp = tex.pixels_per_point();
    st.take_sizes();

    let id = ui.make_id("pages");
    let resp = ui.interact_drag(id);
    let rect = resp.rect; // last frame's, as for images
    let n = st.layout.len();
    let mut zoom = st.zoom.unwrap_or_else(|| st.fit(rect));

    if rect.w > 0.0 && rect.h > 0.0 {
        // Zoom: pinch, Ctrl/Cmd-scroll, keys, double-click; about the pointer.
        let mut new_zoom = st.zoom;
        let zooming_scroll = resp.modifiers.ctrl || resp.modifiers.logo;
        if resp.hovered {
            let mut factor = 1.0 + tex.trackpad_pinch();
            if zooming_scroll {
                factor *= (resp.scroll.y / 300.0).exp();
            }
            if ui.key_pressed(Key::Equal) {
                factor *= 1.25;
            }
            if ui.key_pressed(Key::Minus) {
                factor /= 1.25;
            }
            if factor != 1.0 {
                new_zoom = Some((zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM));
            }
            if ui.key_pressed(Key::Num0) {
                new_zoom = None;
            }
            if ui.key_pressed(Key::Num1) {
                new_zoom = Some(1.0);
            }
        }
        // A touch pinch needn't count as hovering.
        if resp.pinch != 0.0 {
            new_zoom = Some((new_zoom.unwrap_or(zoom) * (1.0 + resp.pinch)).clamp(MIN_ZOOM, MAX_ZOOM));
        }
        if resp.double_clicked {
            new_zoom = if st.zoom.is_some() { None } else { Some(1.0) };
        }
        if new_zoom != st.zoom {
            let target = new_zoom.unwrap_or_else(|| st.fit(rect));
            let gesture = ui.gesture();
            let anchor = if gesture.active {
                gesture.center - Vec2::new(rect.x, rect.y)
            } else if resp.hovered {
                resp.mouse_pos - Vec2::new(rect.x, rect.y)
            } else {
                Vec2::new(rect.w / 2.0, rect.h / 2.0)
            };
            // The document point under the anchor stays there.
            let y = st.top_y(zoom) + anchor.y;
            let page = st.layout.page_at(y, zoom);
            let into = (y - st.layout.top(page, zoom)) / zoom;
            let content_x = (st.x + anchor.x - pad(rect.w, st.layout.widest, zoom)) / zoom;
            st.zoom = new_zoom;
            zoom = target;
            st.set_top_y(st.layout.top(page, zoom) + into * zoom - anchor.y, zoom, rect.h);
            st.x = content_x * zoom + pad(rect.w, st.layout.widest, zoom) - anchor.x;
        }

        // Scroll: wheel and trackpad, dragging, keys.
        let mut d = Vec2::ZERO;
        if resp.hovered && !zooming_scroll {
            d = d - resp.scroll;
        }
        d = d - resp.pan2;
        if resp.active {
            d = d - resp.drag_delta;
        }
        // A touch drag let go keeps moving, slowing down; any new touch stops it.
        let dt = ui.input().dt;
        if resp.pressed || resp.active || resp.pinch != 0.0 {
            st.fling = Vec2::ZERO;
        }
        if TOUCH && resp.released {
            let v = ui.pointer_velocity();
            st.fling = Vec2::ZERO - v;
        }
        if st.fling != Vec2::ZERO {
            d += st.fling * dt;
            st.fling = st.fling * (-FLING_FRICTION * dt).exp();
            let (top, end) = (st.top_y(zoom), (st.layout.height(zoom) - rect.h).max(0.0));
            let at_end = (top <= 0.0 && st.fling.y < 0.0) || (top >= end && st.fling.y > 0.0);
            if at_end || st.fling.x.hypot(st.fling.y) < FLING_CUTOFF {
                st.fling = Vec2::ZERO;
            }
            ui.request_repaint();
        }
        let mut top = st.top_y(zoom);
        if resp.hovered {
            let page_step = rect.h - 2.0 * GAP - 40.0;
            let shift = ui.key_down(Key::ShiftLeft) || ui.key_down(Key::ShiftRight);
            if ui.key_pressed(Key::PageDown) || (ui.key_pressed(Key::Space) && !shift) {
                d.y += page_step;
            }
            if ui.key_pressed(Key::PageUp) || (ui.key_pressed(Key::Space) && shift) {
                d.y -= page_step;
            }
            if ui.key_pressed(Key::ArrowDown) {
                d.y += 48.0;
            }
            if ui.key_pressed(Key::ArrowUp) {
                d.y -= 48.0;
            }
            if ui.key_pressed(Key::Home) {
                top = 0.0;
            }
            if ui.key_pressed(Key::End) {
                top = f32::MAX / 2.0;
            }
        }
        st.set_top_y(top + d.y, zoom, rect.h);
        let content_w = st.layout.widest * zoom + 2.0 * MARGIN;
        st.x = (st.x + d.x).clamp(0.0, (content_w - rect.w).max(0.0));
    }

    // The pages in the window, placed.
    let mut placed = Vec::new();
    if rect.w > 0.0 && rect.h > 0.0 {
        let top = st.top_y(zoom);
        let left = rect.x + pad(rect.w, st.layout.widest, zoom) - st.x;
        let snap = |v: f32| (v * ppp).round() / ppp;
        let mut i = st.layout.page_at(top, zoom);
        while i < n && st.layout.top(i, zoom) < top + rect.h {
            let s = st.layout.sizes[i];
            let x = snap(left + (st.layout.widest - s.width) * zoom / 2.0);
            let y = snap(rect.y + st.layout.top(i, zoom) - top);
            placed.push(Placed { index: i, rect: Rect::new(x, y, snap(s.width * zoom), snap(s.height * zoom)) });
            i += 1;
        }
    }

    // What to render: visible tiles nearest the middle first, then the pages just
    // above and below.
    let step = step_for(zoom * ppp);
    let mut visible = Vec::new();
    for p in &placed {
        tiles_in(p, st.layout.sizes[p.index], step, zoom, rect, &mut visible);
    }
    let middle = rect.center();
    let side = tile_points(step) * zoom;
    let dist = |k: &TileKey, page: &Rect| {
        let c = Vec2::new(page.x + (k.tx as f32 + 0.5) * side, page.y + (k.ty as f32 + 0.5) * side);
        let v = c - middle;
        v.x.hypot(v.y)
    };
    visible.retain(|(k, _)| !st.errors.contains_key(&(k.page as usize)));
    visible.sort_by(|(a, pa), (b, pb)| dist(a, pa).total_cmp(&dist(b, pb)));
    // Previews of the pages on screen, then their sharp tiles, then the neighbours.
    let whole = |index: usize, s: PageSize, out: &mut Vec<TileKey>| {
        // Coarse enough that the whole page fits one tile, however far zoomed in.
        let fits = ((TILE as f32 / s.width.max(s.height).max(1.0)).log2() * STEPS_PER_OCTAVE).floor() as i16;
        out.push(TileKey { page: index as u32, step: (step - PREVIEW_STEPS).min(fits), tx: 0, ty: 0 });
    };
    let mut wanted: Vec<TileKey> = Vec::new();
    for p in &placed {
        if !st.errors.contains_key(&p.index) {
            whole(p.index, st.layout.sizes[p.index], &mut wanted);
        }
    }
    wanted.extend(visible.iter().map(|(k, _)| *k));
    if let (Some(first), Some(last)) = (placed.first(), placed.last()) {
        for (index, dy) in [(last.index + 1, 1.0), (first.index.wrapping_sub(1), -1.0)] {
            if index < n && !st.errors.contains_key(&index) {
                let s = st.layout.sizes[index];
                whole(index, s, &mut wanted);
                // Where the page will be once scrolled to: just past this edge.
                let y = if dy > 0.0 { rect.y + rect.h } else { rect.y - s.height * zoom };
                let near = Placed { index, rect: Rect::new(first.rect.x, y, s.width * zoom, s.height * zoom) };
                let mut more = Vec::new();
                tiles_in(&near, s, step, zoom, Rect::new(rect.x, y, rect.w, s.height * zoom), &mut more);
                wanted.extend(more.into_iter().map(|(k, _)| k));
            }
        }
    }
    let visible: Vec<TileKey> = visible.into_iter().map(|(k, _)| k).collect();
    if wanted != st.wanted {
        if let Some(w) = &st.worker {
            w.lock().wanted = wanted.clone();
            w.shared.1.notify_all();
        }
        st.wanted = wanted;
    }
    if settle {
        st.wait_for(&visible);
    }
    if st.take_tiles(tex, settle) {
        ui.request_repaint();
    }

    st.evict(tex, step);

    // Draw: each page white, then its tiles, coarser scales under finer, the wanted
    // scale on top.
    let mut draws: Vec<(Rect, TextureId, i16)> = Vec::new();
    for p in &placed {
        for (key, t) in st.tiles.iter_mut().filter(|(k, _)| k.page as usize == p.index) {
            let Some(region) = tile_region(*key, st.layout.sizes[p.index]) else { continue };
            // Edges snapped to pixels, shared with the neighbouring tiles: no seams.
            let snap = |v: f32| (v * ppp).round() / ppp;
            let (x0, y0) = (snap(p.rect.x + region.x * zoom), snap(p.rect.y + region.y * zoom));
            let (x1, y1) = (snap(p.rect.x + (region.x + region.width) * zoom), snap(p.rect.y + (region.y + region.height) * zoom));
            let r = Rect::new(x0, y0, x1 - x0, y1 - y0);
            if r.intersect(&rect).is_some() {
                t.used = st.frame;
                let rank = if key.step == step { i16::MAX } else { -(key.step - step).abs() };
                draws.push((r, t.tex, rank));
            }
        }
    }
    draws.sort_by_key(|d| d.2);

    let current = placed.iter().min_by(|a, b| (a.rect.center().y - middle.y).abs().total_cmp(&(b.rect.center().y - middle.y).abs())).map_or(0, |p| p.index);
    let label = format!("{} / {n}  ·  {:.0}%", current + 1, zoom * 100.0);
    let pages: Vec<(Rect, Option<String>)> = placed.iter().map(|p| (p.rect, st.errors.get(&p.index).cloned())).collect();
    let pal = ui.theme.palette;
    let frame = Frame { fill: pal.bg_inset, clip: true, ..Frame::none() };
    ui.container(libgui::Layout::column(), frame, |ui| {
        ui.add_leaf(id, libgui::Layout::leaf(Size::Grow(1.0), Size::Grow(1.0)), Vec2::ZERO, true, move |p, area| {
            for (r, error) in &pages {
                p.shadow(*r, 0.0, 6.0, Color::rgba(0.0, 0.0, 0.0, 0.35));
                p.rect(*r, Color::WHITE, 0.0);
                if let Some(e) = error {
                    p.text(Vec2::new(r.x + 16.0, r.y + 16.0), 13.0, Color::rgba(0.6, 0.1, 0.1, 1.0), format!("This page couldn't be shown: {e}"));
                }
            }
            for (r, t, _) in &draws {
                p.image_with_alpha(*r, *t, [0.0, 0.0, 1.0, 1.0], 0.0, Color::WHITE, ImageAlpha::Straight);
            }
            let size = p.measure(12.0, label.as_str());
            let chip = Rect::new(area.x + 10.0, area.y + area.h - size.y - 20.0, size.x + 24.0, size.y + 10.0);
            p.rect(chip, Color::rgba(0.0, 0.0, 0.0, 0.55), chip.h / 2.0);
            p.text(Vec2::new(chip.x + 12.0, chip.y + 5.0), 12.0, Color::WHITE, label.as_str());
        });
    });
}

/// Space left of the widest page so pages sit centred when narrower than the window.
fn pad(view_w: f32, widest: f32, zoom: f32) -> f32 {
    MARGIN + ((view_w - widest * zoom - 2.0 * MARGIN) / 2.0).max(0.0)
}

/// The tiles of placed page `p` at `step` that fall inside `view`, each with the
/// page's rect.
fn tiles_in(p: &Placed, size: PageSize, step: i16, zoom: f32, view: Rect, out: &mut Vec<(TileKey, Rect)>) {
    let Some(vis) = p.rect.intersect(&view) else { return };
    let side = tile_points(step) * zoom;
    let cols = (size.width / tile_points(step)).ceil() as i64;
    let rows = (size.height / tile_points(step)).ceil() as i64;
    let tx0 = ((vis.x - p.rect.x) / side).floor().max(0.0) as i64;
    let ty0 = ((vis.y - p.rect.y) / side).floor().max(0.0) as i64;
    let tx1 = (((vis.x + vis.w - p.rect.x) / side).ceil() as i64).min(cols);
    let ty1 = (((vis.y + vis.h - p.rect.y) / side).ceil() as i64).min(rows);
    for ty in ty0..ty1.min(u16::MAX as i64) {
        for tx in tx0..tx1.min(u16::MAX as i64) {
            out.push((TileKey { page: p.index as u32, step, tx: tx as u16, ty: ty as u16 }, p.rect));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LETTER: PageSize = PageSize { width: 612.0, height: 792.0 };

    #[test]
    fn layout_finds_pages_by_position() {
        let l = Layout::new(3, LETTER);
        assert_eq!(l.top(0, 1.0), GAP);
        assert_eq!(l.top(1, 1.0), GAP + 792.0 + GAP);
        assert_eq!(l.page_at(0.0, 1.0), 0);
        assert_eq!(l.page_at(GAP + 791.0, 1.0), 0);
        assert_eq!(l.page_at(l.top(1, 1.0), 1.0), 1);
        assert_eq!(l.page_at(1e9, 1.0), 2);
        assert_eq!(l.height(0.5), 3.0 * 396.0 + 4.0 * GAP);
    }

    #[test]
    fn scroll_position_survives_pages_above_changing_size() {
        let mut v = PagesView { layout: Layout::new(3, LETTER), ..Default::default() };
        let y = v.layout.top(2, 1.0) + 100.0;
        v.set_top_y(y, 1.0, 500.0);
        assert_eq!(v.anchor, (2, 100.0));
        // Page 1 turns out twice as tall: page 3's content stays at the window's top.
        v.layout.sizes[0].height *= 2.0;
        v.layout.update();
        assert_eq!(v.anchor, (2, 100.0));
        assert_eq!(v.top_y(1.0), v.layout.top(2, 1.0) + 100.0);
        // Clamped to the document.
        v.set_top_y(-50.0, 1.0, 500.0);
        assert_eq!(v.top_y(1.0), 0.0);
        v.set_top_y(1e9, 1.0, 500.0);
        assert_eq!(v.top_y(1.0), v.layout.height(1.0) - 500.0);
    }

    #[test]
    fn tiles_cover_the_page_and_stop_at_its_edge() {
        let step = step_for(2.0);
        assert_eq!(step_scale(step), 2.0);
        assert_eq!(tile_points(step), 512.0);
        let r = tile_region(TileKey { page: 0, step, tx: 1, ty: 1 }, LETTER).unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (512.0, 512.0, 100.0, 280.0));
        assert!(tile_region(TileKey { page: 0, step, tx: 2, ty: 0 }, LETTER).is_none());
        // A whole page in view at 2× is a 2 × 2 grid.
        let p = Placed { index: 0, rect: Rect::new(0.0, 0.0, 612.0, 792.0) };
        let mut out = Vec::new();
        tiles_in(&p, LETTER, step, 1.0, Rect::new(0.0, 0.0, 2000.0, 2000.0), &mut out);
        assert_eq!(out.len(), 4);
        // Only the top half in view: the top row.
        out.clear();
        tiles_in(&p, LETTER, step, 1.0, Rect::new(0.0, 0.0, 2000.0, 300.0), &mut out);
        assert_eq!(out.iter().map(|(k, _)| k.ty).max(), Some(0));
    }

    #[test]
    fn default_zoom_fills_narrow_windows_but_not_wide_ones() {
        let v = PagesView { layout: Layout::new(3, LETTER), ..Default::default() };
        // A phone: page width fills the screen.
        let phone = v.fit(Rect::new(0.0, 0.0, 390.0, 760.0));
        assert!((phone - (390.0 - 2.0 * MARGIN) / 612.0).abs() < 1e-4);
        // A wide window: 100%, not 180%.
        assert_eq!(v.fit(Rect::new(0.0, 0.0, 1200.0, 700.0)), 1.0);
    }
}
