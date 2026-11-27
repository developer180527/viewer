# Apple UI: a SwiftUI shell over the same Rust core

Status: proposal, 2026-11-18. Nothing here is built. Today Viewer runs on iPhone and iPad as the Rust binary itself, drawing its own UI with libgui (`ios/project.yml`).

**The goal:** on iPhone, iPad, Mac and Apple Vision Pro, Viewer has a native SwiftUI interface. Everywhere else it keeps libgui. On Apple platforms, libgui may stay as a second interface the user can switch to.

**Why:** visionOS. There, native controls get look-and-pinch interaction, hover highlights, ornaments and volumes from the system. A UI drawn by the app itself can't have them (§5). Native controls also give VoiceOver, Dynamic Type, keyboard focus and platform menus on the other Apple platforms, without building them ourselves.

## 1. The shape

Swift owns the app on Apple platforms, and Rust is a library it calls. Everything below the UI is the same Rust on every platform.

```
            Apple: SwiftUI shell                 Elsewhere: libgui shell
   ┌───────────────────────────────────┐   ┌──────────────────────────┐
   │ SwiftUI: windows, navigation,     │   │ libgui: windows, menus,  │
   │ menus, gestures, Info, editing UI │   │ navigation, views        │
   └──────────┬────────────────────────┘   └────────────┬─────────────┘
              │ Swift bindings (UniFFI)                 │ Rust
   ┌──────────▼─────────────────────────────────────────▼─────────────┐
   │ viewer-core: the app's logic. Open, facets, commands, edit       │
   │ sessions, settings. No UI.                                        │
   ├───────────────────────────────────────────────────────────────────┤
   │ libfileparser (reading)   ·   libviewer renderers (Metal via wgpu)│
   └───────────────────────────────────────────────────────────────────┘
```

**The rule: the shells present, and the core decides.** Anything both interfaces must do the same way lives in `viewer-core`: which file is open and where, its facets, commands, editing sessions, settings, recent files. A shell only turns them into controls. So the two interfaces can't drift into behaving differently, only into looking different.

## 2. `viewer-core`: the app without its UI

✅ 2026-11-19: `crates/core`, with no UI dependency (no libgui, winit or file picker); the libgui app is its first client. The crate that held this name before (content models, the loader registry) is now `viewer-model`. What moved, and what's still to:

| Moves to `viewer-core` | Stays in each shell |
|---|---|
| ✅ Opening a file, a folder or a container's entry, on a loader thread, with the app's policy | Windows, scenes, menus, toolbars |
| ✅ The pages (newest last), Back, and the document's facets and the facet shown | Layout, navigation, animation |
| ✅ Commands (open, open entry, view as, back) and whether each is available; the view-as choices | Gestures and keyboard shortcuts, mapped onto commands |
| ✅ What the screen says, as data: title, error, notices (fallback reader, blocked files and their fix), info panel, status line | Drawing them |
| ✅ What the user picked, by number (`GrantId`): released when no page reads through it | The platform's token for it, which Apple releases on the thread that took it |
| ⬜ The place in a document (page, row, scroll, camera): libgui keeps it per widget today | Each page's view state, by page id |
| ⬜ Editing sessions (`EditSession`, docs/design/editing.md §6) | The editing tools' UI |
| ⬜ Settings and recent files (none yet) | Settings screens |

The place in a document is part of the core's state, not a view's. That's what lets the user switch interface, or reopen a file, and land where they were.

## 3. Swift bindings

- **UniFFI** generates the Swift API from Rust, for `viewer-core` and for libfileparser's content models. It's Mozilla's, used in shipping apps, and it handles errors, callbacks and async across the boundary. The C API planned for other languages (docs/design/libfileparser.md §7, `libfileparser-c`) stays separate: UniFFI is for the Swift shell only.
- **One `.xcframework`** holds the Rust libraries for each Apple target: iOS, the iOS simulator, macOS, visionOS, and the visionOS simulator.
- **Content crosses by range, as it does in Rust.** A table gives its row count and then the rows asked for; a tree gives a node's children when it opens. SwiftUI's lazy lists ask for what's on screen, so a million-row table costs the visible rows, not a million copies. This is the same contract every libviewer view already uses (docs/design/libviewer.md §1).
- **Every call can fail, and none blocks the main thread.** Opening and reading run on the core's threads; Swift awaits them (`async`), and `Notify` becomes a Swift callback when more of a document arrives.

