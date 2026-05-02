import Foundation

/// Redact a user file path for logs. Keeps the last two path
/// components so failures stay debuggable while folder names like
/// "Mom_Birthday_2024" don't end up in logs. Paths under
/// Application Support (model files, DBs) pass through verbatim —
/// they're structural and useful as-is.
public func redactPathForLog(_ path: String) -> String {
    if path.contains("/Library/Application Support/") { return path }
    let parts = (path as NSString).pathComponents
    let tail = parts.suffix(2).joined(separator: "/")
    return tail.isEmpty ? "…" : "…/\(tail)"
}
