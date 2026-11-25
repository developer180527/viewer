//! The Metal view's Rust side: a [`ViewerCanvas`] draws the session's current page with
//! libviewer, the renderers the libgui app draws with, into the `CAMetalLayer` the
//! shell hands it. The shell forwards input in points (origin top left), calls
//! [`ViewerCanvas::draw`] when woken, after input, or when [`Frame::repaint_after`]
//! says, and refreshes its chrome when a frame opened something.
//!
//! libgui's `Ui` is not `Send`, so the canvas itself lives in a slot of the main
//! thread, and the object Swift holds only names it. Every call is the main thread's;
//! one from elsewhere finds no canvas and does nothing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use libgui::{Cursor, InputEvent, Key, Modifiers, PointerButton, TouchPhase as Phase, Vec2, WheelUnit};
use viewer_surface::chrome::{Request, ViewerApp};
use viewer_surface::views::Action;
use viewer_surface::{Canvas, Wake};

use crate::ViewerSession;

/// A canvas, and for one showing libgui's whole interface, that interface.
struct Slot {
    canvas: Canvas,
    app: Option<ViewerApp>,
}

thread_local! {
    static CANVASES: RefCell<HashMap<u64, Slot>> = RefCell::new(HashMap::new());
}
static NEXT: AtomicU64 = AtomicU64::new(1);
/// Canvases whose object was dropped on another thread: dropped on the main thread
/// at its next call.
static ORPHANS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

fn with<R>(id: u64, f: impl FnOnce(&mut Canvas) -> R) -> Option<R> {
    with_slot(id, |s| f(&mut s.canvas))
}

fn with_slot<R>(id: u64, f: impl FnOnce(&mut Slot) -> R) -> Option<R> {
    let orphans = std::mem::take(&mut *ORPHANS.lock().unwrap_or_else(|e| e.into_inner()));
    CANVASES.with(|c| {
        let mut c = c.borrow_mut();
        for o in orphans {
            c.remove(&o);
        }
        c.get_mut(&id).map(f)
    })
}

#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum CanvasError {
    /// No GPU, or the layer can't be drawn to.
    Gpu(String),
}

impl std::fmt::Display for CanvasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gpu(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for CanvasError {}

/// The pointer shape a frame asks for.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Arrow,
    Pointer,
    ResizeHorizontal,
    ResizeVertical,
    ResizeDiagonal,
    Grab,
    Grabbing,
    Text,
    Crosshair,
}

/// What libgui's interface asks the shell to do: its picker.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum AppRequest {
    /// Open…: a file or folder to open (`open_granted` it).
    Open,
    /// Open Folder… on a blocked model: ask for `folder`, then reopen `reopen` from
    /// inside it.
    AllowFolder { reopen: String, folder: Option<String> },
}

/// What a frame asks of the shell.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct Frame {
    /// Seconds until the next frame is wanted (0: every display refresh); `None`:
    /// only after input or a wake.
    pub repaint_after: Option<f64>,
    pub cursor: CursorShape,
    /// What's open (or the settings) changed in this frame: an entry opened, Back,
    /// a setting. Refresh what's shown from the session.
    pub opened: bool,
    /// libgui's interface asked for the picker.
    pub request: Option<AppRequest>,
}

/// Where a finger is in its touch.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    Start,
    Move,
    End,
    /// The system took the touch (a gesture of its own): no tap.
    Cancel,
}

/// A mouse button.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Primary,
    Secondary,
    Middle,
}

#[derive(uniffi::Object)]
pub struct ViewerCanvas {
    id: u64,
    session: Arc<ViewerSession>,
    thread: std::thread::ThreadId,
}

