//! PDF pages through MuPDF (docs/pdf.md §2), behind `viewer_core::PageSource`.
//!
//! MuPDF is reached only through shim/shim.c, which turns its setjmp-based errors
//! into return codes. Each document has its own MuPDF context and is used under a
//! mutex, so the source is `Send + Sync` without giving MuPDF locks.

use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, bail};
use viewer_core::{FileSource, PageImage, PageRect, PageSize, PageSource, Result, render_size};

#[repr(C)]
struct VwDoc {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn vw_open(data: *const u8, len: usize, magic: *const c_char, store: usize, err: *mut c_char, err_len: usize) -> *mut VwDoc;
    fn vw_close(d: *mut VwDoc);
    fn vw_needs_password(d: *mut VwDoc) -> c_int;
    fn vw_page_count(d: *mut VwDoc, count: *mut c_int, err: *mut c_char, err_len: usize) -> c_int;
    fn vw_page_bounds(d: *mut VwDoc, index: c_int, out: *mut f32, err: *mut c_char, err_len: usize) -> c_int;
    fn vw_render(d: *mut VwDoc, index: c_int, scale: f32, x: f32, y: f32, w: c_int, h: c_int, rgba: *mut u8, err: *mut c_char, err_len: usize) -> c_int;
    fn vw_metadata(d: *mut VwDoc, key: *const c_char, buf: *mut c_char, len: usize) -> c_int;
}

/// MuPDF's cache of decoded fonts and images, per document.
const STORE_BYTES: usize = 64 << 20;

/// An error buffer for one shim call.
struct Err([c_char; 256]);

impl Err {
    fn new() -> Self {
        Self([0; 256])
    }
    fn ptr(&mut self) -> (*mut c_char, usize) {
        (self.0.as_mut_ptr(), self.0.len())
    }
    fn message(&self) -> String {
        // SAFETY: the shim always NUL-terminates within the buffer (it starts zeroed).
        unsafe { CStr::from_ptr(self.0.as_ptr()) }.to_string_lossy().into_owned()
    }
}

struct Doc(*mut VwDoc);

// SAFETY: a MuPDF context isn't tied to a thread, only not usable from two at once;
// `MupdfDocument` only touches it under its mutex.
unsafe impl Send for Doc {}

impl Drop for Doc {
    fn drop(&mut self) {
        // SAFETY: opened by vw_open, closed once.
        unsafe { vw_close(self.0) }
    }
}

pub struct MupdfDocument {
    // Declared before `source`: the document reads from its bytes, so it goes first.
    doc: Mutex<Doc>,
    _source: FileSource,
    pages: usize,
    sizes: Vec<OnceLock<std::result::Result<PageSize, String>>>,
}

/// Facts about the document for the info panel.
pub struct Info {
    /// MuPDF's name for the format, e.g. "PDF 1.7".
    pub format: Option<String>,
    pub encryption: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
}

impl MupdfDocument {
    /// Opens `source` as `magic` (a MIME type such as "application/pdf", or a file name
    /// whose extension MuPDF knows). Reads the page count only; pages load on demand.
    pub fn open(source: FileSource, magic: &str) -> Result<Self> {
        let magic = CString::new(magic).context("bad document type")?;
        let bytes = source.bytes();
        let mut err = Err::new();
        let (e, el) = err.ptr();
        // SAFETY: `bytes` lives in `source`, which the document keeps (and drops last).
        let d = unsafe { vw_open(bytes.as_ptr(), bytes.len(), magic.as_ptr(), STORE_BYTES, e, el) };
        if d.is_null() {
            bail!("{}", err.message());
        }
        let doc = Doc(d);
        // SAFETY: `doc` is open.
        if unsafe { vw_needs_password(doc.0) } != 0 {
            bail!("This document is protected by a password.");
        }
        let mut count = 0;
        let (e, el) = err.ptr();
        // SAFETY: as above.
        if unsafe { vw_page_count(doc.0, &mut count, e, el) } != 0 {
            bail!("{}", err.message());
        }
        let pages = count.max(0) as usize;
        Ok(Self { doc: Mutex::new(doc), _source: source, pages, sizes: (0..pages).map(|_| OnceLock::new()).collect() })
    }

    pub fn info(&self) -> Info {
        let doc = self.doc.lock().unwrap_or_else(|e| e.into_inner());
        let get = |key: &str| {
            let key = CString::new(key).unwrap();
            let mut buf = [0 as c_char; 512];
            // SAFETY: `doc` is open and locked; the shim NUL-terminates within `buf`.
            let n = unsafe { vw_metadata(doc.0, key.as_ptr(), buf.as_mut_ptr(), buf.len()) };
            if n < 0 {
                return None;
            }
            let s = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        Info {
            format: get("format"),
            encryption: get("encryption").filter(|e| e != "None"),
            title: get("info:Title"),
            author: get("info:Author"),
            creator: get("info:Creator"),
            producer: get("info:Producer"),
        }
    }

    fn bounds(&self, index: usize) -> Result<[f32; 4]> {
        if index >= self.pages {
            bail!("no page {}", index + 1);
        }
        let doc = self.doc.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = [0f32; 4];
        let mut err = Err::new();
        let (e, el) = err.ptr();
        // SAFETY: `doc` is open and locked; `out` holds four floats.
        if unsafe { vw_page_bounds(doc.0, index as c_int, out.as_mut_ptr(), e, el) } != 0 {
            bail!("page {}: {}", index + 1, err.message());
        }
        Ok(out)
    }
}

impl PageSource for MupdfDocument {
    fn backend(&self) -> &'static str {
        "MuPDF"
    }

    fn page_count(&self) -> usize {
        self.pages
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        let Some(slot) = self.sizes.get(index) else { bail!("no page {}", index + 1) };
        slot.get_or_init(|| {
            let [x0, y0, x1, y1] = self.bounds(index).map_err(|e| e.to_string())?;
            Ok(PageSize { width: (x1 - x0).max(0.0), height: (y1 - y0).max(0.0) })
        })
        .clone()
        .map_err(anyhow::Error::msg)
    }

    fn render(&self, index: usize, scale: f32, region: PageRect) -> Result<PageImage> {
        self.page_size(index)?;
        let (w, h) = render_size(scale, region)?;
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        let doc = self.doc.lock().unwrap_or_else(|e| e.into_inner());
        let mut err = Err::new();
        let (e, el) = err.ptr();
        // SAFETY: `doc` is open and locked; `rgba` is exactly w * h * 4 bytes.
        let r = unsafe { vw_render(doc.0, index as c_int, scale, region.x, region.y, w as c_int, h as c_int, rgba.as_mut_ptr(), e, el) };
        if r != 0 {
            bail!("page {}: {}", index + 1, err.message());
        }
        Ok(PageImage { width: w, height: h, rgba })
    }
}
