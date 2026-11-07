# libfileparser: open any file, in any app

Status: design agreed 2026-11-06. Built: `-core` and `-testkit` (2026-11-06); `-3d` (2026-11-06–07, [libfileparser-3d.md](libfileparser-3d.md)); `-text`, `-archives` and the `libfileparser` front door, which Viewer opens 3D files, archives and folders through, its View as listing every facet (§12 step 3, 2026-11-07); `-images` (§12 step 4, 2026-11-07). Data and docs are still Viewer's own loaders. Pre-1.0: every API here may change until §5's checks are in place. Crate names are settled; type names are a first draft. Builds on [document-model.md](document-model.md). [libfileparser-3d.md](libfileparser-3d.md) applies this design to 3D.

**Goal:** any app (an IDE, a game engine editor, a chat app, a file manager) can open and show any kind of file, taking only the parts it needs, cheaply. Viewer is the first user, not the owner: nothing here knows about Viewer, libgui or a GPU.

## 1. What every category library must be

These four hold for **every** category library (`-images`, `-3d`, `-docs`, `-data`, `-text`, `-archives`, `-media`, and any added later). The rest of this document is how.

| # | Requirement | Means | How (§) |
|---|---|---|---|
| 1 | **Complete for its category** | It aims to read every format of its kind, and every facet of each (document-model.md §3). Formats it can't read yet are still detected and named, never guessed | §3, §6 |
| 2 | **A stable, flat, clear API** | Every category has the same core shape, so knowing one means knowing the others; a category may add API its kind needs. Internals change freely behind it | §4, §5 |
| 3 | **A new format is trivial to add** | One module implementing one trait, one line to register it, one corpus file. Everything else applies automatically | §6 |
| 4 | **Embeddable** | No global state, no threads or runtime of its own, no file system access of its own, few dependencies, every platform, and a C API | §7 |

## 2. Crates

```
libfileparser                  all categories together, in the shared model: detection
   │                           across them, readings merged into one document, features
   ├── libfileparser-images    ┐
   ├── libfileparser-3d        │
   ├── libfileparser-docs      │  category libraries: each reads ANY file of its kind,
   ├── libfileparser-data      │  into documents with facets, and works on its own
   ├── libfileparser-sheets    │  (§2.1)
   ├── libfileparser-datasets  │  (§2.1)
   ├── libfileparser-text      │
   ├── libfileparser-archives  │
   ├── libfileparser-media     ┘  (later; FFmpeg)
   └── libfileparser-core      contracts: documents, facets, content models, hooks, Reader

optional, beside them:
   libfileparser-thumb         any document → a small RGBA preview, on the CPU
   libfileparser-remote        parsing in a helper process, behind the same traits
   libfileparser-c             a C API for C, C++, Swift, Kotlin
   libfileparser-testkit       fakes for the hooks, and the conformance suite every reader passes
```

| Crate | Reads | Primary facets | Uses |
|---|---|---|---|
| **`-core`** | — | Defines everything below | Nothing heavy |
| **`-images`** | PNG, JPEG, WebP, GIF, TIFF, BMP, PNM, EXR, HDR, SVG; later HEIC, RAW, JPEG XL, PSD | Raster | A leaf |
| **`-3d`** | glTF/GLB/VRM, FBX, OBJ, STL, PLY, USD/USDA/USDC/USDZ, `.blend`; later CAD, point clouds | Scene | `-images`, ufbx, `usd-lite` |
| **`-docs`** | PDF; later comics, DOCX, ODT, RTF, EPUB, PPTX, ODP | Pages, Flow | `-images`; feature `mupdf` (AGPL); PDFKit on Apple |
| **`-data`** | Data written as text: structured, config, delimited tables, schemas, RDF, graphs, geo, logs, science records, SQL, contacts and calendars, business messages, localization, feeds (§2.1) | Tree, Table | Pure Rust, a feature per family |
| **`-sheets`** | ODS, FODS, SYLK, DIF; later XLSX, XLSM, XLS, Numbers | Workbook (a Container of Tables) | zip, XML |
| **`-datasets`** | HDF5, NetCDF, FITS, MATLAB, R, SAS, Stata, SPSS, Parquet, ORC, Feather, SQLite | Table, Tree | Heavy readers, as features |
| **`-text`** | Text, code; Markdown until `-docs` owns it | Text, Markdown | — |
| **`-archives`** | Zip, tar, gzip, folders; later 7z, RAR, ISO | Container | — |
| **`-media`** (later) | Audio, video, subtitles | Media | FFmpeg (LGPL), optional |
| **`libfileparser`** | Whatever is enabled | — | The categories |

**Boundaries between categories:**

