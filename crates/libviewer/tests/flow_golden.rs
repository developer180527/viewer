//! The formatted-text view, drawn: a document using every block and mark, in the
//! app's faces (Inter, with its bold, italic and bold italic, and Hack), light and
//! dark, compared with `tests/golden/`. After an intended change:
//!
//!     UPDATE_GOLDEN=1 cargo test -p libviewer --test flow_golden
//!
//! and look at the new PNGs before committing them. A failing render leaves what was
//! drawn in `target/tmp/flow-golden/`.

use std::sync::Arc;

use libfileparser_core::{Block, FileSource, Inline, OwnedFlow, Raster};
use libgui::*;
use libviewer::{Frame3d, Host, ImageOpener, SceneGpu};

const INTER: &[u8] = include_bytes!("../../surface/assets/Inter.ttf");
const INTER_BOLD: &[u8] = include_bytes!("../../surface/assets/Inter-Bold.ttf");
const INTER_ITALIC: &[u8] = include_bytes!("../../surface/assets/Inter-Italic.ttf");
const INTER_BOLD_ITALIC: &[u8] = include_bytes!("../../surface/assets/Inter-BoldItalic.ttf");
const HACK: &[u8] = include_bytes!("../../surface/assets/Hack-Regular.ttf");

/// Pixels that may differ: rasterisers round slightly differently across platforms.
const PIXEL_TOLERANCE: f64 = 0.01;
const SIZE: (f32, f32) = (640.0, 900.0);

fn t(s: &str) -> Inline {
    Inline::Text(s.into())
}

fn document() -> Vec<Block> {
    vec![
        Block::Heading { level: 1, text: vec![t("Heading one")] },
        Block::Paragraph(vec![
            t("Plain, "),
            Inline::Strong(vec![t("bold")]),
            t(", "),
            Inline::Emphasis(vec![t("italic")]),
            t(", "),
            Inline::Strong(vec![Inline::Emphasis(vec![t("bold italic")])]),
            t(", "),
            Inline::Code("code".into()),
            t(", "),
            Inline::Strikethrough(vec![t("struck")]),
            t(" and "),
            Inline::Link { target: "https://example.org".into(), text: vec![t("a link")] },
            t("."),
        ]),
        Block::Heading { level: 2, text: vec![t("Heading two")] },
        Block::Heading { level: 3, text: vec![t("Heading three")] },
        Block::List {
            ordered: false,
            start: 1,
            items: vec![
                vec![Block::Paragraph(vec![t("First item")])],
                vec![Block::Paragraph(vec![t("Second, "), Inline::Strong(vec![t("bold")])]), Block::List { ordered: false, start: 1, items: vec![vec![Block::Paragraph(vec![t("Nested")])]] }],
            ],
        },
        Block::List { ordered: true, start: 3, items: vec![vec![Block::Paragraph(vec![t("Three")])], vec![Block::Paragraph(vec![t("Four")])]] },
        Block::Quote(vec![Block::Paragraph(vec![t("A quote with "), Inline::Emphasis(vec![t("emphasis")]), t(".")])]),
        Block::Code { language: Some("rust".into()), text: "fn main() {\n    println!(\"hi\");\n}".into() },
        Block::Table { header_rows: 1, rows: vec![vec![vec![t("Name")], vec![t("Size")]], vec![vec![t("alpha")], vec![t("12")]], vec![vec![Inline::Strong(vec![t("beta")])], vec![t("3")]]] },
        Block::Image { reference: "media/stripes.raw".into(), alt: "Stripes".into() },
        Block::Image { reference: "media/wide.raw".into(), alt: "A wide one".into() },
        Block::Image { reference: "missing.png".into(), alt: "Not in the document".into() },
        Block::Rule,
        Block::Paragraph(vec![t("The end.")]),
    ]
}

/// A test image file: its width and height (u32, little-endian), then RGBA.
fn raw(w: u32, h: u32, px: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut b = [w.to_le_bytes(), h.to_le_bytes()].concat();
    for y in 0..h {
        for x in 0..w {
            b.extend_from_slice(&px(x, y));
        }
    }
    b
}

/// What the app's image readers would do, for the test's raw images.
fn opener() -> ImageOpener {
    Arc::new(|file: &FileSource| {
        let b = file.range(0, file.len() as usize).to_vec();
        let (w, h) = (u32::from_le_bytes(b.get(0..4)?.try_into().ok()?), u32::from_le_bytes(b.get(4..8)?.try_into().ok()?));
        Raster::new(w, h, b[8..].to_vec()).ok()
    })
}

/// The app's side, drawn on the CPU: images to the soft renderer, no 3D.
struct TestHost {
    soft: libgui_soft::SoftRenderer,
    uploads: Vec<(u32, u32)>,
}

