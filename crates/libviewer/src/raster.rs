//! Images: uploaded to the GPU in tiles, a budget's worth per frame, and drawn with
//! zoom and pan. An image in bands is decoded on a thread of the view's own, pulling
//! from its `BandSource` a band at a time, at most two bands ahead of the GPU: the
//! whole RGBA image never sits in memory, and closing the page stops the decoder.
//!
//! **Big images** (past [`FULL_BYTES`] at full size: a 48-megapixel photo is 195 MB)
//! go to the GPU as a preview, box-filtered down on the decoder thread to at most
//! [`PREVIEW_SIDE`], sharp at fit on any screen. Zoomed in past the preview's
//! resolution, once the zoom holds, the image is decoded again and only the part in
//! view (and a margin) goes up at full resolution, within [`DETAIL_BYTES`]; panning
//! elsewhere fetches that part. Meanwhile the preview is drawn under it, scaled.

use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use libfileparser_core::{Band, BandSource, Pixels, Raster};
use libgui::*;

use crate::Host;

/// Images go to the GPU in tiles of this side, a few per frame (`UPLOAD_BUDGET`):
/// staging memory stays small and the first tiles show sooner.
const TILE: u32 = 2048;
const UPLOAD_BUDGET: usize = 48 << 20;
const TOUCH: bool = cfg!(any(target_os = "ios", target_os = "android"));
/// Images up to this many bytes (RGBA) go to the GPU whole, at full resolution.
const FULL_BYTES: usize = if TOUCH { 64 << 20 } else { 128 << 20 };
/// A bigger image's preview, longest side at most.
pub const PREVIEW_SIDE: u32 = 4096;
/// Full-resolution detail kept for a zoomed-in big image.
const DETAIL_BYTES: usize = if TOUCH { 48 << 20 } else { 96 << 20 };
/// Detail is fetched once the view has held this long: not for every step of a pinch
/// or a drag (each pass decodes the whole image again).
const DETAIL_SETTLE: Duration = Duration::from_millis(200);

#[derive(Default)]
pub struct RasterView {
    /// What's drawn at any zoom: the image itself, or for a big one its preview. Rects
    /// in image pixels (a preview tile covers more pixels than it holds).
    tiles: Vec<(TextureId, Rect)>,
    /// Tiles uploaded so far, in row-major order (whole images).
    uploaded: usize,
    /// Rows on the GPU so far, in image pixels, and why decoding stopped early.
    rows: u32,
    error: Option<String>,
    /// The decoder thread's bands, once started; `None` again when it's done.
    stream: Option<Receiver<Result<Band, String>>>,
    started: bool,
    done: bool,
    /// `None` means "fit to window".
    zoom: Option<f32>,
    offset: Vec2,
    /// A big image's preview: image pixels per preview pixel (1: not a big image).
    factor: u32,
    detail: Detail,
}

/// A big image's full-resolution tiles where it's zoomed in.
#[derive(Default)]
struct Detail {
    tiles: Vec<(TextureId, Rect)>,
    /// The region they cover (or will, once the pass in progress ends), image pixels.
    region: Option<Rect>,
    /// The pass decoding it: bands cropped to the region's columns.
    stream: Option<Receiver<Result<Band, String>>>,
    /// The region's left edge and width in pixels, for the bands in flight.
    x: u32,
    width: u32,
    /// The view as last seen (zoom and where), and since when: detail waits for it to
    /// hold, so dragging or pinching shows the preview and lets go to sharpen.
    view_since: Option<([i32; 3], Instant)>,
}

impl RasterView {
    /// GPU textures this view owns, to free when its page closes.
    pub fn textures(&self) -> Vec<TextureId> {
        self.tiles.iter().chain(&self.detail.tiles).map(|(t, _)| *t).collect()
    }
}

