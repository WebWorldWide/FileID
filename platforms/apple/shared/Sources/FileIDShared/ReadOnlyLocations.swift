import Foundation

public enum ReadOnlyLocations {
    public static let roots = [URL(fileURLWithPath: "/Volumes/Adlon", isDirectory: true)]

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
            if literal == protected || literal.hasPrefix(prefix) { throw ProtectionError(path: url.path) }
        }
        try rejectBrokenAliases(url)
        let candidate = resolved(url)
        for root in protectedRoots {
            try rejectBrokenAliases(root)
            let protected = resolved(root).path
            let path = candidate.path
            if path.caseInsensitiveCompare(protected) == .orderedSame
                || path.lowercased().hasPrefix(protected.lowercased().hasSuffix("/") ? protected.lowercased() : protected.lowercased() + "/") {
                throw ProtectionError(path: url.path)
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
        public var errorDescription: String? {
            "This location is read-only example data. FileID cannot modify it: \(path)"
        }
    }
}
