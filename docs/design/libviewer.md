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
- **What stays in the app:** windows, menus, navigation, policy (which files may be read), the highlighter (`Highlights`: Viewer's is syntect), and views for content libfileparser has no model for yet.
- **Threads:** the raster view decodes bands on a thread of its own (the library side, libfileparser, starts none: it gives a band source, and the view decides where to pull from it).

## 4. Order of work

1. ✅ **Split out** (2026-11-07): the views, the host contract and the 3D helper, moved from `crates/app/src/lg/`; views read libfileparser's content models directly, so Viewer's facets are no longer converted (trees lazy without a cap, tables live, containers listed on demand). Viewer's own loaders (JSON, TOML, CSV) build libfileparser trees and tables; its highlighted text is a `TextSource` with `Highlights`.
2. **Renderers apart from views:** the toolkit-free part (tiling, band streaming, 3D frames) as a crate of its own, so a view for another UI toolkit reuses it (architecture.md §2).
3. ✅ **Pages and Flow** with `-docs`, and their views here (2026-11-08): the tiled page view moved from Viewer; `FlowView` shows formatted text (Markdown, DOCX, ODT, EPUB, RTF, Org) with libgui's Markdown widget. ✅ 2026-11-19: in Inter's bold, italic and bold italic, which the app registers the first time formatted text is drawn; with its images, which a flow serves by reference (`FlowSource::resource`: a package's parts, a notebook's outputs, a FictionBook's binaries, a Kindle book's image records, a message's `cid:` parts), the host opens (`Host::image_opener`), and the view decodes and scales to 2048 px on a thread of its own, uploads a few per frame, and fits to the column. Not yet: underline, superscript and subscript (Markdown has none), and images within a line of text (their alt text).
4. **Links in views:** select in one view, highlight in another (`Document::links`), as an inspector does.
5. ✅ **Find in file** (2026-11-28). The search is libfileparser's (`libfileparser_core::find`): each content model searched through its own range reads, a batch of lines, rows, entries or blocks, or a megabyte of bytes, at a time, case ignored, each match a `Spot` in its view's terms (a line and the bytes in it, a cell, an entry, a node and the way to it, a block, a page and its rectangles). Pages are searched by the engine (`PageSource::find_text`: MuPDF's structured text, PDFKit's page string and selections). A hex view finds the query as text and, when it's hex digits ("4d 5a"), as the bytes they spell. libviewer's `Search` runs one on a thread of its own, keeps the first 10,000 matches and wakes the window as they arrive; `show_found` draws them and brings the current one into view. Viewer's find bar (⌘F, Enter and Shift-Enter, ⌘G and Shift-⌘G, Escape) searches the current page and follows it, View as included.
   - Text and hex mark each match; trees open to the current match and select it; PDF pages box each match and scroll to the current one.
   - Not yet: **tables and containers** mark matching cells and select the current row, but can't scroll to a row far off (libgui's table takes no row to reveal). **Formatted text** tints the part (a few paragraphs) holding the current match, as the Markdown widget can't mark words. Both need libgui changes. The Apple apps' native interface has no find bar yet. Images and 3D scenes have nothing to find.