/// Uploads the next tiles of a whole image within the budget. Returns true when all
/// are on the GPU.
fn upload(host: &mut dyn Host, width: u32, height: u32, rgba: &[u8], st: &mut RasterView) -> bool {
    let tile = TILE.min(host.max_side().max(256));
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
        let id = host.upload(w, h, &buf);
        st.tiles.push((id, Rect::new(tx as f32, ty as f32, w as f32, h as f32)));
        st.uploaded += 1;
        spent += buf.len();
    }
    st.uploaded == total
}

/// Uploads one band, `width` pixels wide, as a row of tiles `TILE` wide, into `out`:
/// each tile's rect `scale` image pixels per band pixel, from image pixel `x0`, kept
/// within the image's `size`.
fn upload_band(host: &mut dyn Host, width: u32, band: &Band, scale: u32, x0: u32, size: Vec2, out: &mut Vec<(TextureId, Rect)>) -> u32 {
    let tile = TILE.min(host.max_side().max(256));
    let rows = (band.rgba.len() / (width as usize * 4).max(1)) as u32;
    for tx in (0..width).step_by(tile as usize) {
        let w = tile.min(width - tx);
        let mut buf = Vec::with_capacity((w * rows * 4) as usize);
        for y in 0..rows {
            let row = ((y * width + tx) * 4) as usize;
            buf.extend_from_slice(&band.rgba[row..row + (w * 4) as usize]);
        }
        let id = host.upload(w, rows, &buf);
        let (x, y) = ((x0 + tx * scale) as f32, (band.y * scale) as f32);
        let r = Rect::new(x, y, ((w * scale) as f32).min(size.x - x), ((rows * scale) as f32).min(size.y - y));
        out.push((id, r));
    }
    (band.y + rows) * scale
}

/// Decodes `source` on a thread of its own, sending what `shape` makes of each band
/// (two ahead at most: a hidden window makes the decoder wait). Dropping the receiver
/// stops it.
fn decode<F>(source: std::sync::Arc<dyn BandSource>, wake: crate::Waker, mut shape: F) -> Result<Receiver<Result<Band, String>>, String>
where
    F: FnMut(Band, &mut dyn FnMut(Band) -> bool) -> bool + Send + 'static,
{
    let (tx, rx) = sync_channel::<Result<Band, String>>(2);
    std::thread::Builder::new()
        .name("image decode".into())
        .spawn(move || {
            let send = |item: Result<Band, String>| {
                let ok = tx.send(item).is_ok();
                wake();
                ok
            };
            match source.bands() {
                Ok(bands) => {
                    for band in bands {
                        let go_on = match band {
                            Ok(band) => shape(band, &mut |b| send(Ok(b))),
                            Err(e) => {
                                send(Err(e.to_string()));
                                false
                            }
                        };
                        if !go_on {
                            break;
                        }
                    }
                }
                Err(e) => {
                    send(Err(e.to_string()));
                }
            }
            drop(tx);
            wake();
        })
        .map(|_| rx)
        .map_err(|e| format!("no thread to decode on: {e}"))
}

/// Box-filters full-width bands down by `factor` (image pixels per preview pixel, each
/// way) into preview bands, a few dozen rows at a time.
struct Downscale {
    width: u32,
    height: u32,
    factor: u32,
    /// Sums of the source rows in hand, per preview pixel and channel, and how many.
    sums: Vec<u32>,
    rows_in: u32,
    /// Preview rows done and not yet sent, and the first one's row.
    out: Vec<u8>,
    out_y: u32,
    next_y: u32,
}

impl Downscale {
    fn new(width: u32, height: u32, factor: u32) -> Self {
        let pw = width.div_ceil(factor) as usize;
        Self { width, height, factor, sums: vec![0; pw * 4], rows_in: 0, out: Vec::new(), out_y: 0, next_y: 0 }
    }

