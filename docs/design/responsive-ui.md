# Responsive UI: from a phone held upright to a desktop window

One layout, built from the window's size each frame. No per-platform screens and
no separate phone and desktop apps (PLAN.md §12.1). This doc decides how the window
chrome adapts; each view (text, table, image, 3D…) adapts on its own, below.

## 1. What drives the layout

Three inputs, read every frame. All of them can change while the window is open:
the window resizes, the phone rotates, an iPad enters split view, a mouse is plugged in.

| Input | Source | Used for |
|---|---|---|
| **Width class** (logical px, from `FrameInfo.screen_size`) | the window | where the chrome goes |
| **Input kind**: touch or pointer | libgui `PointerKind` of the last event | how big things are |
| **Safe-area insets** (notch, home bar, rounded corners, Android cutouts) | the host: winit on iOS/Android, zero on desktop | padding around the chrome |

Height matters only in one place: a short landscape phone (height < 480) hides the
status bar and auto-hides the toolbar, so the content keeps the screen.

**Width classes**, with hysteresis (switch up at the threshold, down 24 px below it)
so dragging the window edge across a threshold doesn't flicker:

| Class | Width | Typically |
|---|---|---|
| **Compact** | < 600 | phone in portrait, iPad slide-over, a narrow desktop window |
| **Medium** | 600–1023 | phone in landscape, iPad portrait or split view, half-screen desktop |
| **Expanded** | ≥ 1024 | iPad landscape, desktop |

The classes depend on width only, never on the platform. A desktop window dragged
narrow gets the phone layout, which is also how the compact layout gets tested on a
Mac without a device.

## 2. The chrome in each class

Today's chrome (`lg/app.rs`): toolbar (Open, Back, breadcrumbs, "View as", Info),
the view, an optional 300 px info panel on the right, and a status bar.

