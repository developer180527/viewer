// viewer-ffi from Swift, with no UI: what the shell sees after each command, on the
// repository's samples. Run by apple/test.sh.

import Foundation
import QuartzCore

final class Wakes: Waker, @unchecked Sendable {
    private let lock = NSLock()
    private var n = 0
    func wake() { lock.lock(); n += 1; lock.unlock() }
    var count: Int { lock.lock(); defer { lock.unlock() }; return n }
}

var failures = 0
func check(_ ok: Bool, _ what: String, line: Int = #line) {
    if !ok { failures += 1; print("FAIL line \(line): \(what)") }
}

/// Polls until the open in flight finishes, as the shell does when woken.
func settle(_ s: ViewerSession) {
    let deadline = Date().addingTimeInterval(30)
    while !s.poll() {
        if Date() > deadline { print("FAIL: an open never finished"); exit(1) }
        usleep(1000)
    }
}

let root = CommandLine.arguments[1]
let wakes = Wakes()
let s = ViewerSession(waker: wakes)
check(s.status() == .empty, "nothing open")
check(s.contentKind() == .none, "no content")

// A table, by range.
s.openPath(path: "\(root)/samples/table.csv")
if case .loading = s.status() {} else { check(false, "loading while opening") }
settle(s)
check(wakes.count >= 1, "the loader woke Swift")
check(s.breadcrumbs() == ["table.csv"], "\(s.breadcrumbs())")
check(s.contentKind() == .table, "a CSV is a table: \(s.contentKind())")
let columns = s.tableColumns()
check(!columns.isEmpty, "columns")
let rows = s.tableRows(start: 0, count: 3)
check(!rows.isEmpty && rows[0].count == columns.count, "rows as wide as the columns: \(rows)")
check(s.tableRows(start: s.tableRowCount(), count: 5).isEmpty, "nothing past the end")
if case let .shown(format, size, _, _) = s.status() {
    check(!format.isEmpty && size != nil, "status: \(format) \(String(describing: size))")
} else { check(false, "shown") }
check(s.info().contains(.row(label: "Name", value: "table.csv")), "info names the file")

// View as its source: text, by range.
let choices = s.viewChoices()
check(choices.names.count > 1, "\(choices.names)")
if let source = choices.names.firstIndex(of: "Source") {
    s.chooseView(index: UInt32(source))
    settle(s)
    check(s.contentKind() == .text, "the source is text: \(s.contentKind())")
    check(s.textLineCount() > 1, "lines")
    let first = s.textLines(start: 0, count: 1)
    check(first.count == 1 && first[0].contains(columns[0]), "the header line: \(first)")
    check(s.viewChoices().current == UInt32(source), "the source is shown")
} else { check(false, "a Source choice") }

// Into a zip, and back.
s.openPath(path: "\(root)/samples/bundle.zip")
settle(s)
check(s.contentKind() == .container, "a zip lists entries")
let entries = s.entries(start: 0, count: s.entryCount())
if let json = entries.firstIndex(where: { $0.path.hasSuffix("data.json") }) {
    let zip = s.pageId()
    s.openEntry(index: UInt64(json))
    settle(s)
    check(s.breadcrumbs().count == 2 && s.canGoBack(), "\(s.breadcrumbs())")
    check(s.back() && s.pageId() == zip, "back to the zip")
} else { check(false, "data.json in the zip: \(entries)") }

// A fallback says why.
s.openPath(path: "\(root)/corpus/files/hostile/bomb.txt.gz")
settle(s)
if case let .fallback(message)? = s.notices().first {
    check(message.hasPrefix("Couldn't read this as "), message)
} else { check(false, "a fallback notice: \(s.notices())") }

// Grants: released once no page reads through them; a failed open's at once.
let g = s.openGranted(picked: "\(root)/samples", reopen: nil, bookmark: nil)
settle(s)
check(s.takeReleased().isEmpty, "the folder's page holds its grant")
s.openPath(path: "\(root)/samples/data.json")
settle(s)
check(s.takeReleased() == [g], "released with its page")
let bad = s.openGranted(picked: "\(root)/samples/missing.bin", reopen: nil, bookmark: nil)
settle(s)
check(s.error() != nil && s.takeReleased() == [bad], "a failed open's grant goes at once")
check(humanBytes(bytes: 1536) == "1.5 KiB", humanBytes(bytes: 1536))

