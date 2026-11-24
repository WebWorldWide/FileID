import Foundation

public enum ReadOnlyLocations {
    /// The owner's photo drive stays protected by name even where the volume flag cannot be read (owner policy).
    public static let roots = [URL(fileURLWithPath: "/Volumes/Adlon", isDirectory: true)]

    public static func sentence(forDrive name: String) -> String {
        "\u{201C}\(name)\u{201D} is a read-only drive. FileID can show and search these files but can\u{2019}t change them."
    }

    /// Replaced in tests; flags a path as read-only without touching the cache (a test cannot make a real read-only mount).
    nonisolated(unsafe) public static var pathOverride: (@Sendable (URL) -> String?)?

    public final class VolumeCache: @unchecked Sendable {
        let lock = NSLock()
        var entries: [String: (at: Date, name: String?)] = [:]
        public init() {}
    }
    static let volumeCache = VolumeCache()
    public static let volumeTTL: TimeInterval = 3

    /// Forget cached volume state so a remount is noticed now (scan start, an explicit refresh).
    public static func refresh() {
        volumeCache.lock.lock(); defer { volumeCache.lock.unlock() }
        volumeCache.entries.removeAll()
    }

    private static func systemVolumeReadOnlyName(_ url: URL) -> String? {
        var existing = url
        while !FileManager.default.fileExists(atPath: existing.path) {
            let parent = existing.deletingLastPathComponent()
            if parent.path == existing.path { return nil }
            existing = parent
        }
        guard let values = try? existing.resourceValues(forKeys: [.volumeIsReadOnlyKey, .volumeNameKey, .volumeURLKey]),
              values.volumeIsReadOnly == true else { return nil }
        return values.volumeName ?? values.volume?.lastPathComponent ?? "this drive"
    }

    /// The name of the read-only drive `url` is on (volume flag, or the Adlon name guard), or nil when it can be changed.
    public static func readOnlyDriveName(for url: URL) -> String? {
        let literal = url.standardizedFileURL.path.lowercased()
        for root in roots {
            let protected = root.standardizedFileURL.path.lowercased()
            if literal == protected || literal.hasPrefix(protected + "/") { return root.lastPathComponent }
        }
        if let name = pathOverride?(url) { return name }
        return volumeName(for: url, cache: volumeCache, now: Date(), probe: systemVolumeReadOnlyName)
    }

    /// The cached volume lookup: one probe per mount per TTL, so a remount is noticed within seconds.
    public static func volumeName(for url: URL, cache: VolumeCache, now: Date, probe: (URL) -> String?) -> String? {
        let mount = mountPoint(of: url) ?? ""
        cache.lock.lock()
        if let hit = cache.entries[mount], now.timeIntervalSince(hit.at) < volumeTTL {
            cache.lock.unlock()
            return hit.name
        }
        cache.lock.unlock()
        let name = probe(url)
        cache.lock.lock(); cache.entries[mount] = (now, name); cache.lock.unlock()
        return name
    }

    private static func mountPoint(of url: URL) -> String? {
        var existing = url
        while !FileManager.default.fileExists(atPath: existing.path) {
            let parent = existing.deletingLastPathComponent()
            if parent.path == existing.path { return nil }
            existing = parent
        }
        return (try? existing.resourceValues(forKeys: [.volumeURLKey]))?.volume?.resolvingSymlinksInPath().path
    }

    /// True when every URL sits on a read-only drive (used to disable a control whose targets can't be changed).
    public static func allReadOnly(_ urls: [URL]) -> String? {
        guard !urls.isEmpty else { return nil }
        var name: String?
        for url in urls {
            guard let drive = readOnlyDriveName(for: url) else { return nil }
            name = name ?? drive
        }
        return name
    }

