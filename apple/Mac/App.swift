// Viewer's SwiftUI shell for macOS: native chrome (toolbar, view-as, info inspector,
// notices, status bar) around viewer-core, and native views for text, tables and
// containers. Pages, images, 3D and formatted text are drawn by libviewer, as in the
// libgui app, in a Metal view (Canvas.swift).

import SwiftUI
import UniformTypeIdentifiers

@main
struct ViewerApp: App {
    @StateObject private var shell = Shell()

    var body: some Scene {
        WindowGroup {
            Root().environmentObject(shell)
                .preferredColorScheme(shell.colorScheme)
        }
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("Open…") { shell.openPanel() }.keyboardShortcut("o")
                Menu("Open Recent") {
                    ForEach(shell.recents, id: \.path) { item in
                        Button(item.name) { shell.openRecent(item) }
                    }
                    Divider()
                    Button("Clear Menu") { shell.clearRecents() }.disabled(shell.recents.isEmpty)
                }
            }
            CommandGroup(after: .toolbar) {
                Button("Back") { shell.back() }.keyboardShortcut("[").disabled(!shell.canGoBack)
            }
        }
        Settings {
            SettingsView().environmentObject(shell)
                .preferredColorScheme(shell.colorScheme)
        }
    }
}

/// The window: SwiftUI's interface, or libgui's (the Interface setting), both on the
/// same session, so switching keeps what's open and where.
struct Root: View {
    @EnvironmentObject var shell: Shell
    var body: some View {
        if shell.settings.interface == .libgui {
            DrawnView(whole: true)
                .frame(minWidth: 640, minHeight: 420)
                .navigationTitle(shell.title)
                .onOpenURL { url in if url.isFileURL { shell.open(url) } }
        } else {
            NativeRoot()
        }
    }
}

struct NativeRoot: View {
    @EnvironmentObject var shell: Shell
    @State private var showInfo = false

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(shell.notices.enumerated()), id: \.offset) { _, n in NoticeBar(notice: n) }
            if let error = shell.error { Banner(text: error, tint: .red) }
            ContentArea().frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            HStack {
                Text(shell.statusText).font(.callout).foregroundStyle(.secondary).lineLimit(1)
                Spacer()
            }
            .padding(.horizontal, 10).padding(.vertical, 4)
        }
        .frame(minWidth: 640, minHeight: 420)
        .navigationTitle(shell.title)
        .toolbar {
            ToolbarItem(placement: .navigation) {
                Button { shell.back() } label: { Label("Back", systemImage: "chevron.left") }
                    .disabled(!shell.canGoBack)
            }
            ToolbarItem {
                if shell.choices.names.count > 1 {
                    Picker("View as", selection: Binding(get: { Int(shell.choices.current) }, set: { shell.choose($0) })) {
                        ForEach(Array(shell.choices.names.enumerated()), id: \.offset) { i, name in Text(name).tag(i) }
                    }
                    .help("View as")
                }
            }
            ToolbarItem {
                Button { shell.openPanel() } label: { Label("Open", systemImage: "folder") }
            }
            ToolbarItem {
                Button { showInfo.toggle() } label: { Label("Info", systemImage: "info.circle") }
                    .disabled(shell.info.isEmpty)
            }
        }
        // Files opened from Finder, the Dock or `open -a`: the system grants each.
        .onOpenURL { url in if url.isFileURL { shell.open(url) } }
        // Where the reader is in the file on screen, for the next time it's opened.
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in shell.save() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didResignActiveNotification)) { _ in shell.save() }
        .inspector(isPresented: $showInfo) { InfoPanel().inspectorColumnWidth(min: 220, ideal: 280) }
        .onDrop(of: [.fileURL], isTargeted: nil) { providers in
            guard let p = providers.first else { return false }
            _ = p.loadObject(ofClass: URL.self) { url, _ in
                if let url { DispatchQueue.main.async { MainActor.assumeIsolated { shell.open(url) } } }
            }
            return true
        }
    }
}

struct Banner: View {
    let text: String
    let tint: Color
    var body: some View {
        HStack { Text(text).lineLimit(2); Spacer() }
            .padding(8).background(tint.opacity(0.12))
    }
}

struct NoticeBar: View {
    @EnvironmentObject var shell: Shell
    let notice: NoticeItem
    var body: some View {
        switch notice {
        case let .fallback(message):
            Banner(text: message, tint: .orange)
        case let .blocked(message, reopen, folder):
            HStack {
                Text(message).lineLimit(2)
                Spacer()
                Button("Allow Folder…") { shell.allow(reopen: reopen, folder: folder) }
            }
            .padding(8).background(Color.yellow.opacity(0.15))
        }
    }
}

struct InfoPanel: View {
    @EnvironmentObject var shell: Shell
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(shell.info.enumerated()), id: \.offset) { _, item in
                    switch item {
                    case let .row(label, value):
                        HStack(alignment: .firstTextBaseline) {
                            Text(label).foregroundStyle(.secondary).frame(width: 90, alignment: .trailing)
                            Text(value).textSelection(.enabled)
                        }
                    case let .section(title, text):
                        Text(title).font(.headline).padding(.top, 8)
                        Text(text).font(.callout.monospaced()).textSelection(.enabled)
                    }
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