impl Host for TestHost {
    fn max_side(&self) -> u32 {
        8192
    }
    fn upload(&mut self, width: u32, height: u32, rgba: &[u8]) -> TextureId {
        self.uploads.push((width, height));
        self.soft.register_texture(libgui_soft::Texture { width, height, data: rgba.to_vec() })
    }
    fn pixels_per_point(&self) -> f32 {
        1.0
    }
    fn image_opener(&self) -> Option<ImageOpener> {
        Some(opener())
    }
    fn settles(&self) -> bool {
        true
    }
    fn free(&mut self, _: TextureId) {}
    fn render_scene(&mut self, _: &mut SceneGpu, _: &rendererx::scene_data::CpuScene, _: (u32, u32), _: &rendererx::Camera, _: [f32; 4]) -> Frame3d {
        Frame3d::failed("no 3D in this test")
    }
}

fn flow() -> Arc<OwnedFlow> {
    let stripes = raw(160, 60, |x, _| if (x / 20) % 2 == 0 { [220, 60, 40, 255] } else { [40, 90, 200, 255] });
    let wide = raw(3000, 40, |x, _| [(x * 255 / 3000) as u8, 160, 80, 255]);
    Arc::new(OwnedFlow::new(document()).with_resource("media/stripes.raw", FileSource::from_bytes("stripes.raw", stripes)).with_resource("media/wide.raw", FileSource::from_bytes("wide.raw", wide)))
}

fn render(dark: bool) -> image::RgbaImage {
    let theme = if dark { Theme::dark() } else { Theme::light() };
    let mut ui = Ui::with_rasterizer(theme, Box::new(FontStack::from_fonts(&[INTER]).unwrap()));
    for (role, bytes) in [(FontRole::Bold, INTER_BOLD), (FontRole::Italic, INTER_ITALIC), (FontRole::BoldItalic, INTER_BOLD_ITALIC), (FontRole::Mono, HACK)] {
        let face = ui.fonts.add_font(bytes).unwrap();
        ui.set_font_role(role, face);
    }
    let mut view = libviewer::FlowView::new(flow());
    let scale = 1.0;
    let info = FrameInfo { screen_size: Vec2::new(SIZE.0, SIZE.1), scale, dt: 1.0 / 60.0 };
    let mut host = TestHost { soft: libgui_soft::SoftRenderer::new(), uploads: Vec::new() };
    let mut last = None;
    // Rects are a frame late, so settle first, as the app's screenshots do.
    for _ in 0..3 {
        ui.begin_frame(info);
        libviewer::flow(&mut ui, &mut view, &mut host);
        let out = ui.end_frame();
        last = Some(host.soft.render_to_image(&out, SIZE.0 as u32, SIZE.1 as u32));
    }
    let img = last.unwrap();
    image::RgbaImage::from_raw(img.width, img.height, img.data).unwrap()
}

fn check(name: &str, got: &image::RgbaImage) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let path = dir.join(format!("{name}.png"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        got.save(&path).unwrap();
        return;
    }
    let want = image::open(&path).unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display())).to_rgba8();
    assert_eq!(want.dimensions(), got.dimensions(), "{name}: size");
    let differ = want.pixels().zip(got.pixels()).filter(|(a, b)| a.0.iter().zip(b.0.iter()).any(|(x, y)| x.abs_diff(*y) > 24)).count();
    let share = differ as f64 / (want.width() * want.height()) as f64;
    if share > PIXEL_TOLERANCE {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/flow-golden");
        std::fs::create_dir_all(&out).unwrap();
        got.save(out.join(format!("{name}.png"))).unwrap();
        panic!("{name}: {:.1}% of pixels differ from {} (drawn: {})", share * 100.0, path.display(), out.join(format!("{name}.png")).display());
    }
}

/// The images a document names are opened, scaled to 2048 px at most and uploaded
/// once; one it doesn't carry shows its alt text.
#[test]
fn images_are_decoded_scaled_and_uploaded_once() {
    let theme = Theme::light();
    let mut ui = Ui::with_rasterizer(theme, Box::new(FontStack::from_fonts(&[INTER]).unwrap()));
    let mut view = libviewer::FlowView::new(flow());
    let mut host = TestHost { soft: libgui_soft::SoftRenderer::new(), uploads: Vec::new() };
    for _ in 0..3 {
        ui.begin_frame(FrameInfo { screen_size: Vec2::new(SIZE.0, SIZE.1), scale: 1.0, dt: 1.0 / 60.0 });
        libviewer::flow(&mut ui, &mut view, &mut host);
        let _ = ui.end_frame();
    }
    assert_eq!(host.uploads, vec![(160, 60), (2048, 27)]);
    assert_eq!(view.textures().len(), 2);
}

#[test]
fn formatted_text_light() {
    check("flow-light", &render(false));
}

#[test]
fn formatted_text_dark() {
    check("flow-dark", &render(true));
}
