// Native views of what viewer-core holds, asked for by range: only the rows on screen
// cross from Rust, a chunk at a time. What libviewer draws goes to the Metal view
// (Canvas.swift).

import SwiftUI

/// Rows fetched from the session in chunks, for one page. Not observed: it fills itself
/// as the list asks, from calls that return at once.
final class Chunks<Row> {
    private let chunk: UInt64 = 256
    private var cache: [UInt64: [Row]] = [:]
    private let fetch: (UInt64, UInt64) -> [Row]
    init(fetch: @escaping (UInt64, UInt64) -> [Row]) { self.fetch = fetch }

    func row(_ i: UInt64) -> Row? {
        let start = i / chunk * chunk
        if cache[start] == nil {
            if cache.count > 64 { cache.removeAll() }
            cache[start] = fetch(start, chunk)
        }
        let rows = cache[start]!
        let at = Int(i - start)
        return at < rows.count ? rows[at] : nil
    }
}

struct ContentArea: View {
    @EnvironmentObject var shell: Shell
    var body: some View {
        switch shell.kind {
        case .none:
            VStack(spacing: 10) {
                Image(systemName: "doc.viewfinder").font(.system(size: 44)).foregroundStyle(.tertiary)
                Text("Open a file or folder, or drop one here").foregroundStyle(.secondary)
                Button("Open…") { shell.openPanel() }
            }
        case .text: TextLines(session: shell.session).id(shell.page)
        case .table: TableRows(session: shell.session).id(shell.page)
        case .container: Entries(session: shell.session).id(shell.page)
        case .drawn:
            if let error = shell.canvasError {
                Text("Can't draw this here: \(error)").foregroundStyle(.secondary)
            } else {
                // libviewer's renderers, as in the libgui app. Keyed by nothing: one
                // canvas for every page, which keeps each page's view state itself.
                DrawnView()
            }
        }
    }
}

struct TextLines: View {
    let count: UInt64
    let lines: Chunks<String>
    init(session: ViewerSession) {
        count = session.textLineCount()
        lines = Chunks { session.textLines(start: $0, count: $1) }
    }
    var body: some View {
        List(0..<Int(count), id: \.self) { i in
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text("\(i + 1)").foregroundStyle(.tertiary).frame(minWidth: 44, alignment: .trailing)
                Text(lines.row(UInt64(i)) ?? "").textSelection(.enabled)
            }
            .font(.system(.body, design: .monospaced))
            .listRowSeparator(.hidden)
        }
        .listStyle(.plain)
        .environment(\.defaultMinListRowHeight, 16)
    }
}

struct TableRows: View {
    let columns: [String]
    let count: UInt64
    let rows: Chunks<[String]>
    private let width: CGFloat = 160
    init(session: ViewerSession) {
        columns = session.tableColumns()
        count = session.tableRowCount()
        rows = Chunks { session.tableRows(start: $0, count: $1) }
    }
    var body: some View {
        ScrollView([.horizontal, .vertical]) {
            LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                Section {
                    ForEach(0..<Int(count), id: \.self) { i in
                        let row = rows.row(UInt64(i)) ?? []
                        HStack(spacing: 0) {
                            ForEach(columns.indices, id: \.self) { c in
                                Text(c < row.count ? row[c] : "").lineLimit(1)
                                    .frame(width: width, alignment: .leading).padding(.horizontal, 6)
                            }
                        }
                        .padding(.vertical, 3)
                        .background(i % 2 == 1 ? Color.primary.opacity(0.04) : .clear)
                    }
                } header: {
                    HStack(spacing: 0) {
                        ForEach(columns, id: \.self) { name in
                            Text(name).bold().lineLimit(1).frame(width: width, alignment: .leading).padding(.horizontal, 6)
                        }
                    }
                    .padding(.vertical, 4)
                    .background(.bar)
                }
            }
        }
    }
}

struct Entries: View {
    @EnvironmentObject var shell: Shell
    let count: UInt64
    let entries: Chunks<EntryItem>
    @State private var selection: Int?
    init(session: ViewerSession) {
        count = session.entryCount()
        entries = Chunks { session.entries(start: $0, count: $1) }
    }
    var body: some View {
        List(0..<Int(count), id: \.self, selection: $selection) { i in
            if let e = entries.row(UInt64(i)) {
                HStack {
                    Image(systemName: e.folder ? "folder" : "doc").foregroundStyle(.secondary)
                    Text(e.path)
                    Spacer()
                    if !e.folder { Text(humanBytes(bytes: e.size)).foregroundStyle(.secondary).monospacedDigit() }
                }
            }
        }
        .contextMenu(forSelectionType: Int.self) { _ in } primaryAction: { picked in
            if let i = picked.first { shell.openEntry(UInt64(i)) }
        }
    }
}
