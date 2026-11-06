# Architecture: several products, one repo

Status: direction agreed, nothing moved yet. Written 2026-11-03. The parsing libraries are designed in detail in [libfileparser.md](libfileparser.md); this note holds the principles and the products around them.

**Everything here is explicitly pre-v1.** APIs change freely for months, and each library says so in its README and version (0.x). No compatibility promises yet.

## 1. The principle

We aren't making one product. We're making an app, parsing libraries, contracts, renderers and more, so that anyone can reuse each piece freely.

**Libraries provide mechanism; the app decides policy.** Whether to spawn a helper process, sandbox, load lazily, or work around iOS's limits is Viewer's decision. A library never spawns a process or decides those things itself. It exposes hooks.

**The caveat:** a library's contract must *allow* running across a process boundary, or no hook can move it there later.

## 2. The products

| Product | Contains | Depends on |
|---|---|---|
| **Contracts:** `libfileparser-core` | `Document`, `Content`, the hook traits, `Limits`, errors | Almost nothing |
| **Parsers:** `libfileparser-images`, `-3d`, `-docs`, `-data`, `-text`, `-archives`, later `-media` | One crate per category, each reading any file of its kind into a document with facets (document-model.md), each usable alone | Contracts (and `-images`, a leaf) |
| **Front door:** `libfileparser` | Routing, default hooks, laziness, features | The categories enabled |
| **Previews:** `libfileparser-thumb` | Any document → RGBA on the CPU | Contracts |
| **Renderers** | GPU: RendererX for scenes; tiled images and pages later | Contracts' data types |
| **Isolation kit:** `libfileparser-remote` | Wire protocol, shared memory, remote wrappers; optional | Contracts |
| **UI adapters** | libgui widgets over the renderers | Renderers, libgui |
| **Viewer** | All policy: helpers, sandboxing, lazy loading, menus | All of the above |

Parsers and renderers don't depend on each other, so either can be used alone. Lower-level libraries (RendererX, `usd-lite`, and third-party ones) don't know the contracts at all: a thin adapter crate connects each one to them.

Licences: everything is MIT/Apache except optional AGPL backends such as MuPDF, which are off by default. Enabling one is the app's choice and obligation. A default build has no permissive PDF backend off Apple platforms until `hayro` or PDFium is added as an option.

## 3. Rules

**1. Contracts hold only data and narrow interfaces.** No pointers into a parser's memory, and every call can fail, because a helper can die.

```rust
pub trait PageSource: Send + Sync {
    fn page_count(&self) -> usize;
    fn render(&self, page: usize, scale: f32, region: PageRect, into: &mut dyn PixelSink, cancel: &Cancel) -> Result<()>;
}
```

**2. Parsers receive everything through hooks.**

```rust
pub struct ParseContext<'a> {
    pub files: &'a dyn FileResolver,  // neighbouring files: the app decides what's allowed
    pub limits: Limits,               // memory, sizes, nesting depth
    pub cancel: Cancel,
    pub spawn: &'a dyn Spawn,         // background work runs where the app says
    pub notify: &'a dyn Notify,       // "more arrived"; replaces the global WAKE
}
```

An allocator hook for C libraries (§5) joins these only when a helper process doesn't already cover it.

**3. Registration is explicit and lazy.** The app lists what it wants, and nothing runs until a matching file opens. Backends are Cargo features.

**4. Isolation is an add-on.** In-process and remote sources implement the same trait, and the app chooses:

```rust
let pages: Arc<dyn PageSource> = local_pages(doc);
let pages: Arc<dyn PageSource> = RemotePages::connect(pipe);
```

Viewer's choice: helper on desktop, in-process on iOS.

**5. Simple use stays simple.** `libfileparser::open(path)` works with strict default limits.

