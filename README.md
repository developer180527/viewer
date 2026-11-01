# Viewer

Open any file: a fast, cross-platform viewer written in Rust. See [PLAN.md](PLAN.md) for the full design.

## Status: desktop MVP

| Content model | Formats |
|---|---|
| Raster | PNG, JPEG, GIF, WebP, BMP, TIFF, ICO, TGA, QOI, PNM, HDR, EXR, DDS, farbfeld, SVG |
| Text | Source code with syntax highlighting (syntect), plain text, logs (multi-GB, memory-mapped) |
| Rich text | Markdown |
| Tree | JSON, JSON Lines, TOML |
| Table | CSV, TSV, PSV (delimiter is sniffed) |
| Container | Zip (and zip-based formats), tar, tar.gz, gz. Entries open in place, nested to any depth |
| Hex | Everything else, with detected MIME type and entropy |

The **View as** menu re-opens the current file with any loader that claims it (e.g. view a `.toml` as text).

## Run

```bash
cargo run --release -- samples/bundle.zip
```

Keys: `⌘/Ctrl+O` open · `Backspace` / `Alt+←` back · `I` info panel. Images: scroll or pinch to zoom, drag to pan, double-click toggles fit / 100%, `0` fit, `1` 100%.

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
crates/fmt-archive   zip / tar / gzip containers
crates/app           libgui + wgpu shell and views
bench/               sample generator, startup benchmark
samples/             one small file per supported format
```