    /// Takes in a band; sends preview bands as they fill. False when sending failed.
    fn band(&mut self, band: Band, send: &mut dyn FnMut(Band) -> bool) -> bool {
        let (w, f) = (self.width as usize, self.factor as usize);
        let pw = self.width.div_ceil(self.factor) as usize;
        let rows = band.rgba.len() / (w * 4).max(1);
        for r in 0..rows {
            let row = &band.rgba[r * w * 4..(r + 1) * w * 4];
            for (x, px) in row.as_chunks::<4>().0.iter().enumerate() {
                let cell = (x / f) * 4;
                for (sum, v) in self.sums[cell..cell + 4].iter_mut().zip(px) {
                    *sum += *v as u32;
                }
            }
            self.rows_in += 1;
            let y = band.y as usize + r;
            if self.rows_in as usize == f || y + 1 == self.height as usize {
                if self.out.is_empty() {
                    self.out_y = self.next_y;
                }
                for px in 0..pw {
                    let cols = f.min(w - px * f) as u32;
                    let n = cols * self.rows_in;
                    for c in 0..4 {
                        self.out.push((self.sums[px * 4 + c] / n.max(1)) as u8);
                    }
                }
                self.sums.iter_mut().for_each(|s| *s = 0);
                self.rows_in = 0;
                self.next_y += 1;
                if self.out.len() >= pw * 4 * 256 && !send(Band::new(self.out_y, std::mem::take(&mut self.out))) {
                    return false;
                }
            }
        }
        if self.next_y as usize * f >= self.height as usize && !self.out.is_empty() {
            return send(Band::new(self.out_y, std::mem::take(&mut self.out)));
        }
        true
    }
}

/// Moves pixels to the GPU: a whole image a budget's worth of tiles per frame, one in
/// bands as its decoder delivers them (a big one as its preview).
fn feed(ui: &mut Ui, r: &Raster, st: &mut RasterView, host: &mut dyn Host) {
    if st.done {
        return;
    }
    let (width, height) = (r.width(), r.height());
    let size = Vec2::new(width as f32, height as f32);
    let finished = match r.pixels() {
        Pixels::Full(rgba) if width > 0 && height > 0 => {
            let done = upload(host, width, height, rgba, st);
            if !done {
                ui.request_repaint();
            }
            done
        }
        Pixels::Bands(source) => {
            if !std::mem::replace(&mut st.started, true) {
                st.factor = preview_factor(width, height);
                let wake = host.waker();
                let spawned = if st.factor > 1 {
                    let mut down = Downscale::new(width, height, st.factor);
                    decode(source.clone(), wake, move |band, send| down.band(band, send))
                } else {
                    decode(source.clone(), wake, |band, send| send(band))
                };
                match spawned {
                    Ok(rx) => st.stream = Some(rx),
                    Err(e) => st.error = Some(e),
                }
            }
            let factor = st.factor.max(1);
            let band_width = width.div_ceil(factor);
            // Taken while bands are uploaded (they borrow the view), and put back.
            match st.stream.take() {
                None => true,
                Some(stream) => {
                    // Screenshots wait for the whole image; windows take what has arrived.
                    let block = host.settles();
                    let mut spent = 0;
                    let finished = loop {
                        let next = if block { stream.recv().map_err(|_| TryRecvError::Disconnected) } else { stream.try_recv() };
                        match next {
                            Ok(Ok(band)) => {
                                let rows = upload_band(host, band_width, &band, factor, 0, size, &mut st.tiles);
                                st.rows = st.rows.max(rows.min(height));
                                spent += band.rgba.len();
                                if !block && spent >= UPLOAD_BUDGET {
                                    ui.request_repaint();
                                    break false;
                                }
                            }
                            Ok(Err(e)) => {
                                st.error = Some(e);
                                break true;
                            }
                            // The next band wakes the window.
                            Err(TryRecvError::Empty) => break false,
                            Err(TryRecvError::Disconnected) => break true,
                        }
                    };
                    st.stream = Some(stream);
                    finished
                }
            }
        }
        _ => true,
    };
    if finished {
        st.done = true;
        st.stream = None;
        host.uploads_done();
    }
}

