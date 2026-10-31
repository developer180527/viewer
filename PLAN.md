# Viewer — the "VLC of file viewers"

A Rust app that opens (almost) any file: images, documents, 3D, media, archives, data, binaries.
Runs on desktop, mobile, BSD and Chinese OSes, and starts fast enough to be the system's default preview app.

> Library names reflect the ecosystem as of late 2026. Check that each is still maintained before committing to it.

---

## 0. Non-negotiable goals

1. **Opens anything.** If no proper viewer exists, it falls back to hex, strings and metadata. It never says "unsupported".
2. **Ultra-fast startup.** Target the first pixel of the file within **≤ 100 ms** (warm) and **≤ 250 ms** (cold) on desktop. See §7.
3. **Truly cross-platform:**
   - Windows, macOS, Linux
   - iOS, iPadOS, Android
   - FreeBSD, OpenBSD, NetBSD
   - Chinese OSes: Kylin, UOS/Deepin, openEuler, HarmonyOS NEXT / OpenHarmony
4. **Don't reinvent.** Use existing libraries (Rust first, C/C++ when clearly better; see §9).
5. **Safe on hostile files.** Sandbox the parsers, fuzz them, and enforce resource limits.

---

## 1. Core architecture: many formats, few renderers

Every format **loader** converts a file into one of about 8 **content models**. Each model has one renderer.

| Content model | Used for | Renderer |
|---|---|---|
| **Raster** (tiled image or pages) | Images, PDF pages, DjVu, comics, RAW, DICOM | wgpu textured tiles, zoom/pan, mipmaps |
| **Scene** (meshes, materials, points) | glTF, OBJ, STL, FBX, CAD, LiDAR, splats | Small custom wgpu PBR renderer (or `three-d`) |
| **RichText** (styled flow) | Markdown, EPUB, DOCX-lite, text, code | `parley` / `cosmic-text` + `syntect` |
| **Table** (virtualised) | CSV, XLSX, Parquet, SQLite, Arrow | Lazy grid that fetches only visible rows |
| **Tree** | JSON, YAML, XML, archives, HDF5 | Collapsible tree, lazy children |
| **Media** (A/V frames) | Video, audio | libmpv / ffmpeg → texture; `symphonia` for audio |
| **Hex** (raw bytes) | Unknown files, executables | mmap hex view + strings + entropy plot |
| **Web** (fallback) | HTML and HTML-ish formats | `wry` webview (system WebView) |

```rust
trait FormatLoader: Sync {
    fn id(&self) -> &'static str;
    fn probe(&self, header: &[u8], ext: Option<&str>) -> Confidence; // must be cheap, no allocation
    fn open(&self, src: FileSource) -> Result<Box<dyn Document>>;
}

trait Document: Send {
    fn model(&self) -> ContentModel;
    fn metadata(&self) -> Metadata;
    fn children(&self) -> Option<Vec<Entry>>; // containers can be opened recursively
    fn quick_preview(&self) -> Option<Raster> { None } // embedded thumbnail, for instant first paint
}
```

- **Detection:** magic bytes first (`infer`, `file-format`), then the extension, then content sniffing.
- **Recursion:** a zip holding a PDF holding images can be browsed level by level.
- **Platform backends:** a loader can be backed by an OS framework instead of a bundled library (see §5). The renderer doesn't care which.

---

## 2. Formats and libraries

### Images
| Formats | Library |
|---|---|
| PNG, JPEG, GIF, WebP, BMP, TIFF, ICO, TGA, QOI, PNM | `image` / `zune-image` (`zune-jpeg` for speed) |
| SVG | `resvg` + `usvg` |
| AVIF | `image` (`avif-native`, dav1d) |
| HEIC / HEIF | `libheif-rs` (C) or the OS codec |
| JPEG XL | `jxl-oxide` |
| EXR / HDR | `exr`, `image` |
| Camera RAW | **LibRaw** (C++, best coverage) or `rawler` (pure Rust) |
| PSD | `psd` (composite) |
| DDS / KTX2 | `image_dds`, `ktx2` |
| EXIF / XMP | `kamadak-exif`, `xmp` |

