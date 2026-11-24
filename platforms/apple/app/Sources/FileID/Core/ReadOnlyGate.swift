import SwiftUI
import FileIDShared
import FileIDModelInfo

/// Which cataloged paths sit under a location the engine reports read-only. A plain prefix match, so a view can ask per
/// file without touching the disk; the engine stays the authority and refuses the write itself.
struct ReadOnlyGate: Equatable, Sendable {
    struct Entry: Equatable, Sendable {
        let path: String
        let name: String
    }

    let entries: [Entry]

    static let none = ReadOnlyGate(entries: [])

    init(entries: [Entry]) { self.entries = entries }

    init(_ roots: [LibraryRoot]) {
        var entries = roots.filter { $0.readOnly == true }.map {
            Entry(path: Self.trimmed($0.path), name: $0.display)
        }
        for belt in ReadOnlyLocations.roots where !entries.contains(where: { $0.path == belt.path }) {
            entries.append(Entry(path: belt.path, name: belt.lastPathComponent))
        }
        self.init(entries: entries.sorted { $0.path.count > $1.path.count })
    }

    func drive(for path: String) -> String? {
        entries.first { path == $0.path || path.hasPrefix($0.path + "/") }?.name
    }

    func drive(for url: URL) -> String? { drive(for: url.path) }

    /// The reason sentence when there is at least one target and every one is read-only; nil when any can be changed.
    func reasonIfAll(_ paths: [String]) -> String? {
        guard !paths.isEmpty else { return nil }
        var name: String?
        for path in paths {
            guard let drive = drive(for: path) else { return nil }
            name = name ?? drive
        }
        return name.map(ReadOnlyLocations.sentence(forDrive:))
    }

    func reasonIfAll(_ urls: [URL]) -> String? { reasonIfAll(urls.map(\.path)) }

    /// How many of these paths are read-only.
    func count(_ paths: [String]) -> Int { paths.reduce(0) { $0 + (drive(for: $1) == nil ? 0 : 1) } }

    private static func trimmed(_ path: String) -> String {
        var path = path
        while path.count > 1 && path.hasSuffix("/") { path.removeLast() }
        return path
    }
}

extension View {
    /// Disables (never hides) a control whose targets are all read-only, with the reason as help and VoiceOver hint.
    func readOnlyGated(_ reason: String?) -> some View {
        modifier(ReadOnlyGatedModifier(reason: reason))
    }
}

private struct ReadOnlyGatedModifier: ViewModifier {
    let reason: String?
    func body(content: Content) -> some View {
        if let reason {
            content.disabled(true).help(reason).accessibilityHint(reason)
        } else {
            content
        }
    }
}

/// The quiet "Read-only" badge shown beside a read-only location.
struct ReadOnlyBadge: View {
    var body: some View {
        StatusPill(text: "READ-ONLY", color: Theme.text3)
            .help("FileID can show and search these files but can’t change them.")
            .accessibilityLabel("Read-only")
    }
}

extension EngineClient {
    /// Set when every location in the library is read-only, so no control that changes files could do anything.
    var libraryReadOnlyReason: String? {
        let roots = libraryRoots
        let gate = readOnlyGate
        guard let first = roots.first, roots.allSatisfy({ gate.drive(for: $0.path) != nil }) else { return nil }
        return ReadOnlyLocations.sentence(forDrive: gate.drive(for: first.path) ?? first.display)
    }
}
