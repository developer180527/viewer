# Next steps: decisions and order

Written 2026-11-02. PLAN.md §11 has the roadmap's status; this note holds the reasoning behind its next steps.

**Priority: open every kind of file properly first.** Editing and further GUI work come after. The editing design waits in [editing.md](design/editing.md).

## 1. Every view of a file stays available

A file can be several things at once, and **View as** offers all of them. A format-specific view never takes the others away. With libfileparser this becomes the **facet** model (docs/design/document-model.md): a document lists its own facets (a DOCX's document, structure and parts), the front door adds raw and source facets, and other readings (zip) add only what's new. Until then, it works through loaders as below.

A `.docx` has four views:

| View | Shows | Loader |
|---|---|---|
| **Document** (default) | The formatted document | The new document loader |
| **Zip** | The files inside, which open in place | `zip` |
| **Text** | The bytes as text | `text` |
| **Hex** | The raw bytes | `hex` |

The same goes for every zip-based format: XLSX, PPTX, ODT, ODS, ODP, EPUB, CBZ, and also JAR and APK. Other containers follow the same rule, for example a CBR also offers its archive view once RAR listing exists.

**What this requires:**
- The format's loader must rank above `zip`, which claims these files by their `PK` signature at `Confidence::MAGIC`. Otherwise the zip list stays the default.
- The format's loader must fall through to `zip` when the file isn't really that format, or is damaged. `Registry::open` already moves to the next candidate on failure.
- Each new format gets a test that lists all four views and opens each one.

`ViewerApp`'s View as list already builds on `Registry::candidates` and always adds Text, and Hex accepts every file, so little new UI should be needed. Confirm this with the first format, XLSX.

## 2. Menu bar

One command list drives every platform, so shortcuts, enabled states and handlers can't drift apart:

```rust
Command { id: Open, title: "Open…", shortcut: Cmd+O, enabled: bool }
```

Menus: File, Edit, View, Window, Help.

| Platform | Where commands appear | How |
|---|---|---|
| **macOS** | The real system menu bar at the top of the screen, with the app menu (About, Hide, Quit) | `NSMenu` through `muda` |
| **Windows** | A native menu bar in the window | `muda` |
| **Linux, BSD** | A libgui-drawn bar in the window | `muda` only covers GTK here; GTK would hurt startup and portability |
| **iPad** | The system menu (`UIMenuBuilder`), with the toolbar as the main way in | Native bridge |
| **iPhone, Android** | No menu bar: the toolbar and its `…` menu | libgui |

- The command list and the drawn bar belong in libgui. The native bridges can be a small MIT/Apache crate, like libfilepicker.
- On macOS the menu must attach at startup without slowing opening. The toolbar then keeps only what people use constantly.
- Today Viewer has no menu bar, and libgui only has drawn ones in its demo apps.

## 3. Order of work

1. **Test corpus,** first with PDF, 3D and images, then growing with every format: "supported" means passing it on every platform. Its design is in [pdf.md](design/pdf.md) §8. It comes first because it guards the restructure in step 3.
2. **Command list and native macOS menu.** Small, and required on the Mac.
3. **libfileparser** ([libfileparser.md](design/libfileparser.md)): `-core`, then `-3d`, then the front door, then the other categories; a thumbnail tool as its second user. New formats from here on land in their category crate.
4. **Formats:**

| Order | Formats | Why | Work |
|---|---|---|---|
| 1 | **XLSX, XLS, ODS** | Very common; the `Table` view fits; `calamine` is pure Rust | Small: sheet tabs, big files |
| 2 | **CBZ, CBR** | Reuses the page view | Very small; check unrar's licence for CBR |
| 3 | **EPUB, then DOCX, ODT, RTF** | One shared flowing-document view serves them all | Largest; won't match Word's layout exactly at first |
| 4 | **PPTX, ODP** | Slides can reuse `PageSource`, but drawing them is hard | Medium to large |
| 5 | **Audio and video** | Core to "opens anything"; our media library on FFmpeg's libraries, not libVLC (PLAN.md §12.6) | Medium to large: the library, then a small player; a separate track |
| 6 | **Long tail** | 7z, RAR, SQLite, fonts, HEIC, RAW, STEP, DICOM | Many small jobs |

Not chosen for the App Store build: MuPDF's own EPUB, XPS and other extras, which docs/design/pdf.md §2 keeps off iOS. Reconsider EPUB with the flowing-document view (format 3): our own reader would work on iOS too, whereas MuPDF's wouldn't. That would change the "not on iOS" row for EPUB.

## 4. 3D editing, for later

Viewer isn't Blender.

| Level | What | Verdict |
|---|---|---|
| **Inspect** | Hide or isolate objects, measure, section view, wireframe, unit and up-axis fix | First; most of it doesn't change the file |
| **Light edits** | Move, rotate, scale, delete, rename objects; swap a texture; basic material values | Yes, glTF first |
| **Cleanup and convert** | Fix orientation or scale, reduce polygons, compress textures, export to GLB, STL, USDZ | Yes |
| **Modelling** | Moving vertices, sculpting, rigging, animating | No |

Writing glTF back is safe. FBX and `.blend` can't be rewritten faithfully, so they offer "Save a copy as GLB".
