// Viewer's SwiftUI shell for iPhone and iPad: the Mac shell's session and native lists
// (Shared/), in a navigation stack, with files from the document picker or from Files
// ("Open in Viewer"). Pages, images, 3D and formatted text are drawn by libviewer in a
// Metal view (Canvas.swift), as on the Mac.

import SwiftUI
import UniformTypeIdentifiers

@main
struct ViewerApp: App {
    @StateObject private var shell = Shell()
    @Environment(\.scenePhase) private var phase

    var body: some Scene {
        WindowGroup {
            Root().environmentObject(shell)
                .preferredColorScheme(shell.colorScheme)
                // Opened from Files, Share or another app: the system grants it.
                .onOpenURL { url in if url.isFileURL { shell.open(url) } }
        }
        .onChange(of: phase) { _, now in
            // Where the reader is, for next time: the app may not be woken again.
            if now != .active { shell.save() }
        }
    }
}

/// iPad (regular width): a sidebar of recent files beside the content, Info as an
/// inspector column. iPhone (compact): the content alone in a stack, Info as a sheet.
struct Root: View {
    @EnvironmentObject var shell: Shell
    @Environment(\.horizontalSizeClass) private var width
    @State private var showInfo = false
    @State private var showSettings = false
    /// The sidebar shows until the user hides it.
    @State private var columns = NavigationSplitViewVisibility.doubleColumn

    var body: some View {
        Group {
            if shell.settings.interface == .libgui {
                // libgui's whole interface, edge to edge; it keeps clear of the notch
                // and home indicator itself.
                DrawnView(whole: true).ignoresSafeArea()
            } else if width == .regular {
                NavigationSplitView(columnVisibility: $columns) {
                    Sidebar(showSettings: $showSettings)
                        .navigationTitle("Viewer")
                        .navigationSplitViewColumnWidth(min: 240, ideal: 300, max: 380)
                } detail: {
                    Detail(showInfo: $showInfo, showSettings: $showSettings, wide: true)
                }
                .navigationSplitViewStyle(.balanced)
            } else {
                NavigationStack { Detail(showInfo: $showInfo, showSettings: $showSettings, wide: false) }
            }
        }
        .sheet(isPresented: $showSettings) {
            NavigationStack {
                SettingsView()
                    .navigationTitle("Settings").navigationBarTitleDisplayMode(.inline)
                    .toolbar { Button("Done") { showSettings = false } }
            }
        }
        .fileImporter(isPresented: $shell.picking, allowedContentTypes: shell.allowing == nil ? [.item, .folder] : [.folder]) { result in
            if case let .success(url) = result { shell.picked(url) }
        }
    }
}

/// The iPad's sidebar: Open, and the recent files, the one on screen marked.
struct Sidebar: View {
    @EnvironmentObject var shell: Shell
    @Binding var showSettings: Bool
    var body: some View {
        List {
            Section {
                Button { shell.openPanel() } label: { Label("Open…", systemImage: "folder") }
                Button { showSettings = true } label: { Label("Settings", systemImage: "gearshape") }
            }
            if !shell.recents.isEmpty {
                Section("Recent") {
                    ForEach(shell.recents, id: \.path) { item in
                        Button { shell.openRecent(item) } label: {
                            Label {
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(item.name).lineLimit(1)
                                    Text((item.path as NSString).deletingLastPathComponent)
                                        .font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                                }
                            } icon: { Image(systemName: "doc") }
                        }
                        .listRowBackground(shell.isOpen(item) ? Color.accentColor.opacity(0.15) : nil)
                        .swipeActions { Button("Remove", role: .destructive) { shell.forget(item) } }
                    }
                }
            }
        }
        .listStyle(.sidebar)
        .tint(.primary)
    }
}

/// The page on screen, its notices and status, and the toolbar.
struct Detail: View {
    @EnvironmentObject var shell: Shell
    @Binding var showInfo: Bool
    @Binding var showSettings: Bool
    /// iPad width: view-as as a segmented control, the path in the title.
    let wide: Bool

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(shell.notices.enumerated()), id: \.offset) { _, n in NoticeBar(notice: n) }
            if let error = shell.error { Banner(text: error, tint: .red) }
            ContentArea().frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            HStack {
                Text(shell.statusText).font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                Spacer()
            }
            .padding(.horizontal, 12).padding(.vertical, 4)
        }
        .navigationTitle(wide ? shell.title : (shell.crumbs.last ?? "Viewer"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                if shell.canGoBack {
                    Button { shell.back() } label: { Label("Back", systemImage: "chevron.left") }
                        .keyboardShortcut("[")
                }
            }
            if wide, (2...4).contains(shell.choices.names.count) {
                ToolbarItem(placement: .principal) {
                    Picker("View as", selection: viewAs) {
                        ForEach(Array(shell.choices.names.enumerated()), id: \.offset) { i, name in Text(name).tag(i) }
                    }
                    .pickerStyle(.segmented)
                    .fixedSize()
                }
            }
            ToolbarItemGroup(placement: .topBarTrailing) {
                if shell.choices.names.count > (wide ? 4 : 1) {
                    Menu {
                        Picker("View as", selection: viewAs) {
                            ForEach(Array(shell.choices.names.enumerated()), id: \.offset) { i, name in Text(name).tag(i) }
                        }
                    } label: { Label("View as", systemImage: "eye") }
                }
                Button { showInfo.toggle() } label: { Label("Info", systemImage: "info.circle") }
                    .disabled(shell.info.isEmpty)
                    .keyboardShortcut("i")
                if !wide {
                    Menu {
                        Button { shell.openPanel() } label: { Label("Open…", systemImage: "folder") }
                        if !shell.recents.isEmpty {
                            Section("Recent") {
                                ForEach(shell.recents, id: \.path) { item in
                                    Button(item.name) { shell.openRecent(item) }
                                }
                            }
                        }
                        Divider()
                        Button { showSettings = true } label: { Label("Settings", systemImage: "gearshape") }
                    } label: { Label("Open", systemImage: "folder") } primaryAction: { shell.openPanel() }
                }
            }
        }
        .background { Button("") { shell.openPanel() }.keyboardShortcut("o").hidden() }
        // A column beside the content on iPad, a sheet on iPhone.
        .inspector(isPresented: $showInfo) {
            InfoPanel()
                .inspectorColumnWidth(min: 260, ideal: 320, max: 420)
                .presentationDetents([.medium, .large])
        }
    }

    private var viewAs: Binding<Int> {
        Binding(get: { Int(shell.choices.current) }, set: { shell.choose($0) })
    }
}

struct Banner: View {
    let text: String
    let tint: Color
    var body: some View {
        HStack { Text(text).font(.callout).lineLimit(3); Spacer() }
            .padding(10).background(tint.opacity(0.12))
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
                Text(message).font(.callout).lineLimit(3)
                Spacer()
                Button("Allow Folder…") { shell.allow(reopen: reopen, folder: folder) }
            }
            .padding(10).background(Color.yellow.opacity(0.15))
        }
    }
}

struct InfoPanel: View {
    @EnvironmentObject var shell: Shell
    var body: some View {
        List {
            ForEach(Array(shell.info.enumerated()), id: \.offset) { _, item in
                switch item {
                case let .row(label, value):
                    LabeledContent(label) { Text(value).textSelection(.enabled) }
                case let .section(title, text):
                    Section(title) { Text(text).font(.callout.monospaced()).textSelection(.enabled) }
                }
            }
        }
    }
}
