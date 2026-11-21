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
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var width
    /// The iPad's sidebar lists them already.
    private var recentsHere: Bool { width != .regular }
    #else
    private let recentsHere = true
    #endif
    var body: some View {
        switch shell.kind {
        case .none:
            VStack(spacing: 10) {
                Image(systemName: "doc.viewfinder").font(.system(size: 44)).foregroundStyle(.tertiary)
                Text("Open a file or folder, or drop one here").foregroundStyle(.secondary)
                Button("Open…") { shell.openPanel() }
                if recentsHere, !shell.recents.isEmpty {
                    RecentList().frame(maxWidth: 420, maxHeight: 260).padding(.top, 12)
                }
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

/// A table's cells fetched a block at a time (128 rows by 16 columns), for one page:
/// a sheet 126 columns wide costs what's on screen, not every cell of each row.
final class CellBlocks {
    private let rows: UInt64 = 128
    private let columns: UInt64 = 16
    private var cache: [Pair: [[String]]] = [:]
    private let fetch: (UInt64, UInt64, UInt64, UInt64) -> [[String]]
    private struct Pair: Hashable { let row: UInt64, column: UInt64 }
    init(fetch: @escaping (UInt64, UInt64, UInt64, UInt64) -> [[String]]) { self.fetch = fetch }

    func cell(_ row: UInt64, _ column: Int) -> String {
        let key = Pair(row: row / rows * rows, column: UInt64(column) / columns * columns)
        if cache[key] == nil {
            if cache.count > 64 { cache.removeAll() }
            cache[key] = fetch(key.row, rows, key.column, columns)
        }
        let block = cache[key]!
        let r = Int(row - key.row), c = column - Int(key.column)
        return r < block.count && c < block[r].count ? block[r][c] : ""
    }
}

struct TableRows: View {
    let columns: [String]
    let count: UInt64
    let cells: CellBlocks
    private let width: CGFloat = 160
    private var pitch: CGFloat { width + 12 }
    /// The leftmost column in view and how many fit across: only those are built, and one
    /// either side. A 126-column sheet built every cell of every row that scrolled in.
    /// Both whole numbers, so a scroll changes them only when a column edge passes.
    @State private var left = 0
    @State private var across = 0
    private var shown: Range<Int> {
        let first = min(max(0, left - 1), columns.count)
        return first..<min(columns.count, max(first, left + across + 1))
    }
    init(session: ViewerSession) {
        columns = session.tableColumns()
        count = session.tableRowCount()
        cells = CellBlocks { session.tableCells(start: $0, count: $1, column: $2, columns: $3) }
    }
    var body: some View {
        let shown = shown
        GeometryReader { outer in
            ScrollView([.horizontal, .vertical]) {
                LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                    Section {
                        ForEach(0..<Int(count), id: \.self) { i in
                            line(shown) { c in cells.cell(UInt64(i), c) }
                            .padding(.vertical, 3)
                            .background(i % 2 == 1 ? Color.primary.opacity(0.04) : .clear)
                        }
                    } header: {
                        line(shown, bold: true) { c in columns[c] }
                            .padding(.vertical, 4)
                            .background(.bar)
                    }
                }
                .background(GeometryReader { g in
                    Color.clear.onChange(of: g.frame(in: .named("table")).minX, initial: true) { _, x in
                        let l = max(0, Int(-x / pitch))
                        if l != left { left = l }
                    }
                })
            }
            .coordinateSpace(name: "table")
            .onChange(of: outer.size.width, initial: true) { _, w in
                let n = Int((w / pitch).rounded(.up)) + 1
                if n != across { across = n }
            }
        }
    }

    /// One row's cells for `range`, at their place in the full width.
    private func line(_ range: Range<Int>, bold: Bool = false, _ text: @escaping (Int) -> String) -> some View {
        HStack(spacing: 0) {
            ForEach(range, id: \.self) { c in
                Text(text(c)).bold(bold).lineLimit(1).frame(width: width, alignment: .leading).padding(.horizontal, 6)
            }
        }
        .padding(.leading, CGFloat(range.lowerBound) * pitch)
        .frame(width: CGFloat(columns.count) * pitch, alignment: .leading)
    }
}

/// A container's entries (a zip's files, a folder's children), a row each, built as
/// they scroll into view: a lazy stack, not a `List(selection:)`, which rebuilt all
/// 5000 rows of a big archive each time Back returned to it (0.7 s on the Mac).
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
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(0..<Int(count), id: \.self) { i in
                    if let e = entries.row(UInt64(i)) { row(e, i) }
                }
            }
        }
    }

    private func row(_ e: EntryItem, _ i: Int) -> some View {
        HStack {
            Image(systemName: e.folder ? "folder" : "doc").foregroundStyle(.secondary)
            Text(e.path).lineLimit(1)
            Spacer()
            if !e.folder { Text(humanBytes(bytes: e.size)).foregroundStyle(.secondary).monospacedDigit() }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .background(selection == i ? Color.accentColor.opacity(0.18) : i % 2 == 1 ? Color.primary.opacity(0.03) : .clear)
        .contentShape(Rectangle())
        // The Mac selects with a click and opens with a double click; a touch screen
        // opens with a tap.
        #if os(macOS)
        .onTapGesture(count: 2) { shell.openEntry(UInt64(i)) }
        .onTapGesture { selection = i }
        #else
        .onTapGesture { shell.openEntry(UInt64(i)) }
        #endif
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityAction { shell.openEntry(UInt64(i)) }
    }
}

/// Recently opened files, on the empty screen.
struct RecentList: View {
    @EnvironmentObject var shell: Shell
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Recent").font(.headline).foregroundStyle(.secondary)
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(shell.recents, id: \.path) { item in
                        Button { shell.openRecent(item) } label: {
                            HStack {
                                Image(systemName: "doc").foregroundStyle(.secondary)
                                VStack(alignment: .leading, spacing: 0) {
                                    Text(item.name).lineLimit(1)
                                    Text((item.path as NSString).deletingLastPathComponent).font(.caption).foregroundStyle(.tertiary).lineLimit(1).truncationMode(.middle)
                                }
                                Spacer()
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .padding(.vertical, 3)
                    }
                }
            }
        }
    }
}
