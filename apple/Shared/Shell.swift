// The shell's state: a ViewerSession, woken from Rust's loader threads, and the
// security-scoped URLs it reads through.

import SwiftUI
#if os(macOS)
import AppKit
#endif

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
    /// Recently opened files, newest first (viewer-core's list).
    @Published private(set) var recents: [RecentItem] = []
    /// The Metal view's: a wake may mean a decoded texture or a compiled shader.
    var onWake: (() -> Void)?

    init() {
        let wake = Wake()
        session = ViewerSession(waker: wake)
        wake.shell = self
        if let dir = try? FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true) {
            session.useRecentsFile(file: dir.appendingPathComponent("recents.txt").path)
        }
        recents = session.recents()
        // For testing: `-open <path>` at launch (simctl launch, Xcode's scheme).
        let args = ProcessInfo.processInfo.arguments
        if let i = args.firstIndex(of: "-open"), i + 1 < args.count {
            open(URL(fileURLWithPath: args[i + 1]))
        }
    }

    // MARK: commands

    #if os(macOS)
    func openPanel() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        if panel.runModal() == .OK, let url = panel.url { open(url) }
    }

    #else
    /// The document picker is up: for a file or folder, or (with `allowing`) for the
    /// folder a blocked file needs.
    @Published var picking = false
    @Published private(set) var allowing: String?

    func openPanel() { allowing = nil; picking = true }

    /// A blocked file's fix: pick the folder it needs.
    func allow(reopen: String, folder: String?) { allowing = reopen; picking = true }

    /// What the picker gave.
    func picked(_ url: URL) {
        let reopen = allowing
        allowing = nil
        open(url, reopen: reopen)
    }
    #endif

    /// Opens what the user picked or dropped; with `reopen`, the file inside it.
    func open(_ url: URL, reopen: String? = nil) {
        let scoped = url.startAccessingSecurityScopedResource()
        let id = session.openGranted(picked: url.path, reopen: reopen, bookmark: bookmark(url))
        if scoped { grants[id] = url }
        refresh()
    }

    /// The token for opening `url` again after the app quits.
    private func bookmark(_ url: URL) -> Data? {
        #if os(macOS)
        try? url.bookmarkData(options: [.withSecurityScope, .securityScopeAllowOnlyReadAccess], includingResourceValuesForKeys: nil, relativeTo: nil)
        #else
        try? url.bookmarkData(options: .minimalBookmark, includingResourceValuesForKeys: nil, relativeTo: nil)
        #endif
    }

    /// Opens a recent file through its bookmark. One that no longer resolves (moved
    /// to another disk, deleted) leaves the list.
    func openRecent(_ item: RecentItem) {
        var stale = false
        #if os(macOS)
        let options: URL.BookmarkResolutionOptions = [.withSecurityScope, .withoutUI]
        #else
        let options: URL.BookmarkResolutionOptions = [.withoutUI]
        #endif
        guard !item.bookmark.isEmpty,
              let url = try? URL(resolvingBookmarkData: item.bookmark, options: options, relativeTo: nil, bookmarkDataIsStale: &stale)
        else {
            session.forgetRecent(path: item.path)
            refresh()
            error = "\(item.name) can't be found any more."
            return
        }
        // A bookmark for the folder a model was allowed through opens the model inside.
        open(url, reopen: url.path == item.path ? nil : item.path)
    }

    /// The file on screen (or the one its pages were opened from): the newest recent,
    /// while something is open.
    func isOpen(_ item: RecentItem) -> Bool { page != nil && item.path == recents.first?.path }

    func forget(_ item: RecentItem) { session.forgetRecent(path: item.path); save(); refresh() }

    func clearRecents() { session.clearRecents(); save(); refresh() }

    /// Writes the recent list and the places in it.
    func save() {
        if let why = session.saveRecents() { NSLog("Viewer: recent files not saved: %@", why) }
    }

    #if os(macOS)
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
    #endif

    func openEntry(_ index: UInt64) { session.openEntry(index: index); refresh() }
    func back() { _ = session.back(); save(); refresh() }
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
        let was = page
        page = session.pageId()
        recents = session.recents()
        // Another file: keep where the reader was in the last one.
        if page != was { save() }
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
