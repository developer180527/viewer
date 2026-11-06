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

Every format **loader** converts a file into one of about 8 **content models**. Each model has one renderer. A file is often several things at once (a USDZ is a zip, a USD stage and a 3D scene), so with libfileparser a document has several **facets**, each in one of these models: docs/design/document-model.md.

| Content model | Used for | Renderer |
|---|---|---|
| **Raster** (tiled image) | Images, RAW, DICOM | wgpu textured tiles, zoom/pan, mipmaps |
| **Pages** (`PageSource`) | PDF, comics, DjVu, slides | Tiled scrolling page view (docs/design/pdf.md §7) |
| **Scene** (meshes, materials, points) | glTF, OBJ, STL, FBX, USD, CAD, LiDAR, splats | RendererX, our wgpu renderer (docs/design/3d-renderer.md) |
| **RichText** (styled flow) | Markdown, EPUB, DOCX-lite, text, code | `parley` / `cosmic-text` + `syntect` |
| **Table** (virtualised) | CSV, XLSX, Parquet, SQLite, Arrow | Lazy grid that fetches only visible rows |
| **Tree** | JSON, YAML, XML, archives, HDF5 | Collapsible tree, lazy children |
| **Media** (A/V frames) | Video, audio | FFmpeg's libraries (decode into our buffers) → texture; `symphonia` for audio |
| **Hex** (raw bytes) | Unknown files, executables | mmap hex view + strings + entropy plot |
| **Web** (fallback) | HTML and HTML-ish formats | `wry` webview (system WebView) |

```rust
trait FormatLoader: Sync {
    fn id(&self) -> &'static str;
    fn probe(&self, header: &[u8], ext: Option<&str>, name: &str) -> Confidence; // cheap, no allocation
    fn open(&self, src: &FileSource) -> Result<Document>;
}

struct Document {
    format: String,
    content: Content,          // Raster, Text, Markdown, Tree, Table, Container, Hex, Scene, Pages
    metadata: Vec<(String, String)>,
    unreadable: Unreadables,   // referenced files blocked or missing; grows while textures load
}
```

This is today's shape, in `viewer-core`. Where it is heading (documents with facets, contracts in their own crate, readers taking a `ParseContext` of hooks): docs/design/document-model.md and docs/design/libfileparser.md.

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
| PDF | **MuPDF** built from source (a submodule), everywhere but iOS; **PDFKit** on iOS, because the App Store can't carry AGPL. Both behind `PageSource` (docs/design/pdf.md) — **done**. `hayro` as the pure-Rust option to watch |
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
| glTF / GLB (primary) | `gltf-lite` (own reader, zero-copy from the mapped file) — **done** |
| OBJ (+ MTL) | `ufbx` (one C file, official Rust bindings) — **done** |
| STL | Our own parser (binary and ASCII, welded, Z-up) — **done** |
| PLY + Gaussian splats | Our own parser (meshes, point clouds, colours) — **done**; splats drawn as splats later |
| FBX | `ufbx` — **done** |
| DAE, 3DS, X, LWO, … | `russimp` (Assimp, C++), on demand |
| STEP / IGES / BREP (CAD) | OpenCASCADE (FFI, `opencascade-rs`) or `truck` → tessellate |
| USD / USDA / USDC / USDZ | `usd-lite` (our pure-Rust crate, reusable: Sdf-style layers from text, binary crate 0.4–0.10 and packages, plus geometry/preview-material helpers) — **done**, with composition (sublayers, references, payloads, inherits, variants, specializes; checked against `usdcat --flatten`); not yet: layer offsets/animation, relocates |
| 3MF | `zip` + `quick-xml` |
| LAS / LAZ | `las` |
| Blend | Saved preview image (`TEST` block) — **done**; geometry: Blender headless → glTF (converter bridge) |

