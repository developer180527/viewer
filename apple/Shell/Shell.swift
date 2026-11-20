// The shell's state: a ViewerSession, woken from Rust's loader threads, and the
// security-scoped URLs it reads through.

import AppKit
import SwiftUI

/// Rust calls this from a loader thread; the UI takes the result on the main thread.
final class Wake: Waker, @unchecked Sendable {
    weak var shell: Shell?
    func wake() {
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.shell?.pump() } }
    }
}

@MainActor
final class Shell: ObservableObject {
    let session: ViewerSession
    /// Grants viewer-core reads through, by its number: released when it says so.
    private var grants: [UInt64: URL] = [:]

    @Published private(set) var crumbs: [String] = []
    @Published private(set) var status: StatusLine = .empty
    @Published private(set) var kind: ContentKind = .none
    @Published private(set) var choices = ViewChoices(names: [], current: 0)
    @Published private(set) var notices: [NoticeItem] = []
    @Published private(set) var info: [InfoItem] = []
    @Published private(set) var error: String?
    @Published private(set) var canGoBack = false
    /// The page on screen; views key their caches by it.
    @Published private(set) var page: UInt64?
    /// Why the Metal view can't draw (no GPU), once it has tried.
    @Published private(set) var canvasError: String?
    /// The Metal view's: a wake may mean a decoded texture or a compiled shader.
    var onWake: (() -> Void)?

    init() {
        let wake = Wake()
        session = ViewerSession(waker: wake)
        wake.shell = self
    }

    // MARK: commands

    func openPanel() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        if panel.runModal() == .OK, let url = panel.url { open(url) }
    }

    /// Opens what the user picked or dropped; with `reopen`, the file inside it.
    func open(_ url: URL, reopen: String? = nil) {
        let scoped = url.startAccessingSecurityScopedResource()
        let id = session.openGranted(picked: url.path, reopen: reopen)
        if scoped { grants[id] = url }
        refresh()
    }

    /// A blocked file's fix: pick the folder it needs, opening the panel there.
    func allow(reopen: String, folder: String?) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.prompt = "Allow"
        panel.message = "Allow Viewer to read this folder, so the file can read the files beside it."
        if let folder { panel.directoryURL = URL(fileURLWithPath: folder) }
        if panel.runModal() == .OK, let url = panel.url { open(url, reopen: reopen) }
    }

    func openEntry(_ index: UInt64) { session.openEntry(index: index); refresh() }
    func back() { _ = session.back(); refresh() }
    func choose(_ index: Int) { session.chooseView(index: UInt32(index)); refresh() }

    // MARK: updates

    /// Woken: take in what finished.
    func pump() {
        while session.poll() {}
        refresh()
        onWake?()
    }

    func canvasFailed(_ message: String) { canvasError = message }

    func refresh() {
        crumbs = session.breadcrumbs()
        status = session.status()
        kind = session.contentKind()
        choices = session.viewChoices()
        notices = session.notices()
        info = session.info()
        error = session.error()
        canGoBack = session.canGoBack()
        page = session.pageId()
        for id in session.takeReleased() {
            grants.removeValue(forKey: id)?.stopAccessingSecurityScopedResource()
        }
    }

    var title: String { crumbs.isEmpty ? "Viewer" : crumbs.joined(separator: " › ") }

    var statusText: String {
        switch status {
        case .empty: return "Open a file or folder"
        case let .loading(name): return "Opening \(name)…"
        case let .shown(format, size, model, ms):
            var parts = [format]
            if let size { parts.append(humanBytes(bytes: size)) }
            parts.append(model)
            parts.append(String(format: "%.0f ms", ms))
            return parts.joined(separator: " · ")
        }
    }
}
