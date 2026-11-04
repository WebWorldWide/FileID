// Settings → Locations: every folder or drive the catalog has scanned, with whether it is
// reachable right now. An unplugged drive reads OFFLINE and its catalog rows are kept.
import SwiftUI
import FileIDShared

struct LocationsCard: View {
    let engine: EngineClient

    private var roots: [LibraryRoot] { engine.libraryRoots }

    var body: some View {
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("Locations").font(Theme.Font.body(14, .semibold))
                    Spacer()
                    Button("Refresh") { engine.refreshLibraryRoots() }
                        .buttonStyle(DSSecondaryButtonStyle())
                        .accessibilityLabel("Refresh locations")
                }
                if roots.isEmpty {
                    MetaText("No locations yet. Scan a folder or drive and it appears here.")
                } else {
                    ForEach(roots, id: \.id) { root in
                        LocationRow(root: root, engine: engine)
                        if root.id != roots.last?.id { Divider().opacity(0.3) }
                    }
                    MetaText("Offline locations keep their tags, faces and descriptions. They come back when the drive does.")
                }
            }
        }
        .onAppear { engine.refreshLibraryRoots() }
    }
}

/// Text for a location row. A plain type, not part of the view, so it carries no main-actor isolation.
enum LocationText {
    static func summary(_ root: LibraryRoot, now: Date = Date()) -> String {
        let count = root.fileCount.formatted()
        let size = ByteCountFormatter.string(fromByteCount: root.totalBytes, countStyle: .file)
        let scanned = root.lastScanAt.map { relative(Date(timeIntervalSince1970: $0), now: now) } ?? "not scanned yet"
        return "\(count) \(root.fileCount == 1 ? "file" : "files") · \(size) · \(scanned)"
    }

    static func relative(_ date: Date, now: Date) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return "scanned " + formatter.localizedString(for: date, relativeTo: now)
    }
}

struct LocationRow: View {
    let root: LibraryRoot
    let engine: EngineClient

    private var symbol: String {
        switch root.kind {
        case "network": "server.rack"
        case "external": "externaldrive"
        default: "folder"
        }
    }

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: symbol).font(.system(size: 16)).foregroundStyle(Theme.text3).frame(width: 22)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    Text(root.display).font(Theme.Font.body(13.5, .medium)).foregroundStyle(Theme.text1)
                    if !root.online { StatusPill(text: "OFFLINE", color: Theme.warning) }
                }
                MetaText(LocationText.summary(root))
                MetaText(PathText.abbreviated(root.path)).lineLimit(1).truncationMode(.middle).help(root.path)
            }
            Spacer()
            Button("Rescan") { engine.startScan(rootURL: URL(fileURLWithPath: root.path)) }
                .buttonStyle(DSSecondaryButtonStyle())
                .disabled(!root.online)
                .help(root.online ? "Scan this location again" : "Reconnect the drive to rescan")
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(root.display), \(root.online ? "online" : "offline"), \(LocationText.summary(root))")
    }
}
