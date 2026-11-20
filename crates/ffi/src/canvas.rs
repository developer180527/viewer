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

use libgui::{Cursor, InputEvent, Modifiers, PointerButton, Vec2, WheelUnit};
use viewer_surface::views::Action;
use viewer_surface::{Canvas, Wake};

use crate::ViewerSession;

thread_local! {
    static CANVASES: RefCell<HashMap<u64, Canvas>> = RefCell::new(HashMap::new());
}
static NEXT: AtomicU64 = AtomicU64::new(1);
/// Canvases whose object was dropped on another thread: dropped on the main thread
/// at its next call.
static ORPHANS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

fn with<R>(id: u64, f: impl FnOnce(&mut Canvas) -> R) -> Option<R> {
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

/// What a frame asks of the shell.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct Frame {
    /// Seconds until the next frame is wanted (0: every display refresh); `None`:
    /// only after input or a wake.
    pub repaint_after: Option<f64>,
    pub cursor: CursorShape,
    /// The view opened something (an entry of a container): refresh the chrome.
    pub opened: bool,
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
        CANVASES.with(|c| c.borrow_mut().insert(id, canvas));
        Ok(Arc::new(Self { id, session, thread: std::thread::current().id() }))
    }

    /// Draws the current page if anything changed. Returns what the frame asks for.
    pub fn draw(&self) -> Frame {
        let mut action = None;
        let frame = with(self.id, |c| {
            let session = self.session.session();
            let drawn = c.draw(session.pages(), session.current());
            action = drawn.action;
            Frame { repaint_after: drawn.repaint_after.map(f64::from), cursor: cursor(drawn.cursor), opened: false }
        });
        let mut frame = frame.unwrap_or(Frame { repaint_after: None, cursor: CursorShape::Arrow, opened: false });
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

    pub fn set_dark(&self, dark: bool) {
        with(self.id, |c| c.set_dark(dark));
    }

    /// Something outside changed what's shown (the shell was woken): build the next
    /// frame.
    pub fn touch(&self) {
        with(self.id, |c| c.touch());
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

    /// A trackpad pinch, as AppKit's magnification (0.1 is 10% bigger).
    pub fn pinch(&self, delta: f32) {
        with(self.id, |c| c.pinch(delta));
    }

    pub fn modifiers(&self, shift: bool, ctrl: bool, alt: bool, command: bool) {
        self.push(InputEvent::ModifiersChanged(Modifiers { shift, ctrl, alt, logo: command }));
    }

    /// The view lost focus: every button and key is released.
    pub fn focus_lost(&self) {
        self.push(InputEvent::FocusLost);
    }
}

impl ViewerCanvas {
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
