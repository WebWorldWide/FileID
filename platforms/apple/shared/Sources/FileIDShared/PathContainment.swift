import Foundation

/// Resolved-to-resolved prefix containment with a separator boundary —
/// the SEC-7 guard both restructure apply paths (engine + app) use so a
/// symlinked bucket component can't route a move outside the tree the
/// user authorized, and a sibling like `…/PhotosBackup` can't
/// prefix-match `…/Photos`.
///
/// Symlinks are resolved against the deepest EXISTING ancestor of `dir`, and
/// the not-yet-created tail is re-appended literally (then standardized to
/// collapse any `.`/`..`). This matters because `resolvingSymlinksInPath()`
/// applies its macOS `/private` shortening ONLY when the resulting path
/// exists — so resolving a non-existent destination parent directly yielded a
/// different canonical form than the (existing) `resolvedRoot`, wrongly
/// rejecting a valid in-root move whose intermediate folder hasn't been
/// created yet (check-before-create). Resolving only existing paths keeps the
/// canonicalization symmetric while still resolving any real symlink in the
/// existing prefix — the actual SEC-7 escape vector. (F-C3-021)
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
    // Standardize AFTER resolving the existing prefix's symlinks: the tail is
    // non-existent (no symlinks to traverse), so collapsing `..` here can't be
    // used to slip past an existing symlink.
    let resolvedPath = resolved.standardizedFileURL.path
    if resolvedPath == resolvedRoot { return true }
    let rootPrefix = resolvedRoot.hasSuffix("/") ? resolvedRoot : resolvedRoot + "/"
    return resolvedPath.hasPrefix(rootPrefix)
}
