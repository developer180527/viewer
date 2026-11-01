# PDF in Viewer

Status: both backends in, behind `PageSource`; the app shows page 1 as an interim view (§10). Roadmap step 2 (PLAN.md §11), the first document format.

## 1. Goals

1. **First page fast.** Page 1 on screen within ~150 ms of opening a typical PDF (warm), before the rest of the document is touched.
2. **Renders real-world PDFs correctly:** embedded and system fonts, vector art, images, forms and annotations as printed.
3. **Every platform:** macOS, Windows, Linux, iOS/iPadOS, Android, BSD, with one view.
4. **Bounded memory** on huge documents (thousands of pages, poster-size pages): only what's visible is rendered.
5. **Safe on hostile files:** no scripts, no network, and a crash in the renderer must not be allowed to take the app down once the worker sandbox exists (PLAN.md §8).

Not in v1: editing, form filling, signatures, printing. Text selection, search, links and the outline come right after v1 (§9).

## 2. Two backends, one view

| Platform | Backend | Why |
|---|---|---|
| macOS, Windows, Linux, BSD, Android, … | **MuPDF**, built from source | Excellent rendering, small and fast, plain C that builds anywhere; also opens XPS. |
| iOS / iPadOS | **PDFKit** (Apple) | MuPDF is AGPL, which doesn't fit the App Store's terms (§3). PDFKit is part of the OS: no size cost, native quality. |
| `minimal` pure-Rust build | `hayro`, later | Tracked, not in v1. |

Both sit behind one trait (§5), so the view, the cache and the tests don't know which one runs. The backend is chosen at build time (§4), never at runtime.

PDFium was the other candidate: a permissive licence, but a heavy Chromium-style build and no extras. It stays the fallback if MuPDF ever doesn't work out on a platform.

## 3. Licences

- **libgui, libfilepicker, RendererX and every `viewer-*` crate that doesn't link MuPDF stay `MIT OR Apache-2.0`.**
- **`viewer-pdf-mupdf`** (the only crate that links MuPDF) is `AGPL-3.0-or-later`, set in its own `Cargo.toml`, overriding the workspace licence.
- **The Viewer app** built with MuPDF is distributed under the AGPL: the repo stays public, the README says which builds include MuPDF, and the About screen credits it with a link to the source.
- **The iOS build has no MuPDF** at all: `cargo tree -p viewer --target aarch64-apple-ios` must not list `viewer-pdf-mupdf`. CI checks this (§8).
- **Artifex:** we'll ask whether they would allow MuPDF in the App Store build of a free, open-source viewer. If they say yes, iOS can switch to MuPDF too with a build flag; nothing else changes.

### What iOS doesn't get

iOS gets exactly what PDFKit does, and nothing is rebuilt to make up the difference. Features only MuPDF provides stay on the other platforms:

| Only off iOS (MuPDF) | On iOS |
|---|---|
| More formats MuPDF opens: XPS/OXPS, EPUB, FB2, MOBI, CBZ via MuPDF, SVG via MuPDF | Not opened |
| Deep PDF editing: true redaction, digital signatures, object-level edits, repair/clean/linearise, incremental save | Not offered |

On iOS, PDF viewing plus PDFKit's own basic editing (annotations, forms, page reorder/rotate/delete) is the whole set.

**Say it clearly, never fail silently:**
- A file in one of these formats on iOS opens a page that names the format and says: "*Format* files can't be opened on iPhone and iPad. Viewer opens them on macOS, Windows, Linux and Android." It still shows the Info panel (name, size, type), like any unsupported file.
- Editing features MuPDF alone provides don't appear in the iOS UI at all (no greyed-out buttons that do nothing). Where a user might look for one (for example the redaction tool), a short note says it's available on the other platforms.
- The README's format table has an "iOS" column, and these rows read "not on iOS".
- The loader registry knows this: on iOS these formats are registered as *known but unsupported here*, so probing still names the format correctly instead of guessing "binary data".

If Artifex ever allows MuPDF in the App Store build, these rows simply switch on.

## 4. Crates and build

```
crates/
  core/           viewer-core        Content::Pages, PageSource and its types (MIT/Apache)
  pdf-mupdf/      viewer-pdf-mupdf   MuPDF backend + its FFI (AGPL)
  pdf-pdfkit/     viewer-pdf-pdfkit  PDFKit backend via objc2-pdf-kit (MIT/Apache)
  fmt-pdf/        viewer-fmt-pdf     the FormatLoader; picks the backend by target (MIT/Apache)
third_party/
  mupdf/          git submodule, pinned to a release tag (with its own thirdparty/ submodules)
```

- `viewer-fmt-pdf` depends on `viewer-pdf-mupdf` with `cfg(not(target_os = "ios"))` and on `viewer-pdf-pdfkit` with `cfg(target_vendor = "apple")`. On macOS both exist; MuPDF is the default, and PDFKit is selectable for comparison in tests and benchmarks.
- **Building MuPDF:** `viewer-pdf-mupdf/build.rs` runs MuPDF's own `make` (Unix, BSD, Android NDK) or its Visual Studio project (Windows) into `target/`, then links the static libraries.
  - **Configuration:** no JavaScript (`HAVE_MUJS=no`), no network, a trimmed font set (`TOFU`/CJK fonts as a later, lazily loaded extra).
  - **Caching:** the build output is keyed by the submodule commit plus the flags, so a normal `cargo build` doesn't rebuild C.
  - **FFI:** our own small hand-written FFI layer (only the calls we use) plus a C shim for MuPDF's `fz_try`/`fz_catch` (setjmp-based errors must never unwind through Rust).
