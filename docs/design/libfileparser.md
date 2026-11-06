# libfileparser: open any file, in any app

Status: design agreed 2026-11-06; nothing built yet. Pre-1.0: every API here may change. Crate names are settled; type names are a first draft. Builds on [document-model.md](document-model.md); the 3D crate is designed in [libfileparser-3d.md](libfileparser-3d.md).

**Goal:** any app (an IDE, a game engine editor, a chat app, a file manager) can open and show any kind of file, taking only the parts it needs, cheaply. Viewer is the first user, not the owner: nothing here knows about Viewer, libgui or a GPU.

## 1. Crates

```
libfileparser                  front door: detection across categories, merging readings
   │                           into one list of facets, default hooks, features
   ├── libfileparser-images    ┐
   ├── libfileparser-3d        │
   ├── libfileparser-docs      │  category crates: each reads ANY file of its kind,
   ├── libfileparser-data      │  produces documents with facets, works on its own
   ├── libfileparser-text      │
   ├── libfileparser-archives  │
   ├── libfileparser-media     ┘  (later; FFmpeg)
   └── libfileparser-core      contracts: documents, facets, content models, hook traits

optional, beside them:
   libfileparser-thumb         any document → a small RGBA preview, on the CPU
   libfileparser-remote        parsing in a helper process, behind the same traits
   libfileparser-c             a C API for C, C++, Swift, Kotlin
```

| Crate | Reads | Primary facets it makes | Uses |
|---|---|---|---|
| **`-core`** | — | Defines everything below | Nothing heavy |
| **`-images`** | PNG, JPEG, WebP, GIF, TIFF, BMP, PNM, EXR, HDR, SVG; later HEIC, RAW, JPEG XL, PSD | Raster | A leaf |
| **`-3d`** | glTF/GLB/VRM, FBX, OBJ, STL, PLY, USD/USDA/USDC/USDZ, `.blend`; later CAD, point clouds | Scene | `-images` (textures), ufbx, `usd-lite` |
| **`-docs`** | PDF; later comics, DOCX, ODT, RTF, EPUB, PPTX, ODP | Pages, Flow | `-images`; feature `mupdf` (AGPL); PDFKit on Apple |
| **`-data`** | JSON, JSON Lines, TOML, YAML, XML, CSV/TSV; later XLSX, ODS, Parquet, SQLite | Table, Tree | — |
| **`-text`** | Text, code, logs, Markdown | Text, Markdown | — |
| **`-archives`** | Zip, tar, gzip, folders; later 7z, RAR, ISO | Container | — |
| **`-media`** (later) | Audio, video, subtitles | Media | FFmpeg (LGPL), optional |
| **`libfileparser`** | Whatever is enabled | — | The categories |

### Boundaries

1. **A category crate reads its formats completely,** including their structure, packages, dependencies and inspector facets ([document-model.md](document-model.md) §3). The USD reader, not a generic one, knows prims, layers and variants.
2. **Generic facets live in one place:** Raw (hex) in `-core`, plain Source (text by lines) in `-text`, container listings in `-archives`. The front door attaches them; category crates don't repeat them.
3. **Sideways only toward leaves:** `-3d` and `-docs` may use `-images` (and `-core`'s generic text and tree types); nothing may use `-3d` or `-docs`. The graph stays acyclic and each crate light.
4. **Heavy or copyleft parts are features inside their category** (`-docs` + `mupdf`, `-media` + FFmpeg), named in the feature list, never on by accident. A default build is MIT/Apache throughout.

## 2. `-core`

Only data and narrow traits; no format code.

### Documents and facets

As in [document-model.md](document-model.md) §5: a `Document` has a format, an ordered list of facet descriptors, metadata and dependencies; `facet(i)` makes a facet on first use.

### Content models

What a facet's data looks like. Large ones are **sources read by range**, never loaded whole.

| Model | Shape | Read by |
|---|---|---|
| `Raster` | Size, then pixels whole or in bands | Bands |
| `Pages` (`PageSource`) | Page count and sizes; render a region into the caller's buffer | Page and region |
| `Flow` (later) | Reflowing styled text with images (DOCX, EPUB) | Blocks |
| `Text` (`TextSource`) | Line count, encoding, language | Lines *a..b* |
| `Markdown` | Source text | — |
| `Table` (`TableSource`) | Columns, row count (maybe estimated), sheets | Rows *a..b* |
| `Tree` (`TreeSource`) | Nodes with a label, a value and children | Children of a node |
| `Container` | Entries (path, size, packed size, kind) | Open one entry as a file |
| `Scene` | `rendererx-scene` data: meshes, materials, textures, nodes; no GPU | Arrives progressively |
| `Media` (later) | Streams and metadata; decode a frame or samples into the caller's buffer | Time |
| `Hex` | The bytes | Offset ranges |

`Content` is `#[non_exhaustive]`, so new models don't break users.

`-core` includes the generic implementations everyone shares: a `TextSource` over any bytes (line index, encoding detection), a `TreeSource` over owned nodes, and the hex model.

### Hooks

```rust
pub struct ParseContext<'a> {
    pub files: &'a dyn FileResolver,   // neighbouring files: textures, .mtl, layers, buffers
    pub limits: Limits,                // pixels, sizes, unpack ratio, depth, memory, time
    pub cancel: Cancel,                // the document was closed: stop
    pub spawn: &'a dyn Spawn,          // where background work runs
    pub notify: &'a dyn Notify,        // "more arrived": a band, a texture, a page
}
```

| Hook | The app decides | Viewer's choice |
|---|---|---|
| `files` | Which neighbouring files may be read, and how | iOS: inside the picked folder's grant; desktop: next to the file |
| `limits` | How much a file may cost | Desktop and mobile presets (PLAN.md §8) |
| `cancel` | When to stop | Closing a page cancels its document |
| `spawn` | Threads, or none | A small pool; WASM or an engine: inline, or the engine's jobs |
| `notify` | How progress is heard | Wakes the window's event loop |

### Errors

```rust
pub enum ParseError {
    NotThisFormat,          // the front door tries the next reading
    Corrupt(String),        // the next reading, keeping the reason
    TooLarge(Limit),        // stop and say so; don't fall back to hex silently
    Unreadable(Dependency), // a file it needs is blocked or missing
    Cancelled,
    Io(std::io::Error),
}
```

## 3. Category crates

Same small surface in each, so they're interchangeable to the front door and usable alone:

```rust
pub fn readers() -> &'static [&'static dyn Reader];

pub trait Reader: Sync {
    fn id(&self) -> &'static str;                                   // "usd", "gltf"
    fn detect(&self, header: &[u8], name: &str) -> Detection;       // confidence, and for text formats their language
    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document, ParseError>;
}
```

- `detect` reads only the first bytes and the name, and allocates nothing. For a text-based format it also names the language, so the front door's Source facet highlights it correctly.
- `open` builds only what the primary facet needs and lists the rest ([document-model.md](document-model.md) §5).
- Beyond `src`, files come only through `cx.files`; background work only through `cx.spawn`; progress only through `cx.notify`. No `std::fs`, no threads, no globals.
- Every size a file claims is checked against `cx.limits` before anything is allocated.

## 4. `libfileparser`: the front door

```rust
let doc = libfileparser::open("scene.usdz")?;                     // the simple case

let lib = libfileparser::Library::new()                          // the app decides
    .files(MyGrants::new())
    .spawn(engine.jobs())
    .notify(ui.waker())
    .limits(Limits::mobile());
let doc = lib.open(&src)?;
for view in doc.views() { /* role, name, content model */ }       // every facet of every reading
let stage = doc.view(1)?;                                         // made now, on first use
```

What it does:

1. **Detection** across every enabled reader, best first.
2. **Open the best reading;** on `NotThisFormat` or `Corrupt`, the next.
3. **Merge views:** the document's facets, then the generic ones (Raw always; Source when text; Package when a container the reader didn't cover), then other readings' facets that add something new, made only when asked ([document-model.md](document-model.md) §4).
4. **Default hooks:** `StdFiles` (files beside the source, folder-grant aware), `ThreadPool`, `Limits::desktop()` and `Limits::mobile()`, a no-op `Notify`. Each replaceable alone.
5. **Containers:** a Package entry opens through the same router, as a child document.
6. **Features** choose categories (`images`, `3d`, `docs`, `data`, `text`, `archives`, `media`) and backends (`docs-mupdf`). Presets: `minimal` (pure Rust, permissive, WASM too), `default` (common formats, permissive), `full`.
7. **Nothing runs at startup:** readers are a static table; no category initialises until a matching file opens.