1. **A category reads its formats completely,** including structure, packages, dependencies and inspector facets. The USD reader, not a generic one, knows prims, layers and variants.
2. **Generic facets live in one place:** Source (the file's text by line) and Raw (its bytes) in `-core`, which every category's documents end with (`Document::with_file_facets`), so a category used alone has them; container listings in `-archives`, which the front door adds to a zip no category listed. No category writes its own. (Until 2026-11-07 Source came from the front door only, so `-3d` alone had none.)
3. **Sideways only toward leaves:** `-3d` and `-docs` may use `-images` and `-core`'s generic types; nothing may use `-3d` or `-docs`.
4. **Heavy or copyleft parts are features inside their category** (`-docs` + `mupdf`, `-media` + FFmpeg), named in the feature list, never on by accident. A default build is MIT/Apache throughout.
5. **Every format has one owner; other categories may also read it** (§2.1).

### 2.1 Which category owns which format

Decided 2026-11-08. ✅ The rule is built (2026-11-08): `ReaderInfo::also_reads` (item 6); `Library::detect` puts owners first and adds the others' primary facets as Structure facets, made when asked for; Viewer's adapter ranks the same way. Formats: as marked below.

**The rule.**

1. **One owner per format:** the category that matches what the file is *for*. HTML is a document, SVG an image, subtitles media. In the front door, the owner's reading is the document and its primary facet.
2. **Others may also read it,** from their own angle, when their model says something useful: `-data` reads an SVG as an XML tree. Used alone, that category opens the file that way, so "reads any file of its kind" means every format it owns plus every format its model fits. In the front door it doesn't compete: the Library asks it as a facet provider, and its facets come after the owner's, before Source and Raw, made only when asked for.
3. **No owner enabled or built yet:** the best other reading opens the file instead. Subtitles open as `-data`'s cue table until `-media` exists; then the media facet goes in front and the cue table stays. Adding an owner later is a minor version (§5).
4. **Text needs no claim:** `-core` gives every text file Source and Raw (§2, boundary 2), so `-text` owns plain text and code only, never JSON, SQL or logs.
5. **Content decides; the extension is a hint.** A reader claims a file only when its bytes match (the ambiguous extensions are below). What nothing claims opens as text or Raw, never as a wrong tree.
   **Specific beats generic** (2026-11-08). Many formats share one extension or one container syntax: XML is SVG, GPX, KML, RSS, plists and XSD; JSON is glTF, GeoJSON and HAR; zip is USDZ, DOCX and ODS. A *generic* reader (XML, JSON, zip) detects at most `Confidence::Extension`, even when the content proves the syntax. A *specific* reader sniffs what makes it itself: the root element (`<svg`, `<gpx`, `<!DOCTYPE plist`), a key (`"asset"` for glTF, `"type": "FeatureCollection"`), or a zip entry name (`mimetype`, `*.usda`), at `Content` or `Magic`, or at `Certain` for a format inside a generic container. So a `.xml` that is really GPX opens as GPX, and with no specific reader as a tree.
6. **How:** a reader's `extensions` are the formats it owns; `also_reads(extensions, model)` names the ones it only also reads, and the model of the facet it adds (a new field on a `#[non_exhaustive]` type, so not breaking). The Library opens with the owner and adds the others as facet providers. A guess from content is no stronger than an extension (`Confidence::Extension`), so it never outranks a reader that owns the file's extension: a `.gltf` is glTF, though it looks like JSON.
7. **Parsing lives once.** When two categories read one format, the parser is in one crate (or a `-lite` format library) and the other uses it, keeping §2's boundary 3.

**`-data`: owner.** One crate, a Cargo feature per family, all on by default.

| Family (feature) | Formats | Facets |
|---|---|---|
| Structured (always) | ✅ `.json` `.jsonl` `.ndjson` `.json5` `.jsonc` `.hjson` `.yaml` `.yml` `.toml` `.xml` `.plist`; `.sgml` `.sgm` not yet | Tree; JSON Lines a Table of its keys first |
| Delimited (always) | ✅ `.csv` `.tsv` `.dsv` `.psv` `.ssv` `.dat` `.prn`; `.fwf` not yet | Table |
| `config` | ✅ `.ini` `.properties` `.env` `.conf` `.cfg`; `.hcl` `.tf` `.cue` `.dhall` `.ncl` `.kdl` `.cron` not yet | Tree (parsed as written, never evaluated); cron a Table |
| `schemas` | `.proto` `.avsc` `.thrift` `.fbs` `.capnp` `.smithy` `.graphql` `.gql` `.openapi` `.asyncapi` `.xsd` `.rng` `.sch` `.wsdl` `.dbml` `.prisma` | Tree of types and fields |
| `rdf` | `.rdf` `.owl` `.ttl` `.nt` `.nq` `.trig` `.jsonld` `.trix` `.rq` `.sparql` | Table of triples or quads, Tree; queries a Tree |
| `graphs` | `.graphml` `.gexf` `.gxl` `.dot`, Graph Modelling `.gml` | Tables of nodes and edges, Tree |
| `geo` | `.geojson` `.topojson` `.kml` `.kmz` `.gpx` `.gml` `.wkt` `.osm` `.nmea` | Tree, Table of features; a map view later, in libviewer |
| `logs` | `.log` `.syslog` `.gelf` `.cef` `.ltsv` `.har` | Table of entries |
| `science` | `.arff` `.dimacs` `.fasta` `.fa` `.fna` `.fastq` `.fq` `.gb` `.gbk` `.gff` `.gff3` `.bed` variant-call `.vcf` `.sam` `.pdb` `.ent` `.cif` | Table of records, Tree |
| `sql` | `.sql` `.dump` `.cql` | Tree of statements; Tables from `CREATE`/`INSERT` |
| `records` | vCard `.vcf`, `.ics` `.ical` `.ldif` | Table of entries, Tree |
| `business` | `.edi` `.x12` `.hl7` `.ofx` `.qif` `.fix` `.fin` `.xbrl` | Tree of segments, Table |
| `l10n` | `.po` `.pot` `.mo` `.xlf` `.xliff` `.tmx` `.resx` `.strings` `.stringsdict` `.arb` | Table of keys and translations |
| `feeds` | `.m3u` `.m3u8` `.pls` `.xspf` `.rss` `.atom` | Table of entries, Tree |

✅ Built (2026-11-08), `crates/libfileparser-data`, replacing Viewer's `viewer-fmt-data`. Readers `json`, `json-lines`, `json5`, `hjson`, `yaml`, `toml`, `xml`, `plist`, `ini`, `properties`, `env`, `delimited`.
- **Trees** keep the file's order and show numbers as written; YAML aliases are shown, never expanded. Every node links to its lines in Source and back (`Links`), and JSON Lines' rows to their lines and nodes.
- **Tables** are read by row range from an index of row starts (12 bytes a row), quotes respected; rows link to their lines.
- **No input overflows the stack:** the parsers keep nesting on their own stacks or in events, and `limits.depth` (512 on desktop) stops them.
- A `.json` with comments or trailing commas opens as "JSON with comments". `.conf`, `.cfg`, `.dat` and `.prn` are claimed only when their content fits.
- Until a family's own reader exists, formats written in JSON (`.geojson`, `.har`, `.ipynb`, `.jsonld`, `.avsc`, `.arb`, …) open as JSON trees and formats written in XML (`.xsd`, `.rss`, `.kml`, `.gpx`, `.graphml`, `.xliff`, `.rdf`, …) as XML trees. `-data` also reads `.svg`, `.xhtml` and `.ttml` as XML.
- The other families are still to come, each a reader of its own.

`.kmz` is a zip: `-data` reads the KML inside, and the zip is a Package facet from `-archives`.

**`-sheets` and `-datasets`: owners.** They are separate from `-data` because of their dependencies and models (2026-11-08):

| Crate | Formats | Why not in `-data` |
|---|---|---|
| **`-sheets`** | `.ods` `.fods` `.slk` `.dif`; later `.xlsx` `.xlsm` `.xls` `.numbers` | Workbooks: several sheets, typed cells, formulas and their values, merged cells; zip and XML underneath |
| **`-datasets`** | `.h5` `.hdf5` `.nc` `.fits` `.fit` `.mat` `.rds` `.rda` `.sas7bdat` `.dta` `.sav` `.parquet` `.orc` `.feather` `.sqlite` `.sqlite3` `.db` | Binary arrays and column stores; large readers, some in C, each a feature |

**Owned elsewhere; `-data` also reads:**

| Formats | Owner | `-data` adds |
|---|---|---|
| `.html` `.htm` `.xhtml` | `-docs` | DOM tree |
| `.md` `.markdown` | `-docs` (`-text` until then) | Front matter as a Tree |
| `.rst` `.adoc` `.org` `.textile` `.rtf` | `-docs` | Nothing |
| `.svg` | `-images` | XML tree |
| `.srt` `.vtt` `.ttml` | `-media` | Table of cues; opens them until `-media` exists |

**Owned here; other categories also read:** `.pdb` `.ent` `.cif` (`-data`), a molecule Scene from `-3d` (later); `.fits` (`-datasets`), an image from `-images`; `.kmz` (`-data`), the Package facet from `-archives`.

**Ambiguous extensions,** told apart by content:

| Extension | Could be | Tells them apart |
|---|---|---|
| `.vcf` | Variant calls (`science`) or vCard (`records`) | `##fileformat=VCF` vs `BEGIN:VCARD` |
| `.gml` | Geography Markup (`geo`) or Graph Modelling (`graphs`) | XML vs `graph [` |
| `.fit` | FITS or Garmin FIT (both `-datasets`) | `SIMPLE  =` vs `.FIT` at byte 8 |
| `.mat` | MATLAB v5 or v7.3 (HDF5), both `-datasets` | Header text vs HDF5 signature |
| `.db` | SQLite, or anything else (Thumbs.db, Berkeley DB) | SQLite header; otherwise Raw |
| `.dat` `.prn` | Delimited text, or anything | Delimiters found; otherwise Raw |
| `.conf` `.cfg` | INI-like, or a program's own syntax | Sections or `key = value` lines; otherwise text |
| `.sch` | Schematron, or a circuit schematic | XML only; otherwise Raw |
| `.ent` | PDB, or an XML entity file | PDB record names; otherwise text |
| `.fin` | SWIFT MT message, or anything | `{1:` block; otherwise Raw |
| `.strings` | Text or binary plist | `bplist` signature |

## 3. `-core`: the contracts

Only data and narrow traits; no format code.

### Documents and facets

As in [document-model.md](document-model.md) §5: a `Document` has a format, an ordered list of facet descriptors, metadata and dependencies; `facet(i)` makes a facet on first use.

### Content models

What a facet's data looks like. Large ones are **sources read by range**, never loaded whole.

**Every source follows one rule** (2026-11-06): it gives a count first (`Count::Exact`, or `Count::AtLeast` when counting would mean reading the whole file), then hands out **ranges** of items; every read **can fail** (`Result`), so a lazy or remote source can report an I/O error, damage or cancellation; and what it returns is **owned or shared** (`Bytes`: a range that keeps the mapped file alive), never a borrow of the source's insides. A range past the end returns fewer items; an id or index the source never handed out is `InvalidRequest`. So any source can later be lazy, decoded on demand, or served from a helper process without changing callers.

| Model | Shape | Read by |
|---|---|---|
| `Raster` | Size, then pixels: whole, or a `BandSource` that decodes bands of rows when asked, on the caller's thread, each time from the top. A document holds how to decode an image, never the decoded image; the app chooses where to decode and how much (`Raster::decode` for all at once) | Bands |
| `Pages` (`PageSource`) | Page count and sizes; render a region into the caller's buffer | Page and region |
| `Flow` (later) | Reflowing styled text with images (DOCX, EPUB) | Blocks |
| `Text` (`TextSource`) | `info()`: line count, encoding, language | `lines(a..b)` as shared bytes |
| `Markdown` (with `-text`) | Source text | — |
| `Table` (`TableSource`) | `info()`: columns (a name and a `ValueKind` each), row count (maybe estimated) | `rows(a..b)` as typed `Cell`s |
| `Tree` (`TreeSource`) | Nodes with an id, a label, a value, a `ValueKind` and whether they have children | `child_count(node)`, then `children(node, a..b)`; `value(node)` for a leaf's whole value as a `Cell`; `find(path)` and `path(node)` where the tree has paths |
| `Container` | `entry_count()`; entries (path, size, packed size, kind) | `entries(a..b)`; open one entry as a file |
| `Scene` | `scene-data` data: meshes, materials, textures, nodes; no GPU | Arrives progressively |
| `Media` (later) | Streams and metadata; decode a frame or samples into the caller's buffer | Time, through a decoder made for each use |
| `Hex` | The bytes | Offset ranges |

Built so far: `Raster`, `Text`, `Tree`, `Table`, `Container`, `Scene` and `Hex`, what `-3d` needs; `Pages`, `Flow`, `Markdown` and `Media` arrive with their categories.

**Media, when it comes, is a factory, not a decoder.** `Content` is cheap to clone and shared between views, but a decoder has state: a position, buffered frames, a codec context. So `Media` holds what describes the streams (counts, durations, sizes) and makes a new decoder each time one is asked for (`media.decoder(stream)`), as `BandSource::bands()` starts again on each call. Two views of one video then each play from where they are, and a thumbnailer seeking to the middle disturbs nobody.

**A facet is made once** (2026-11-08). `Document::facet` holds a per-facet lock while making it, so a second thread asking at the same time waits for that result instead of making it again: indexing a 2 GB log happens once. A maker that fails is tried again on the next ask. A maker must not ask for its own facet.

**Typed values** (2026-11-08). A table cell is a `Cell`: `Null`, `Bool`, `Int`, `Float`, `Number` (kept as written when converting would change it: `1.50`, `007`, a 30-digit integer), `Text`, `Bytes`, `Date`, or `Error` (a value that couldn't be read). Null is not empty text. A column says its `ValueKind` (`Number`, `Text`…, `Mixed`, or `Unknown`), so a view aligns numbers and can sort. A tree node says its kind too (`Map`, `List`, `Group`, `Text`, `Number`, `Null`…), so `"1"` and `1` look different. `TreeNode::value` is plain text without quotes, possibly shortened past `TreeNode::PREVIEW_CHARS` to keep listing children cheap; `TreeSource::value` gives a leaf whole. Shortening for display, and quoting text, are the view's.

**Sources take no context.** `lines`, `rows`, `children` and the like run long after `open`, without a `ParseContext`: a source that may do long work keeps the document's `Cancel` (and limits) from when it was made, and checks them itself.

`-core` also holds the generic implementations every category shares: `LineText` (a `TextSource` over any bytes, indexed by line), `OwnedTree`, `OwnedTable`, and the hex model. `FileSource::part` makes a range of a file a file of its own, sharing its bytes: a container's entries (a GLB's chunks, an image inside a model) open that way without a copy.

### Hooks

```rust
#[derive(Clone)]
pub struct ParseContext {
    pub files: Arc<dyn FileResolver>,  // neighbouring files: textures, .mtl, layers, buffers
    pub limits: Limits,                // pixels, sizes, unpack ratio, depth, geometry, nodes, rows, entries, expansion, memory
    pub cancel: Cancel,                // the document was closed: stop
    pub spawn: Arc<dyn Spawn>,         // where background work runs
    pub notify: Arc<dyn Notify>,       // "more arrived": a band, a texture, a page
}

let cx = ParseContext::new()                     // no other files, desktop limits,
    .with_files(FolderFiles::new())              // background work inline, no notifications
    .with_limits(Limits::mobile());
```

**Limits by kind** (2026-11-08): one count for vertices, rows and records was too coarse, so each kind has its own: `geometry` (vertices, indices, triangles), `nodes` (tree nodes, a format's records), `rows`, `entries` (archives, folders), and `expansion`, the values reached through references rather than written out (YAML aliases, XML entities, instancing), so a small file can't expand past it without any one allocation being large. Today's readers don't expand references (`-data` shows aliases and leaves unknown entities as written); a reader that does checks `expansion`.

The hooks are owned, not borrowed: background work (texture decoding) outlives the `open` call that started it, and takes its own clone of the context.

`-core` includes the simplest hooks, so a category library works on its own, `InlineSpawn` (background work runs at once on the caller's thread) and `NoNotify`, and three file resolvers, safest first (2026-11-06):

| Resolver | Reads | For |
|---|---|---|
| **`NoFiles`** (the default for `ParseContext::new()`) | Nothing beyond the bytes given | Servers and thumbnailers handling untrusted uploads; anything that hasn't chosen |
| **`FolderFiles`** | The source's folder and below; optionally *n* folders up (`allow_parent_levels`) | Viewers and editors. A reference leaving the folder (`../..`, an absolute path, a symlink out) is `Unreadable::Outside` and isn't read |
| **`AnyFiles`** | Anything: absolute paths, `..` anywhere | Pipelines over files you trust, never files from elsewhere |

Why: a hostile model could otherwise name `/home/me/private.png` as a texture and get it read and drawn, which matters most for apps embedding the library on a server. An exporter's absolute texture path still works with `FolderFiles`: readers look for the file by name inside the folder (`Dependencies::resolve_any`). The front door adds a thread pool and folder-grant-aware files.

**Background work** goes through `cx.spawn_job`, which catches a panic, and the format code inside a job (a decoder) runs in `ParseContext::catching`, so its bookkeeping (counting pending textures) still happens when it panics.

**One place reads other files:** `Dependencies::resolve(cx, src, reference)` asks `cx.files` and records the outcome as read, blocked or missing, so a document's dependency list is complete by construction. A file that may be in several places (a texture stored with the exporter's path, found by name beside the model or in `textures/`) uses `Dependencies::resolve_any`, which tries the candidates in order and records one outcome under the name the document gives. Each entry also says what it is used as (`set_kind`: "texture", "layer") and where it was found, and `Document::with_dependencies_facet` shows the list as a Dependencies facet, the same table in every category, kept current as background work adds to it.

| Hook | The app decides | Viewer's choice |
|---|---|---|
| `files` | Which neighbouring files may be read, and how | `FolderFiles` with one level up (`../textures/`); on iOS the OS also refuses what's outside the picked folder's grant, reported as blocked |
| `limits` | How much a file may cost | Desktop and mobile presets (PLAN.md §8) |
| `cancel` | When to stop | Closing a page cancels its document |
| `spawn` | Threads, or none | A small pool; WASM or an engine: inline, or the engine's jobs |
| `notify` | How progress is heard | Wakes the window's event loop |

### Errors

```rust
#[non_exhaustive]
pub enum ParseError {
    NotThisFormat,          // the front door tries the next reading
    #[non_exhaustive]
    Corrupt { why: String, at: Option<Position> },   // the next reading, keeping the reason and where
    TooLarge(Limit),        // stop and say so; don't fall back to hex silently
    Unreadable { reference: String, reason: Unreadable },   // a file it needs is blocked, missing or outside
    Cancelled,
    Io(std::io::Error),
    Unsupported(String),    // known, but not readable here: "EPUB isn't available on iPhone and iPad"
    InvalidRequest(String), // the caller's mistake (no such facet, node or entry), never the file's
}
```

Only `NotThisFormat` and `Corrupt` let the front door try the next reading.

**Where** (2026-11-08): `ParseError::corrupt_at(why, Position)` and `error.position()`. A `Position` is a line and column (from 1, as editors show them) or a byte offset; `Position::in_text` finds the line and column of an offset. `-data` gives every parse error one: "line 3, column 14".

**Warnings:** damage that doesn't stop a file opening is a `Warning` on the document (`Document::warnings`): a message, where in the file, and the item it concerns. A bad line of a JSON Lines file is a row whose first cell is `Cell::Error`, with a warning on that row, not a failed file shown as text; only a bad first line means the file isn't JSON Lines.

**Categories:** `Category::Other` is for files no category reads (`open_raw`), so an app filtering on Data never sees them. `Sheets` and `Datasets` are in the enum for the crates to come.

## 4. The API of every category library

**Similar, not identical:** every category has this core shape, the same in each. A category may add API its kind needs (§4.1); format-specific detail still lives in facets, not in per-format API.

```rust
// e.g. libfileparser_3d
pub fn readers() -> &'static [&'static dyn Reader];                       // every format it reads
pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)>;  // which fit, best first
pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document, ParseError>;   // the best that succeeds
pub fn open_with(id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document, ParseError>;
```

Each is one line over `-core`'s routing, which every category shares: `libfileparser_core::{detect, open, open_with}` take the reader list, try readers best first, move on only when a reader says the file isn't its format or is damaged, and turn a reader's panic into `Corrupt`.

and one trait, defined in `-core`, the same for every format of every category:

```rust
pub trait Reader: Sync {
    fn info(&self) -> &ReaderInfo;                                           // id, name, extensions, MIME types, category
    fn detect(&self, header: &[u8], name: &str) -> Detection;                // confidence; for text formats, the language
    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document, ParseError>;
}
```

| Item | Contract |
|---|---|
| `detect` | Reads only the bytes it's given (the header) and the name; should allocate nothing; never panics on any input |
| `open` | Builds only what the primary facet needs and lists the rest; files beyond `src` only through `cx.files`, background work only through `cx.spawn`, progress only through `cx.notify`; checks every claimed size against `cx.limits` before allocating |
| `Document` | Facets, metadata and dependencies, as document-model.md §5 |

An app that knows how to call `libfileparser_images::open` knows how to call `libfileparser_3d::open`. The front door (§8) offers the core shape again, across all categories.

### 4.1 Category additions

A category may add to the core shape what only its kind needs, under the same rules (§5): stable, small, documented, and never required for the basic open-and-show path.

| Category | May add, for example |
|---|---|
| `-3d` | Options for opening a scene (units and up axis to convert to, whether to keep CPU copies after upload), scene-level queries (find a node by path, bounds), texture job control |
| `-docs` | Page render options (scale, region, annotations on or off), text and search by page |
| `-images` | Decode at a reduced size, read a region, the embedded thumbnail and metadata alone |
| `-data` | Column types, sheet selection, row filters pushed down to the reader |
| `-text` | Encoding override, line search |
| `-media` | Seek, decode at a time, stream selection |

Additions are per category, never per format: a USD-only call belongs in a USD facet, or in `usd-lite` itself.

## 5. Keeping the API stable while the inside changes

The API in §4 is the boundary. Behind it, a category may change anything: replace a C library with Rust (ufbx → our own FBX reader), make an eager parser lazy, swap a JSON parser, add a backend (`hayro` beside MuPDF and PDFKit). These rules keep that possible:

1. **No public fields on types that may grow;** accessors, and `#[non_exhaustive]` on public structs and enums, so adding a field, variant or error kind breaks nobody.
2. **Query APIs, not data structures:** facets and sources hand out handles, iterators and ranges, never the `Vec` or `HashMap` a parser happens to use.
3. **No dependency types in public APIs** unless deliberately re-exported (no `serde_json::Value`, no `ufbx` types): swapping a dependency stays internal.
4. **Lazy by contract, even if eager inside:** anything that can be large is read by range at the API, so a parser can become lazy later without callers noticing.
5. **Shared bytes, not borrowed internals:** zero-copy data is handed out as shared handles that keep the mapping alive, never slices tied to a parser's lifetime.
6. **Traits callers only use are sealed;** traits callers implement (`Reader`, `FacetProvider`, the hooks) are the deliberate extension points (§6), small and with default methods so they can grow.
7. **Checked in CI:** every library's public API is snapshotted (`cargo public-api`) and `cargo semver-checks` runs on each change, so any change to a boundary is a visible diff in review, and a breaking one needs a version bump.

✅ Applied (2026-11-07) to every libfileparser crate, the format libraries (`usd-lite`, `gltf-lite`), `scene-data` and `libviewer`: public structs with public fields are `#[non_exhaustive]` and have constructors or `with_*` methods where apps build them (`Band::new`, `Frame3d::shown`, `TextureData::new`, `Spec::new`, `usd_lite::Limits::with_max_elements`); a struct an app fills field by field starts from `Default` (`SceneLimits`). Enums are `#[non_exhaustive]` unless their set is closed by a spec or by a renderer that must handle every case, and those say so in their docs: `Topology`, `TextureFormat`, `Indices`, `AlphaMode`, USD's `Specifier`, `UpAxis` and `Channel`. Id newtypes (`NodeId`, `MeshId`…) stay plain: an id doesn't grow. Snapshots are in `api/` beside each crate's `Cargo.toml`, checked in CI with the semver check (§12).

### What is contract

An app may rely on these; changing one is a breaking change, with a version bump:

| Contract | Examples |
|---|---|
| Every public item in a crate's API snapshot | `Document::facet`, `TreeSource::find`, `SceneDocument::new` |
| Roles, content models, scene item kinds | `Role::Structure`, `Model::Tree`, `SceneItem::Node` |
| Reader ids | `gltf`, `usd`, `fbx`, `obj`, `stl`, `ply`, `blend`, `image`, `svg`, `text`, `zip`, `tar`, `gzip` |
| Facet names a category's design lists for a format | Scene, Stage, Layers, Variants, glTF, JSON chunk, Chunks, Records, Embedded media, Objects, Header, Blocks, Packed files, Package, Dependencies, Materials, Textures, Extensions used, Source, Raw, Image, Text, Files |
| Facet order: the primary first; Source then Raw last | |
| Tree path syntaxes | USD prim and property paths; JSON pointers (glTF); `Objects/<record>/<name>` (FBX); object names and `usemtl <name>` (OBJ); layer identifiers (Layers); `-data`'s trees: `/`-separated labels, `~0` `~1` `~2` for `~` `/` `[`, `[n]` for the n-th sibling of a label (a JSON Pointer for JSON; `/feed/entry[2]/@id` for XML) |
| A table's columns: their number, order and meaning | Materials: name, shading, maps, base colour, metallic, roughness, alpha |
| Value kinds and cell types: a column's or node's `ValueKind`, and which `Cell` variant a reader gives | `-data`: JSON numbers are numbers, strings text, `null` null; a CSV column of numbers is `Number` |

Display text, which may change in any release: format names ("glTF binary (GLB)"), metadata keys and values, tree labels and values, column headings, the text of text cells, warnings and error messages (their positions are contract). An app that needs one of these to be stable asks for it to become contract: a typed accessor, not a string to parse.

### Versions

Every crate is 0.x until its API settles. A breaking change (as `cargo semver-checks` finds it) bumps the crate's minor version (0.1 → 0.2), and every crate that re-exports it. 0.3.0 (2026-11-08): typed cells and columns, node kinds and whole values, positioned errors and warnings, `Category::Other`, limits by kind. Before an item is removed it is `#[deprecated]` for one minor release, pointing at its replacement, so an app updating one step at a time compiles at each step.

## 6. Adding a format, and other extensions

### A new format: one module, one line, one file

Adding, say, 3MF to `-3d`:

1. **`readers/threemf.rs`:** a type implementing `Reader`. It may parse by itself or wrap a format library (§6.2); it builds the scene with the category's shared helpers (scene building, textures, dependencies).
2. **One line** adding it to the category's `readers()` list.
3. **One corpus file** (or more), plus its expected summary (corpus/README.md).

Nothing else changes: the front door, detection, the generic facets, View-as, the conformance suite (§9) and every app pick it up. A format that needs a heavy dependency is behind its own feature.

Each category provides the shared helpers that make step 1 short: `-3d` has scene building, texture jobs, units and axes; `-docs` has page layout and fonts; `-data` has table and tree builders; `-archives` has entry listing. A reader is mostly the format-specific part. The helpers are **public, in the category's `build` module** (one of its §4.1 additions), so a reader written outside the crate is as short as one inside it; it joins the category's readers by passing its own list (`readers()` plus its own) to `-core`'s `open`. The conformance suite walks `readers()` and fails for a reader with no samples, so none passes unchecked.

### Format libraries underneath

When a format is big enough to be useful on its own (USD, glTF, FBX), its parsing lives in a **format library** that knows nothing of libfileparser: its own small API, its own error type, its own `Limits` and resolver trait. The reader adapts it: it connects the library's resolver to `cx.files`, takes its limits from `cx.limits`, and maps its errors to `ParseError`. `usd-lite` is the model. A tool that only wants USD then uses `usd-lite` alone. Small formats (STL, PLY) can be parsed directly in their reader; either way the category's API doesn't change.

Format libraries follow §5's rules too, and keep their own extension points: glTF extensions, USD schemas, PLY properties, FBX custom properties. Data no extension understands is kept, not dropped, so it still shows in the structure facet.

### Other extension points

| Extension | Lets anyone | Example |
|---|---|---|
| **Register a reader** | Add a format to the front door without forking a category | A proprietary format |
| **Replace a reader** by id | Swap a built-in reader | An in-house FBX reader in place of ufbx |
| **Facet providers** | Add a facet to a format without changing its reader | A game engine adds a "Physics" inspector to USD. ✅ Built (2026-11-07): an app puts them on its `ParseContext` (`with_facet_provider`), not on the front door, so they reach a category used alone too; `-core`'s `open`, `open_with` and `open_guarded` add their facets to every document, before Source and Raw; one that fails or panics fails its facet alone |
| **Hooks** | Decide files, limits, threads, progress, cancellation | iOS grants; an engine's job system |

```rust
pub trait FacetProvider: Sync {
    fn formats(&self) -> &[&str];                                    // reader ids: ["usd"]
    fn facets(&self, doc: &Document) -> Vec<FacetInfo>;             // what it would add, cheaply
    fn make(&self, doc: &Document, facet: &FacetInfo, cx: &ParseContext) -> Result<Facet, ParseError>;
}

let lib = libfileparser::Library::new()          // ✅ built (2026-11-07)
    .reader(&MyFormatReader)                     // ahead of plain text, behind the built-ins
    .replace("fbx", &InHouseFbx);
let cx = ParseContext::new().with_facet_provider(PhysicsInspector);
```

`FacetProvider::make` is given the document, so a provider builds on the reader's facets (the Stage, say) instead of reading the file again.

## 7. Embeddable

Every category library, alone or through the front door, can live inside another program without taking it over:

| Property | Means |
|---|---|
| **No global state** | No statics that hold data, no global registries beyond the static reader list; two instances in one process don't interfere |
| **No threads or runtime of its own** | Background work only through `Spawn`; with an inline spawner, everything runs on the caller's thread (WASM, engines that own their threads). No async runtime required |
| **No file system access of its own** | Everything beyond the given bytes through `FileResolver`, so in-memory files, sandboxes and virtual file systems work |
| **Bounded** | `Limits` on every claimed size; `memory()` on every document; caches sized by the caller |
| **Few dependencies, all optional where heavy** | Per-format features; `minimal` builds are pure Rust |
| **Every platform** | Desktop, iOS, Android; `-core`, the pure-Rust categories and `-thumb` also WASM. ✅ Checked 2026-11-08: `-core` (without `mmap` and `scene`), `-data`, `-text`, `-archives`, `-images` and the front door without `3d` build for `wasm32-unknown-unknown`. `-3d` doesn't: ufbx is C |
| **Lean** | `-core`'s features `mmap` (large files mapped) and `scene` (the 3D content model, `scene-data`) are off unless a crate asks: `-3d` and libviewer ask for `scene`, the front door's default asks for `mmap`. A data-only embed has neither |
| **Files that change** | `FileSource::open` reads a file under 16 MiB into memory and maps a larger one. A mapped file truncated by another process faults the process (SIGBUS), which nothing can catch, so a file that may change, a log being written or rotated, is opened with `FileSource::open_live`: read by position (`read_at`, `range`), never mapped, and reading short if it shrinks. Line indexes (`LineText`) and the hex view read such a file a range at a time, never whole; a reader that asks for `bytes()` gets a copy, made once. SQLite, Parquet and HDF5 (`-datasets`) will read the same way |
| **Not only Rust** | `libfileparser-c` exposes the core shape (§4) and the category additions to C, C++, Swift and Kotlin |
| **No panics across the boundary** | A panic in format code fails only what it was doing: opening (`Corrupt`), making a facet (that facet alone), a background job (that job; its bookkeeping still runs) |

## 8. `libfileparser`: every category together

`libfileparser` holds the category libraries together in the shared model, and does nothing else. It has no policy and no defaults of its own: it takes the same `ParseContext` as any category, with `-core`'s defaults (`NoFiles`, `InlineSpawn`, desktop limits) unless the app gives its own. An app that wants one kind of file takes that category alone (`libfileparser-3d`, `libfileparser-text`); it never needs this crate.

```rust
let cx = ParseContext::new().with_files(FolderFiles::new());     // the app's policy, as for any category
let doc = libfileparser::open(&src, &cx)?;                       // the same shape as a category
for facet in doc.facets() { /* role, name, content model */ }    // the reading's facets, then Source, Package, Raw
let stage = doc.facet(1, &cx)?;                                   // made now, on first use
```

✅ Built (2026-11-07), `crates/libfileparser`. It returns `-core`'s `Document`, so an app learns one type; `libfileparser` re-exports `-core`, so it needs one dependency. Beside `readers`, `detect`, `open` and `open_with`: `open_reporting` (the readers tried and why each said no, for an info panel), `open_raw` (the generic facets alone, for a file nothing reads), `open_entry` (a container entry, through detection again) and `reader(id)`. Generic facets are made only when asked for. Plain text detects at extension strength at most and comes last, so a `.gltf` opens as a model with its JSON as Source, not as text. Not yet: other readings' facets (item 3's last part), child documents keeping their parent, and the `minimal`/`full` presets.

What it adds is only what no single category can do:

1. **Detection across categories:** every enabled reader, best first.
2. **Open the best reading;** on `NotThisFormat` or `Corrupt`, the next.
3. **One document from several readings:** the best reading's facets, then the generic ones (Raw always; Source when text; Package when a container the reader didn't cover), then other readings' facets that add something new, made only when asked (document-model.md §4).
4. **Containers:** a Package entry opens through the same detection, as a child document.
5. **Features** choose categories (`images`, `3d`, `docs`, `data`, `sheets`, `datasets`, `text`, `archives`, `media`) and backends (`docs-mupdf`). Presets: `minimal` (pure Rust, permissive, WASM too), `default` (common formats, permissive), `full`.
6. **Nothing runs at startup:** readers are a static table; no category initialises until a matching file opens.

Thread pools, folder grants, platform limits and the like are the app's, through the hooks; Viewer's are in `crates/formats/src/libfileparser.rs`.

## 9. Tests, from outside the boundary

Tests are written against §4's API, never against internals, so they stay the same when the inside changes: a rewrite is proven by the same tests passing.

| What | Tests | How |
|---|---|---|
| **Every reader** | The **conformance suite** (below) | `libfileparser-testkit`, run automatically for each entry in `readers()` against its sample files |
| | Facet tests: each facet of each format against small known files | Per reader |
| **Format libraries** | Spec tests against official samples and reference tools (Khronos glTF samples; OpenUSD test assets and `usdcat --flatten`) | In `tests/`, using only the public API |
| | Fuzzing: bytes in, API calls out; no panic, hang, or allocation past `Limits` | A `cargo-fuzz` target per library |
| **Each library's API** | Public-API snapshot and semver checks | CI (§5) |
| **Everything together** | The corpus: every file, every view it lists | corpus/README.md, opening every facet |
| | Differential tests where two implementations share an API | MuPDF vs PDFKit vs `hayro`; ufbx vs a new FBX reader |
| | Budgets: time to the primary facet, peak memory | Benchmarks per category, in CI |

**`libfileparser-testkit`**, for our readers and anyone writing one:

| Fake | Lets a test |
|---|---|
| `MemFiles` | Supply neighbouring files from memory |
| `DenyFiles` | Refuse every neighbour: the iOS "picked one file" case, on any machine |
| `RecordingFiles` | Assert exactly which neighbouring files a reader asked for |
| `InlineSpawn` | Run background work at once, deterministically |
| `CountingNotify` | Check progress was reported |
| `Limits::tiny()` | Make every limit trip on small inputs |

**Conformance**, every reader. What the suite checks today, and what is still only reviewed by hand, marked:

1. `detect` never panics on any bytes, and recognises its own samples the same way twice. *Not checked yet: that it allocates nothing.*
2. `open` reads other files only through `cx.files`: samples have no path, so a direct file read finds nothing, and every file asked for must be recorded in `Dependencies`. *Not checked yet: that it starts no thread of its own and keeps no global state.*
3. Every facet the document lists can be made, and making it twice gives the same content.
4. With `DenyFiles`, every needed neighbour is reported blocked in `Dependencies`, and the primary facet still opens where the format allows.
5. With `tiny_limits()`, a sample fails with `TooLarge`, never by allocating or hanging; a sample small enough to fit is marked `fits_tiny_limits`, so a reader that never checks limits is caught.
6. After `cancel`, background work stops and nothing more is notified.
7. The same file gives the same document.
8. `memory()` is reported and within budget for its samples.
9. ✅ The document ends with Raw, and has Source when the file is text (unless its primary facet is the text).
10. ✅ Trees: the same children when asked twice, a count that matches them, and every node's `path` found again by `find` (up to 2,000 nodes per tree).
11. ✅ Links: from tree nodes, table rows, scene items and Source lines, every place `Document::links` gives exists (its facet, node, row, entry, scene item or line).

## 10. Who uses what

| App | Takes | Gets |
|---|---|---|
| **Any app that should open and show any file** | `libfileparser` + **libviewer** (architecture.md §2) | Every kind of file read and shown, with its own policy through the hooks |
| Chat app, mail, file manager | `libfileparser` + `-thumb` | What a file is, metadata, a preview image; no GPU, no UI |
| IDE or editor | `libfileparser` with `text`, `data`, `images`, `docs`, `3d`; libviewer's renderers, or its views where the toolkit matches | Every facet: source with its language, structure trees, tables, pages, scenes, in its own widgets or libviewer's |
| Game engine editor | `-3d` + `-images` alone | Scenes, structure (USD stage, glTF nodes) and dependencies as data, with its own files and jobs through the hooks |
| A tool for one format | Its format library alone (`usd-lite`) | That format's own API, no documents or hooks |
| Viewer | All of it, libviewer, and `-remote` for MuPDF on desktop | Everything, with its own policy and shell |
| Non-Rust apps | `-c` | The same, through C |

## 11. What stays out

| Not in libfileparser | Where |
|---|---|
| Drawing documents, and views of them | **libviewer**, the rendering half (architecture.md §2): RendererX for scenes ([3d-renderer.md](3d-renderer.md)), tiled images and pages, and views for a UI toolkit (libgui first) |
| Syntax highlighting, themes | A small crate over `Text` + language, or the app |
| Editing | Edit sessions per format ([editing.md](editing.md)) |
| Process isolation | `-remote`, optional |
| Policy | The app, through the hooks |

## 12. Building it

The corpus passes after every step.

1. ✅ **`-core` and `-testkit`** (2026-11-06): documents with facets, content models with range sources, generic Text/Tree/Hex, `Reader`, hooks, `Limits`, `ParseError`, `Dependencies`, `MemoryUse` (`FacetProvider` was listed here but only written 2026-11-07, §6); the fakes and the conformance suite. Sources made ranged, fallible and shared; safe file resolvers (`NoFiles` default, `FolderFiles`, `AnyFiles`); panics caught in facets and jobs; `Unsupported` and `InvalidRequest` errors (all 2026-11-06). ✅ API snapshots (`cargo public-api`) and `cargo semver-checks` in CI (2026-11-07, §5).
2. 🟡 **`-3d`**, as in [libfileparser-3d.md](libfileparser-3d.md) (moved and on the hooks 2026-11-06; its other facets to come): it uses every hook and has the richest facets. If the design holds there, it holds everywhere.
3. ✅ **The front door** with `-3d`, `-text` (Source facets) and `-archives` (Package facets); Viewer's 3D loading and View-as list switch to it (2026-11-07). `-archives` is Viewer's former `fmt-archive`, moved: limits come from the context, tar entries are views of the archive. Viewer keeps the front door's document with the page, so View as lists its facets (Scene, Stage, Layers…, Source, Raw) and switches between them without reading the file again; `--view-as` takes a facet name too. Until 2026-11-07 Viewer converted a facet to its own content to show it; now libviewer's views read the content models directly ([libviewer.md](libviewer.md)).
4. 🟡 **The other categories,** one at a time: `-images`, `-data` (with §2.1's owner and also-reads field), `-docs`, then `-sheets` and `-datasets`, moving today's `viewer-fmt-*` loaders, each meeting §1. ✅ `-images` (2026-11-07): Viewer's `fmt-image` moved. Opening reads the header only (size, colour type, the image-pixel limit); pixels are a band source: non-interlaced PNGs row by row, other formats decoded whole in their own layout and converted to RGBA a band at a time, SVG rendered when asked. Viewer decodes images up to 16 megapixels whole on its loader thread and streams bigger ones band by band to the GPU from a thread of its own, as before. Not yet: `-3d` decoding its textures through `-images` (§2's "sideways toward leaves"), §4.1's reduced-size and region decoding. ✅ `-data` (2026-11-08): Viewer's `fmt-data` moved and grown to the structured, delimited and config formats of §2.1, with the owner/also-reads rule in `-core` and the front door.
5. **`-thumb`** and a thumbnail CLI, as the first user outside Viewer.
6. **`-remote`** (MuPDF in a helper on desktop), then **`-c`**.

Everything stays in the Viewer workspace until the API settles, then moves to its own repo, as libfilepicker did.