## 4. What draws each kind of content

| Content model | In the SwiftUI shell |
|---|---|
| Table, Tree, Text, Flow (documents), Container | **Native SwiftUI views:** `Table`, `OutlineGroup`, `List`, text views, over the range API. Native look, selection, context menus, VoiceOver and Dynamic Type come with them. |
| Raster, Pages, Hex | **A Metal view hosting libviewer's renderer.** Rust draws into the view's `CAMetalLayer` through wgpu; SwiftUI's gestures (pinch, pan, double-tap) drive its zoom and position through `viewer-core`. |
| Scene (3D, molecules) on iPhone, iPad, Mac | The same Metal view, with RendererX and the existing orbit, pan and zoom. |
| Scene on Vision Pro | **RealityKit**, in a volume (§5). |
| Media (later) | AVKit's player, with the media library's decoding where AVFoundation can't play a format. |

The renderers don't change: they draw into a surface they're given, as they do inside libgui today (docs/design/libviewer.md §2's `Host`). The Metal view is one more host.

## 5. visionOS

On Vision Pro, **apps never see where the user is looking.** The system highlights what's looked at only for its own views (`hoverEffect`), and only then delivers the pinch. A UI an app draws itself still receives pinches, but its buttons can't light up when looked at, so it feels broken. So on visionOS:

- **Every control is SwiftUI.** libgui may appear only as a drawn canvas inside a window, never for buttons, lists or menus.
- **Windows** show 2D content (tables, documents, pages, images) exactly as on iPad. Commands live in **ornaments** beside the window, not in a toolbar inside it.
- **3D opens in a volume:** `scene-data`'s meshes, materials and textures become RealityKit entities (`MeshResource`, `PhysicallyBasedMaterial`). The user walks round it, and pinches and drags to turn and scale it, through RealityKit's gestures. A molecule's atoms and bonds (docs/design/libfileparser.md §2.1) suit a volume particularly well.
- **Later, an immersive space** for a model at real size, if it earns its place.

RealityKit is the second 3D renderer on Apple platforms. It shares nothing with RendererX but the scene data, which is why the boundary between parsing and rendering matters (docs/design/architecture.md §2).

## 6. Switching interface

Optional, and last in the build order. A setting, **Interface: Native / libgui**, available on iPhone, iPad and Mac (not on visionOS, §5):

- **libgui inside the Apple app** runs in a view like the renderers' Metal view: Rust draws libgui's frame into it, and UIKit or AppKit forwards touches, pointer, scroll and keys.
- **libgui must become embeddable for this:** the host drives frames and supplies input, instead of libgui owning the window and event loop. That's a capability to add to libgui, not a fork of it.
- **Switching keeps the place:** the root view changes, and the open document, facet and position come from `viewer-core` (§2).

The cost is that every feature's UI exists twice on Apple platforms (editing tools, Info, settings): `viewer-core` keeps the behaviour single, but not the screens. If that cost isn't worth paying, the libgui mode can stay on the Mac only, for development and checking the two interfaces agree.

✅ 2026-11-20, built: the libgui app's interface is `viewer_surface::chrome::ViewerApp`, shared. It asks its host for the picker (`Request::Open`, `Request::AllowFolder`) rather than calling one: the libgui app answers with libfilepicker (`crates/app/src/lg/picking.rs`), the Apple apps with their own. `Canvas::draw_app` draws it on the same canvas the Metal view uses, and viewer-ffi's `ViewerCanvas.app(...)` puts it on a `CAMetalLayer`, lending it the shared session for each frame under the lock, so both interfaces see one session. The setting is viewer-core's `Settings::interface`; the Apple apps swap their root view when it changes, and libgui's Settings menu has "Use the native interface" there. Checked on the iPhone simulator: switching both ways keeps the open PDF at its page and zoom. Not done: drag and drop into libgui's interface on the Mac, and VoiceOver (libgui has none), which is why native stays the default.

## 7. Accessibility

On Apple platforms, the SwiftUI shell is where accessibility comes from: native views are accessible by default. Content drawn by Rust (pages, images, 3D) still needs a description and, where it has structure, accessible children, and those come from the same facets libgui's AccessKit tree will use (a PDF's tagged structure, an image's description, a scene's object list). The parsers supply that meaning once, for both shells.

