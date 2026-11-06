# libfileparser-3d: any 3D file, every facet of it

Status: design agreed 2026-11-06; today's code is `viewer-fmt-3d`, `rendererx-gltf` and `usd-lite`, not yet in this shape. Built on [document-model.md](document-model.md), and applies [libfileparser.md](libfileparser.md): the four requirements of its §1, its flat API (§4) and its rules (§5–7). This document holds only what is particular to 3D.

**Goal:** read any 3D file into everything it is: a scene to draw, its own structure (USD prims, glTF nodes, FBX records), the text it's written in, the files packed inside it, the files it depends on, and its materials and textures, so an engine editor, an IDE or a viewer can each take what it needs.

## 1. What it produces, per format

✅ today (as one content) · 🟡 partly · ⬜ to build. Facets in each row are in the order shown.

| Format | Primary | Structure | Source | Package | Dependencies | Inspector |
|---|---|---|---|---|---|---|
| **USDA** | Scene ✅ | **Stage**: prims, types, attributes, metadata ⬜ · **Layers**: the layer stack and composition arcs ⬜ | USD text, language `usd` (front door) | — | Sublayers, references, payloads, textures 🟡 | **Variants** (sets and selections) ⬜ · Materials ⬜ · Textures ⬜ |
| **USDC** | Scene ✅ | Stage ⬜ · Layers ⬜ · **Crate**: tokens, paths, specs, sections ⬜ | — | — | As USDA 🟡 | As USDA ⬜ |
| **USDZ** | Scene ✅ | Stage ⬜ · Layers ⬜ | — | **Package**: layers and textures inside (front door's zip listing) | Outside references, if any 🟡 | As USDA ⬜ |
| **glTF** (`.gltf`) | Scene ✅ | **glTF**: scenes, nodes, meshes, primitives, accessors, materials, animations ⬜ | JSON, language `json` (front door) | — | Buffers, images 🟡 | Materials · Textures · Extensions used ⬜ |
| **GLB / VRM** | Scene ✅ | glTF ⬜ · VRM: humanoid, expressions ⬜ | **JSON chunk** (the reader provides it: no generic reader can find it) ⬜ | **Chunks**: JSON, BIN, embedded images ⬜ | External buffers or images, if any 🟡 | As glTF ⬜ |
| **FBX** (binary) | Scene ✅ | **Records**: the FBX node tree, properties ⬜ | — | **Embedded media** (textures in the file) ⬜ | Textures 🟡 | Materials · Textures · Takes ⬜ |
| **FBX** (ASCII) | Scene ✅ | Records ⬜ | FBX text, language `fbx` (front door) | — | Textures 🟡 | As binary ⬜ |
| **OBJ** | Scene ✅ | **Objects**: objects, groups, materials used ⬜ | OBJ text, language `obj` (front door) | — | `.mtl` files, textures ✅ | Materials ⬜ |
| **STL** | Scene ✅ | — | ASCII STL as text (front door) | — | — | Solids, facet count ⬜ |
| **PLY** | Scene ✅ | **Header**: elements and properties ⬜ | ASCII PLY as text (front door) | — | — | Splat attributes, if any ⬜ |
| **`.blend`** | Preview image ✅ (later Scene) | **Blocks**: datablocks by type ⬜ | — | **Packed files** ⬜ | Linked libraries, external images ⬜ | — |
| Later: **STEP/IGES**, **3MF**, **LAS/LAZ**, **Gaussian splats**, **DAE** | Scene | Their own trees | STEP and DAE are text | 3MF is a zip | — | — |

Every file also gets **Raw** (hex) from the front door. A **MTL** opened on its own is a Source (text, language `mtl`) with a Materials inspector.

## 2. The facets' content models

| Facet | Content model | Notes |
|---|---|---|
| Scene | `Scene` (`rendererx-scene` data) | Geometry first, textures arrive later (§5) |
| Stage, glTF, Records, Objects, Header, Blocks, Crate | `Tree` | Nodes carry a label, a type, a value or summary, and children made on demand. Large trees (an FBX with 100,000 records) list children lazily |
| Layers | `Tree` | Layer → its sublayers and arcs, each with its file and status |
| Variants | `Tree` | Variant set → variants, with the current selection marked |
| Materials | `Table` | One row per material: name, shading model, each map with its texture, the main factors |
| Textures | `Table` | One row per texture: name, file or embedded, size, colour space, status (decoded, pending, blocked, missing) |
| Dependencies | `Table` | Every referenced file: what refers to it, resolved path, status |
| Package, Chunks, Embedded media, Packed files | `Container` | Entries open as files through the front door |
| JSON chunk | `Text` (language `json`) | |

## 3. Links between facets

Each reader records cheap cross-references (indices, not copies) where they exist, so an app can select in one facet and highlight in another:

| From | To |
|---|---|
| Stage prim | Its Scene node; its line in the USDA source; the layer that defines it |
| glTF node / primitive | Scene node and mesh; its byte range in the JSON |
| FBX record | Scene node, for models and geometry |
| OBJ object | Scene node; its first line in the source |
| Material row | Scene material; its texture rows |

Links are optional per format and come after the facets themselves.

## 4. Inside the crate, and adding a format

Its public API is the core shape every category has (libfileparser.md §4): `readers()`, `detect`, `open`, `open_with`, plus 3D's additions (§4.1 there): scene options (units, up axis, CPU copies), scene queries, texture job control. Anything specific to one format is reached through its facets.

```
libfileparser-3d
  readers/   gltf, fbx (+ obj via ufbx), stl, ply, usd, blend    one Reader each
  scene/     building rendererx-scene data: meshes, materials, nodes, bounds, up-axis and units
  textures/  texture jobs: find (files hook), decode (-images), reduce, mips; via spawn and notify
  facets/    tree, table and container builders shared by the readers
  deps/      the Dependencies list: every referenced file and its status
```

`scene/`, `textures/`, `facets/` and `deps/` are the shared helpers that keep a new reader short (libfileparser.md §6): adding 3MF is a `readers/threemf.rs` that unzips its model XML and calls `scene/` to build meshes and materials, one line in `readers()`, and a corpus file. Textures, dependencies, units, the Dependencies and Textures facets, and the conformance suite come with it.

Under the readers, format libraries that know nothing of libfileparser and stay usable on their own (libfileparser.md §6). Each keeps its own extension points (glTF extensions, USD schemas, PLY properties, FBX custom properties) and keeps data no extension understands.

| Library | Role | Adapter in `-3d` |
|---|---|---|
| `usd-lite` (ours) | USD layers (text, crate, USDZ), composition into a stage | `readers/usd`: its `Resolver` is backed by `cx.files`; its stage becomes the Stage, Layers and Variants facets and the Scene |
| `rendererx-gltf` (ours) | glTF/GLB reading, zero-copy from the mapped file | `readers/gltf`; its JSON tree becomes the glTF facet. Renamed to describe what it is (a glTF reader), since it isn't part of the renderer |
| `ufbx` (C, vetted) | FBX and OBJ | `readers/fbx`: file-open, allocator and thread hooks routed to `cx` (architecture.md §5) |
| `rendererx-scene` | The scene data model; no GPU | Shared with RendererX, so a scene goes to the renderer without conversion |

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
| `viewer-fmt-3d` (`lib.rs`, `stl.rs`, `ply.rs`, `usd.rs`, `ufbx_scene.rs`, `blend.rs`) | `libfileparser-3d/readers/*` |
| `viewer-fmt-3d/textures.rs` | `libfileparser-3d/textures`, decoding through `-images` |
| `Unreadables` | `Dependencies` (every referenced file, not only failures) |
| The global `WAKE`, `std::thread::spawn` in texture decoding | `cx.notify`, `cx.spawn` |
| `rendererx-gltf` | Kept as the glTF reader library (renamed later); adapted by `readers/gltf` |
| `usd-lite` | Unchanged as a library; `readers/usd` adds the facets |

## 9. Order of work

The corpus passes after every step; each step adds corpus files for what it adds.

1. **Separate and audit:** `rendererx-gltf` renamed as the glTF reader it is, apart from RendererX; `usd-lite`, the glTF reader and the ufbx binding checked against libfileparser.md §5 and given spec tests, a fuzz target and a public-API snapshot. STL and PLY stay small parsers inside their readers.
2. **Move as is:** the readers into `libfileparser-3d` on `-core`, each producing a document whose only facet is the Scene. Hooks replace `std::fs`, threads and `WAKE`. Viewer opens 3D through it.
3. **Dependencies** for every reader, and the Dependencies and Textures facets.
4. **USD:** Stage, Layers and Variants facets; the Materials facet.
5. **glTF/GLB:** the glTF tree, GLB chunks and JSON chunk.
6. **FBX, OBJ, PLY, `.blend`:** Records, Objects, Header, Blocks, embedded and packed files.
7. **Links between facets** (§3), starting with USD prim ↔ Scene node ↔ source line.
8. **New formats** in their own readers: 3MF, STEP, LAS, splats.
