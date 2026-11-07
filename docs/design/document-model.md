# Document model: files, readings and facets

Status: agreed 2026-11-06; not built yet. Replaces "one file, one content type" (today's `Document { content }`). Every parser library builds on it: [libfileparser.md](libfileparser.md), [libfileparser-3d.md](libfileparser-3d.md).

## 1. The problem

A file is rarely one thing.

| File | What it also is |
|---|---|
| `scene.usda` | USD **text** (a language, worth reading as code), a **stage** of prims with attributes and variants, a **layer stack** of other files, and a **3D scene** |
| `scene.usdz` | A **zip** of layers and textures, a stage, a layer stack, and a 3D scene |
| `model.gltf` | **JSON** text, a glTF **document** (nodes, meshes, accessors), external **buffers and images**, and a 3D scene |
| `model.glb` | Binary **chunks** (JSON, buffer, embedded images), and everything a `.gltf` is |
| `model.fbx` | A tree of FBX **records**, **embedded textures**, and a 3D scene |
| `report.docx` | A **zip** of XML parts and media, a **document**, and text |
| `paper.pdf` | **Pages**, an **outline**, **attachments**, and extractable text |

Today a loader returns one `Content`, and "View as" can only re-open the whole file with a different loader. So a USD file shows as a scene *or* as text, never as its prim hierarchy, and nothing knows that the text, the hierarchy and the scene are the same thing.

## 2. Terms

| Term | Means | Example (`scene.usdz`) |
|---|---|---|
| **File** | Bytes with a name and maybe a location | The `.usdz` |
| **Reader** | Code that understands one format | The USD reader in `-3d`; the zip reader in `-archives` |
| **Reading** | One reader's claim on a file, with its confidence | "USD package, certain"; "zip, certain" |
| **Document** | What one reading produces: a format, **facets**, metadata, dependencies | The USD document |
| **Facet** | One way to look at a document, in one content model, made on demand | Scene, Stage, Layers, Package, Raw |
| **Content model** | The shape of a facet's data, which a renderer or widget knows how to show | Scene, Tree, Table, Text, Container, Hex… |
| **Dependency** | Another file the document uses, what for, where it was found, and whether it could be read | `wood.png` (texture, found at `textures/wood.png`, read), `base.usda` (layer, blocked) |

## 3. Facets

A document has an ordered list of facets. The first is the one shown by default.

### Roles

Every facet has a role, so apps can find "the hierarchy" or "the source" the same way for every format.

| Role | Is | Content model | Examples |
|---|---|---|---|
| **Primary** | What the format is for | Scene, Pages, Raster, Flow, Table, Media… | A USD scene, a PDF's pages, a spreadsheet's sheets |
| **Structure** | How the format organises itself | Tree (usually), Table | USD prims and their attributes; glTF nodes, meshes and accessors; FBX records; a PDF outline; JSON or XML keys |
| **Source** | The text the file is written in | Text, with a language | USDA as USD; `.gltf` as JSON; OBJ and MTL; a GLB's JSON chunk |
| **Package** | The files packed inside | Container | A USDZ's entries; a GLB's chunks and embedded images; a DOCX's parts; a PDF's attachments |
| **Dependencies** | Files outside it that it uses | Table | USD sublayers and references; glTF buffers and images; OBJ's `.mtl` and textures |
| **Inspector** | Format-specific lists worth their own view | Table, Tree | Materials and their maps; textures with sizes; USD variant sets; PDF fonts |
| **Raw** | The bytes | Hex | Every file |

A format offers only the roles it has: an STL has Primary, Source (if ASCII) and Raw; a USDZ has nearly all of them.

### Who provides them

- **The format's reader** provides what only it understands: Primary, Structure, Package and Inspector facets, and the Dependencies list. Only the USD reader knows what a prim is.
- **The front door** adds the generic ones every file can have, so readers don't each re-implement them:
  - **Raw** for every file.
  - **Source** when the file is text: `-text`'s reader, with the language the format's reader names (USDA is "usd"; `.gltf` is "json").
  - **Package** when the file is a known container (zip, tar) and the format's reader didn't provide its own.
- A reader may also provide a Source or Package facet itself when no generic reader could: a GLB's JSON chunk is text, but only the glTF reader can find it.
- **Anyone else** can add facets to a format through a facet provider, without changing its reader: a game engine adds a "Physics" inspector to USD (libfileparser.md §6).

### Linked facets

Facets of one document describe the same thing, so they can point into each other: a prim in the Stage tree knows its line in the USDA source and its node in the Scene; a mesh in the Scene knows its glTF accessor. Links are optional, cheap (indices, not copies) and let an app select in one view and highlight in another. They are added per format when useful, not required.

## 4. Readings and facets together

Two different questions:

- **Which readers claim this file?** (readings) A `.docx` is claimed by the DOCX reader and the zip reader; a `.json` that happens to be a glTF by the glTF reader and the JSON reader.
- **What can this document show?** (facets) The DOCX reading has Document, Structure, Package and Raw facets.

The front door merges them into **one list of views** for the app, without repeats:

```
scene.usda
  Scene        primary      (USD reading)
  Stage        structure    (USD reading)
  Layers       dependencies (USD reading)
  Variants     inspector    (USD reading)
  Source: USD  source       (front door, text, language from the USD reader)
  Raw          raw          (front door)
```

A second reading only adds what the first doesn't cover: for a `.docx`, the zip reading's listing *is* the DOCX document's Package facet, so it appears once. A reading whose reader fails (a `.json` that isn't really glTF) contributes nothing, and the next one becomes primary.