- **Lazy loading:** MuPDF is linked statically, but nothing in it runs until the first PDF opens (no global init at startup). PDFKit is a system framework, so it is loaded by the OS on first use anyway.

## 5. Content model: pages

PDFs are the first *paged* document; comics (CBZ), multi-page TIFF and EPUB-as-pages later share the shape.

```rust
pub enum Content {
    // …
    Pages(Arc<dyn PageSource>),
}

pub trait PageSource: Send + Sync {
    fn page_count(&self) -> usize;
    /// Page size in points (1/72 inch), for layout before anything is rendered.
    fn page_size(&self, index: usize) -> Size;
    /// Renders `region` of a page (in page points) at `scale` pixels per point.
    /// Called from the page-render thread; may be slow.
    fn render(&self, index: usize, scale: f32, region: Rect) -> Result<RgbaImage>;
    // later: text(index), links(index), outline()
}
```

The loader only opens the file and reads page sizes. That is fast, so the page appears at once and rendering happens in the view.

## 6. Threading

Neither backend is used from more than one thread at a time:

- **MuPDF:** one `fz_context` per document, cloned per thread with MuPDF's lock callbacks if we add more render threads later. v1 uses one.
- **PDFKit:** `PDFDocument`/`PDFPage` aren't documented as thread-safe; one thread owns them.

So there is one **page-render thread** that owns every open document:

- The view sends requests `(document, page, scale, tile)` with a priority: visible tiles first, then the pages just above and below, then thumbnails.
- Each request carries the view's generation; when the user scrolls or zooms, stale requests are dropped before they render.
- MuPDF requests also carry a cookie (`fz_cookie`) so a slow page in progress can be aborted.
- Finished tiles come back over a channel and wake the UI, as decoded 3D textures do.
- Closing a page drops its document on that thread.

## 7. The view

- **Continuous vertical scroll** of all pages, laid out from their sizes (no rendering needed), with a gap between pages.
- **Zoom:** fit width (default on phones), fit page, and free zoom by pinch / scroll. While zooming, the last render is scaled; a sharp re-render follows when it settles.
- **Tiles:** pages render in tiles of at most 2048 px, so a poster page at high zoom renders only what's on screen. Tiles upload through the same texture path as images.
- **Cache:** rendered tiles are kept in an LRU within a budget (desktop 256 MB, mobile 96 MB of RGBA), counted against the app's memory budgets (`bench/memory-budgets.txt`).
- **Chrome:** page counter ("12 / 340"), jump to page, previous/next keys (`PageUp`/`PageDown`, `Home`/`End`), and a thumbnail strip on expanded-width windows.
- **Dark mode:** pages stay as printed (white). An optional "invert" may come later.

## 8. Errors, security and tests

- **Password-protected:** "This PDF is encrypted" in place of the pages until the password prompt (§9).
- **Damaged files:** both backends repair what they can; a page that fails shows a placeholder with the error, and the rest of the document still works.
- **No JavaScript, no network:** MuPDF is built without MuJS; PDFKit doesn't run PDF scripts. Links open only when the user clicks them.
- **Crashes:** until the worker process exists, the backend runs in-process on its thread. `catch_unwind` cannot stop a crash in C, so a malicious PDF can still crash the app; accepted for v1, and the reason for PLAN.md §8's worker. The trait (§5) is where the worker cuts in.
- **Limits:** page count and page size are capped (a page over 14,400 × 14,400 points is clamped, as Acrobat does); render size per tile is bounded by the texture budget.
- **Corpus:** the PDF part of the test corpus (manifest with checksums, expected page counts and sizes, golden renders of page 1 with a tolerance, hostile files that must fail cleanly, time budgets). Both backends must pass it; MuPDF and PDFKit get separate goldens where they legitimately differ (anti-aliasing), with a looser cross-backend check.
- **CI:** builds MuPDF on every desktop target and Android; checks the iOS dependency tree has no MuPDF (§3).

## 9. After v1

1. Text selection and copy, search with highlights (MuPDF `fz_stext`; PDFKit `PDFSelection`).
2. Links (internal: jump; external: confirm, then open) and the outline as a sidebar.
3. Password prompt.
4. MuPDF's other formats (XPS, EPUB, FB2, MOBI, CBZ) off iOS; on iOS the "not on iPhone and iPad" page (§2).
5. Basic editing with the editors milestone: annotations, form filling, page reorder/rotate/delete, on both backends. Deep editing (redaction, signatures, repair) on MuPDF only; not offered on iOS (§2).

## 10. Plan

1. ✅ **MuPDF from source on macOS:** submodule at 1.28.6, `build.rs` (~20 s clean, incremental after), a C shim catching MuPDF's errors. A 22-page, 2.2 MB textbook chapter: open 4 ms, page 1 at 2× on screen in 111 ms.
2. ✅ **PDFKit** through `objc2-pdf-kit`, tested on macOS: open 53 ms, page 1 in 122 ms; renders match MuPDF. ⬜ Measure on the iPhone and iPad.
3. ✅ `viewer-core`: `Content::Pages` + `PageSource` (+ hand-written test PDFs in `viewer_core::testing`).
4. ✅ `viewer-pdf-mupdf` and `viewer-pdf-pdfkit` behind the trait; `viewer-fmt-pdf` loader (`VIEWER_PDF_BACKEND=pdfkit` on macOS). The iOS dependency tree has no MuPDF. ⬜ The page-render thread (comes with the view).
5. The view: layout, tiles, zoom, cache, chrome.
6. The PDF corpus and its runner; both backends passing it.
7. Builds: Windows (MSVC), Linux, Android NDK, BSD; CI with the iOS dependency check; README and licence notes.
8. Then §9.
