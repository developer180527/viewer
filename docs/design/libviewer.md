# libviewer: show any facet of any file, in any app

Status: first crate split out of Viewer 2026-11-07 (`crates/libviewer`). Pre-1.0. The rendering half of [architecture.md](architecture.md) §2; libfileparser ([libfileparser.md](libfileparser.md)) is the reading half.

**Goal:** an app that reads a file with libfileparser shows every facet of it with libviewer, without knowing the file's format: one view per content model, not one per format. Adding a format to libfileparser adds nothing here.

## 1. What it is

| Content model (libfileparser-core) | View | Reads |
|---|---|---|
| `Text` | `text`: numbered lines, coloured by the app's `Highlights` | The visible lines, as they scroll into view |
| `Hex` | `hex`: offset, 16 bytes, printable text | The visible rows of the file's bytes |
| `Tree` | `tree`: expandable outline | A node's children when it is first expanded; up to 10,000 per node, the rest summarised in one row |
| `Table` | `table`: columns from the table's info | The visible rows, every frame: a live table (Textures going from decoding to decoded) shows what it holds now |
| `Container` | `container`: name, size, packed size | The visible entries; the clicked one comes back as `Action::OpenEntry`, to open as a file of its own |
| `Raster` | `raster`: zoom, pan, tiles on the GPU | Whole pixels a budget of tiles per frame; an image in bands from its `BandSource` on a thread of the view's own, two bands ahead of the GPU, stopped when the view is dropped |
| `Scene` | `scene`: RendererX, orbit, pan, zoom | The scene as it fills in; textures as they arrive |

`View::new(&content)` makes a content's view state (scroll, what's expanded, GPU tiles), kept between frames; `show(ui, &content, &mut view, host, highlights, dark)` draws it with the right view. `View::textures()` lists the GPU textures to free when the page closes.

Nothing is converted or copied up front: a view reads its content through the content model's own range API, so a tree of a million FBX records, a 100,000-row CSV and a 16K image cost what's on screen.

## 2. The host

The app is the `Host`, the one contract between views and the app:

| Method | For |
|---|---|
| `upload`, `free`, `max_side` | Images: GPU textures, or the CPU renderer's for screenshots |
| `render_scene` (with `draw_scene`, `SceneGpu`, `Frame3d`) | 3D: RendererX on the app's device |
| `pixels_per_point`, `trackpad_pinch` | Native-resolution 3D, trackpad zoom |
| `settles` | Screenshots: wait for bands and textures before drawing |
| `uploads_done` | Free upload staging after a big upload |
| `waker` | Wake a sleeping window when a band or texture arrives |

Viewer has two hosts: the window (wgpu) and `--screenshot` (libgui's CPU renderer, a windowless GPU for 3D).

## 3. Boundaries

- **libviewer depends on** `libfileparser-core` (the content models), libgui, RendererX and wgpu. Never on a category library or a format: it can't know one.
- **What stays in the app:** windows, menus, navigation, policy (which files may be read), the highlighter (`Highlights`: Viewer's is syntect), and views for content libfileparser has no model for yet: Markdown and PDF pages, which move here with `-docs`.
- **Threads:** the raster view decodes bands on a thread of its own (the library side, libfileparser, starts none: it gives a band source, and the view decides where to pull from it).

## 4. Order of work

1. ✅ **Split out** (2026-11-07): the views, the host contract and the 3D helper, moved from `crates/app/src/lg/`; views read libfileparser's content models directly, so Viewer's facets are no longer converted (trees lazy without a cap, tables live, containers listed on demand). Viewer's own loaders (JSON, TOML, CSV) build libfileparser trees and tables; its highlighted text is a `TextSource` with `Highlights`.
2. **Renderers apart from views:** the toolkit-free part (tiling, band streaming, 3D frames) as a crate of its own, so a view for another UI toolkit reuses it (architecture.md §2).
3. **Pages and Markdown** with `-docs`, and their views here.
4. **Links in views:** select in one view, highlight in another (`Document::links`), as an inspector does.