### Documents
| Formats | Library / approach |
|---|---|
| PDF | **`pdfium-render`** + PDFium (permissive); `hayro` as the pure-Rust option to watch. Avoid MuPDF (AGPL) unless the project goes AGPL |
| EPUB | `epub` → HTML → RichText or webview |
| Markdown | `comrak` |
| Text / code / logs | `syntect`, `ropey`, `memmap2`; must handle multi-GB files |
| XLSX / XLS / ODS | `calamine` → Table |
| DOCX / PPTX / ODT / RTF | Basic: `docx-rs` → RichText. Faithful: LibreOffice headless → PDF (converter bridge) |
| CBZ / CBR / CB7 | `zip`, `unrar`, `sevenz-rust` → Raster pages |
| DjVu | `djvulibre` (C, FFI) |
| Jupyter `.ipynb` | `serde_json` + comrak + syntect |
| Email `.eml` / `.mbox` | `mail-parser` |
| Typst / LaTeX output | Show the source; optionally compile with `typst` (Rust crate) |

### 3D
| Formats | Library |
|---|---|
| glTF / GLB (primary) | `rendererx-gltf` (own reader, zero-copy from the mapped file) — **done** |
| OBJ (+ MTL) | `ufbx` (one C file, official Rust bindings) — **done** |
| STL | `stl_io` |
| PLY + Gaussian splats | `ply-rs` + custom splat shader |
| FBX | `ufbx` — **done** |
| DAE, 3DS, X, LWO, … | `russimp` (Assimp, C++), on demand |
| STEP / IGES / BREP (CAD) | OpenCASCADE (FFI, `opencascade-rs`) or `truck` → tessellate |
| USD / USDA / USDC / USDZ | `usd-lite` (our pure-Rust crate, reusable: Sdf-style layers from text, binary crate 0.4–0.10 and packages, plus geometry/preview-material helpers) — **done**, with composition (sublayers, references, payloads, inherits, variants, specializes; checked against `usdcat --flatten`); not yet: layer offsets/animation, relocates |
| 3MF | `zip` + `quick-xml` |
| LAS / LAZ | `las` |
| Blend | Saved preview image (`TEST` block) — **done**; geometry: Blender headless → glTF (converter bridge) |

### Audio / video
- **Default:** `libmpv` (via `libmpv2`) rendering into our wgpu texture. Gives every codec, subtitles and hardware decoding.
- **Lite or mobile builds:** platform decoders (AVFoundation, MediaCodec, Media Foundation) + `symphonia` / `cpal` for audio.
- Waveform / spectrogram views: `symphonia` + `rustfft`.
- **Playback is not ours to solve.** libmpv/ffmpeg (desktop, Android) and AVFoundation (iOS) already handle codecs, seeking, subtitles and hardware decoding. We embed them; we never write a player.
- **Editing is deliberately tiny** (§12.6): trim (video and audio) and gain/normalise (audio). No timeline, no effects.

### Archives
`zip`, `tar`, `flate2`, `xz2`, `zstd`, `bzip2`, `sevenz-rust`, `unrar`, ISO via `cdfs`. Shown as a Tree; any entry can be opened in place.

### Data / science
| Formats | Library |
|---|---|
| JSON / YAML / TOML / XML | `serde_*`, `quick-xml` |
| CSV / TSV | `csv` |
| Parquet / Arrow / Feather | `arrow`, `parquet` |
| SQLite | `rusqlite` (bundled) |
| HDF5 / NetCDF | `hdf5`, `netcdf` |
| NumPy | `npyz` |
| FITS | `fitsio` |
| DICOM | `dicom-rs` |
| GeoJSON / Shapefile / GeoTIFF / GPX | `geojson`, `shapefile`, `gdal`, `gpx` |

### Developer / misc
| Formats | Library |
|---|---|
| Fonts | `skrifa` / `ttf-parser` + `parley` (glyph grid, waterfall) |
| ELF / PE / Mach-O / WASM | `goblin`, `wasmparser` |
| Disassembly | `iced-x86`, `capstone` |
| Graphviz `.dot` | `layout-rs` |
| Unknown | Hex + strings + entropy (always available) |

---

## 3. Niche formats: four tiers