## 5. Laziness

Opening must stay cheap however many facets a file has.

1. **Detect** reads only the first bytes and the name, for every reader: no parsing yet.
2. **Open** runs only the best reading, and only as far as its primary facet needs; it lists the other facets as descriptors (role, name, content model) without making them.
3. **A facet is made the first time it is asked for,** and kept while the document is open (within the caller's memory budget; a facet that can be remade cheaply may be dropped).
4. **Other readings run only when one of their facets is asked for.**
5. **Inside a facet, big content is still read by range:** pages, rows, lines, tree children, tiles, frames (libfileparser.md §3).

So opening a 2 GB USDZ builds the scene and lists five other facets; the Stage tree is built only if someone opens it, and the Package listing reads only the zip's central directory.

```rust
pub struct Document {
    pub format: FormatInfo,                 // name, MIME, category
    pub facets: Vec<FacetInfo>,             // role, name, content model, made or not
    pub metadata: Vec<(String, String)>,
    pub dependencies: Dependencies,         // every referenced file: read, blocked or missing
}

impl Document {
    pub fn facet(&self, index: usize, cx: &ParseContext) -> Result<Facet, ParseError>;  // made on first call
    pub fn memory(&self) -> MemoryUse;
}

pub struct Facet { pub info: FacetInfo, pub content: Content }
pub enum Role { Primary, Structure, Source, Package, Dependencies, Inspector, Raw }
```

`Dependencies` replaces today's `Unreadables`: it lists **every** referenced file with its status, not only the failures, so a Dependencies facet and the "Open Folder…" banner come from the same list.

## 6. Nesting

A Package facet's entries are files. Opening one goes back through the front door like any file, as a child of this document:

```
scene.usdz → Package → textures/wood.png → Raster
                     → layers/set.usdc  → Scene, Stage, …
```

A dependency (a USD reference to `../props/chair.usda`) is not opened as a separate document unless the user asks: it is part of this document's scene, read through the `files` hook.

## 7. What the app does with it

Viewer (and any app) shows the primary facet, lists the others in "View as", and keeps every facet available. For Viewer:

- The toolbar's view menu lists facets grouped by role, in the order above.
- A facet whose content model the app can't show yet (a Tree of USD prims before the tree widget handles it) is still listed, and falls back to a generic view.
- Linked facets let a later inspector select a prim in the Stage and highlight it in the Scene.

The model doesn't decide layout: an app can show Scene and Stage side by side, or only the Scene.

## 8. Rules

1. Every file has at least a Raw facet: nothing is ever "unsupported" (PLAN.md §0).
2. A reader never re-implements a generic facet the front door provides (Raw, plain Source, a zip listing).
3. Facet descriptors are known after open; facet content is made on demand.
4. Facets of one document share parsed data (Arc), never copy it.
5. Roles are fixed (§3); facet names are the format's own words ("Stage", "Layers", "Accessors").
6. A facet that fails to build fails alone: the document and its other facets stay usable.
