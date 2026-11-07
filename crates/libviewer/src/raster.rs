//! Images: uploaded to the GPU in tiles, a budget's worth per frame, and drawn with
//! zoom and pan. An image in bands is decoded on a thread of the view's own, pulling
//! from its `BandSource` a band at a time, at most two bands ahead of the GPU: the
//! whole RGBA image never sits in memory, and closing the page stops the decoder.

use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

use libfileparser_core::{Band, Pixels, Raster};
use libgui::*;

use crate::Host;

/// Images go to the GPU in tiles of this side, a few per frame (`UPLOAD_BUDGET`):
/// staging memory stays small and the first tiles show sooner.
const TILE: u32 = 2048;
const UPLOAD_BUDGET: usize = 48 << 20;

#[derive(Default)]
pub struct RasterView {
    tiles: Vec<(TextureId, Rect)>,
    /// Tiles uploaded so far, in row-major order (whole images).
    uploaded: usize,
    /// Rows on the GPU so far (images in bands), and why decoding stopped early.
    rows: u32,
    error: Option<String>,
    /// The decoder thread's bands, once started; `None` again when it's done.
    stream: Option<Receiver<Result<Band, String>>>,
    started: bool,
    done: bool,
    /// `None` means "fit to window".
    zoom: Option<f32>,
    offset: Vec2,
}

impl RasterView {
    /// GPU textures this view owns, to free when its page closes.
    pub fn textures(&self) -> Vec<TextureId> {
        self.tiles.iter().map(|(t, _)| *t).collect()
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

/// Uploads one band as a row of tiles, `TILE` wide and the band's height.
fn upload_band(host: &mut dyn Host, width: u32, band: &Band, st: &mut RasterView) {
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
        st.tiles.push((id, Rect::new(tx as f32, band.y as f32, w as f32, rows as f32)));
    }
    st.rows = st.rows.max(band.y + rows);
}

/// Moves pixels to the GPU: a whole image a budget's worth of tiles per frame, one in
/// bands as its decoder delivers them.
fn feed(ui: &mut Ui, r: &Raster, st: &mut RasterView, host: &mut dyn Host) {
    if st.done {
        return;
    }
    let (width, height) = (r.width(), r.height());
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
                // Two bands ahead at most: a hidden window makes the decoder wait.
                let (tx, rx) = sync_channel::<Result<Band, String>>(2);
                let (source, wake) = (source.clone(), host.waker());
                let spawned = std::thread::Builder::new().name("image decode".into()).spawn(move || {
                    let send = |item| {
                        let ok = tx.send(item).is_ok();
                        wake();
                        ok
                    };
                    match source.bands() {
                        Ok(bands) => {
                            for band in bands {
                                let failed = band.is_err();
                                if !send(band.map_err(|e| e.to_string())) || failed {
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
                });
                match spawned {
                    Ok(_) => st.stream = Some(rx),
                    Err(e) => st.error = Some(format!("no thread to decode on: {e}")),
                }
            }
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
                                upload_band(host, width, &band, st);
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

    let (tiles, zoom, offset) = (st.tiles.clone(), st.zoom, st.offset);
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