    /// The drive name and counts when any target is on a read-only drive; a batch with one such target is refused whole.
    public static func batchRefusal(_ urls: [URL]) -> BatchRefusal? {
        var first: String?
        var count = 0
        for url in urls {
            if let drive = readOnlyDriveName(for: url) { count += 1; first = first ?? drive }
        }
        guard let first else { return nil }
        return BatchRefusal(drive: first, readOnly: count, total: urls.count)
    }

    public struct BatchRefusal: Sendable, Equatable, LocalizedError {
        public let drive: String
        public let readOnly: Int
        public let total: Int
        public var errorDescription: String? {
            let base = ReadOnlyLocations.sentence(forDrive: drive)
            return total <= 1 ? base : "\(base) \(readOnly) of \(total) files are on it, so nothing was changed."
        }
    }

    public static func requireSourceMutation(_ url: URL) throws {
        try requireWritable(url)
        let components = resolved(url).pathComponents.map { $0.lowercased() }
        let bundles = [".fcpbundle", ".photoslibrary", ".photolibrary", ".aplibrary", ".imovielibrary", ".logicx", ".band"]
        let mediaFolders = Set(["finalcutoriginalmedia", "finalcutproxymedia"])
        if components.contains(where: { component in bundles.contains(where: { component.hasSuffix($0) }) || mediaFolders.contains(component.filter { !$0.isWhitespace }) }) {
            throw ManagedLibraryError()
        }
    }

    public struct ManagedLibraryError: LocalizedError, Sendable {
        public var errorDescription: String? { "This file belongs to a managed media library. Export a new version instead of modifying its original." }
    }

    public static func requireWritable(_ url: URL, protectedRoots: [URL] = roots) throws {
        let literal = url.standardizedFileURL.path.lowercased()
        for root in protectedRoots {
            let protected = root.standardizedFileURL.path.lowercased()
            let prefix = protected.hasSuffix("/") ? protected : protected + "/"
            if literal == protected || literal.hasPrefix(prefix) { throw ProtectionError(path: url.path, drive: root.lastPathComponent) }
        }
        if protectedRoots == roots, let drive = readOnlyDriveName(for: url) { throw ProtectionError(path: url.path, drive: drive) }
        try rejectBrokenAliases(url)
        let candidate = resolved(url)
        for root in protectedRoots {
            try rejectBrokenAliases(root)
            let protected = resolved(root).path
            let path = candidate.path
            if path.caseInsensitiveCompare(protected) == .orderedSame
                || path.lowercased().hasPrefix(protected.lowercased().hasSuffix("/") ? protected.lowercased() : protected.lowercased() + "/") {
                throw ProtectionError(path: url.path, drive: root.lastPathComponent)
            }
        }
    }

    private static func rejectBrokenAliases(_ url: URL) throws {
        var ancestor = url
        while !FileManager.default.fileExists(atPath: ancestor.path) {
            if (try? FileManager.default.destinationOfSymbolicLink(atPath: ancestor.path)) != nil {
                throw UnsafeAliasError()
            }
            let parent = ancestor.deletingLastPathComponent()
            if parent.path == ancestor.path { break }
            ancestor = parent
        }
    }

    public struct UnsafeAliasError: LocalizedError, Sendable {
        public var errorDescription: String? { "This location contains a symbolic link with a missing target. Select an existing folder instead." }
    }

    public static func resolved(_ url: URL) -> URL {
        var ancestor = url
        var suffix: [String] = []
        while !FileManager.default.fileExists(atPath: ancestor.path) {
            let parent = ancestor.deletingLastPathComponent()
            if parent.path == ancestor.path { break }
            suffix.append(ancestor.lastPathComponent)
            ancestor = parent
        }
        var result = ancestor.resolvingSymlinksInPath()
        for component in suffix.reversed() { result.appendPathComponent(component) }
        return result.standardizedFileURL
    }

    public struct ProtectionError: LocalizedError, Sendable {
        public let path: String
        public let drive: String
        public init(path: String, drive: String = "this drive") { self.path = path; self.drive = drive }
        public var errorDescription: String? { ReadOnlyLocations.sentence(forDrive: drive) }
    }
}