#[uniffi::export]
impl ViewerCanvas {
    /// A canvas on `layer`, a `CAMetalLayer`'s address, `width`×`height` pixels at
    /// `scale` pixels per point. The layer must outlive the canvas. Main thread only.
    #[uniffi::constructor]
    pub fn new(session: Arc<ViewerSession>, layer: u64, width: u32, height: u32, scale: f32, dark: bool) -> Result<Arc<Self>, CanvasError> {
        if layer == 0 {
            return Err(CanvasError::Gpu("no layer".into()));
        }
        let waker = session.waker.clone();
        let wake: Wake = Arc::new(move || waker.wake());
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "visionos"))]
        // SAFETY: the caller's: a live CAMetalLayer that outlives the canvas.
        let canvas = unsafe { Canvas::on_metal_layer(layer as usize as *mut std::ffi::c_void, width, height, scale, dark, wake) };
        #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "visionos")))]
        let canvas: anyhow::Result<Canvas> = {
            let _ = (width, height, scale, dark, wake);
            Err(anyhow::anyhow!("a Metal layer needs an Apple platform"))
        };
        let canvas = canvas.map_err(|e| CanvasError::Gpu(format!("{e:#}")))?;
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        CANVASES.with(|c| c.borrow_mut().insert(id, Slot { canvas, app: None }));
        Ok(Arc::new(Self { id, session, thread: std::thread::current().id() }))
    }

    /// A canvas showing libgui's whole interface for the session (the libgui app's
    /// window: toolbar, menus, info, the page), the Interface setting's libgui. It
    /// reads and changes the same session as the native interface, so switching keeps
    /// what's open and where. Main thread only.
    #[uniffi::constructor]
    pub fn app(session: Arc<ViewerSession>, layer: u64, width: u32, height: u32, scale: f32, dark: bool) -> Result<Arc<Self>, CanvasError> {
        let this = Self::new(session.clone(), layer, width, height, scale, dark)?;
        // The interface lends its session to the shared one each frame (see `draw`);
        // between frames it holds this empty one.
        let waker = session.waker.clone();
        let hooks = viewer_core::Hooks::new(Arc::new(move || waker.wake()));
        let mut app = ViewerApp::new(viewer_core::Session::new(hooks), dark);
        app.set_system_dark(dark);
        app.offers_native = true;
        with_slot(this.id, |s| {
            s.canvas.set_touch_screen(cfg!(any(target_os = "ios", target_os = "visionos")));
            s.app = Some(app);
        });
        Ok(this)
    }

    /// The edges of the canvas the system covers (the notch, the home indicator), in
    /// points: libgui's interface keeps its controls inside them.
    pub fn set_safe_area(&self, top: f32, left: f32, bottom: f32, right: f32) {
        with_slot(self.id, |s| {
            if let Some(app) = s.app.as_mut() {
                app.safe_area = libgui::Insets { top, left, bottom, right };
            }
            s.canvas.touch();
        });
    }

    /// Draws the current page (or libgui's interface) if anything changed. Returns
    /// what the frame asks for.
    pub fn draw(&self) -> Frame {
        if let Some(frame) = with_slot(self.id, |s| s.app.is_some().then(|| self.draw_app(s))).flatten() {
            return frame;
        }
        let mut action = None;
        let frame = with(self.id, |c| {
            let mut session = self.session.session();
            // A file opened again goes back to where the reader was.
            let id = session.current().map(|p| p.id);
            if let Some(id) = id
                && !c.has_view(id)
                && let Some(p) = session.place(id)
            {
                c.restore(id, libviewer::PagesPlace { page: p.page as usize, offset: p.offset, zoom: p.zoom });
            }
            let drawn = c.draw(session.pages(), session.current());
            action = drawn.action;
            if let Some(id) = id
                && let Some(p) = c.place(id)
            {
                session.set_place(id, viewer_core::Place { page: p.page.min(u32::MAX as usize) as u32, offset: p.offset, zoom: p.zoom });
            }
            Frame { repaint_after: drawn.repaint_after.map(f64::from), cursor: cursor(drawn.cursor), opened: false, request: None }
        });
        let mut frame = frame.unwrap_or(Frame { repaint_after: None, cursor: CursorShape::Arrow, opened: false, request: None });
        if let Some(Action::OpenEntry(container, index)) = action {
            self.session.session().open_entry(container, index);
            frame.opened = true;
        }
        frame
    }

    /// The view's size in pixels, and its scale (pixels per point).
    pub fn resize(&self, width: u32, height: u32, scale: f32) {
        with(self.id, |c| c.resize(width, height, scale));
    }

    /// On an iPhone or iPad: hints speak of taps and fingers until a mouse or
    /// trackpad hovers.
    pub fn set_touch_screen(&self, touch: bool) {
        with(self.id, |c| c.set_touch_screen(touch));
    }

    /// Whether the view is dark (its appearance). libgui's interface takes it as the
    /// system's, which its Theme setting can override.
    pub fn set_dark(&self, dark: bool) {
        with_slot(self.id, |s| match s.app.as_mut() {
            Some(app) => {
                app.set_system_dark(dark);
                s.canvas.touch();
            }
            None => s.canvas.set_dark(dark),
        });
    }

    /// Something outside changed what's shown (the shell was woken): build the next
    /// frame.
    pub fn touch(&self) {
        with(self.id, |c| c.touch());
    }

    /// Frees what the canvas can render again (a paged document's tiles, spare
    /// textures): its window went to the background, or the system is short of
    /// memory. It renders what's on screen again when it next draws.
    pub fn trim(&self) {
        with(self.id, |c| c.trim());
    }

    // ---------------------------------------------------------------- input, in points

    pub fn pointer_moved(&self, x: f32, y: f32) {
        self.push(InputEvent::PointerMoved { pos: Vec2::new(x, y) });
    }

    pub fn pointer_left(&self) {
        self.push(InputEvent::PointerLeft);
    }

    pub fn pointer_button(&self, x: f32, y: f32, button: MouseButton, pressed: bool) {
        let button = match button {
            MouseButton::Primary => PointerButton::Primary,
            MouseButton::Secondary => PointerButton::Secondary,
            MouseButton::Middle => PointerButton::Middle,
        };
        self.push(InputEvent::PointerMoved { pos: Vec2::new(x, y) });
        self.push(InputEvent::PointerButton { button, pressed });
    }

    /// A scroll: `precise` for a trackpad or Magic Mouse (points), else wheel lines.
    /// Signs as AppKit's scrolling deltas.
    pub fn wheel(&self, dx: f32, dy: f32, precise: bool) {
        let unit = if precise { WheelUnit::Pixel } else { WheelUnit::Line };
        self.push(InputEvent::Wheel { delta: Vec2::new(dx, dy), unit });
    }

    /// A finger, by a number that stays its own from `Start` to `End` or `Cancel`.
    /// libgui makes taps, drags, flings and two-finger pinches of them.
    pub fn touch_point(&self, id: u64, phase: TouchPhase, x: f32, y: f32) {
        let phase = match phase {
            TouchPhase::Start => Phase::Start,
            TouchPhase::Move => Phase::Move,
            TouchPhase::End => Phase::End,
            TouchPhase::Cancel => Phase::Cancel,
        };
        self.push(InputEvent::Touch { id, phase, pos: Vec2::new(x, y) });
    }

    /// A trackpad pinch, as AppKit's magnification (0.1 is 10% bigger).
    pub fn pinch(&self, delta: f32) {
        with(self.id, |c| c.pinch(delta));
    }

    pub fn modifiers(&self, shift: bool, ctrl: bool, alt: bool, command: bool) {
        self.push(InputEvent::ModifiersChanged(Modifiers { shift, ctrl, alt, logo: command }));
    }

    /// A key, by AppKit's `NSEvent.keyCode` (a virtual key code). Keys it doesn't
    /// know are ignored. Returns whether it was one.
    pub fn key_mac(&self, code: u16, pressed: bool, repeat: bool) -> bool {
        self.key(mac_key(code), pressed, repeat)
    }

    /// A key, by USB HID usage (UIKit's `UIKey.keyCode`).
    pub fn key_hid(&self, usage: u16, pressed: bool, repeat: bool) -> bool {
        self.key(Key::from_hid_usage(usage), pressed, repeat)
    }

    /// Typed text (characters, not shortcuts).
    pub fn text(&self, text: String) {
        if !text.is_empty() {
            self.push(InputEvent::Text(text));
        }
    }

    /// The view lost focus: every button and key is released.
    pub fn focus_lost(&self) {
        self.push(InputEvent::FocusLost);
    }
}

