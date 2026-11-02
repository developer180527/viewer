# Architecture: several products, one repo

Status: direction agreed, nothing moved yet. Crate names are placeholders. Written 2026-11-03.

**Everything here is explicitly pre-v1.** APIs change freely for months, and each library says so in its README and version (0.x). No compatibility promises yet.

## 1. The principle

We aren't making one product. We're making an app, parsing libraries, contracts, renderers and more, so that anyone can reuse each piece freely.

**Libraries provide mechanism; the app decides policy.** Whether to spawn a helper process, sandbox, load lazily, or work around iOS's limits is Viewer's decision. A library never spawns a process or decides those things itself. It exposes hooks.

**The caveat:** a library's contract must *allow* running across a process boundary, or no hook can move it there later.

## 2. The products

| Product | Contains | Depends on |
|---|---|---|
| **Contracts** | `Document`, `Content`, traits, hook types | Almost nothing |
| **Parsers** | One crate per format family, plus an optional "everything" bundle | Contracts |
| **Renderers** | Pixel and texture renderers per content type | Contracts |
| **Isolation kit** | Wire protocol, shared memory, remote wrappers; optional | Contracts |
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
    pub alloc: &'a dyn AllocHook,     // what C libraries allocate through (§5)
}
```

**3. Registration is explicit and lazy.** The app lists what it wants, and nothing runs until a matching file opens. Backends are Cargo features.

**4. Isolation is an add-on.** In-process and remote sources implement the same trait, and the app chooses:

```rust
let pages: Arc<dyn PageSource> = local_pages(doc);
let pages: Arc<dyn PageSource> = RemotePages::connect(pipe);
```

Viewer's choice: helper on desktop, in-process on iOS.

**5. Simple use stays simple.** `viewer_parse::open(path)` works with strict default limits.

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
| **libVLC** (planned) | **No hook** (unverified) | **Its own threads**, plugins, its own file and network access | Heavy | Helper process on desktop; network off |

Our own code breaks the rule too: `fmt-3d/src/textures.rs` and `core/src/content.rs` start their own threads, and `alloc.rs` is a global allocator only an app can set.

## 6. Order

1. Split contracts out; replace the global `WAKE` with hooks.
2. Change `PageSource` to the sink and cancel form; move loaders onto `ParseContext`.
3. Remove `rayon` from `image`; route our threads through `Spawn`; route MuPDF and ufbx allocators.
4. Move renderers and views out of the app.
5. Build the isolation kit with PDF first, then Viewer's desktop helper. Spike 3D scenes alongside; `CpuScene` fills in gradually and is the hardest to isolate.
6. Continue with the menu bar and XLSX on the new structure (roadmap-notes.md).

Keep one repo for now, and split repos when the APIs settle. A small second user, such as a thumbnailer command-line tool, will test whether the design is right.
