# Performance: what the stress profile found, and the plan

Measured 2026-11-20/21 on the SwiftUI apps (Release, full LTO, symbols kept): a MacBook
Pro (Apple silicon, macOS 27) and an iPhone 17 (iOS 27). The raw tables are
[docs/perf/2026-11-21-mac.md](../perf/2026-11-21-mac.md) and
[docs/perf/2026-11-21-iphone17.md](../perf/2026-11-21-iphone17.md).

## 1. How it was measured

- **The stress set** (`scripts/stress/files.sh`, into `target/stress/files`): the
  corpus's biggest real files of each kind (a 5 MB LaTeX PDF, a 23 MB textured glTF,
  Sponza with its 69 textures, a USDZ, a 12 MB OBJ, DOCX, PPTX, XLSX, a 1.3 MB Markdown
  changelog), and generated ones past what the corpus has: an 8192² PNG, a 300k-row
  CSV, a 39 MB log, a 35 MB JSON, a zip of 5000 entries.
- **The stress runner** (`apple/Shared/Stress.swift`): `-stress <names|all>` or
  `VIEWER_STRESS=…` and the app drives itself with synthetic input into the real
  canvas, one scenario after another (idle, scroll, touch flings, pinch zoom, paging
  keys, image zoom and pan, 3D orbit, document scrolls, the native lists' row fetches,
  archive entry churn, file churn, resize churn, the libgui interface). It prints a
  JSON line per scenario: frames, main-thread draw time (p50/p95/p99/max, draws over
  16.7 ms), CPU time of every thread, the kernel's energy estimate
  (`task_power_info_v2.task_energy`), wakeups, and memory footprint (start, peak, end).
- **The profiles**: each scenario again under Instruments' Time Profiler
  (`scripts/stress/run-mac.sh`, `run-ios.sh`), one trace each, plus a Power Profiler
  trace on the iPhone; `scripts/stress/summarize.py` charges each sample to the first
  frame of the app's own code, so system time (memcpy, kernel traps) lands on the
  function that asked for it.
- **Memory composition**: `footprint` on the Mac app at rest with a file open.

Not measured: GPU time (the kernel's per-task GPU counter reads 0 on both platforms;
Metal System Trace is the next tool), heap allocations by call site (an Allocations
trace), and the libgui app's own winit window (its interface was profiled inside the
SwiftUI app).

## 2. What it found

### Ranked by harm

