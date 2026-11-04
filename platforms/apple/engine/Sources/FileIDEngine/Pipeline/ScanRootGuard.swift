import Foundation

enum ScanRootGuard {
    /// The walk reports children in realpath form (`/private/tmp/...`) and yields
    /// nothing at all for a symlinked root, while `standardizedFileURL` and
    /// `resolvingSymlinksInPath` strip `/private`. Every root-derived key — walk
    /// root, library_roots, skip set, orphan sweep, purge needles — must use this.
    static func canonicalPath(_ path: String) -> String {
        guard let resolved = realpath(path, nil) else {
            return URL(fileURLWithPath: path).standardizedFileURL.path
        }
        defer { free(resolved) }
        return String(cString: resolved)
    }

    /// False when the scan root cannot be trusted to reflect the library: missing,
    /// not a directory, empty or unreadable, or a leftover `/Volumes/<name>`
    /// mount point on the boot volume after the drive was unplugged.
    static func isReachable(_ path: String, fileManager fm: FileManager = .default) -> Bool {
        var isDirectory: ObjCBool = false
        guard fm.fileExists(atPath: path, isDirectory: &isDirectory), isDirectory.boolValue else { return false }
        guard let entries = try? fm.contentsOfDirectory(atPath: path), !entries.isEmpty else { return false }
        if path.hasPrefix("/Volumes/") {
            let url = URL(fileURLWithPath: path)
            let isLink = url.resolvingSymlinksInPath().path != url.standardizedFileURL.path
            let volume = (try? url.resourceValues(forKeys: [.volumeURLKey]))?.volume
            if !isLink, volume?.path == "/" { return false }
        }
        return true
    }
}