impl ViewerCanvas {
    /// A frame of libgui's interface, on the shared session: lent to it for the
    /// frame, under the lock, and taken back.
    fn draw_app(&self, s: &mut Slot) -> Frame {
        let app = s.app.as_mut().expect("an app canvas");
        let mut session = self.session.session();
        let before = Snapshot::of(&session);
        app.swap_session(&mut session);
        let drawn = s.canvas.draw_app(app);
        app.swap_session(&mut session);
        let opened = Snapshot::of(&session) != before;
        let request = app.requests.drain(..).next().map(|r| match r {
            Request::Open => AppRequest::Open,
            Request::AllowFolder { reopen } => AppRequest::AllowFolder {
                folder: viewer_core::Session::open_folder_for(&reopen).map(|f| f.display().to_string()),
                reopen: reopen.display().to_string(),
            },
        });
        Frame { repaint_after: drawn.repaint_after.map(f64::from), cursor: cursor(drawn.cursor), opened, request }
    }

    fn key(&self, key: Option<Key>, pressed: bool, repeat: bool) -> bool {
        let Some(key) = key else { return false };
        self.push(InputEvent::Key { key, pressed, repeat });
        true
    }

    fn push(&self, event: InputEvent) {
        with(self.id, |c| {
            c.push(event);
            c.touch();
        });
    }
}

