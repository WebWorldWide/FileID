import Foundation

/// FileID's own state tree (models, DB, logs). Resolved once; the only
/// prefix allowed to pass through redaction verbatim.
private let fileIDStateRoot: String = {
    guard let base = FileManager.default.urls(
        for: .applicationSupportDirectory, in: .userDomainMask).first
    else { return "" }
    return base.appendingPathComponent("FileID", isDirectory: true).path
}()

/// Redact a user file path for logs. Keeps the last two path
/// components so failures stay debuggable while folder names like
/// "Mom_Birthday_2024" don't end up in logs. Only FileID's OWN state
/// tree passes through verbatim — the old unanchored
/// `contains("/Library/Application Support/")` leaked any user path
/// that merely embedded that substring, username and all (the same
/// ENG-97/#26 class fixed in the Windows engine's redact_path_for_log).
public func redactPathForLog(_ path: String) -> String {
    if !fileIDStateRoot.isEmpty {
        if path == fileIDStateRoot { return path }
        if path.hasPrefix(fileIDStateRoot + "/") { return path }
    }
    let parts = (path as NSString).pathComponents
    // A file directly under a home directory (/Users/<name>/<file>)
    // would keep the username as the parent component of the
    // two-component tail — emit the filename alone instead.
    if parts.count == 4, parts[0] == "/", parts[1] == "Users" {
        return "…/\(parts[3])"
    }
    let tail = parts.suffix(2).joined(separator: "/")
    return tail.isEmpty ? "…" : "…/\(tail)"
}