| Part | Compact | Medium | Expanded (today's layout) |
|---|---|---|---|
| Toolbar | **Top bar**: Back, the file name (tap for the breadcrumb menu), one "⋯" menu holding Open, View as, Info and Share | Top bar with Back, breadcrumbs truncated from the left, View as as an icon menu, Info | Full toolbar |
| Info panel | **Bottom sheet**: a peek (format · size) and full height on drag; dismissed by swiping down | **Overlay drawer** from the right, 320 px, over the content | **Side panel** beside the content, resizable 240–480 px |
| Status bar | Hidden. Its facts (format, size, decode time) go into the sheet's peek | Shown, one line, shortened | Shown |
| Loading | A thin progress line under the top bar | Same | Status bar, as today |
| Containers (zip, tar) | Push navigation: the entry list replaces the content; Back pops | Same | Same today; later a list on the left with a preview on the right |
| Errors | A banner under the top bar | Same | Same |

Rules that hold in every class:
- **The content always gets the remaining space.** Chrome never sits on top of the
  content, except the medium-class drawer and the compact-class sheet, which are dismissed.
- **Every command lives in one place per class.** A command hidden in the "⋯" menu on
  compact is a toolbar button on expanded. It's never in both, and never missing.
- **Breadcrumbs truncate from the left.** The current file stays readable, and the
  root collapses to "…".

## 3. Touch versus pointer

Size follows the input kind, not the width class. An iPad with a trackpad gets
pointer sizing; a touchscreen laptop being touched gets touch sizing.

| | Pointer | Touch |
|---|---|---|
| Minimum target | 24 px | **44 px** (Apple) / 48 dp (Android): the hit area grows, the drawing doesn't have to |
| Row height (tables, trees, entry lists) | 22 px | 40 px |
| Tooltips | on hover | none; long-press shows the same text |
| Context actions | right-click | long-press |
| Scrolling | wheel, scrollbar drag | kinetic drag with libgui's `touch_slop`; scrollbars become thin indicators |

This is a theme switch: libgui's `Theme` already carries sizes and padding, so we
keep two density presets (`Theme::pointer()`, `Theme::touch()`) and swap them when the
input kind changes, rather than writing `if touch` in every widget.

## 4. Each view

Views get their rectangle and the input kind; none of them knows the width class.

| View | Compact / touch behaviour |
|---|---|
| **Image** | Fit to screen; pinch to zoom, double-tap to toggle fit / 100 %, two-finger pan. Band streaming (already in) keeps memory flat at any zoom |
| **3D** | One finger orbits, two fingers pan, pinch dollies, double-tap frames the model. RendererX `controls` already map these gestures; they need touch events from the host |
| **Text / code** | Soft wrap on by default when the width is compact (off on desktop); the line-number gutter shrinks to the digits it needs |
| **Markdown** | Already wraps; the reading width is capped at ~72 characters on wide windows and centred |
| **Table** | Freeze the first column; scroll horizontally; tapping a row opens a **record view** (one row as a field / value list), the usual phone answer to wide tables |
| **Tree (JSON, TOML…)** | Indentation is 12 px on compact (20 px on desktop); deep paths collapse into a breadcrumb at the top of the view |
| **Hex** | The bytes per row follow the width: 8 on compact, 16 on medium, 16 or 32 on expanded. The ASCII column stays |
| **Container list** | One column (name plus size underneath) on compact; name, size and date columns on medium and up |

## 5. Text, scale and the OS

- **All layout is in logical points.** The host already divides by the scale factor;
  phones (3×), Retina (2×) and 1× monitors need no special cases.
- **The system text size** (Dynamic Type on iOS, font scale on Android, the
  accessibility text size on desktops) multiplies the theme's font sizes, clamped to
  0.85–2.0. Layout must survive 2.0 on a compact phone: labels wrap or truncate, and
  nothing overlaps.
- **Light and dark** follow the OS (already done on desktop), live.
- **Safe areas**: the top bar sits below the status bar and notch, the sheet clears
  the home indicator, and in landscape the content clears the cutout side. The content
  itself may run under the rounded corners; the chrome may not.

## 6. Windows and multitasking

This follows PLAN.md §12.3 (one instance, many windows):
- **iPadOS split view / slide-over and Android freeform:** each window has its own width
  class, so one document can be compact in slide-over and expanded beside it.
- **iPhone:** one window, and documents stack. The compact top bar's Back reaches the
  previous document once the navigation stack is empty.
- **Desktop:** the minimum window size drops from 360×240 to **320×240**, so a desktop
  window can show the compact layout.

## 7. What libgui needs

| Need | libgui today | Work |
|---|---|---|
| Width classes with hysteresis | `screen_size` each frame | App-side helper (~30 lines); no libgui change |
| Bottom sheet with drag detents | `modal`, springs (`spring.rs`), touch input | **New widget** in libgui; the main piece of work |
| Overlay drawer | `modal`, overlays | Small widget, or a sheet anchored to the right |
| Overflow ("⋯") menu | combo / popups | Small: a menu of commands |
| Touch and pointer density presets | `Theme` sizes, `touch_slop`, `PointerKind` | Two presets plus a switch on input kind |
| Pinch, long-press, double-tap | raw `Touch` events | Gesture recogniser (in libgui, so every view shares it) |
| Safe-area insets | none | A `FrameInfo` field the host fills (`winit` on iOS/Android) |
| System text size | none | Theme font scale, set by the host |

## 8. Testing it without a phone

- `--screenshot` gains `--size WxH` and `--touch`, and the headless CPU renderer draws
  the compact, medium and expanded layouts on CI. These are golden images per class
  for each sample file, alongside today's headless test.
- On a desktop, drag the window narrow: the same code paths run.
- Device runs (iOS simulator, Android emulator) arrive with the mobile app shells
  (PLAN.md §5); by then the layout is already tested.

## 9. Order of work

1. **Width classes and a narrow desktop window.** Compact toolbar with the "⋯" menu,
   the info panel as a drawer on medium, the status bar hidden on compact. Verified by
   resizing a desktop window, plus `--screenshot --size` goldens.
2. **Touch density and gestures** in libgui: presets, pinch, double-tap, long-press,
   kinetic scroll. Testable with a touchscreen or trackpad on desktop.
3. **The bottom sheet** (libgui widget) for the compact info panel.
4. **Per-view compact behaviour** (§4): table record view, hex width, text soft wrap.
5. **The mobile hosts** (iOS, Android): safe areas, system text size, the first device
   runs. Only this step needs a phone.
