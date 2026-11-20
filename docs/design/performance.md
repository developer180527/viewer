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

### Step 2: take the main thread off tile uploads (finding 4)

4. **Upload on the render worker.** wgpu's `Device` and `Queue` are `Send + Sync`: create
   the texture and `write_texture` on the worker thread that rendered the tile, and
   hand the main thread only the finished texture to register. The main thread's
   per-tile cost drops from a 4 MB copy to a bind-group update.
5. **Smaller tiles on touch devices** (512² instead of 1024²): a quarter of the copy
   per tile and finer cache eviction; keep 1024² on the desktop.

   *Done when:* pdf-fling-touch on iPhone has draw p50 < 4 ms and no draws over budget;
   `Gpu::upload` drops out of the main thread's top functions.

### Step 3: lay out only what's on screen (finding 3)

6. **Virtualize formatted text.** libgui_markdown builds and wraps every block each
   frame. Lay out blocks once per width and cache their heights; each frame build only
   the blocks in view (plus a screen above and below), positioned by the cached
   heights. Key the shaped-text cache by block and style, not by hashing the string.
   This is a libgui change (libgui_markdown and `rich`), then a new pin here; libviewer's
   `FlowView` keeps its API.

   *Done when:* markdown-scroll at 60 fps on iPhone, draw p95 < 8 ms, power < 300 mW
   (from 3.4 W); docx and pptx scrolls unchanged or better.

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
