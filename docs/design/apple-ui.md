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

Today the app's logic lives in the libgui app crate (`crates/app`). It moves into a crate with no UI dependency:

| Moves to `viewer-core` | Stays in each shell |
|---|---|
| Opening a file, with the app's policy (`ParseContext`: files allowed, limits, threads) | Windows, scenes, menus, toolbars |
| The open document: its facets, the facet shown, the place in it (page, row, scroll, camera) | Layout, navigation, animation |
| Commands (open, view as, export, copy) and whether each is available | Gestures and keyboard shortcuts, mapped onto commands |
| Editing sessions (`EditSession`, docs/design/editing.md §6) | The editing tools' UI |
| Settings and recent files | Settings screens |

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

## 7. Accessibility

On Apple platforms, the SwiftUI shell is where accessibility comes from: native views are accessible by default. Content drawn by Rust (pages, images, 3D) still needs a description and, where it has structure, accessible children, and those come from the same facets libgui's AccessKit tree will use (a PDF's tagged structure, an image's description, a scene's object list). The parsers supply that meaning once, for both shells.

## 8. Build order

1. ⬜ **`viewer-core`:** move the app's logic out of the libgui app, so the libgui shell becomes the first client of it. This is useful on every platform, and gives editing's `EditSession` its home.
2. ⬜ **Bindings:** UniFFI for `viewer-core` and the content models; the `.xcframework`; the Metal view hosting libviewer's renderers.
3. ⬜ **SwiftUI on iPad and Mac:** file picker and document browser, facet tabs, Info, tables, trees and documents as native views, pages, images and 3D in the Metal view. Then iPhone's compact layout.
4. ⬜ **visionOS:** windows and ornaments, then a RealityKit volume for scenes and molecules.
5. ⬜ **Embeddable libgui** and the interface setting (§6), if still wanted.

## 9. Open questions

- **Mac: SwiftUI, or SwiftUI with AppKit where it falls short?** Multi-window document apps, menus and very large tables still sometimes need AppKit underneath.
- **The minimum OS versions.** The project targets iOS 15 today. SwiftUI's `Table`, and the lazy containers that keep big content cheap, argue for iOS 17 / macOS 14, and visionOS starts at 1.0.
- **One app or two on the App Store** (iPhone/iPad/visionOS, and Mac), and whether the Mac build ships through the App Store at all (MuPDF's licence, docs/design/pdf.md).