| # | Finding | Evidence | Platform |
|---|---|---|---|
| 1 | **Zooming a PDF gets the app killed.** | pdf-zoom: footprint 2.9 GB within seconds, then signal 9 (iOS's memory limit). On the Mac it peaks at 7.3 GB and stays at 3.6 GB after zooming stops. | iPhone: crash. Mac: 7 GB |
| 2 | **PDF memory grows with use and isn't given back while the file is open.** | iPhone: paging keys 1.3 → 2.5 GB, flings peak 1.4 GB, resizes 1.3 → 1.9 GB, though the tile cache is capped at 96 MB. Memory falls only when another file opens. Zooming creates and frees thousands of 4 MB tile textures. | both |
| 3 | **Formatted documents lay out the whole document every frame.** | markdown-scroll: 8 fps on iPhone (draw p50 112 ms), 10 fps on Mac (96 ms); 3.4 W on iPhone, 7 W on Mac; 99% main thread in libgui's wrap and rich-text layout, 9% just hashing strings for the text cache. | both |
| 4 | **Tile pixels are copied to the GPU on the main thread.** | `Gpu::upload` is 14–30% of all CPU in every PDF scenario; on iPhone a 4 MB tile costs a whole frame (fling draw p50 16.3 ms, 41 of 468 draws over budget, against 1.5 ms for a trackpad scroll). | both |
| 5 | **The tile cache thrashes when the visible tiles don't fit it.** | libgui-idle on iPhone: 30 fps and 24% CPU with nothing moving, uploading tiles in a loop (the libgui interface shows more page at 3× than the 96 MB budget holds with its preview tiles). | iPhone |
| 6 | **Every wake refreshes all of the chrome.** | `Shell.pump` runs `breadcrumbs`, `info`, `status` and the rest on each tile or texture arrival (2–3% CPU plus SwiftUI diffing during PDF and 3D loads). | both |
| 7 | **Returning to a big archive rebuilds its list.** | zip-entry-churn: 0.7 s of main-thread CPU per return on the Mac (44 s, 294 J over 60 rounds), 0.15 s on iPhone; SwiftUI's `List(selection:)` over 5000 rows, rebuilt by `.id(page)`. | Mac mostly |
| 8 | **Resting memory is high.** | Mac, a PDF open and idle: 577 MB (244 MB GPU tiles, the desktop cache's full budget; 225 MB in 57 large heap blocks of ~4 MB). iPhone: 208 MB. | Mac |
| 9 | **Big images and 3D textures are held at full size.** | 8192² PNG: 265 MB of GPU memory (Mac); Sponza: 409 MB, uncompressed, still decoding its JPEGs through a 10 s orbit; PNG decoding runs during image pans. | both |
| 10 | **Spreadsheet rows are slow to fetch.** | sheet-fetch: 2.4–2.5 ms per 64 rows (78% in `tableRows`, formatting every cell), 15× CSV; opening the XLSX takes 360–470 ms. | both |
| 11 | **libgui's interface costs twice the native one.** | PDF scroll: CPU 35% vs 17% (Mac), 67% vs 40% (iPhone); draw p50 2–3 ms vs 0.7–1.5 ms. | both |
| 12 | **Frames are capped at 60 Hz on ProMotion iPhones.** | Every iPhone scenario tops out at 60 fps (the display link's default). | iPhone |

### What's already good

- Launch to first frame: 240 ms (Mac), 130–300 ms (iPhone).
- Idle: 0 frames, 11 wakeups/s, ~0 mW; the display link stops when nothing changes.
- 3D orbit: 120 fps on the Mac, 60 on iPhone, draw p50 0.8–1.7 ms; meshes and glTF fine.
- Text, log and CSV rows: 80–170 µs per screenful from Rust to Swift.
- No leak across churn: file churn levels off (Mac 1.09 → 1.10 → 1.10 GB, iPhone
  452 → 489 → 500 MB over three rounds); archive churn flat at ~66 MB on iPhone.
- Opens: everything under 300 ms except the XLSX (360–470 ms).

## 3. The plan: where, then how

In order. Each step names the scenario that proves it and the target; re-run
`scripts/stress/run-mac.sh` and `run-ios.sh <device>` and compare the tables.

### Step 1: stop the memory blow-up (findings 1, 2, 5); crash fix

1. **Release freed GPU memory every frame.** wgpu frees a destroyed texture only when
   the device is polled after the GPU is done with it; `Canvas::frame` polls only
   while uploads settle. Poll without blocking (`device.poll(PollType::Poll)`) once a
   frame, in viewer-surface's canvas and the libgui app's host. *First, as an
   experiment:* one line, then pdf-zoom on the Mac. If the 3.6 GB tail goes, this was
   finding 2.
2. **Reuse tile textures.** Tiles are all 1024² RGBA: keep a pool of them in the GPU host
   (`Host::upload` takes a texture from the pool, `free` returns it) instead of
   creating and destroying one per tile; zooming then allocates nothing.
3. **Never evict what's on screen, and size the cache to the screen.** In libviewer's
   `PagesView::evict`, protect tiles drawn this frame; make the budget at least the
   visible set plus one screen of preview (from the view's pixel size), and when even
   that can't fit (3× and deep zoom), draw from a coarser step instead of thrashing.
   On the desktop, scale the 256 MB budget to the window rather than filling it at rest.

   *Done when:* pdf-zoom runs on the iPhone (no kill) with peak < 400 MB; pdf-page-keys
   and resize-churn end within 100 MB of where they started; libgui-idle is 0 fps;
   Mac idle-pdf under 300 MB.

   ✅ **Done 2026-11-21.** What did it was the third change: libviewer's page view now
   caps the tiles it asks for to its budget (what's on screen always, the neighbours
   while they fit), never evicts a tile it is asking for, and sizes the budget to the
   view (`cache_budget`: eight screenfuls of pixels, 64 MB to the old caps). The
   evict-and-render-again loop was findings 1, 2 and 5 all at once. The per-frame poll
   and the texture pool (`viewer_surface::TexturePool`, spares from evicted tiles only,
   given up when a page closes) measured neutral for memory; the pool cuts zooming's
   power on iPhone from ~330 to ~220 mW. Measured (tables in
   [docs/perf/2026-11-21-step1-compare.md](../perf/2026-11-21-step1-compare.md)):

   | | Before | After |
   |---|---|---|
   | iPhone pdf-zoom | killed at 2.9 GB | completes, peak 834 MB |
   | iPhone pdf-page-keys peak | 2,488 MB | 345 MB |
   | iPhone pdf-fling-touch | peak 1,419 MB, draw p50 16.3 ms, 41 draws over budget, 341 mW | 336 MB, 1.8 ms, none, 24 mW |
   | iPhone libgui-idle | 30 fps, 24% CPU | 0 fps, 0.1% |
   | iPhone libgui PDF scroll CPU | 67% | 35% |
   | iPhone resize-churn end | 1,771 MB | 763 MB |
   | Mac pdf-zoom peak / end | 7,286 / 3,580 MB | 580 / 540 MB |
   | Mac pdf-fling-touch CPU | 83% | 11% |
   | Mac PDF at rest (footprint) | 577–625 MB | 294 MB |

   Left over: iPhone pdf-zoom still peaks at 834 MB (target 400; the Mac's MuPDF path
   peaks at 550, so PDFKit's own caching at deep zoom is the suspect), and the Mac keeps
   ~1.2 GB after file churn (the iPhone ~520 MB), heap the allocator doesn't return.

### Step 2: take the main thread off tile uploads (finding 4)

4. **Upload on the render worker.** wgpu's `Device` and `Queue` are `Send + Sync`: create
   the texture and `write_texture` on the worker thread that rendered the tile, and
   hand the main thread only the finished texture to register. The main thread's
   per-tile cost drops from a 4 MB copy to a bind-group update.
5. **Smaller tiles on touch devices** (512² instead of 1024²): a quarter of the copy
   per tile and finer cache eviction; keep 1024² on the desktop.

   *Done when:* pdf-fling-touch on iPhone has draw p50 < 4 ms and no draws over budget;
   `Gpu::upload` drops out of the main thread's top functions.

   ✅ **Done 2026-11-21**, with a third change the measurements called for. libviewer's
   `TileUploader` lets the page worker put each tile on the GPU itself (viewer-surface's
   `GpuUploader`, on clones of the device and queue, writing over the pool's spares);
   the main thread only registers it (`Host::adopt`). That alone made zooming *cost
   more* energy on iPhone (A/B in one build, alternating: 1.6–3.0 W against 0.4–0.7 W),
   because uploads no longer throttled to 4 MB a frame meant every tile a pinch passed
   was rendered and uploaded. So: **tiles at a new zoom wait until it has held for
   150 ms** (`ZOOM_SETTLE`, both ways; the tiles at hand are drawn scaled meanwhile;
   zooming out past what the cache holds switches at once). With that, both upload paths
   cost the same over a zoom and the rest after it (`pdf-zoom-total`: ~29% CPU, 66–208
   mW either way), and the worker path keeps less memory (230 MB against 236–277 MB at
   the end) with smoother frames. A check that dropped tiles the view no longer wanted
   was tried and removed: with wanted tiles changing every frame (resizing) it rendered
   the same tiles endlessly. 512² tiles on touch weren't needed: the main thread no
   longer copies tiles at all.

   | iPhone 17 | Step 1 | Step 2 |
   |---|---|---|
   | pdf-zoom peak / end | 970 / 778 MB | 352 / 296 MB |
   | pdf-zoom draw p95 | 4.4 ms | 1.6 ms |
   | pdf-zoom + rest (CPU, power) | — | ~29%, 100–300 mW |
   | pdf-scroll draw p50 / p95 | 1.33 / 2.33 ms | 0.98 / 1.70 ms |
   | pdf-page-keys draw p95 | 2.14 ms | 1.39 ms |
   | libgui PDF scroll peak / p95 | 645 MB / 3.4 ms | 315 MB / 1.6 ms |
   | resize-churn CPU, power | 41%, 385 mW | 29–43%, 16–241 mW (p95 ~16 ms both) |

   Mac: pdf-zoom CPU 35% → 24–29%, draw p95 2.1 → 1.1 ms; pdf-scroll draw p50 0.67 →
   0.23 ms. Tables: [docs/perf/2026-11-21-step2-compare.md](../perf/2026-11-21-step2-compare.md).

### Step 3: lay out only what's on screen (finding 3)

6. **Virtualize formatted text.** libgui_markdown builds and wraps every block each
   frame. Lay out blocks once per width and cache their heights; each frame build only
   the blocks in view (plus a screen above and below), positioned by the cached
   heights. Key the shaped-text cache by block and style, not by hashing the string.
   This is a libgui change (libgui_markdown and `rich`), then a new pin here; libviewer's
   `FlowView` keeps its API.

   *Done when:* markdown-scroll at 60 fps on iPhone, draw p95 < 8 ms, power < 300 mW
   (from 3.4 W); docx and pptx scrolls unchanged or better.

   ✅ **Done 2026-11-21, in libviewer, without changing libgui.** `FlowView` cuts the
   document into chunks of whole blocks (`CHUNK_CHARS`, 1200 bytes of Markdown), each
   its own widget, in libgui's `virtual_rows`: only the chunks in view, and one either
   side, are built. A chunk's height is measured when it's built, at the column's width,
   and estimated from its length until then. Screenshots are unchanged (DOCX byte for
   byte; the changelog within half a point, from rows placing a fractional height).

   | markdown-scroll (1.3 MB changelog) | Before | After |
   |---|---|---|
   | iPhone fps / draw p50 / p95 | 9.5 / 95 / 100 ms | 56 (the 60 Hz cap) / 5.9 / 8.2 ms |
   | iPhone draws over 16 ms | 81 | 0–1 |
   | iPhone CPU / power | 92% / 4.9–5.1 W | 43% / 290 mW |
   | Mac fps / draw p50 / CPU | 10 / 96 ms / 95% | 100 / 4.2 ms / 45% |

   DOCX and PPTX scrolls unchanged. **Left for libgui:** what remains of the 4–6 ms is
   libgui laying out the visible rich text each frame: it hashes each run's string to
   find it in the text cache (7.5%), looks glyph advances up in a hash map (5–7%), and
   finds a paragraph's line-break opportunities three times a frame (min width, layout,
   end-of-frame layout). Caching the break opportunities and the layout per paragraph and
   width, and keying runs by paragraph rather than by string, would take most of it; a
   libgui change and a new pin.

### Step 4: only do work that changed something (findings 6, 7, 12)

7. **Split the wakes.** Rust wakes the shell for two reasons: the session changed (a
   load finished) or the canvas has something to draw (a texture, a tile). Give the
   session a generation counter that bumps on real changes (`ViewerSession.generation()`),
   and have `Shell.pump` refresh the chrome only when it moved; texture wakes go
   straight to the canvas.
8. **Keep big lists cheap.** In `Content.swift`, drop `List(selection:)` for the entries
   (open on tap instead) and keep each page's list alive across Back rather than
   rebuilding it with `.id(page)`.
9. **ProMotion where it shows.** Ask the display link for 120 Hz only while the user is
   touching, scrolling or a fling coasts (`preferredFrameRateRange`), and add
   `CADisableMinimumFrameDurationOnPhone`; stay at 60 Hz or less for animations
   that don't need more.

   *Done when:* the chrome refresh drops out of PDF and 3D profiles; zip-entry-churn on
   the Mac < 50 ms of CPU per return; iPhone flings at 120 fps with energy no worse
   than today's 60 fps flings.

   ✅ **Done 2026-11-21.** (7) needed no Rust change: `Shell.pump` refreshes the chrome
   only when `poll()` took in a finished load; tile and texture wakes go to the canvas
   alone. (8) `Entries` is a lazy stack of rows (click selects and double-click opens on
   the Mac, tap opens on touch; each row a button to VoiceOver). (9) `Pacing` asks the
   display link for 80–120 Hz for a second after input and 30–60 Hz otherwise, and the
   iPhone's Info.plist has `CADisableMinimumFrameDurationOnPhone` (in the plist itself:
   Xcode's `INFOPLIST_KEY_` settings don't know it, and without it the app stayed at 60).

   | | Before | After |
   |---|---|---|
   | iPhone pdf-scroll CPU | 40% | 30% |
   | iPhone zip-entry-churn CPU / power | 77–98% / 2.3–3.3 W | 40% / ~1 W |
   | Mac zip-entry-churn CPU per round (open an entry, Back) | 740 ms | 100 ms |
   | Mac zip-entry-churn energy (60 rounds) | 294 J | 22 J |
   | iPhone 3D orbit, while dragging | 60 fps, 10 mW | 120 fps, 20 mW |
   | Idle | 0 frames | 0 frames |

   The fast rate costs what it should, twice the frames for twice the GPU, and only
   while the user's hands are on it.

   **Noticed on the way:** now and then a scene's draws all take one refresh (8.2 ms at
   120 Hz, on the Mac and the iPhone, in some runs of the same scenario and not others):
   the main thread waiting in `get_current_texture` for a free drawable. Not yet
   understood; a candidate for a Metal System Trace pass (the layer's drawable count,
   the surface's frame latency, presenting with a transaction).

### Step 5: smaller memory at rest (findings 8, 9)

10. **Images:** keep a downscaled copy for fit-to-window and tile from the full image only
    when zoomed past it (the PDF path's tiles, reused); don't re-decode on pan. Target:
    huge.png < 80 MB of GPU memory at fit.
11. **3D textures:** generate mipmaps, cap the base level to what the screen can show
    (2048 on phones), and compress (ASTC on iOS, BC7 on the Mac) once decoded; decode
    the textures the camera sees first. Target: Sponza < 150 MB.
12. **MuPDF's store (Mac):** confirm with an Allocations trace that the 225 MB of large
    heap blocks are MuPDF's resource store, then cap it (`fz_new_context` store size,
    ~64 MB).

### Step 6: the smaller ones (findings 10, 11)

13. **Spreadsheet rows:** format each cell to text once (on load or per 256-row chunk) and
    serve fetches from that; target ≤ CSV's 170 µs per screenful. Profile the XLSX open
    (360–470 ms) after.
14. **libgui's interface:** after steps 1, 2 and 4, profile libgui-pdf-scroll again; the
    rest is the chrome's own per-frame layout (libgui), to be cached like step 3's.

### After each step

Re-run the stress suite on both devices and keep the summary in `docs/perf/` with the
date, so each step's effect is on record. Two additions would make that easier: a
`compare.py` that diffs two runs' tables, and a Metal System Trace pass for GPU time,
which the kernel counters don't give.
