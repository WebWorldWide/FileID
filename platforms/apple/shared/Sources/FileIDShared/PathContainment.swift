import Foundation

/// Resolved-to-resolved prefix containment with a separator boundary: the SEC-7 guard both restructure apply paths use, so a symlinked bucket
/// can't route a move outside the authorized tree, and `…/PhotosBackup` can't prefix-match `…/Photos`.
/// Symlinks are resolved against the deepest existing ancestor of `dir`, and the not-yet-created tail is re-appended literally: resolving a
/// non-existent parent directly would canonicalize differently from the existing root and reject valid moves.
public func pathIsContained(_ dir: URL, inResolvedRoot resolvedRoot: String) -> Bool {
    let fm = FileManager.default
    var existing = dir
    var tail: [String] = []
    while !fm.fileExists(atPath: existing.path) {
        let parent = existing.deletingLastPathComponent()
        if parent.path == existing.path { break }   // reached "/"
        tail.insert(existing.lastPathComponent, at: 0)
        existing = parent
    }
    var resolved = existing.resolvingSymlinksInPath()
    for component in tail { resolved.appendPathComponent(component) }
    // Standardize after resolving the existing prefix: the tail has no symlinks, so collapsing `..` can't slip past one.
    let resolvedPath = resolved.standardizedFileURL.path
    if resolvedPath == resolvedRoot { return true }
    let rootPrefix = resolvedRoot.hasSuffix("/") ? resolvedRoot : resolvedRoot + "/"
    return resolvedPath.hasPrefix(rootPrefix)
}
