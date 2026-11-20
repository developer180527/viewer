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
                // Opened from Files, Share or another app: the system grants it.
                .onOpenURL { url in if url.isFileURL { shell.open(url) } }
        }
        .onChange(of: phase) { _, now in
            // Where the reader is, for next time: the app may not be woken again.
            if now != .active { shell.save() }
        }
    }
}

struct Root: View {
    @EnvironmentObject var shell: Shell
    @State private var showInfo = false

    var body: some View {
        NavigationStack {
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
            .navigationTitle(shell.crumbs.last ?? "Viewer")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    if shell.canGoBack {
                        Button { shell.back() } label: { Label("Back", systemImage: "chevron.left") }
                            .keyboardShortcut("[")
                    }
                }
                ToolbarItemGroup(placement: .topBarTrailing) {
                    if shell.choices.names.count > 1 {
                        Menu {
                            Picker("View as", selection: Binding(get: { Int(shell.choices.current) }, set: { shell.choose($0) })) {
                                ForEach(Array(shell.choices.names.enumerated()), id: \.offset) { i, name in Text(name).tag(i) }
                            }
                        } label: { Label("View as", systemImage: "eye") }
                    }
                    Button { showInfo = true } label: { Label("Info", systemImage: "info.circle") }
                        .disabled(shell.info.isEmpty)
                    Menu {
                        Button { shell.openPanel() } label: { Label("Open…", systemImage: "folder") }
                        if !shell.recents.isEmpty {
                            Section("Recent") {
                                ForEach(shell.recents, id: \.path) { item in
                                    Button(item.name) { shell.openRecent(item) }
                                }
                            }
                        }
                    } label: { Label("Open", systemImage: "folder") } primaryAction: { shell.openPanel() }
                        .keyboardShortcut("o")
                }
            }
            .sheet(isPresented: $showInfo) {
                NavigationStack {
                    InfoPanel()
                        .navigationTitle("Info").navigationBarTitleDisplayMode(.inline)
                        .toolbar { Button("Done") { showInfo = false } }
                }
                .presentationDetents([.medium, .large])
            }
        }
        .fileImporter(isPresented: $shell.picking, allowedContentTypes: shell.allowing == nil ? [.item, .folder] : [.folder]) { result in
            if case let .success(url) = result { shell.picked(url) }
        }
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