## 8. Build order

1. 🟡 **`viewer-core`:** move the app's logic out of the libgui app, so the libgui shell becomes the first client of it. This is useful on every platform, and gives editing's `EditSession` its home. ✅ 2026-11-19: the session, its pages and loading, its commands and what each screen shows, `Send` for Swift's async runtime, tested headless (`crates/core/tests/session.rs`); the libgui app draws it, its screenshots unchanged. Still to move: the place in a document, settings and recent files (§2). ✅ 2026-11-20: recent files (`crates/core/src/recents.rs`: newest first, kept in a text file the shell names, with the shell's bookmark for each) and the place in a paged document (libviewer's `PagesPlace`: page, offset, zoom), restored when the file opens again. ✅ 2026-11-20, later: settings (`crates/core/src/settings.rs`: the theme, and reopening the last file at launch), kept with the recent list in the folder a shell names (`Session::use_data_dir`); the libgui app now uses both (Recent and Settings in its toolbar and compact menu, recent files on the empty screen, the PDF place, the theme as set, `--data-dir`), as do both SwiftUI apps (a Settings window on the Mac, a sheet on iPhone and iPad). Still to move: places in text, trees and flowing documents, which needs libgui to let a scroll area's offset be read and set; reopening a recent file in the libgui app on iOS, which needs libfilepicker to keep a bookmark.
2. ✅ **Bindings:** UniFFI for `viewer-core` and the content models; the `.xcframework`; the Metal view hosting libviewer's renderers. ✅ 2026-11-19: `crates/ffi` (UniFFI 0.32, proc macros) gives Swift a `ViewerSession`: its commands, security-scoped grants, status, info, notices and view-as, and text, tables and containers by range. `scripts/apple.sh` builds the static library (Mac, or `all` for iOS and the simulator too) and generates the bindings and `ViewerFFI.xcframework`. `apple/` is a first SwiftUI Mac shell, sandboxed and opening files through the open panel, drops and Finder, with native text, table and entry views (`apple/run-mac.sh`). `apple/test.sh` drives the session from Swift on the samples. ✅ 2026-11-20, the Metal view: `crates/surface` (viewer-surface) holds what both shells draw with (the fonts, libviewer's GPU host with RendererX and its shader thread, the page views), and its `Canvas` draws a page into a surface another UI owns. viewer-ffi's `ViewerCanvas` puts one on the shell's `CAMetalLayer`; `apple/Mac/Canvas.swift` forwards AppKit's mouse, scroll, pinch and modifiers, draws on a display link only while a frame asks, and sets the cursor. So pages, images, 3D, formatted text, trees and hex are drawn by the same code as in the libgui app (`crates/surface/tests/canvas.rs` checks the pixels offscreen). ✅ 2026-11-20: keys into the canvas (AppKit key codes and UIKit's HID usages, mapped in Rust), Open Recent, and the xcframework hard-linking the static libraries rather than copying them. The debug library stays large (864 MB): it is every format's reader (office_oxide, the 3D and document readers, wgpu, HarfBuzz), unoptimised; what the linker keeps for the app is far smaller (121 MB debug app on iOS).
3. 🟡 **SwiftUI on iPad and Mac:** file picker and document browser, facet tabs, Info, tables, trees and documents as native views, pages, images and 3D in the Metal view. Then iPhone's compact layout. ✅ 2026-11-20, a first iPhone and iPad app: `apple/` is now `Shared/` (the session, native lists, recent files), `Mac/` and `Mobile/`, with a `ViewerMobile` target in `project.yml`. It opens files through the document picker and from Files (open in place), with a navigation stack, View-as and Open Recent menus, and Info in a sheet. `Mobile/Canvas.swift` hosts the same `ViewerCanvas`: touches go to libgui as they are (its drags, flings and two-finger pinch), and an iPad's keyboard, trackpad and mouse send keys, hover, clicks and scrolls. `apple/run-ios.sh` builds it and runs it in the simulator. Later that day, iPad's width: a split view with Open and the recent files in a sidebar, View-as as a segmented control when there are four choices or fewer, and Info in an inspector column (a sheet on iPhone, which keeps the stack). Hints speak of taps and fingers on a touch screen until a mouse hovers (libviewer's `Host::touch_screen`), shorter on a phone. Fixed on the way: the canvas draws two frames after a resize, so a turned iPad lays its page out again without a touch; a 3D scene frames itself again when its view changes shape until the camera is moved (a phone showed nothing, framed for a square). Still to do: the document browser. ✅ 2026-11-20, device hardening: the team in `project.yml`, `apple/run-ios.sh device [release] [ID]`, `scripts/apple.sh ios release` for a device build alone, the app's `viewer-metrics:` console lines (launch to first frame, each open, phys_footprint), and VoiceOver for the Metal view (the file and its kind, "page 2 of 3", direct interaction; libgui's interface says it can't be read). Release on an iPhone 17 (iOS 27): a 34 MB app (121 MB debug); first frame 130–300 ms after launch; GeoTopo.pdf (5 MB) loaded in 19 ms, 157 MB footprint once its pages are drawn; xyzrgb_dragon.obj (12 MB) 91 ms, 188 MB; DragonAttenuation.glb 10 ms, 223 MB. Still to check by hand: pinch, a hardware keyboard, a trackpad, VoiceOver. ✅ 2026-11-25: more than one window on iPad. Each window has its own shell and session (its own pages and grants); the windows share one recent list and the settings, kept in Rust (`Session::share_kept`, `ViewerSession.sharing`), so each sees the files the others open and one save writes them. A window's value is the file it shows: iPadOS restores it with that file, and Open in New Window on a file already in a window brings that window forward. New Window is in the sidebar and on ⌘N. The Mac keeps one shell for all its windows for now; its menu commands would need to follow the focused window first. Later the same day: the windows draw on one GPU (`viewer_surface::GpuContext`: one device and queue, RendererX and its pipelines compiled once, freed with the last canvas), and page views share one tile budget, 96 MB on iOS and 256 MB elsewhere, for all windows together (an even share each at least, more while others hold less). A window whose scene goes to the background frees its tiles and spare textures (`ViewerCanvas.trim`), as does every window but the one in front on a memory warning; each renders its screen again when it's back. Still per window: libgui's UI renderer and font atlas. ✅ 2026-11-28: a file open in one window opens in another without being read again: the sessions sharing what's kept share the decoded documents too (`viewer-core`'s `shared.rs`; `Page::doc` is an `Arc<Document>`), keyed by canonical path, length, modification time and View-as, and held weakly, so a document lives exactly as long as some window shows it. Three windows on a 179 MB log index: 524 MB before, 179 MB after, and each window after the first opens in about 1 ms. Each window keeps its own page and view state (scroll, zoom, the scene's camera, its views' GPU textures). Not yet: two windows opening the same file at the same moment both read it, and entries opened from inside a container aren't shared.
4. ⬜ **visionOS:** windows and ornaments, then a RealityKit volume for scenes and molecules.
5. ⬜ **Embeddable libgui** and the interface setting (§6), if still wanted.

## 9. Open questions

- **Mac: SwiftUI, or SwiftUI with AppKit where it falls short?** Multi-window document apps, menus and very large tables still sometimes need AppKit underneath.
- **The minimum OS versions.** The Mac shell targets macOS 14 (for `.inspector`). The project targets iOS 15 today. SwiftUI's `Table`, and the lazy containers that keep big content cheap, argue for iOS 17 / macOS 14, and visionOS starts at 1.0.
- **One app or two on the App Store** (iPhone/iPad/visionOS, and Mac), and whether the Mac build ships through the App Store at all (MuPDF's licence, docs/design/pdf.md).