### Audio / video
- **Default:** FFmpeg's libraries (`libavformat`, `libavcodec`, `libswresample`; LGPL, no GPL parts) on every platform, behind our own media library: open, list streams, read metadata, decode a frame or audio samples into the caller's buffer. Reads go through our callback (so `FileResolver` and iOS grants apply), threads are set by us, and only the codecs we choose are built in. Hardware decoding through VideoToolbox, MediaCodec and D3D11VA. Getting decoded frames into wgpu is the first spike.
- **Not libVLC** (decided 2026-11-03): it is a player rather than a library: its own threads, clock, audio output, plugins and file and network access, no allocator hook, and 20–150 MB on disk. That breaks "libraries provide mechanism, the app decides policy" (docs/design/architecture.md) and makes the media library heavy for every app that uses it.
- **Hardware decoding** uses each platform's decoders through FFmpeg, so there is one media path everywhere rather than per-platform players.
- **Audio-only light path:** `symphonia` (pure Rust) for common audio; also waveform and spectrogram views with `rustfft`.
- What plays, and what is deliberately left out: §12.6.
- **Codecs are not ours to solve; the player is small and ours.** FFmpeg handles containers, codecs and hardware decoding. Viewer's player is app code on top of the media library: a clock, audio output through `cpal`, seeking, and subtitles from FFmpeg's decoded subtitle streams. No playlists, no streaming protocols, no filters.
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
| Async / threads | Background work through the app's `Spawn` hook; libraries start no threads or global pools of their own (docs/design/architecture.md). `tokio` only where I/O needs it |
| File dialogs / clipboard | `libfilepicker` (`rfd` on desktop, `UIDocumentPickerViewController` on iOS), `arboard` |
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
  - `apple` (ImageIO, PDFKit)
  - `android` (BitmapFactory, PdfRenderer via JNI)
  - `windows` (WIC)

  A platform backend is used where it is the better choice on that platform (PDFKit on iOS, because MuPDF is AGPL). Video is the exception: FFmpeg everywhere, using the platform's hardware decoders through it (§12.6).
- **C dependencies must cross-compile to every target above.** Prefer libraries with prebuilt binaries for all targets or that build from source everywhere (MuPDF). For LoongArch, RISC-V and OHOS, build from source in CI.
- **Feature-gated builds:** `full` (desktop), `mobile`, `minimal` (pure Rust only, for exotic targets). The pure-Rust build is the portability floor: it must compile anywhere `std` + a window exist.
- **iOS has no JIT and no `dlopen` of downloaded code:** everything is static, and WASM plugins run through an interpreter (`wasmi`).
- **CI:** GitHub Actions matrix (macOS, Windows, Ubuntu) + cross-compiled jobs (`cross`) for Android, FreeBSD, LoongArch, RISC-V and OHOS, plus emulator smoke tests.

---

## 6. Licensing

