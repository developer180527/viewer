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
| glTF / GLB (primary) | `gltf` |
| OBJ | `tobj` |
| STL | `stl_io` |
| PLY + Gaussian splats | `ply-rs` + custom splat shader |
| FBX, DAE, 3DS, X, LWO, … (~40) | `russimp` (Assimp, C++) |
| STEP / IGES / BREP (CAD) | OpenCASCADE (FFI, `opencascade-rs`) or `truck` → tessellate |
| USD / USDZ | OpenUSD (C++) FFI, or convert; Rust support is immature |
| 3MF | `zip` + `quick-xml` |
| LAS / LAZ | `las` |
| Blend | Blender headless → glTF (converter bridge) |

### Audio / video
- **Default:** `libmpv` (via `libmpv2`) rendering into our wgpu texture. Gives every codec, subtitles and hardware decoding.
- **Lite or mobile builds:** platform decoders (AVFoundation, MediaCodec, Media Foundation) + `symphonia` / `cpal` for audio.
- Waveform / spectrogram views: `symphonia` + `rustfft`.

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
| UI | `egui` for v1 (fast to build). Re-evaluate `Slint` if mobile UX needs native-feeling widgets |
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

### Process and binary
- **Single instance with IPC.** A second launch hands the path to the running instance (Unix socket / named pipe) and exits in a few ms. Optionally, a tiny resident agent keeps a hidden window warm (user setting, off by default).
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

### Measurement
- A `--trace-startup` flag records timestamps for process start → window → first frame → full quality.
- CI benchmark with `hyperfine` on a fixed corpus. **Startup regressions fail the build.**

---

## 8. Security and robustness

- **Worker process** for every C/C++ parser. Communicates over shared memory + IPC. If it crashes, show "couldn't render, showing hex" instead of crashing the app. Sandbox it with seccomp (Linux), App Sandbox (macOS), AppContainer (Windows), pledge/unveil (OpenBSD), Capsicum (FreeBSD).
  - On iOS, run in-process with the platform backends.
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
│  ├─ app/             # egui shell, tabs, info panel, single-instance IPC
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
4. **Media:** libmpv (desktop), platform decoders (mobile).
5. **Mobile:** Android, then iOS, using platform backends and OS open-file integration.
6. **Hardening:** worker sandbox, fuzzing, signing and packaging.
7. **Wider OS reach:** FreeBSD/OpenBSD ports, LoongArch/RISC-V builds for Kylin/UOS/openEuler, HarmonyOS NEXT shell.
8. **Long tail:** converter bridge, WASM plugin SDK, CAD, DICOM, science formats.
9. **OS preview integration:** Quick Look, Explorer preview handler, freedesktop thumbnailer, quick-preview mode toggled with the spacebar.
