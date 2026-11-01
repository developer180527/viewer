# Viewer

Open any file: a fast, cross-platform viewer written in Rust. See [PLAN.md](PLAN.md) for the full design.

## Status

Runs on macOS, Windows and Linux, and on iPhone and iPad. One UI everywhere (libgui on wgpu); Android is not started yet.

| Content model | Formats |
|---|---|
| Raster | PNG, JPEG, GIF, WebP, BMP, TIFF, ICO, TGA, QOI, PNM, HDR, EXR, DDS, farbfeld, SVG |
| Text | Source code with syntax highlighting (syntect), plain text, logs (multi-GB, memory-mapped) |
| Rich text | Markdown |
| Tree | JSON, JSON Lines, TOML |
| Table | CSV, TSV, PSV (delimiter is sniffed) |
| 3D scene | glTF / GLB / VRM, FBX, OBJ (+ `.mtl`), STL (binary and ASCII), PLY (meshes and point clouds, vertex colours, splat colours), USD / USDA / USDC / USDZ (sublayers, references, payloads, variants), drawn by RendererX: clay, PBR and unlit shading, textures decoded off-thread. Blender `.blend` shows its saved preview image |
| Container | Folders, zip (and zip-based formats, e.g. DOCX, XLSX, EPUB, CBZ, shown as their file lists), tar, tar.gz, gz. Entries open in place, nested to any depth |
| Hex | Everything else, with detected MIME type and entropy |

The **View as** menu re-opens the current file with any loader that claims it (e.g. view a `.toml` as text).

Files a model refers to but Viewer can't read are reported: *blocked* ones (on iOS, picking a model alone doesn't grant the textures next to it) get a banner with **Open Folder…**; *missing* ones are listed in the info panel.

Not yet: PDF and other documents, audio and video, editing. See the roadmap in [PLAN.md](PLAN.md) §11.

## Run

```bash
cargo run --release -- samples/bundle.zip
```

Keys: `⌘/Ctrl+O` open · `Backspace` / `Alt+←` back · `I` info panel. Images: scroll or pinch to zoom, drag to pan, double-click toggles fit / 100%, `0` fit, `1` 100%. 3D: drag to orbit, Shift-drag or two fingers to pan, scroll or pinch to zoom, double-click or `0` to reframe.

On a device: `ios/run-device.sh "<device>" <TEAM_ID>` (or `ios/run-sim.sh` for the simulator).

## Develop

```bash
cargo test --workspace
```

```bash
python3 bench/make-samples.py --big
```

```bash
bench/startup.sh
```

Automation flags: `--benchmark` (prints startup timings and exits once the file is on screen), `--screenshot out.png`, `--view-as <loader>`, `--list-loaders`.

## Layout

```
crates/core          traits, content models, registry, hex fallback
crates/fmt-image     image + SVG loaders
crates/fmt-doc       text / code / Markdown loaders
crates/fmt-data      JSON / TOML / CSV loaders
crates/fmt-archive   zip / tar / gzip containers, folders
crates/fmt-3d        glTF, FBX/OBJ (ufbx), STL, PLY, USD, .blend previews; texture decoding
crates/rendererx*    RendererX: the reusable 3D renderer, its CPU scene and glTF loader
crates/usd-lite      pure-Rust USD reader and composition
crates/app           libgui + wgpu shell and views
ios/                 Xcode wrapper and device / simulator scripts
bench/               sample generator, startup and memory benchmarks
samples/             one small file per supported format
```