| Library | Licence | Rule |
|---|---|---|
| LibRaw (CDDL/LGPL dual), OpenCASCADE (LGPL + exception) | Permissive / LGPL | OK; link LGPL dynamically on desktop |
| FFmpeg | LGPL (no GPL build options: no x264/x265, no `--enable-gpl`) | Dynamic on desktop and Android; on iOS as dynamic frameworks inside the app, with our source public so it can be relinked |
| `dav1d`, `libopenmpt`, `libass`, `rustysynth` | BSD / BSD / ISC / MIT | OK (media, §12.6) |
| `libbluray` | LGPL | OK, dynamic; unencrypted discs only |
| `libdvdnav` / `libdvdread`, `libcdio` | GPL | Avoid: no DVD menus (titles play from their VOBs), no audio CDs |
| H.264, HEVC, AAC (patents, not licences) | Patent pools | Prefer the platform's hardware decoders where they exist; get a proper answer before any commercial release |
| MuPDF | AGPL | **Used, except on iOS.** Only `viewer-pdf-mupdf` and the app builds containing it are AGPL; libgui, libfilepicker, RendererX and the other crates stay MIT/Apache (docs/design/pdf.md §3) |
| Poppler | GPL | Avoid |
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
- **Small core binary.** Heavy libraries (FFmpeg, OpenCASCADE) are **not linked into the main executable**. They live in on-demand modules that are `dlopen`ed on first use (desktop/Android), or in the worker process. Opening a JPEG must never page them in. MuPDF is the exception so far: linked statically, but nothing in it runs until the first PDF (docs/design/pdf.md §4).
- **Release profile:** `lto = "fat"`, `codegen-units = 1`, `opt-level = 3`, strip symbols. Not `panic = "abort"`: a panicking decoder must fall back to the next loader (§12.4). Also try PGO and BOLT (`cargo-pgo`).
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
- **No duplicate copies.** Images upload in 2048 px tiles within a per-frame budget (small staging, first tiles sooner), then the decoded copy is dropped. Images over 16 MP stream: decoded on a thread in ~16 MB bands of rows (PNG row by row; other formats decode in their own layout and convert per band), through a two-band queue (a window that isn't drawing makes the decoder wait; closing the page stops it), uploaded as they arrive and shown top to bottom. A 45 MP PNG's headless peak went 376 → 221 MB. After the last upload the view draws once more so wgpu frees the upload staging (it does so on a later submit, which an idle window never makes). 3D meshes and textures drop their CPU copies after upload; instances share them.
- **Window-sized GPU buffers cost nothing where possible.** 3D MSAA and depth targets are transient attachments: memoryless on Apple and mobile GPUs (−95 MB for a full-window 3D view).
- **Decode small.** Textures are capped at 2048 px; JPEGs decode at 1/2–1/8 scale in the IDCT; EXRs stream into the reduced size; mips are generated from the reduced image.
- **Not the app's:** a process's first render pass makes the Apple GPU driver take ~180 MB that it marks reclaimable straight after (a bare wgpu clear pass shows it). It inflates windowed peaks, not settled memory.
- **Empty app** (headless): 42 MB. **Settled window** on a small file: ~100–110 MB (including ~40 MB of window surfaces).
- Next: JPEGs decoded in bands too (a progressive/scanline JPEG decoder; today they decode whole in RGB, then convert per band), lazy glyph rasterisation, and a resident-agent cap if one ships.

### Measurement
- A `--trace-startup` flag records timestamps for process start → window → **app ready** → **file ready** → full quality.
- CI fails on regressions of either clock, and of the memory budgets above.
- CI benchmark with `hyperfine` on a fixed corpus. **Startup regressions fail the build.**

---

## 8. Security and robustness

- **Parser host (§12.4).** Desktop and Android isolate parsers in a separate process; iOS and HarmonyOS isolate them in-process. Details:
- **Worker process** for every C/C++ parser (desktop). Communicates over shared memory + IPC. If it crashes, show "couldn't render, showing hex" instead of crashing the app. Sandbox it with seccomp (Linux), App Sandbox (macOS), AppContainer (Windows), pledge/unveil (OpenBSD), Capsicum (FreeBSD).
  - On iOS and HarmonyOS, run in-process: pure-Rust parsers, platform backends, vetted C libraries natively, and other C libraries as WASM (§12.4).
- **Fuzz** every loader with `cargo-fuzz`, and integrate OSS-Fuzz later.
- **Limits:** max decoded pixels, decompression ratio (zip bombs), parse time and memory per document. The first two are in `viewer_core::limits` (256 megapixels; inflating past 64 MB at most 200:1), checked by hostile files in the corpus; they move into `ParseContext` later (docs/design/architecture.md).
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

Today (one repo, one Cargo workspace):

```
crates/
  core/                 viewer-core: FileSource, Document, Content, PageSource, registry, hex fallback
  libfileparser-core/  libfileparser-testkit/  libfileparser-3d/      libfileparser (docs/design/libfileparser.md)
  fmt-image/  fmt-doc/  fmt-data/  fmt-archive/  fmt-pdf/             format loaders, moving to libfileparser
  pdf-mupdf/            MuPDF backend (AGPL)        pdf-pdfkit/   PDFKit backend
  rendererx/  scene-data/  gltf-lite/      the reusable 3D renderer (docs/design/3d-renderer.md)
  usd-lite/             pure-Rust USD reader and composition
  formats/              viewer-formats: the registry of every loader, shared by the app and the corpus
  corpus/               viewer-corpus: the test corpus runner (corpus/README.md)
  app/                  libgui shell, views, loading, single window
corpus/                 test corpus: manifest, synthetic files, expected summaries and renders
ios/                    Xcode wrapper, device and simulator scripts
bench/                  startup and memory benchmarks, sample generator
samples/                one small file per supported format
```

Outside it: libgui (UI) and libfilepicker (pickers), each its own repo, pinned by commit.

Where it is heading: two halves any app can embed to open and show any file. **libfileparser** reads (a contracts crate, one crate per category of file, and one crate holding them together; docs/design/libfileparser.md); **libviewer** renders (the renderers, then views for a UI toolkit). Viewer becomes policy and its shell on top of both (docs/design/architecture.md).

---

## 11. Roadmap

✅ done · 🟡 partly done · ⬜ not started. Updated 2026-11-03. **Opening every kind of file comes first; editors and further GUI work follow** (docs/roadmap.md, docs/design/editing.md).

**Next, in order** (reasons in docs/roadmap.md §3 and docs/design/architecture.md §6): the test corpus (✅ `corpus/`, run by `crates/corpus` on every CI platform); the command list and macOS menu; **libfileparser** from the ground up (`-core`, `-3d` first, the front door, the other categories; docs/design/libfileparser.md) with a thumbnail tool as its second user; then formats (XLSX first) in their category crates; then MuPDF in a helper process on desktop.

1. ✅ **MVP (desktop):** shell, detection, Hex, images, text/code, Markdown, JSON tree, CSV, zip. Startup tracing and benchmark from day 1. Since then: folders as containers, image streaming in bands, memory budgets in CI.
2. 🟡 **Documents:** done: PDF (MuPDF, PDFKit on iOS) with a tiled scrolling page view. Next: XLSX, comics, EPUB/DOCX, slides, in the order in docs/roadmap.md §3. DOCX/XLSX/EPUB/CBZ only open as zip listings today, and that view stays available through **View as** beside the new one, text and hex (§1 there).
3. 🟡 **3D:** done: RendererX (wgpu, clay/PBR/unlit, textures, streaming, off-thread pipelines), glTF/GLB/VRM, FBX and OBJ via ufbx (in place of Assimp), USD/USDZ with composition via `usd-lite`, `.blend` preview, blocked/missing file reporting. STL (welded, Z-up turned Y-up) and PLY (ASCII/binary, meshes and point clouds, colours). Left: MToon shading, Gaussian splats drawn as splats (they show as coloured points), animation playback (v2).
4. ⬜ **Media:** our media library on FFmpeg's libraries for reading, decoding and trimming; Viewer's small player on top (§12.6).
5. 🟡 **Mobile:** iOS/iPadOS runs (libfilepicker, folder grants, Open Folder…). Left: opening files tapped in the Files app (`CFBundleDocumentTypes`), bookmarks with a Recents list, then Android.
6. ⬜ **Hardening:** worker sandbox, fuzzing, signing and packaging. (CI tests Linux, Windows and macOS already.)
7. ⬜ **Wider OS reach:** FreeBSD/OpenBSD ports, LoongArch/RISC-V builds for Kylin/UOS/openEuler, HarmonyOS NEXT shell.
8. ⬜ **Long tail:** converter bridge, WASM plugin SDK, CAD, DICOM, science formats.
9. ⬜ **OS preview integration:** Quick Look, Explorer preview handler, freedesktop thumbnailer, quick-preview mode toggled with the spacebar.

---

## 12. Decided architecture (2026-10-31)

These are the decisions that are expensive to change later. Everything else (micro-optimising startup, widget polish, which format comes next) can change freely. Each decision lists what it rules out.

### 12.1 One UI: libgui everywhere
- libgui on desktop, mobile, BSD and HarmonyOS, styled the same everywhere. No per-platform native UI shells.
- The egui frontend is deleted (done: libgui reached parity, and only it shows 3D).
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
- **Vetted C/C++ libraries natively** (decided 2026-11-03): only libraries that are widely shipped and continuously fuzzed (OSS-Fuzz or equivalent), such as FFmpeg, `dav1d`, `libass` and ufbx, each with strict `Limits`, our own fuzzing in CI, and a pinned, regularly updated version. Each one is listed in docs/design/architecture.md §5 with what it may touch.
- **Other C/C++ libraries only as WebAssembly** run in `wasmi` / wasmtime's Pulley interpreter (no JIT, so App-Store-legal). The sandbox is the isolation: a bug in the C code can only corrupt its own linear memory. Slower than native, which is acceptable for niche formats. Platform frameworks (ImageIO, PDFKit, AVFoundation) are used directly: the OS vendor maintains them.
- **Budgets on every parse**: size, nesting depth, decompression ratio, memory and time. A parse over budget is cancelled.
- **Panics are caught** at the loader boundary (already done), falling back to the next loader or hex.
- iOS / macOS **Quick Look extensions** are separate processes managed by the OS, so previews from Files or Mail get process isolation for free.

Phasing: the interface exists now and runs in-process everywhere. The worker process on desktop was to arrive with the first C/C++ parser. MuPDF now is one, and it still runs in-process on its render thread (docs/design/pdf.md §8), so the worker is due.

Rules out: calling a parser directly from UI code; native C on iOS that isn't on the vetted list.

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
| Desktop, Android, iOS | **Our media library on FFmpeg's libraries**, decoded frames into a texture on the shared device (§12.2); LGPL, linked dynamically. Viewer's player (clock, `cpal` audio, seeking, subtitles) sits on top |
| HarmonyOS | the system media kit (AVPlayer), FFmpeg later if needed |

**What plays.** FFmpeg is built with demuxers and decoders only: no network protocols, encoders, filters or device capture. VLC gets most of its file coverage from these same libraries; we take that and leave out the rest of VLC.

| Category | Covered by | Formats |
|---|---|---|
| Video containers | FFmpeg | MP4/M4V, MKV, AVI, MOV, WebM, FLV, OGV/OGM, MPEG-PS/TS/M2TS/MTS, WMV/ASF, 3GP/3G2, MXF, NUT, VOB, RealMedia, DV, Y4M |
| Video codecs | FFmpeg (AV1 through `dav1d`), platform hardware decoders through it | H.264, HEVC, VP8/9, AV1, MPEG-1/2/4, DivX/Xvid, Theora, WMV, ProRes, DNxHD, MJPEG, Cinepak, Dirac, RealVideo and older codecs |
| Audio | FFmpeg | MP3, AAC/M4A, FLAC, WAV, Vorbis, Opus, WMA, ALAC, AIFF, APE, WavPack, AC3/E-AC3, DTS, MPC, TTA, AMR, CAF |
| Tracker music | `libopenmpt` (through FFmpeg) | MOD, XM, IT, S3M |
| MIDI | `rustysynth` + a soundfont | MID |
| Subtitles | FFmpeg reads and decodes; `libass` draws styled ASS; we draw text and bitmap ones | SRT, ASS/SSA, VobSub (SUB/IDX), SMI, VTT, MicroDVD, PGS, DVB, tracks embedded in MKV/MP4 |
| Discs | FFmpeg + our ISO listing; `libbluray` | DVD and VCD titles (from their VOB/MPEG files), ISO images, unencrypted Blu-ray |
| Playlists | Our own small parsers | M3U/M3U8, PLS, XSPF, ASX: local entries only |

**Left out on purpose:** network protocols and streams (HTTP, RTSP, RTMP, HLS, DASH, …), TV tuners, screen capture, DVD menus and audio CDs (their libraries are GPL), encrypted Blu-ray.

Memory: a fixed number of decoded frames in flight (a few, not a buffer of seconds), owned by the caller; hardware frames stay on the GPU where the platform allows.

Editing is **deliberately basic**, using the operation model (§12.5) with the same FFmpeg libraries to write the result:
- **Trim** (video and audio): set in/out points, save. By default it **cuts at keyframes without re-encoding** (instant and lossless; stream copy with FFmpeg's libavformat, which needs no codecs). "Exact cut" re-encodes only up to the next keyframe, with the platform's hardware encoders, and says so.
- **Volume** (audio and the audio track of video): gain in dB and "normalise" (peak or loudness). Needs only the audio to be re-encoded; video is copied.
- Also: a waveform for audio; frame grab as PNG for video.
- Out of scope: timelines, multiple clips, transitions, effects, colour grading, subtitle editing.

### 12.7 Startup is a constraint, not a phase
Every decision above is measured against §7: the startup benchmark runs in CI, and a change that makes "first frame showing the file" slower fails the build. Today's numbers (warm, M-series Mac): ~165 ms. The system font list and the GPU device are set up on threads. What's left is mostly the OS: building the event loop (~30 ms), creating the window (~50 ms) and macOS putting it on screen (~45 ms). Target ≤ 120 ms, then the §7 target.

### 12.8 One layout for every screen size
The chrome adapts to width classes and input kind, not to the platform: see docs/design/responsive-ui.md.

### 12.9 Folder access (iOS, sandboxed macOS)

A picked folder grants everything under it; a picked file grants that file alone,
and nothing reaches upward. Loaders report every referenced file they couldn't
read on `Document::unreadable`, as *blocked* (not permitted) or *missing*. Blocked
files show a banner with **Open Folder…**, which opens the picker in the model's
folder and reopens the model inside the grant; missing ones are listed in the info
panel. Each root page holds its own `Picked`, released when the page goes.
Later, with a Recents list: security-scoped bookmarks (`bookmarkData` on iOS, with
`startAccessingSecurityScopedResource` after resolving, and re-saving stale ones),
and `CFBundleDocumentTypes` + `LSSupportsOpeningDocumentsInPlace` so files tapped
in the Files app open in Viewer (those URLs are single-file grants too).

### 12.10 Still open
- Whether the resident agent (§7) should ship on by default on desktop.