1. **Native Rust crate.** Best option: safe, static, works on every target.
2. **C/C++ library via FFI.** Bundled `-sys` builds, behind cargo features, run in the sandboxed worker (§8).
3. **Converter bridge.** If LibreOffice, Blender, ImageMagick, ffmpeg or Inkscape is installed, convert headless into a supported model. Desktop only.
4. **WASM plugins.** `wasmtime` (desktop/Android) / `wasmi` interpreter (iOS, which bans JIT) + WIT component-model interface. A plugin gets bytes and returns `Raster | Mesh | Table | Tree | RichText`. Sandboxed, and one `.wasm` file runs on every OS, including mobile and HarmonyOS.

The fallback is always Hex.

---

## 4. App shell

| Layer | Choice |
|---|---|
| Windowing / input | `winit` (desktop, Android, iOS) |
| GPU | `wgpu` (Metal / Vulkan / DX12 / GLES) |
| UI | **libgui** on every platform, one shell (decided, §12.1). The egui frontend from the MVP is removed |
| CPU fallback renderer | `softbuffer` + `tiny-skia` (instant first frame, no-GPU machines, old BSD boxes) |
| Async / threads | `rayon` pool + small `tokio` runtime only where I/O needs it |
| File dialogs / clipboard | `rfd`, `arboard` (desktop); platform pickers on mobile |
| Fonts | `fontdb`, which loads system fonts lazily (CJK fonts are not bundled) |

---

## 5. Platform matrix