## 5. Who uses what

| App | Takes | Gets |
|---|---|---|
| Chat app, mail, file manager | `libfileparser` + `-thumb` | What a file is, metadata, a preview image; no GPU, no UI |
| IDE or editor | `libfileparser` with `text`, `data`, `images`, `docs`, `3d` | Every facet: source with its language, structure trees, tables, pages, scenes, in its own widgets |
| Game engine editor | `-3d` + `-images` alone | Scenes, structure (USD stage, glTF nodes) and dependencies as data, with its own files and jobs through the hooks |
| Viewer | All of it, plus `-remote` for MuPDF on desktop | Everything, with its own policy |
| Non-Rust apps | `-c` | The same, through C |

## 6. What stays out

| Not in libfileparser | Where |
|---|---|
| GPU drawing | RendererX for scenes ([3d-renderer.md](3d-renderer.md)); later a small wgpu crate for tiled images and pages |
| Widgets | UI adapters: Viewer's libgui views, later `libfileparser-libgui` |
| Syntax highlighting, themes | A small crate over `Text` + language, or the app |
| Editing | Edit sessions per format ([editing.md](editing.md)) |
| Process isolation | `-remote`, optional |
| Policy | The app, through the hooks |

## 7. Rules every crate follows

The memory and control rules of [architecture.md](architecture.md) §3, as checks:

- Reads only what it needs: the header to detect, then ranges on demand.
- Writes big results into the caller's buffers.
- Checks claimed sizes against `Limits` before allocating.
- Keeps caches bounded and sized by the caller.
- Starts no threads and keeps no globals.
- Reports `memory()`.
- Builds for desktop, iOS and Android; `-core`, the pure-Rust categories and `-thumb` also for WASM.
- Is tested by the corpus (corpus/README.md), each category alone and through the front door; every facet a reader lists is opened by the corpus.

## 8. Building it

The corpus passes after every step.

1. **`-core`:** documents with facets, content models with range sources, generic Text/Tree/Hex, hooks, `Limits`, `ParseError`, `Dependencies`, `MemoryUse`.
2. **`-3d`**, as designed in [libfileparser-3d.md](libfileparser-3d.md): it uses every hook and has the richest facets (USD above all). If the model holds there, it holds everywhere.
3. **The front door** with `-3d`, `-text` (for Source facets) and `-archives` (for Package facets); Viewer's 3D loading and View-as list switch to it.
4. **The other categories,** one at a time: `-images`, `-data`, `-docs`, moving today's `viewer-fmt-*` loaders.
5. **`-thumb`** and a thumbnail CLI, as the first user outside Viewer.
6. **`-remote`** (MuPDF in a helper on desktop), then **`-c`**.

Everything stays in the Viewer workspace until the API settles, then moves to its own repo, as libfilepicker did.