// The Metal view's path from Swift: a ViewerCanvas on a CAMetalLayer (no window
// needed) draws each kind of drawn content, and takes input. The pixels themselves
// are checked by crates/surface/tests/canvas.rs, offscreen.
let layer = CAMetalLayer()
layer.drawableSize = CGSize(width: 1600, height: 1200)
let address = UInt64(UInt(bitPattern: Unmanaged.passUnretained(layer).toOpaque()))
do {
    let canvas = try ViewerCanvas(session: s, layer: address, width: 1600, height: 1200, scale: 2, dark: true)
    for file in ["samples/data.json", "samples/gradient.png", "samples/logo.svg", "samples/README.md",
                 "corpus/files/docs/two-pages.pdf", "corpus/files/3d/cube.glb", "corpus/files/3d/cube.stl"] {
        s.openPath(path: "\(root)/\(file)")
        settle(s)
        check(s.error() == nil, "\(file): \(s.error() ?? "")")
        check(s.contentKind() == .drawn, "\(file) is drawn: \(s.contentKind())")
        // Frames until the view asks for no more (images decode, shaders compile).
        var frames = 0
        var frame = canvas.draw()
        let deadline = Date().addingTimeInterval(20)
        while let t = frame.repaintAfter, t <= 0, Date() < deadline {
            usleep(4000)
            _ = s.poll()
            canvas.touch()
            frame = canvas.draw()
            frames += 1
        }
        check(frame.repaintAfter.map { $0 > 0 } ?? true, "\(file) settles (\(frames) frames)")
        print("  drew \(file) in \(frames + 1) frames")
    }
    canvas.resize(width: 800, height: 600, scale: 1)
    canvas.pointerMoved(x: 100, y: 100)
    canvas.wheel(dx: 0, dy: -40, precise: true)
    canvas.pinch(delta: 0.2)
    // Keys: AppKit's key codes and UIKit's HID usages (Page Down both).
    check(canvas.keyMac(code: 0x79, pressed: true, repeat: false), "Page Down from AppKit")
    _ = canvas.keyMac(code: 0x79, pressed: false, repeat: false)
    check(canvas.keyHid(usage: 0x4E, pressed: true, repeat: false), "Page Down from UIKit")
    _ = canvas.keyHid(usage: 0x4E, pressed: false, repeat: false)
    check(!canvas.keyMac(code: 0x7F, pressed: true, repeat: false), "an unknown key is left to AppKit")
    canvas.text(text: "a")
    _ = canvas.draw()
    // Back through the pages: each view's state is kept, and closed ones freed.
    while s.back() {}
    _ = canvas.draw()
} catch {
    check(false, "a canvas on the layer: \(error)")
}

// libgui's whole interface on the same session: it sees what's open, and what it
// opens the session sees.
do {
    let layer = CAMetalLayer()
    let address = UInt64(UInt(bitPattern: Unmanaged.passUnretained(layer).toOpaque()))
    let app = try ViewerCanvas.app(session: s, layer: address, width: 1200, height: 900, scale: 2, dark: false)
    s.openPath(path: "\(root)/samples/bundle.zip")
    settle(s)
    let first = app.draw()
    check(first.request == nil, "no picker asked for")
    let page = s.pageId()
    // Back and forth through the session from outside: the interface keeps up.
    s.openEntry(index: 0)
    settle(s)
    _ = app.draw()
    check(s.pageId() != page, "an entry opened under the libgui interface")
    _ = s.back()
    _ = app.draw()
    check(s.pageId() == page, "back to the zip")
    app.setSafeArea(top: 47, left: 0, bottom: 34, right: 0)
    _ = app.draw()
} catch {
    check(false, "libgui's interface on the layer: \(error)")
}

// Recent files, kept in a file and read back by the next session.
let recentsDir = NSTemporaryDirectory() + "viewer-data-\(getpid())"
do {
    let a = ViewerSession(waker: Wakes())
    a.useDataDir(dir: recentsDir)
    _ = a.openGranted(picked: "\(root)/samples/data.json", reopen: nil, bookmark: Data([1, 2, 3]))
    settle(a)
    a.openPath(path: "\(root)/samples/table.csv")
    settle(a)
    check(a.recents().map(\.name) == ["table.csv", "data.json"], "recents newest first: \(a.recents().map(\.name))")
    a.setSettings(settings: AppSettings(theme: .dark, reopenLast: false, interface: .libgui))
    check(a.save() == nil, "recents and settings saved")
    let b = ViewerSession(waker: Wakes())
    b.useDataDir(dir: recentsDir)
    check(b.recents() == a.recents(), "recents read back")
    check(b.settings() == AppSettings(theme: .dark, reopenLast: false, interface: .libgui), "settings read back")
    check(b.recents().last?.bookmark == Data([1, 2, 3]), "with the bookmark")
    b.forgetRecent(path: "\(root)/samples/table.csv")
    check(b.recents().count == 1, "one forgotten")
    try? FileManager.default.removeItem(atPath: recentsDir)
}

if failures > 0 { print("\(failures) failed"); exit(1) }
print("viewer-ffi from Swift: all passed")