| Platform | Rust target(s) | Windowing / GPU | Packaging | OS integration |
|---|---|---|---|---|
| Windows 10/11 | `x86_64`/`aarch64-pc-windows-msvc` | winit + DX12/Vulkan | MSI / MSIX, winget | File associations, Explorer preview handler + thumbnail provider |
| macOS | `aarch64`/`x86_64-apple-darwin` | winit + Metal | `.dmg`, notarised, Homebrew | UTI associations, **Quick Look extension** |
| Linux | `x86_64`/`aarch64`/`riscv64gc-unknown-linux-gnu` | winit (Wayland/X11) + Vulkan/GL | AppImage, deb, rpm, Flatpak | `.desktop` MIME, freedesktop thumbnailer |
| iOS / iPadOS | `aarch64-apple-ios` (+ sim) | winit + Metal | Xcode project wrapping a static lib | Document-based app, "Open in…", **Quick Look Preview extension**, Files provider |
| Android | `aarch64`/`armv7`/`x86_64-linux-android` | winit (`android-activity`) + Vulkan/GLES | AAB / APK (`cargo-ndk` + Gradle, or `xbuild`) | `ACTION_VIEW` intent filters, `content://` URIs, share target |
| FreeBSD / OpenBSD / NetBSD | `x86_64-unknown-freebsd` etc. | winit X11/Wayland + Vulkan/GL; CPU fallback | FreeBSD port / pkg, OpenBSD port | `.desktop` MIME |
| Kylin, UOS / Deepin, openEuler | Linux targets + **`loongarch64-unknown-linux-gnu`**, aarch64 | Same as Linux | deb / rpm, plus the vendor app stores' formats | Same as Linux |
| HarmonyOS NEXT / OpenHarmony | `aarch64-unknown-linux-ohos` (and `x86_64`) | ArkTS shell + `XComponent` surface → wgpu (Vulkan/GLES) via `ohos-rs` / `openharmony-ability` (winit isn't upstream for OHOS) | `.hap` | Want/URI file-open handling |

### Cross-platform rules
- **Core is `no_std`-friendly where possible and platform-free.** All OS-specific code lives in `crates/platform-*`.
- **Platform backends.** Each loader family has a trait with these implementations:
  - `bundled` (our libraries)
  - `apple` (ImageIO, PDFKit, AVFoundation)
  - `android` (BitmapFactory, PdfRenderer, MediaCodec via JNI)
  - `windows` (WIC, Media Foundation)

  Mobile builds prefer the platform backends, which means a smaller binary, hardware decoding, and no LGPL static-linking problems on the App Store.
- **C dependencies must cross-compile to every target above.** Prefer libraries with prebuilt binaries for all targets (e.g. `bblanchon/pdfium-binaries` covers Android, iOS, Linux, macOS and Windows). For LoongArch, RISC-V and OHOS, build from source in CI.
- **Feature-gated builds:** `full` (desktop), `mobile`, `minimal` (pure Rust only, for exotic targets). The pure-Rust build is the portability floor: it must compile anywhere `std` + a window exist.
- **iOS has no JIT and no `dlopen` of downloaded code:** everything is static, and WASM plugins run through an interpreter (`wasmi`).
- **CI:** GitHub Actions matrix (macOS, Windows, Ubuntu) + cross-compiled jobs (`cross`) for Android, FreeBSD, LoongArch, RISC-V and OHOS, plus emulator smoke tests.

---

## 6. Licensing

| Library | Licence | Rule |
|---|---|---|
| PDFium, Assimp, LibRaw (CDDL/LGPL dual), OpenCASCADE (LGPL + exception) | Permissive / LGPL | OK; link LGPL dynamically on desktop |
| ffmpeg / libmpv | LGPL (avoid GPL build options) | Dynamic on desktop; **on iOS use AVFoundation instead** (static LGPL + App Store is a problem) |
| MuPDF, Poppler | AGPL / GPL | Avoid unless the whole app is (A)GPL |
| djvulibre | GPL | Optional plugin / converter only |

---

## 7. Ultra-fast startup (default-preview-app requirement)

**Target:** double-click → file visible in ≤ 100 ms warm, ≤ 250 ms cold.

**App first, file second.** Two separate clocks:
- **App ready** — a window with toolbar and chrome is on screen and responds to input. This is a hard limit: ≤ 100 ms cold, and it must **not depend on the file** (type, size, or whether it parses at all).
- **File ready** — the content is drawn. This may take longer for big or heavy files, as long as progress is visible (spinner, thumbnail, first page, untextured mesh) and the window stays responsive.
Nothing that serves a particular format runs before "app ready": no format module loaded, no 3D pipeline built, no parser started on the UI thread.

### Process and binary
- **Single instance, many windows (§12.3).** A second launch hands the path to the running instance (Unix socket / named pipe) and exits in a few ms; the instance opens it in a **new window**. Optionally, a tiny resident agent keeps a hidden window warm (user setting, off by default).
- **Small core binary.** Heavy libraries (PDFium, Assimp, mpv, OpenCASCADE) are **not linked into the main executable**. They live in on-demand modules that are `dlopen`ed on first use (desktop/Android), or in the worker process. Opening a JPEG must never page in PDFium.
- **Release profile:** `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `opt-level = 3`, strip symbols. Also try PGO and BOLT (`cargo-pgo`).
- **Zero static initialisers.** The loader registry is a `const` table of function pointers. No plugin scanning at startup; WASM plugins are indexed lazily and cached.

### First pixel before full load
- **Parallel start.** The window and the GPU initialise *while* the file is being mapped and probed on another thread.
- **Avoid GPU-init latency.** wgpu adapter/device creation (and Vulkan instance creation) can take 50–200 ms. Paint the first frame with the **CPU path** (`softbuffer` + `tiny-skia`), then swap to wgpu when it's ready. Cache shader pipelines (`wgpu::PipelineCache`).
- **Progressive decode:**
  - Show embedded thumbnails first (EXIF thumbnail, RAW preview JPEG, PDF page 1 at low DPI, video keyframe)
  - Use JPEG DCT-domain downscaling to decode at screen size
  - Decode only the visible tiles and pages
- **mmap everything.** Never read a whole file to show the start of it.
- **Lazy fonts.** Embed one small UI font. Load CJK and emoji fonts from the system only when the text needs them.
- **Lazy UI.** Panels (metadata, tree, search) build on first open.

### Memory
Measured by `bench/memory.sh` (footprint on macOS, which is what Activity Monitor shows; RSS elsewhere) and budgeted in `bench/memory-budgets.txt`: `--check` fails when a case goes over. Three numbers per case: headless peak (the budgeted one, most repeatable), windowed peak, and settled (what the open window holds).

Rules, and what the first memory pass found (2026-11-01):
- **Big frees go back to the OS.** macOS's allocator keeps freed large blocks and returns them only under memory pressure (`malloc_zone_pressure_relief` returns nothing), so the footprint stayed at every load's peak. The app's global allocator (`crates/app/src/alloc.rs`) maps blocks of 1 MB and up directly and unmaps them on free. Settled footprint for a 45 MP image: 799 → 290 MB; for a textured 3D model: 408 → 205 MB.
- **No duplicate copies.** Images upload in 2048 px tiles within a per-frame budget (small staging, first tiles sooner), then the decoded copy is dropped. 3D meshes and textures drop their CPU copies after upload; instances share them.
- **Window-sized GPU buffers cost nothing where possible.** 3D MSAA and depth targets are transient attachments: memoryless on Apple and mobile GPUs (−95 MB for a full-window 3D view).
- **Decode small.** Textures are capped at 2048 px; JPEGs decode at 1/2–1/8 scale in the IDCT; EXRs stream into the reduced size; mips are generated from the reduced image.
- **Not the app's:** a process's first render pass makes the Apple GPU driver take ~180 MB that it marks reclaimable straight after (a bare wgpu clear pass shows it). It inflates windowed peaks, not settled memory.
- **Empty app** (headless): 42 MB. **Settled window** on a small file: ~100–110 MB (including ~40 MB of window surfaces).
- Next: decode very large images straight into GPU tiles (rows streamed, never the whole image in RAM), lazy glyph rasterisation, and a resident-agent cap if one ships.

### Measurement
- A `--trace-startup` flag records timestamps for process start → window → **app ready** → **file ready** → full quality.
- CI fails on regressions of either clock, and of the memory budgets above.
- CI benchmark with `hyperfine` on a fixed corpus. **Startup regressions fail the build.**

---

## 8. Security and robustness

- **Parser host (§12.4).** Desktop and Android isolate parsers in a separate process; iOS and HarmonyOS isolate them in-process. Details:
- **Worker process** for every C/C++ parser (desktop). Communicates over shared memory + IPC. If it crashes, show "couldn't render, showing hex" instead of crashing the app. Sandbox it with seccomp (Linux), App Sandbox (macOS), AppContainer (Windows), pledge/unveil (OpenBSD), Capsicum (FreeBSD).
  - On iOS and HarmonyOS, run in-process: pure-Rust parsers, platform backends, and C libraries only as WASM (§12.4).
- **Fuzz** every loader with `cargo-fuzz`, and integrate OSS-Fuzz later.
- **Limits:** max decoded pixels, decompression ratio (zip bombs), parse time and memory per document.
- **Never auto-execute.** Scripts and macros in documents are never run. HTML in the webview runs with JS off and no network by default.

---

## 9. Rust vs C/C++ library policy

Use the **best library for the job**. Rust's job here is the safe shell, the glue, the renderers and the sandbox. It is not a purity contest.

**Decision rule, per format:**
1. A Rust crate with comparable quality exists → use Rust.
2. A C/C++ library is clearly better (coverage, fidelity, speed, maturity) → use it, **behind the same `FormatLoader` trait**, feature-gated, in the worker process, lazily loaded.
3. Keep or track a pure-Rust alternative so the `minimal` build and exotic targets still work, and so we can swap later.

**Binding tools:** `bindgen` (C), `cxx` / `autocxx` (C++), or a thin C shim over the C++ API (most robust across compilers).

---

## 10. Workspace layout

```
viewer/
├─ crates/
│  ├─ core/            # traits, detection, registry, content models
│  ├─ render/          # wgpu + CPU renderers: raster, scene, text, table, tree, hex
│  ├─ app/             # libgui shell, windows, info panel, single-instance IPC
│  ├─ rendererx/       # reusable 3D renderer (docs/3d-renderer.md)
│  ├─ worker/          # sandboxed parser process + IPC protocol
│  ├─ plugin-host/     # wasmtime / wasmi + WIT
│  ├─ platform-desktop/
│  ├─ platform-apple/  # iOS/macOS backends + Quick Look extension glue
│  ├─ platform-android/
│  ├─ platform-ohos/
│  ├─ fmt-image/  fmt-doc/  fmt-3d/  fmt-media/  fmt-archive/  fmt-data/  fmt-dev/
├─ apps/
│  ├─ ios/             # Xcode wrapper
│  ├─ android/         # Gradle wrapper
│  └─ ohos/            # ArkTS wrapper
├─ plugins/            # example WASM plugins
└─ bench/              # startup + decode benchmarks, test corpus
```

---

## 11. Roadmap

1. **MVP (desktop):** shell, detection, Hex, images, text/code, Markdown, JSON tree, CSV, zip. Startup tracing and benchmark from day 1.
2. **Documents:** PDF (PDFium), EPUB, XLSX, comics.
3. **3D:** wgpu viewport, glTF/OBJ/STL/PLY, then Assimp.
4. **Media:** libmpv (desktop), platform decoders (mobile); trim and gain editors (§12.6).
5. **Mobile:** Android, then iOS, using platform backends and OS open-file integration.
6. **Hardening:** worker sandbox, fuzzing, signing and packaging.
7. **Wider OS reach:** FreeBSD/OpenBSD ports, LoongArch/RISC-V builds for Kylin/UOS/openEuler, HarmonyOS NEXT shell.
8. **Long tail:** converter bridge, WASM plugin SDK, CAD, DICOM, science formats.
9. **OS preview integration:** Quick Look, Explorer preview handler, freedesktop thumbnailer, quick-preview mode toggled with the spacebar.

---

## 12. Decided architecture (2026-10-31)

These are the decisions that are expensive to change later. Everything else (micro-optimising startup, widget polish, which format comes next) can change freely. Each decision lists what it rules out.

### 12.1 One UI: libgui everywhere
- libgui on desktop, mobile, BSD and HarmonyOS, styled the same everywhere. No per-platform native UI shells.
- The egui frontend is deleted once the libgui one reaches parity. Until then it is not extended.
- Rules out: SwiftUI / Compose / ArkTS UIs. (HarmonyOS still needs a thin ArkTS wrapper that hosts the `XComponent` surface; it contains no UI.)

### 12.2 One GPU device, shared
- Each process creates **one** `wgpu::Device`. libgui, RendererX, the media player's video textures and image tiles all use it.
- At creation it requests **every optional feature the adapter has** (texture compression BC / ETC2 / ASTC, pipeline cache, …) and the adapter's own limits. The device can't gain features later, and at launch we don't yet know what file will be opened.
- Code checks what the device actually has before using a feature (e.g. compressed textures fall back to RGBA8).
- All windows share the device; each window has only its own surface.
- Image interchange with libgui: RGBA8, sRGB-encoded, straight alpha.

### 12.3 One instance, many windows
- **One app process per user session.** Opening a file (double-click, "Open with", drag onto the Dock icon, `viewer file` in a terminal, a share sheet) always reaches that process, which opens the file **in a new window**. Each window shows its own file with its own navigation stack, and windows don't block each other.
- How the file reaches the running instance:

| Platform | Mechanism |
|---|---|
| macOS | the OS already sends `open file` events to the running app (no IPC needed); a CLI launch uses a Unix socket |
| Windows | named pipe; the second process forwards the path and exits |
| Linux / BSD / Chinese distros | Unix socket under `$XDG_RUNTIME_DIR` (D-Bus activation optional later) |
| iPadOS | multiple scenes (`UISceneSession`): each opened document gets its own scene/window. iPhone: one window, documents stack |
| Android | multi-window / freeform: each document opens a new task (`FLAG_ACTIVITY_NEW_DOCUMENT` + `MULTIPLE_TASK`) in the same process |
| HarmonyOS | one UIAbility per document in the same process |

- Inside the process: windows are independent **UI state** (libgui `Ui`, surface, navigation stack) on top of shared **services** (GPU device, font cache, loader registry, thread pool, parser host). A second window costs one surface and one `Ui`, not a second app.
- Opening a file that is already open focuses its window (configurable).
- **One file, one document (decided 2026-10-31).** The same file open in two windows is one in-memory document. An edit made in either window appears in both at once: operations are applied to the shared document, and every window showing it redraws. Undo history and unsaved state belong to the document, not the window; scroll position, zoom, camera and selection belong to each window. Files are matched by identity (device + inode / file ID), not path, so two paths to the same file are still one document.
- A crash in one document must not close other windows: loaders run in the parser host (§12.4), and the UI catches panics per window and shows an error in that window.
- Rules out: one process per window (slower to open, more memory, no shared caches).

### 12.4 Parser host: same interface, isolation per platform
All file parsing goes through one interface, the **parser host**. Which isolation backs it is a per-platform choice the rest of the app never sees.

| Platform | Isolation | When a parser fails |
|---|---|---|
| Windows, macOS, Linux, BSD, Kylin / UOS / openEuler | **separate worker process**, OS-sandboxed (AppContainer, App Sandbox, seccomp, Capsicum, pledge/unveil) | the worker dies; the window shows "couldn't open" and falls back to hex; the worker restarts |
| Android | **`isolatedProcess` service** (no file or network access; the document is passed as a file descriptor) | same |
| iOS, HarmonyOS | **in-process** (neither allows us to spawn helper processes), hardened as below | the loader returns an error; panics are caught |

In-process hardening (iOS, HarmonyOS, and the first line of defence everywhere):
- **Pure-Rust parsers by default**: memory-safe, so a hostile file can cause an error but not memory corruption.
- **C/C++ libraries only as WebAssembly** run in `wasmi` / wasmtime's Pulley interpreter (no JIT, so App-Store-legal). The sandbox is the isolation: a bug in the C code can only corrupt its own linear memory. Slower than native, which is acceptable for niche formats. Platform frameworks (ImageIO, PDFKit, AVFoundation) are used directly: the OS vendor maintains them.
- **Budgets on every parse**: size, nesting depth, decompression ratio, memory and time. A parse over budget is cancelled.
- **Panics are caught** at the loader boundary (already done), falling back to the next loader or hex.
- iOS / macOS **Quick Look extensions** are separate processes managed by the OS, so previews from Files or Mail get process isolation for free.

Phasing: the interface exists now and runs in-process everywhere. The worker process on desktop arrives together with the first C/C++ parser (PDFium), because today every parser is Rust and gains nothing from it.

Rules out: calling a parser directly from UI code; linking a native C parser into the iOS app.

### 12.5 Documents and the editing model
**Loading.** Loaders produce **plain data that doesn't need the GPU** (CPU-side rasters, text, tables, trees and 3D scenes). This is what lets parsing start at launch, before the window and GPU exist. The renderers upload it later. `viewer-core` never depends on wgpu.

**Editing.** One model for every content type (text, table, tree, raster, 3D scene, media):
- A document is its loaded content plus a list of **operations**. Every change is a small operation that can be applied and reversed, so undo/redo, "unsaved changes" and autosave work the same everywhere.
- Views only display and **send operations**; they never mutate content directly. This makes editors cheap to add and keeps multiple windows on one document consistent.
- **Large files stay memory-mapped**; edits are kept beside the original (a piece table for text and bytes, row/cell overlays for tables, a parameter list for media), never as a full copy. Opening stays as fast as viewing.
- **Saving** is per format and optional: a format without a writer is view-only (or offers "export as …"). Saves go to a temporary file then an atomic rename; the original is never half-written.
- Rules out: views that write into document data; copying a whole file into memory just to make it editable.

### 12.6 Audio and video
Playback and decoding are solved problems, so we embed rather than build:

| Platform | Engine |
|---|---|
| Desktop, Android | **libmpv** (on ffmpeg) rendering into a texture on the shared device (§12.2); LGPL build, linked dynamically |
| iOS | **AVFoundation** (avoids static LGPL in an App Store app) |
| HarmonyOS | the system media kit (AVPlayer), libmpv later if needed |

Editing is **deliberately basic**, using the operation model (§12.5) and the same engines to write the result:
- **Trim** (video and audio): set in/out points, save. By default it **cuts at keyframes without re-encoding** (instant and lossless; ffmpeg stream copy, `AVAssetExportPresetPassthrough`). "Exact cut" re-encodes and says so.
- **Volume** (audio and the audio track of video): gain in dB and "normalise" (peak or loudness). Needs only the audio to be re-encoded; video is copied.
- Also: a waveform for audio; frame grab as PNG for video.
- Out of scope: timelines, multiple clips, transitions, effects, colour grading, subtitle editing.

### 12.7 Startup is a constraint, not a phase
Every decision above is measured against §7: the startup benchmark runs in CI, and a change that makes "first frame showing the file" slower fails the build. Today's numbers (libgui frontend, warm, M-series Mac): ~190 ms. Known cheap fixes: subset the UI font / rasterise lazily (~20 ms), avoid the first surface being reported occluded (~40 ms), tidy event-loop setup. Target ≤ 120 ms, then the §7 target.

### 12.8 Still open
- Whether the resident agent (§7) should ship on by default on desktop.
