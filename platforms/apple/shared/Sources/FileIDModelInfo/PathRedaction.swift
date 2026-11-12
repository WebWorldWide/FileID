import Foundation

/// Redact a path for persistent logs. Keeps the last two components so
/// failures stay debuggable without recording a username or full folder tree.
/// FileID's own database/model paths follow the same rule: they still contain
/// the user's home directory and can include user-selected model locations.
public func redactPathForLog(_ path: String) -> String {
    let normalized = path.replacingOccurrences(of: "\\", with: "/")
    let parts = normalized.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
    guard !parts.isEmpty else { return "…" }

    let homeMarker: Int? = if parts.first == "Users" || parts.first == "home" {
        0
    } else if parts.count > 1,
              parts[0].hasSuffix(":"),
              parts[1].caseInsensitiveCompare("Users") == .orderedSame {
        1
    } else {
        nil
    }
    if let homeMarker {
        let userIndex = homeMarker + 1
        if parts.count == userIndex + 1 { return "…" }
        if parts.count == userIndex + 2 { return "…/\(parts[userIndex + 1])" }
    }

    if normalized.hasPrefix("//"), parts.count <= 2 { return "…" }
    let tail = parts.suffix(2).joined(separator: "/")
    return "…/\(tail)"
}
