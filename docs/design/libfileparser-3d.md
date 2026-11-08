# libfileparser-3d: any 3D file, every facet of it

Status: design agreed 2026-11-06. Built (2026-11-06): `crates/libfileparser-3d` on `-core`, every reader with its Scene (or Preview) facet, files, threads and progress through the hooks, every reader passing conformance on committed samples (the suite walks `readers()`, so a reader without samples fails); one module per reader in `readers/`, the shared helpers public in `build` (§4); Viewer opens 3D through it (§9 step 2). Dependencies and Textures facets for every reader (§9 step 3, 2026-11-06). USD's Stage, Layers and Variants, and Materials for every reader (§9 step 4, 2026-11-07). glTF's tree, GLB's JSON chunk and Chunks, and the Extensions used (§9 step 5, 2026-11-07). FBX's Records and Embedded media, OBJ's Objects, PLY's Header, and `.blend`'s Blocks and Packed files (§9 step 6, 2026-11-07). Links between facets for every reader (§9 step 7, 2026-11-07); glTF byte ranges are still to do. Built on [document-model.md](document-model.md), and applies [libfileparser.md](libfileparser.md): the four requirements of its §1, its flat API (§4) and its rules (§5–7). This document holds only what is particular to 3D.

**Goal:** read any 3D file into everything it is: a scene to draw, its own structure (USD prims, glTF nodes, FBX records), the text it's written in, the files packed inside it, the files it depends on, and its materials and textures, so an engine editor, an IDE or a viewer can each take what it needs.

## 1. What it produces, per format

✅ today (as one content) · 🟡 partly · ⬜ to build. Facets in each row are in the order shown.

