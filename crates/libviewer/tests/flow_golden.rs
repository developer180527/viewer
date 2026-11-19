//! The formatted-text view, drawn: a document using every block and mark, in the
//! app's faces (Inter, with its bold, italic and bold italic, and Hack), light and
//! dark, compared with `tests/golden/`. After an intended change:
//!
//!     UPDATE_GOLDEN=1 cargo test -p libviewer --test flow_golden
//!
//! and look at the new PNGs before committing them. A failing render leaves what was
//! drawn in `target/tmp/flow-golden/`.

use libfileparser_core::{Block, Inline, OwnedFlow};
use libgui::*;

const INTER: &[u8] = include_bytes!("../../app/assets/Inter.ttf");
const INTER_BOLD: &[u8] = include_bytes!("../../app/assets/Inter-Bold.ttf");
const INTER_ITALIC: &[u8] = include_bytes!("../../app/assets/Inter-Italic.ttf");
const INTER_BOLD_ITALIC: &[u8] = include_bytes!("../../app/assets/Inter-BoldItalic.ttf");
const HACK: &[u8] = include_bytes!("../../app/assets/Hack-Regular.ttf");

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
        Block::Rule,
        Block::Paragraph(vec![t("The end.")]),
    ]
}

fn render(dark: bool) -> image::RgbaImage {
    let theme = if dark { Theme::dark() } else { Theme::light() };
    let mut ui = Ui::with_rasterizer(theme, Box::new(FontStack::from_fonts(&[INTER]).unwrap()));
    for (role, bytes) in [(FontRole::Bold, INTER_BOLD), (FontRole::Italic, INTER_ITALIC), (FontRole::BoldItalic, INTER_BOLD_ITALIC), (FontRole::Mono, HACK)] {
        let face = ui.fonts.add_font(bytes).unwrap();
        ui.set_font_role(role, face);
    }
    let flow = OwnedFlow::new(document());
    let mut view = libviewer::FlowView::new(&flow);
    let scale = 1.0;
    let info = FrameInfo { screen_size: Vec2::new(SIZE.0, SIZE.1), scale, dt: 1.0 / 60.0 };
    let mut soft = libgui_soft::SoftRenderer::new();
    let mut last = None;
    // Rects are a frame late, so settle first, as the app's screenshots do.
    for _ in 0..3 {
        ui.begin_frame(info);
        libviewer::flow(&mut ui, &mut view);
        let out = ui.end_frame();
        last = Some(soft.render_to_image(&out, SIZE.0 as u32, SIZE.1 as u32));
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

#[test]
fn formatted_text_light() {
    check("flow-light", &render(false));
}

#[test]
fn formatted_text_dark() {
    check("flow-dark", &render(true));
}
