import Foundation

/// Resolved-to-resolved prefix containment with a separator boundary —
/// the SEC-7 guard both restructure apply paths (engine + app) use so a
/// symlinked bucket component can't route a move outside the tree the
/// user authorized, and a sibling like `…/PhotosBackup` can't
/// prefix-match `…/Photos`. Non-existent path tails stay unresolved
/// (only the existing prefix resolves), matching check-before-create.
public func pathIsContained(_ dir: URL, inResolvedRoot resolvedRoot: String) -> Bool {
    let resolved = dir.resolvingSymlinksInPath().path
    if resolved == resolvedRoot { return true }
    let rootPrefix = resolvedRoot.hasSuffix("/") ? resolvedRoot : resolvedRoot + "/"
    return resolved.hasPrefix(rootPrefix)
}