| Format | Primary | Structure | Source | Package | Dependencies | Inspector |
|---|---|---|---|---|---|---|
| **USDA** | Scene ✅ | **Stage**: prims, types, attributes, metadata ✅ · **Layers**: the layer stack and composition arcs ✅ | USD text, language `usd` (front door) | — | Sublayers, references, payloads (as layers), textures ✅ | **Variants** (sets and selections) ✅ · Materials ✅ · Textures ✅ |
| **USDC** | Scene ✅ | Stage ✅ · Layers ✅ · **Crate**: tokens, paths, specs, sections ⬜ | — | — | As USDA ✅ | As USDA |
| **USDZ** | Scene ✅ | Stage ✅ · Layers ✅ | — | **Package**: layers and textures inside (front door's zip listing) | Outside references, if any ✅ | As USDA |
| **glTF** (`.gltf`) | Scene ✅ | **glTF**: scenes, nodes, meshes, primitives, accessors, materials, animations ✅ | JSON, language `json` (front door) | — | Buffers, images ✅ | Textures ✅ · Materials ✅ · Extensions used ✅ |
| **GLB / VRM** | Scene ✅ | glTF ✅ · VRM: humanoid, expressions ⬜ | **JSON chunk** (the reader provides it: no generic reader can find it) ✅ | **Chunks**: JSON, BIN, embedded images ✅ | External buffers or images, if any ✅ | As glTF |
| **FBX** (binary) | Scene ✅ | **Records**: the FBX node tree, properties ✅ | — | **Embedded media** (textures in the file) ✅ | Textures ✅ | Textures ✅ · Materials ✅ · Takes ⬜ |
| **FBX** (ASCII) | Scene ✅ | Records ✅ | FBX text, language `fbx` (front door) | — | Textures ✅ | As binary |
| **OBJ** | Scene ✅ | **Objects**: objects, groups, materials used ✅ | OBJ text, language `obj` (front door) | — | `.mtl` files, textures ✅ | Textures ✅ · Materials ✅ |
| **STL** | Scene ✅ | — | ASCII STL as text (front door) | — | — | Solids, facet count ⬜ |
| **PLY** | Scene ✅ | **Header**: elements and properties ✅ | ASCII PLY as text (front door) | — | — | Splat attributes, if any ⬜ |
| **`.blend`** | Scene ✅ (meshes, objects, viewport colours, as saved: modifiers not applied), or the Preview image when there's no mesh | **Blocks**: datablocks by type ✅ | — | **Packed files** ✅ | Linked libraries, external images ⬜ | Preview ✅ |
| Later: **STEP/IGES**, **3MF**, **LAS/LAZ**, **Gaussian splats**, **DAE** | Scene | Their own trees | STEP and DAE are text | 3MF is a zip | — | — |

Every file also gets **Raw** (hex) from the front door. A **MTL** opened on its own is a Source (text, language `mtl`) with a Materials inspector.

## 2. The facets' content models

| Facet | Content model | Notes |
|---|---|---|
| Scene | `Scene` (`scene-data`) | Geometry first, textures arrive later (§5) |
| Stage, glTF, Records, Objects, Header, Blocks, Crate | `Tree` | Nodes carry a label, a type, a value or summary, and children made on demand. Large trees (an FBX with 100,000 records) list children lazily |
| Records (FBX) | `Tree` | ✅ Each record by name with its first properties (`1000, "Geometry::Tri", "Mesh"`; an array as `f64[9]`, marked when compressed; raw data by size). Binary: read in place, a node's id is its record's offset and children are found when asked for, so nothing is copied. ASCII: parsed the first time it's opened, within the items and depth limits |
| Objects (OBJ) | `Tree` | ✅ The `.mtl` files it names, then each object with its groups and the materials they use, with face counts and the line each starts on. Faces before any `o` are an unnamed object |
| Header (PLY) | `Tree` | ✅ The format, comments, then each element with its count and its properties' types (`list uchar int`) |
| Blocks (`.blend`) | `Tree` | ✅ Datablocks by kind (Object, Mesh, Material, Light…) with their names, then the other blocks by code and struct with counts and sizes. Named through the file's own SDNA, so no Blender version is assumed; a file without one is listed by code |
| glTF | `Tree` | ✅ The file's JSON, keys in the file's order: an object by its name or its first keys, an array by its length, a long string (a `data:` URI) cut short. An index is shown with the name of what it points to (`mesh = 0 · Tyre`, a node's `children`, a primitive's attributes). Parsed again the first time it is opened |
| Stage (USD) | `Tree` | ✅ The composed stage: its metadata, then each prim with its type (`class` and `over` marked), its metadata, its properties (a value, `point3f[8]` for an array, a connection's source, a relationship's targets) and its children, read a level at a time. Composed again the first time it is opened, rather than kept beside the scene for a facet that may never be |
| Layers | `Tree` | ✅ The root layer, and under each layer the sublayers, references and payloads it authors, each with its status (read, missing, not allowed) and the prims it comes into; under each, the arcs of the layer it read. A layer reached twice is expanded the first time. Only when the stage has arcs to other layers |
| Variants | `Tree` | ✅ By prim: each variant set with its selection, then its variants, the selected one marked. Only when the stage has variant sets |
| Materials | `Table` | ✅ One row per material the file has: name, shading model (marked when inferred), each map with its texture's name as the Textures facet lists it, base colour, metallic, roughness, alpha mode. Not for files without materials of their own (STL, PLY) |
| Textures | `Table` | ✅ One row per texture: name, where it came from (file, embedded, packed), size as decoded, colour space, status (decoding, decoded, couldn't be decoded, missing, not allowed). Live: asked again, it shows the textures as they are now. Only when the scene has textures or names some |
| Dependencies | `Table` | ✅ Every referenced file: what it's used as (texture, buffer, layer, material library), where it was found when that differs, status. Made by `-core` from the document's `Dependencies` (`Document::with_dependencies_facet`), so every category has the same one. Only when the file names other files |
| Package, Chunks, Embedded media, Packed files | `Container` | Entries open as files through the front door. ✅ GLB's Chunks: `JSON.json`, `BIN.bin` (and any other chunk), then each image stored in the BIN chunk as `images/<name>.<ext>`; each opens as a view of the GLB's bytes (`FileSource::part`), never a copy. ✅ FBX's Embedded media: each `Video` with `Content`, as `media/<file name>`. ✅ `.blend`'s Packed files: each `PackedFile`'s bytes as `packed/<file name>`, named from the path recorded beside it |
| JSON chunk | `Text` (language `json`) | ✅ GLB only (a `.gltf` is JSON already, the front door's Source). Exporters write it on one line, so a long one is laid out an object per line |
| Extensions used | `Table` | ✅ glTF: each extension the file uses, whether it requires it, and whether it's read here (read, not read, not supported). Only when it uses any |

## 3. Links between facets

Each reader records cheap cross-references (indices, not copies) where they exist, so an app can select in one facet and highlight in another:

| From | To |
|---|---|
| Stage prim (or property) | ✅ Its Scene nodes and meshes, and back; the line its spec starts on in the USDA text when the root layer defines it (and a line gives the prim that starts nearest above it); the layer that defines it, in Layers (a referenced prim's is the asset's layer) |
| glTF node / primitive / material | ✅ Scene node (a node's second and later primitives are child nodes), mesh, material, and back. ⬜ Its byte range in the JSON |
| FBX record | ✅ A `Model` record and its Scene nodes, a `Geometry` its meshes, a `Material` its material, and back; binary and ASCII alike |
| OBJ object | ✅ Its Scene nodes and the line it starts on (groups and `mtllib` too; a line gives the one starting nearest above it); `usemtl` and its Scene material |
| Material row | ✅ Scene material and its texture rows, and back; a scene node gives its material's row |
| Texture row | ✅ Scene texture, and the material rows that use it |

How (`-core`, document-model.md "Linked facets"): `Document::links(place)`. `SceneDocument` links Materials, Textures and the Scene for every reader; a reader that passes `SceneOrigins` (each scene item by the path of what describes it in its structure facet: `/World/Car`, `/nodes/3`) to `SceneDocument::origins` gets its structure facet ↔ Scene links too; anything else a format links (USD's lines and layers) is its own `Links`. usd-lite records each text prim's line (`Layer::lines`) and where each composed prim is defined (`Stage::sources`); gltf-lite reports which node, primitive and material each scene item came from. The Stage is composed again the first time its facet or a link asks, then shared by both.

FBX and OBJ are matched by name, as ufbx keeps an FBX object's id only when it retains the whole parsed file: a record under `Objects` has the path `Objects/<record>/<object name>` (`Objects/Model/Tri`), an OBJ object its name, and a name met again gets `#1`, `#2`… in file order. ufbx's nodes, meshes and materials are keyed the same way, in file order, so the n-th `Tri` is the n-th record of that name.

## 4. Inside the crate, and adding a format

Its public API is the core shape every category has (libfileparser.md §4): `readers()`, `detect`, `open`, `open_with`, plus 3D's additions (§4.1 there): scene options (units, up axis, CPU copies), scene queries, texture job control. Anything specific to one format is reached through its facets.

```
libfileparser-3d
  readers/   gltf, fbx (+ obj, both via ufbx), stl, ply, usd, blend   ✅ one module each: its Reader and its parsing;
             usd/facets: its Stage, Layers and Variants; gltf/facets: glTF, JSON chunk, Chunks, Extensions used;
             fbx/records: Records, Embedded media; fbx/objects: OBJ's Objects; blend/blocks: SDNA, Blocks, Packed files
  build      ✅ public: new_scene (a scene within the limits), SceneDocument (the document every scene becomes),
             SceneOrigins (scene items by their path in a structure facet), scene_error, detect_by,
             decode_textures, find_texture, ImageJob/ImageSource
  textures   ✅ texture jobs: find (files hook), decode, reduce, mips; via spawn and notify (later through -images)
```

Builders every category shares are in `-core`: `OwnedTree` (with paths, `add_at`), `OwnedTable`, `LineText`, `FileParts` (files inside a file, as views of it: GLB chunks, a USDZ's files, FBX embedded media, `.blend` packed files all use it), and `Document::with_file_facets` (Source and Raw).

`SceneDocument::new(reader, src, format, scene)` makes the document every 3D reader returns: the Scene, the format's own facets (`SceneDocument::facets`, right after it), Dependencies, Materials and Textures, then the file's Source (when it is text, in the language the reader's `detect` names) and Raw, last. It also links Materials, Textures and the Scene, and, given `SceneOrigins` (`SceneDocument::origins`), the format's structure facet and the Scene. `build` is public, so a reader outside the crate uses the same pieces (`tests/extend.rs` adds an OFF reader that way, with a structure facet linked to its scene, and passes conformance).

**Adding a 3D format** (3MF, say):

1. **`readers/threemf.rs`** (or `readers/threemf/` with a `facets.rs`), a type implementing `Reader`:
   - `detect`: `build::detect_by(&INFO, magic, name)`, plus `.with_language(..)` when the format is written as text.
   - `open`: parse (directly, or through a format library, §4 table below); fill a scene from `build::new_scene(cx)` (it keeps to the limits); other files only through `Dependencies::resolve` / `resolve_any`, textures through `build::find_texture` and `build::decode_textures`.
   - Its own structure as a facet: an `OwnedTree` whose nodes have paths (`add_at`) or a `TreeSource` with `find`/`path`; files inside it as `FileParts`.
   - Each scene item it adds keyed by the path of what describes it (`SceneOrigins::node`, `mesh`, `material`).
   - Return `SceneDocument::new(self, src, name, scene)` with `.facets(..)`, `.origins(origins, "<facet name>")`, `.textures(..)`, `.dependencies(deps)`, `.warnings(..)`, `.build()`.
2. **One line** in `readers()` in `lib.rs`.
3. **A sample:** `corpus/make.py` writes it, `corpus/manifest.toml` lists it, `UPDATE_CORPUS=1` writes its summary, and `tests/conformance.rs` lists it for the reader's id. The suite fails for a reader without samples, and checks the rest: hooks only, limits, cancellation, Raw last and Source for text, trees that answer the same twice and find their own paths, and links that all lead somewhere (libfileparser.md §9).

Nothing else changes: the front door, Viewer's View as, and every app's facet list pick it up. Outside the crate the same reader joins `-3d` by a list of its own (`readers().to_vec()` plus it, to `-core`'s `open`) or the front door by `libfileparser::Library::new().reader(&ThreeMfReader)`; `Library::replace("fbx", ..)` swaps a built-in one. An app adds a facet to an existing format without a reader at all: a `FacetProvider` on its `ParseContext` (`tests/extend.rs` adds a USD one).

Under the readers, format libraries that know nothing of libfileparser and stay usable on their own (libfileparser.md §6). Each keeps its own extension points (glTF extensions, USD schemas, PLY properties, FBX custom properties) and keeps data no extension understands.

| Library | Role | Adapter in `-3d` |
|---|---|---|
| `usd-lite` (ours) | USD layers (text, crate, USDZ), composition into a stage | `readers/usd`: its `Resolver` is backed by `cx.files`; its stage becomes the Scene and the Stage facet, and the arcs and variant sets composition met (`Stage::arcs`, `Stage::variant_sets`) the Layers and Variants facets |
| `gltf-lite` (ours) | glTF/GLB reading, zero-copy from the mapped file | `readers/gltf`; its JSON tree becomes the glTF facet. Was `rendererx-gltf`; renamed 2026-11-06 for what it is, a glTF reader, apart from the renderer |
| `ufbx` (C, vetted) | FBX and OBJ | `readers/fbx`: file-open, allocator and thread hooks routed to `cx` (architecture.md §5) |
| `scene-data` (was `rendererx-scene`) | The scene data model; no GPU | Shared with RendererX, so a scene goes to the renderer without conversion |

## 5. How the hooks are used

| Hook | In `-3d` |
|---|---|
| `files` | Every file beyond the source: glTF buffers and images, OBJ's `.mtl`, USD sublayers, references and payloads, FBX and `.blend` external textures. Each result goes into Dependencies as read, blocked or missing. Nothing touches `std::fs` |
| `limits` | Vertex and index counts, accessor sizes, texture pixels, composition depth, number of layers, nesting of FBX records, total scene memory. Checked before allocating |
| `spawn` | Texture decoding (one job per texture), and building large Structure trees on demand |
| `notify` | Each texture arriving in the scene; a lazily built facet becoming ready |
| `cancel` | Stops texture jobs and facet builds for a closed document |

Order on open: parse and compose → build geometry into the Scene → return the document (geometry on screen) → textures arrive in the background → other facets are built only when asked.

## 6. Units, axes and scale

Every reader converts to one convention so engines and renderers don't each handle it: metres, Y up, right-handed. The original units, up axis and scale are kept as metadata and in the Scene, so an app can show them or undo the conversion (USD `metersPerUnit` and `upAxis`, FBX axis system, STL and PLY as Z up).

## 7. Limits and hostile files

- Claimed counts (vertices, indices, accessors, records, triangles in an STL header) are checked against the bytes actually present and against `Limits`.
- Composition is bounded: layer count, reference depth, cycles detected.
- Textures obey the image limits of `-images`, then are reduced to the texture cap (2048 px today).
- A failing facet fails alone (document-model.md §8): a broken Variants facet doesn't stop the Scene.
- The corpus holds hostile 3D files for each reader (truncated GLB, out-of-range OBJ indices, an STL claiming a billion triangles today; more per reader as they move).

## 8. Moving today's code

| Today | Becomes |
|---|---|
| ✅ `viewer-fmt-3d` (`lib.rs`, `stl.rs`, `ply.rs`, `usd.rs`, `ufbx_scene.rs`, `blend.rs`) | `libfileparser-3d/readers/*` |
| `viewer-fmt-3d/textures.rs` | `libfileparser-3d/textures`, decoding through `-images` |
| `Unreadables` | `Dependencies` (every referenced file, not only failures) |
| The global `WAKE`, `std::thread::spawn` in texture decoding | `cx.notify`, `cx.spawn` |
| ✅ `rendererx-gltf`, `rendererx-scene` | Renamed `gltf-lite` and `scene-data` (2026-11-06): format and data libraries, apart from the renderer. `gltf-lite` is adapted by `readers/gltf` |
| `usd-lite` | Unchanged as a library; `readers/usd` adds the facets |

## 9. Order of work

The corpus passes after every step; each step adds corpus files for what it adds.

1. **Separate and audit:** 🟡 `gltf-lite` and `scene-data` renamed apart from RendererX (2026-11-06); still to do: `usd-lite`, the glTF reader and the ufbx binding checked against libfileparser.md §5 and given spec tests, a fuzz target and a public-API snapshot. STL and PLY stay small parsers inside their readers.
2. ✅ **Move as is:** the readers into `libfileparser-3d` on `-core`, each producing a document whose only facet is the Scene. Hooks replace `std::fs`, threads and `WAKE`. Viewer opens 3D through it. Then (2026-11-06) one module per reader, the helpers public in `build`, conformance over every entry of `readers()`, and FBX (ASCII) and `.blend` samples in the corpus.
3. ✅ **Dependencies** for every reader, and the Dependencies and Textures facets (2026-11-06). Viewer shows them, and every other facet here, in its View as list through the front door (libfileparser.md §12 step 3, 2026-11-07).
4. ✅ **USD:** Stage, Layers and Variants facets; the Materials facet for every reader (2026-11-07).
5. ✅ **glTF/GLB:** the glTF tree, GLB chunks and JSON chunk, and the Extensions used (2026-11-07). Still to do: VRM's humanoid and expressions.
6. ✅ **FBX, OBJ, PLY, `.blend`:** Records, Objects, Header, Blocks, embedded and packed files (2026-11-07), with binary FBX and a `.blend` with an SDNA and a packed image in the corpus. Still to do: FBX takes, STL solids, PLY splat attributes, `.blend` linked libraries and external images as dependencies, and compressed `.blend` files.
7. ✅ **Links between facets** (§3): the link API in `-core`, USD (prim ↔ Scene, source line, layer), glTF (node, primitive, material ↔ Scene), FBX records and OBJ objects ↔ Scene (and OBJ's lines), Materials and Textures for every reader (2026-11-07). Still to do: glTF byte ranges in the JSON.
8. ✅ **What real files needed** (2026-11-08), found by `corpus/fetch-real-3d.sh`: USD's implicit shapes, curves and points; OBJ and FBX lines, points, NURBS curves and surfaces; OFF; Draco and meshopt in glTF; glTF 1.0, text and binary; `.blend` scenes from 2.4 to 5.x; a real file with nothing to draw opens on its structure. Of 1,996 genuine files, 1,983 open as their format. Still to do: KTX2/Basis textures (no mature Rust reader), Blender modifiers and textures, cubic curves evaluated rather than drawn through their control points.
9. **New formats** in their own readers: 3MF, STEP, LAS, splats.
