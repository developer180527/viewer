# Decisions

Every decision that is expensive to change, newest first, with where it's explained. A decision is changed by adding a new entry that replaces it, never by editing the old one.

| Date | Decision | Where |
|---|---|---|
| 2026-11-06 | **`rendererx-gltf` → `gltf-lite`, `rendererx-scene` → `scene-data`:** the glTF reader and the scene data are libraries of their own that parsers and RendererX share, not parts of the renderer | docs/design/libfileparser-3d.md §4 |
| 2026-11-06 | **Sources are ranged, fallible and shared:** a count first, then ranges; every read returns `Result`; results are owned or `Bytes`, never borrows. **Files are safe by default:** `NoFiles` unless the app chooses `FolderFiles` (the source's folder and below, optionally *n* levels up) or `AnyFiles`; Viewer uses `FolderFiles` with one level up. Panics in format code fail only what they were doing | docs/design/libfileparser.md §3 |
| 2026-11-06 | **Every category library is complete for its category, has a stable flat API (the same core shape in every category, with additions its kind needs), takes a new format as one `Reader` and one line, and is embeddable** (no globals, threads or file access of its own; C API). Internals change freely behind the API: no public fields, query APIs, no dependency types, `#[non_exhaustive]`, API snapshots and semver checks in CI (planned, not running yet); tests only through the API, with a conformance suite for every reader | docs/design/libfileparser.md §1, §4–9 |
| 2026-11-06 | **A file has facets.** A reader produces a document with several facets (primary, structure, source, package, dependencies, inspector, raw), made on demand; the front door adds the generic ones and merges other readings | docs/design/document-model.md |
| 2026-11-06 | **3D reads everything a 3D file is,** not only its scene: USD stage, layers and variants; glTF tree and GLB chunks; FBX records; dependencies; materials and textures | docs/design/libfileparser-3d.md |
| 2026-11-03 | **libfileparser:** a contracts crate defining the hooks; one crate per category of file, each usable alone; a front door with the hooks' defaults. Typed errors; big content read by range | docs/design/libfileparser.md |
| 2026-11-03 | **Media on FFmpeg's libraries,** behind our own media library and a small player of our own; **not libVLC**. Demuxers and decoders only; network, capture and GPL parts left out | PLAN.md §2, §12.6 |
| 2026-11-03 | **Vetted C libraries may run natively on iOS** (widely shipped, continuously fuzzed: FFmpeg, `dav1d`, `libass`, ufbx); other C libraries only as WebAssembly | PLAN.md §12.4 |
| 2026-11-03 | **Memory rules for every library:** read only what's needed, write into the caller's buffers, honour limits, bounded caches, no hidden threads, report memory | docs/design/architecture.md §3 |
| 2026-11-03 | **Order:** test corpus, then the macOS menu, then libfileparser, then formats, then MuPDF in a helper process | docs/roadmap.md |
| 2026-11-03 | **Libraries provide mechanism; the app decides policy,** through hooks | docs/design/architecture.md §1 |
| 2026-11-03 | **Limits:** images up to 256 megapixels; inflating past 64 MB at most 200:1 | PLAN.md §8, `crates/core/src/limits.rs` |
| 2026-11-02 | **PDF: MuPDF everywhere but iOS, PDFKit on iOS,** behind `PageSource`; only the MuPDF crate and the builds containing it are AGPL | docs/design/pdf.md §2–3 |
| 2026-11-02 | **Every view of a file stays available** ("View as"): a format's own view never hides zip, text or hex | docs/roadmap.md §1 |
| 2026-11-02 | **Editing: learn it once.** One session shell, four tool groups in the same places for every format | docs/design/editing.md |
| 2026-11-01 | **Folder access:** a picked folder grants everything under it, a picked file only itself; blocked files offer Open Folder… | PLAN.md §12.9 |
| 2026-11-01 | **One layout for every screen size,** by width class and input kind | docs/design/responsive-ui.md |
| 2026-10-31 | **One UI: libgui everywhere**; egui removed | PLAN.md §12.1 |
| 2026-10-31 | **One GPU device per process,** shared by every window and renderer | PLAN.md §12.2 |
| 2026-10-31 | **One instance, many windows; one file, one document** across windows | PLAN.md §12.3 |
| 2026-10-31 | **Parser host:** one interface; a helper process on desktop and Android, in-process on iOS and HarmonyOS | PLAN.md §12.4 |
| 2026-10-31 | **Documents are GPU-free data; editing is operations** on them | PLAN.md §12.5 |
| 2026-10-31 | **Startup is a constraint:** "app ready" never waits for the file; startup regressions fail CI | PLAN.md §7, §12.7 |
| 2026-10-31 | **RendererX:** a reusable wgpu renderer with swappable shading models | docs/design/3d-renderer.md §17 |

Still open: PLAN.md §12.10.