**6. Memory is part of every contract.** Any app should be able to read any kind of file cheaply, so every library:
- reads only what it needs: headers first, files mapped, the rest streamed; never a whole file when part will do;
- writes into the caller's buffers (sinks), so the caller decides where memory goes;
- honours `Limits`: every size, count and nesting depth is capped, and going over is an error, not a crash;
- keeps caches bounded, sized by the caller (the PDF tile cache);
- starts no hidden threads or global pools: background work goes through `Spawn`;
- reports what it holds ("this document: 42 MB"), so apps can enforce budgets.

A large library underneath (MuPDF, FFmpeg) is fine as long as it obeys these from the outside.

## 4. Who owns the contracts

They live in their own crate, not in the app, because a library can't depend on an app. Viewer is the first customer and drives what goes in. Each library defines its own native API and the hooks it needs; adapters map it onto the contracts.

## 5. Control over third-party libraries

The goal: memory, threads and OS access go through us wherever a library allows it. Where it doesn't, the library runs in a helper process, where the OS limits it.

Audited 2026-11-03 from the dependency tree, headers and our source. Rows marked "unverified" are from general knowledge.

| Library | Memory | Threads | OS access | To do |
|---|---|---|---|---|
| **MuPDF** | Hook (`fz_alloc_context`), not used yet; store cap set | Starts none itself that we've seen; lock hooks exist. Bundled OpenJPEG has its own thread code: unverified whether used | Reads from our memory stream; has file code for other paths | Pass an allocator. Check OpenJPEG threads |
| **ufbx** | Hooks for temp and result allocators, not used yet | Thread pool hook | File-open callback | Route all three |
| **`image` + `rayon`** | Global allocator | **Global rayon pool, which we switched on ourselves** | None beyond our bytes | Remove the `rayon` feature |
| **`exr`** | Global allocator | Depends on `rayon-core`. Our texture path calls `.non_parallel()`; `image`'s EXR path unverified | None | Check, and always force single-threaded |
| **`resvg` / `fontdb`** | Global allocator | None seen | **Scans and maps system fonts** in the SVG loader | Make fonts a hook |
| **`zip`, `tar`, `flate2`, `zlib-rs`, `bzip2`, `lzma-rust2`, `syntect`, `calamine` (planned)** | Global allocator | None seen | Bytes from us. `tar` pulls `filetime`, unused for reading | Limits only |
| **PDFKit, CoreGraphics** | **No control** | **No control** | OS framework | Accept; we own only the output buffer |
| **FFmpeg libraries** (planned; libVLC dropped, PLAN.md §12.6) | `av_malloc` has no runtime hook; cap with `av_max_alloc` and count frames, which we own | `thread_count` per decoder, set by us | Reads only through our `AVIOContext` callback; network protocols not built | Build only the codecs and demuxers we list; no GPL parts |

Our own code breaks the rule too: `core/src/content.rs` starts its own threads (`fmt-3d`'s texture decoding no longer does: it runs through `Spawn` in `libfileparser-3d`), and `alloc.rs` is a global allocator only an app can set.

## 6. Order

Opening every kind of file stays the priority (docs/roadmap.md), so the restructure comes in a thin slice first, and the rest only when something needs it.

1. ✅ **Test corpus** (corpus/README.md): it is what proves each later step broke nothing.
2. **libfileparser, from the ground up** (libfileparser.md §12): `-core` with the hooks and lazy sources; `-3d` first, because it uses every hook; the front door; then the other categories one at a time. Contracts carry data as owned bytes or a shared-memory handle, never a pointer into a parser (3D's mapped vertex data included).
3. **A second user:** `-thumb` and a thumbnail command-line tool on the libraries alone. Whatever it can't do without reaching into Viewer shows where a boundary is wrong.
4. **Formats** continue on the new structure (docs/roadmap.md §3), each in its category crate.
5. **MuPDF in a helper process on desktop:** the first real use of isolation, for the most common hostile format.
6. **Only when needed:** remove `rayon` from `image` and route our threads through `Spawn`; allocator hooks for C libraries, as counting for budgets (§5); the general isolation kit; moving renderers and views out of the app.

Keep one repo for now, and split repos when the APIs settle.