impl Drop for ViewerCanvas {
    fn drop(&mut self) {
        if std::thread::current().id() == self.thread {
            CANVASES.with(|c| c.borrow_mut().remove(&self.id));
        } else {
            ORPHANS.lock().unwrap_or_else(|e| e.into_inner()).push(self.id);
        }
    }
}

fn cursor(c: Cursor) -> CursorShape {
    match c {
        Cursor::Default => CursorShape::Arrow,
        Cursor::Pointer => CursorShape::Pointer,
        Cursor::ResizeHorizontal => CursorShape::ResizeHorizontal,
        Cursor::ResizeVertical => CursorShape::ResizeVertical,
        Cursor::ResizeDiagonal => CursorShape::ResizeDiagonal,
        Cursor::Grab => CursorShape::Grab,
        Cursor::Grabbing => CursorShape::Grabbing,
        Cursor::Text => CursorShape::Text,
        Cursor::Crosshair => CursorShape::Crosshair,
    }
}

/// AppKit's virtual key codes (Carbon's `kVK_*`, ANSI layout positions).
fn mac_key(code: u16) -> Option<Key> {
    use Key::*;
    Some(match code {
        0x00 => A, 0x0B => B, 0x08 => C, 0x02 => D, 0x0E => E, 0x03 => F, 0x05 => G,
        0x04 => H, 0x22 => I, 0x26 => J, 0x28 => K, 0x25 => L, 0x2E => M, 0x2D => N,
        0x1F => O, 0x23 => P, 0x0C => Q, 0x0F => R, 0x01 => S, 0x11 => T, 0x20 => U,
        0x09 => V, 0x0D => W, 0x07 => X, 0x10 => Y, 0x06 => Z,
        0x1D => Num0, 0x12 => Num1, 0x13 => Num2, 0x14 => Num3, 0x15 => Num4,
        0x17 => Num5, 0x16 => Num6, 0x1A => Num7, 0x1C => Num8, 0x19 => Num9,
        0x7A => F1, 0x78 => F2, 0x63 => F3, 0x76 => F4, 0x60 => F5, 0x61 => F6,
        0x62 => F7, 0x64 => F8, 0x65 => F9, 0x6D => F10, 0x67 => F11, 0x6F => F12,
        0x24 => Enter, 0x35 => Escape, 0x33 => Backspace, 0x30 => Tab, 0x31 => Space,
        0x1B => Minus, 0x18 => Equal, 0x21 => BracketLeft, 0x1E => BracketRight,
        0x2A => Backslash, 0x29 => Semicolon, 0x27 => Quote, 0x32 => Backquote,
        0x2B => Comma, 0x2F => Period, 0x2C => Slash,
        0x39 => CapsLock, 0x75 => Delete, 0x73 => Home, 0x77 => End,
        0x74 => PageUp, 0x79 => PageDown,
        0x7C => ArrowRight, 0x7B => ArrowLeft, 0x7D => ArrowDown, 0x7E => ArrowUp,
        0x4B => NumpadDivide, 0x43 => NumpadMultiply, 0x4E => NumpadSubtract,
        0x45 => NumpadAdd, 0x4C => NumpadEnter, 0x41 => NumpadDecimal,
        0x52 => Numpad0, 0x53 => Numpad1, 0x54 => Numpad2, 0x55 => Numpad3,
        0x56 => Numpad4, 0x57 => Numpad5, 0x58 => Numpad6, 0x59 => Numpad7,
        0x5B => Numpad8, 0x5C => Numpad9,
        0x3B => ControlLeft, 0x38 => ShiftLeft, 0x3A => AltLeft, 0x37 => SuperLeft,
        0x3E => ControlRight, 0x3C => ShiftRight, 0x3D => AltRight, 0x36 => SuperRight,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_keys_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for code in 0..=0x7F {
            if let Some(k) = mac_key(code) {
                assert!(seen.insert(k), "{k:?} twice");
            }
        }
        assert_eq!(mac_key(0x79), Some(Key::PageDown));
        assert_eq!(mac_key(0x31), Some(Key::Space));
    }
}

/// What the native interface shows from the session, to tell whether a frame of
/// libgui's changed it.
#[derive(PartialEq)]
struct Snapshot {
    page: Option<u64>,
    pages: usize,
    loading: Option<String>,
    error: Option<String>,
    settings: viewer_core::Settings,
}

impl Snapshot {
    fn of(s: &viewer_core::Session) -> Self {
        Self {
            page: s.current().map(|p| p.id),
            pages: s.pages().len(),
            loading: s.loading().map(str::to_owned),
            error: s.error().map(str::to_owned),
            settings: s.settings().clone(),
        }
    }
}
