//! The fonts every shell draws with: Inter for text, Hack for code, Inter's bold and
//! italic faces for formatted text, and the system's fonts behind each for scripts
//! they lack.

use anyhow::anyhow;
use libgui::*;

const INTER: &[u8] = include_bytes!("../assets/Inter.ttf");
const HACK: &[u8] = include_bytes!("../assets/Hack-Regular.ttf");
// Inter's static bold, italic and bold italic (v4.0, as `Inter.ttf`; libgui draws a
// variable font at its default weight only), registered the first time formatted
// text is drawn: most files never need them, and each costs a few ms to register.
const INTER_BOLD: &[u8] = include_bytes!("../assets/Inter-Bold.ttf");
const INTER_ITALIC: &[u8] = include_bytes!("../assets/Inter-Italic.ttf");
const INTER_BOLD_ITALIC: &[u8] = include_bytes!("../assets/Inter-BoldItalic.ttf");

/// A `Ui` with the app's fonts and the platform's keymap.
pub fn new_ui(dark: bool) -> anyhow::Result<Ui> {
    let theme = if dark { Theme::dark() } else { Theme::light() };
    // Inter first, then the system's fonts, opened lazily the first time a
    // script Inter lacks (CJK file names, say) is drawn.
    let stack = FontStack::from_fonts(&[INTER]).map_err(|e| anyhow!("loading fonts: {e:?}"))?;
    let mut ui = Ui::with_rasterizer(theme, Box::new(stack.with_source(Box::new(LazySystemFonts))));
    let mono = ui.fonts.add_font(HACK).map_err(|e| anyhow!("loading mono font: {e:?}"))?;
    ui.set_font_role(FontRole::Mono, mono);
    libgui_keymap::Keymap::<u8>::for_current_platform().install(&mut ui);
    Ok(ui)
}

/// Registers bold, italic and bold italic faces once per thread, before formatted
/// text first draws; until then those roles fall back to the body face.
pub fn ensure_rich_faces(ui: &mut Ui) {
    thread_local! {
        static TRIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if TRIED.with(|t| t.replace(true)) {
        return;
    }
    for (role, bytes) in [(FontRole::Bold, INTER_BOLD), (FontRole::Italic, INTER_ITALIC), (FontRole::BoldItalic, INTER_BOLD_ITALIC)] {
        // With the system's fonts behind it, as the body face has, so bold Japanese
        // or Arabic is drawn, in the system's face, rather than as boxes.
        match FontStack::from_fonts(&[bytes]) {
            Ok(stack) => {
                let face = ui.fonts.add_rasterizer(Box::new(stack.with_source(Box::new(LazySystemFonts))));
                ui.set_font_role(role, face);
            }
            Err(e) => eprintln!("viewer: loading the {role:?} face: {e:?}"),
        }
    }
}

/// The installed fonts, listed on a thread (`scan_system_fonts`, started first
/// thing): listing them reads every font directory, 5–10 ms warm but close to
/// 200 ms from a cold disk, and most files never need one.
static SYSTEM_FONTS: std::sync::OnceLock<libgui_sysfonts::SystemFonts> = std::sync::OnceLock::new();
static SYSTEM_FONTS_SCAN: std::sync::Mutex<Option<std::thread::JoinHandle<libgui_sysfonts::SystemFonts>>> =
    std::sync::Mutex::new(None);

/// Starts listing the system's fonts on a thread. Optional: without it, the first
/// character that needs one lists them then.
pub fn scan_system_fonts() {
    *SYSTEM_FONTS_SCAN.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::thread::spawn(libgui_sysfonts::SystemFonts::new));
}

/// The system fonts, waiting for the scan the first time a character needs them.
struct LazySystemFonts;

impl FontSource for LazySystemFonts {
    fn face_for(&self, ch: char) -> Option<Box<dyn FontRasterizer>> {
        SYSTEM_FONTS
            .get_or_init(|| {
                let scan = SYSTEM_FONTS_SCAN.lock().unwrap_or_else(|e| e.into_inner()).take();
                scan.and_then(|t| t.join().ok()).unwrap_or_default()
            })
            .face_for(ch)
    }
}
