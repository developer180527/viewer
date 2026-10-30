//! Tiled image canvas with zoom and pan. Images larger than the GPU's maximum
//! texture size are split into several textures.

use eframe::egui::{self, Color32, Key, Pos2, Rect, Sense, TextureFilter, TextureOptions, Vec2};
use viewer_core::Raster;

#[derive(Default)]
pub struct RasterState {
    tiles: Vec<(egui::TextureHandle, Rect)>,
    /// `None` means "fit to window".
    zoom: Option<f32>,
    offset: Vec2,
}

const OPTIONS: TextureOptions = TextureOptions {
    // Crisp pixels when zoomed in, smooth when zoomed out.
    magnification: TextureFilter::Nearest,
    minification: TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: Some(TextureFilter::Linear),
};

fn upload(ctx: &egui::Context, r: &Raster) -> Vec<(egui::TextureHandle, Rect)> {
    let tile = ctx.input(|i| i.max_texture_side).max(256) as u32;
    let mut tiles = Vec::new();
    for ty in (0..r.height).step_by(tile as usize) {
        for tx in (0..r.width).step_by(tile as usize) {
            let (w, h) = (tile.min(r.width - tx), tile.min(r.height - ty));
            let mut buf = Vec::with_capacity((w * h * 4) as usize);
            for y in ty..ty + h {
                let row = ((y * r.width + tx) * 4) as usize;
                buf.extend_from_slice(&r.rgba[row..row + (w * 4) as usize]);
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &buf);
            let tex = ctx.load_texture(format!("tile{tx}x{ty}"), img, OPTIONS);
            let rect = Rect::from_min_size(Pos2::new(tx as f32, ty as f32), Vec2::new(w as f32, h as f32));
            tiles.push((tex, rect));
        }
    }
    tiles
}

pub fn show(ui: &mut egui::Ui, r: &Raster, st: &mut RasterState) {
    if st.tiles.is_empty() && r.width > 0 && r.height > 0 {
        st.tiles = upload(ui.ctx(), r);
    }
    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let size = Vec2::new(r.width as f32, r.height as f32);
    let fit = (rect.width() / size.x).min(rect.height() / size.y).min(1.0);
    let zoom = st.zoom.unwrap_or(fit);

    let mut new_zoom = zoom;
    if resp.hovered() {
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        new_zoom *= pinch * (scroll / 300.0).exp();
    }
    if resp.has_focus() || resp.hovered() {
        ui.input(|i| {
            if i.key_pressed(Key::Num0) {
                st.zoom = None;
                st.offset = Vec2::ZERO;
            }
            if i.key_pressed(Key::Num1) {
                new_zoom = 1.0;
            }
            if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) {
                new_zoom *= 1.25;
            }
            if i.key_pressed(Key::Minus) {
                new_zoom /= 1.25;
            }
        });
    }
    if resp.double_clicked() {
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
        let anchor = resp.hover_pos().unwrap_or(rect.center());
        let image_pt = (anchor - rect.center() - st.offset) / zoom;
        st.offset = anchor - rect.center() - image_pt * new_zoom;
        st.zoom = Some(new_zoom);
    }
    if resp.dragged() {
        st.offset += resp.drag_delta();
        st.zoom.get_or_insert(zoom);
    }
    let zoom = st.zoom.unwrap_or(fit);

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    let origin = rect.center() + st.offset - size * zoom / 2.0;
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    for (tex, tile) in &st.tiles {
        let screen = Rect::from_min_size(origin + tile.min.to_vec2() * zoom, tile.size() * zoom);
        if screen.intersects(rect) {
            painter.image(tex.id(), screen, uv, Color32::WHITE);
        }
    }
    painter.text(
        rect.left_bottom() + Vec2::new(8.0, -8.0),
        egui::Align2::LEFT_BOTTOM,
        format!("{:.0}%  ·  {} × {}", zoom * 100.0, r.width, r.height),
        egui::FontId::proportional(12.0),
        ui.visuals().weak_text_color(),
    );
}