/// Image pixels per preview pixel for an image this big: 1 (no preview) when it fits
/// [`FULL_BYTES`], else enough to bring its longest side within [`PREVIEW_SIDE`].
/// The part of a full-width band (`width` pixels) in columns `x..x + w` and rows
/// `y0..y1`; `None` when the band has no rows there.
fn crop_band(band: &Band, width: u32, x: u32, w: u32, y0: u32, y1: u32) -> Option<Band> {
    let rows = (band.rgba.len() / (width as usize * 4).max(1)) as u32;
    let (top, bottom) = (band.y.max(y0), (band.y + rows).min(y1));
    if top >= bottom {
        return None;
    }
    let mut out = Vec::with_capacity((w * (bottom - top) * 4) as usize);
    for y in top..bottom {
        let row = (((y - band.y) * width + x) * 4) as usize;
        out.extend_from_slice(&band.rgba[row..row + (w * 4) as usize]);
    }
    Some(Band::new(top, out))
}

fn preview_factor(width: u32, height: u32) -> u32 {
    if (width as usize) * (height as usize) * 4 <= FULL_BYTES {
        1
    } else {
        width.max(height).div_ceil(PREVIEW_SIDE).max(2)
    }
}

/// A big image zoomed in past its preview: fetches full-resolution tiles for the part
/// in view, `view` (image pixels), once the zoom has held; takes in what's arrived.
fn detail(ui: &mut Ui, r: &Raster, st: &mut RasterView, host: &mut dyn Host, view: Rect, zoom: f32) {
    let Pixels::Bands(source) = r.pixels() else { return };
    let (width, height) = (r.width(), r.height());
    let size = Vec2::new(width as f32, height as f32);
    let d = &mut st.detail;
    // What's arrived.
    if let Some(stream) = d.stream.take() {
        let mut spent = 0;
        let mut open = true;
        loop {
            match stream.try_recv() {
                Ok(Ok(band)) => {
                    spent += band.rgba.len();
                    upload_band(host, d.width, &band, 1, d.x, size, &mut d.tiles);
                    if spent >= UPLOAD_BUDGET {
                        ui.request_repaint();
                        break;
                    }
                }
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                    open = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if open {
            d.stream = Some(stream);
        }
    }
    // Wanted: zoomed past the preview (a preview pixel bigger than a screen pixel),
    // the zoom held, and the view not inside what's fetched or being fetched.
    let screen_px = zoom * host.pixels_per_point();
    let wanted = st.factor > 1 && st.done && screen_px * st.factor as f32 > 1.25;
    let now = Instant::now();
    // Whole screen pixels of position, a hundredth of a percent of zoom: what moved.
    let key = [(view.x * screen_px).round() as i32, (view.y * screen_px).round() as i32, (zoom * 1e4).round() as i32];
    let held = match d.view_since {
        Some((k, since)) if k == key => now - since >= DETAIL_SETTLE,
        _ => {
            d.view_since = Some((key, now));
            false
        }
    };
    if wanted && !held {
        ui.request_repaint_in(DETAIL_SETTLE.as_secs_f32());
    }
    let covered = d.region.is_some_and(|g| g.x <= view.x && g.y <= view.y && g.right() >= view.right() && g.bottom() >= view.bottom());
    if !wanted || !held || covered || d.stream.is_some() {
        return;
    }
    // The view and a margin, within the image and the budget.
    let grow = |v: Rect, k: f32| Rect::new(v.x - v.w * k, v.y - v.h * k, v.w * (1.0 + 2.0 * k), v.h * (1.0 + 2.0 * k));
    let clamp = |v: Rect| {
        let (x0, y0) = (v.x.max(0.0).floor(), v.y.max(0.0).floor());
        let (x1, y1) = (v.right().min(size.x).ceil(), v.bottom().min(size.y).ceil());
        Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    };
    let mut region = clamp(grow(view, 0.25));
    if region.w * region.h * 4.0 > DETAIL_BYTES as f32 {
        region = clamp(view);
    }
    if region.w < 1.0 || region.h < 1.0 || region.w * region.h * 4.0 > DETAIL_BYTES as f32 {
        return;
    }
    // What's fetched elsewhere goes: this region replaces it.
    for (t, _) in d.tiles.drain(..) {
        host.free(t);
    }
    let (x, w, y0, y1) = (region.x as u32, region.w as u32, region.y as u32, region.bottom() as u32);
    d.region = Some(region);
    d.x = x;
    d.width = w;
    let crop = move |band: Band, send: &mut dyn FnMut(Band) -> bool| {
        let rows = (band.rgba.len() / (width as usize * 4).max(1)) as u32;
        match crop_band(&band, width, x, w, y0, y1) {
            Some(cropped) => send(cropped) && band.y + rows < y1,
            // Above the region: on to the next band; below it: done.
            None => band.y + rows < y1,
        }
    };
    match decode(source.clone(), host.waker(), crop) {
        Ok(rx) => d.stream = Some(rx),
        Err(e) => st.error = Some(e),
    }
}

pub fn raster(ui: &mut Ui, r: &Raster, st: &mut RasterView, host: &mut dyn Host) {
    feed(ui, r, st, host);
    let (w, h) = (r.width(), r.height());
    let progress = match (&st.error, st.done) {
        (Some(e), _) => Some(format!("  ·  stopped at row {} of {h}: {e}", st.rows)),
        (None, false) if matches!(r.pixels(), Pixels::Bands(_)) => Some(format!("  ·  decoding {}%", st.rows as u64 * 100 / h.max(1) as u64)),
        _ => None,
    };
    let id = ui.make_id("raster");
    let resp = ui.interact_drag(id);
    let rect = resp.rect; // last frame's: one frame of latency, as libgui documents
    let size = Vec2::new(w as f32, h as f32);
    let fit = move |rect: Rect| if rect.w > 0.0 { (rect.w / size.x).min(rect.h / size.y).min(1.0) } else { 1.0 };
    let zoom = st.zoom.unwrap_or(fit(rect));

    let mut new_zoom = zoom;
    new_zoom *= 1.0 + resp.pinch;
    if resp.hovered {
        new_zoom *= (resp.scroll.y / 300.0).exp() * (1.0 + host.trackpad_pinch());
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

    // The part of the image in view, in image pixels, for a big image's detail.
    let zoom_now = st.zoom.unwrap_or(fit(rect));
    if st.factor > 1 && rect.w > 0.0 {
        let origin = rect.center() + st.offset - size * (zoom_now / 2.0);
        let view = Rect::new((rect.x - origin.x) / zoom_now, (rect.y - origin.y) / zoom_now, rect.w / zoom_now, rect.h / zoom_now);
        detail(ui, r, st, host, view, zoom_now);
    }

    // The image (or its preview), then any detail over it.
    let tiles: Vec<(TextureId, Rect)> = st.tiles.iter().chain(&st.detail.tiles).copied().collect();
    let (zoom, offset) = (st.zoom, st.offset);
    let ppp = host.pixels_per_point().max(1.0);
    let bg = ui.theme.palette.bg_inset;
    let faint = ui.theme.palette.text_muted;
    let frame = Frame { fill: bg, clip: true, ..Frame::none() };
    ui.container(Layout::column(), frame, |ui| {
        ui.add_leaf(id, Layout::leaf(Size::Grow(1.0), Size::Grow(1.0)), Vec2::ZERO, true, move |p, rect| {
            let zoom = zoom.unwrap_or(fit(rect));
            let origin = rect.center() + offset - size * (zoom / 2.0);
            // Edges snapped to device pixels, each shared with the neighbouring tile's:
            // no hairline of background between tiles.
            let snap = |v: f32| (v * ppp).round() / ppp;
            for (t, tile) in &tiles {
                let (x0, y0) = (snap(origin.x + tile.x * zoom), snap(origin.y + tile.y * zoom));
                let (x1, y1) = (snap(origin.x + tile.right() * zoom), snap(origin.y + tile.bottom() * zoom));
                let screen = Rect::new(x0, y0, x1 - x0, y1 - y0);
                if screen.intersect(&rect).is_some() {
                    p.image_with_alpha(screen, *t, [0.0, 0.0, 1.0, 1.0], 0.0, Color::WHITE, ImageAlpha::Straight);
                }
            }
            let label = format!("{:.0}%  ·  {w} × {h}{}", zoom * 100.0, progress.as_deref().unwrap_or(""));
            p.text(Vec2::new(rect.x + 8.0, rect.y + rect.h - 22.0), 12.0, faint, label);
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_big_images_get_a_preview() {
        assert_eq!(preview_factor(4000, 3000), 1, "a 12 MP photo goes whole");
        let f = preview_factor(8064, 6048);
        assert!(f >= 2 && 8064u32.div_ceil(f) <= PREVIEW_SIDE, "48 MP: factor {f}");
        assert_eq!(preview_factor(8192, 8192), 2);
    }

    #[test]
    fn detail_takes_the_region_out_of_each_band() {
        // A 6×4 image whose pixels say where they are: (x, y).
        let px = |x: u32, y: u32| [x as u8, y as u8, 0, 255];
        let band = |y0: u32, rows: u32| Band::new(y0, (y0..y0 + rows).flat_map(|y| (0..6).flat_map(move |x| px(x, y))).collect());
        // Columns 2..5, rows 1..3, from a band of rows 0..2: its row 1 only.
        let c = crop_band(&band(0, 2), 6, 2, 3, 1, 3).unwrap();
        assert_eq!(c.y, 1);
        assert_eq!(c.rgba, [px(2, 1), px(3, 1), px(4, 1)].concat());
        // A band of rows 2..4: its row 2.
        let c = crop_band(&band(2, 2), 6, 2, 3, 1, 3).unwrap();
        assert_eq!((c.y, c.rgba.len()), (2, 3 * 4));
        assert_eq!(&c.rgba[..4], &px(2, 2));
        // Outside the rows: nothing.
        assert!(crop_band(&band(3, 1), 6, 2, 3, 1, 3).is_none());
    }

    #[test]
    fn downscaling_averages_each_cell_and_covers_the_edges() {
        // 5×3, factor 2: a 3×2 preview; the last column and row are half cells.
        let (w, h) = (5u32, 3u32);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[(x * 10) as u8, (y * 100) as u8, 7, 255]);
            }
        }
        let mut down = Downscale::new(w, h, 2);
        let mut out = Vec::new();
        // Fed in two bands, split mid-cell.
        assert!(down.band(Band::new(0, rgba[..(w * 4) as usize].to_vec()), &mut |b| {
            out.push(b);
            true
        }));
        assert!(down.band(Band::new(1, rgba[(w * 4) as usize..].to_vec()), &mut |b| {
            out.push(b);
            true
        }));
        let px: Vec<u8> = out.iter().flat_map(|b| b.rgba.clone()).collect();
        assert_eq!(out[0].y, 0);
        assert_eq!(px.len(), 3 * 2 * 4);
        // Row 0, cell 0: x 0,10 and y 0,100 → 5, 50.
        assert_eq!(&px[0..4], &[5, 50, 7, 255]);
        // Row 0, cell 2: x 40 alone.
        assert_eq!(&px[8..12], &[40, 50, 7, 255]);
        // Row 1 (the last, one source row): y 200.
        assert_eq!(&px[12..16], &[5, 200, 7, 255]);
    }
}
