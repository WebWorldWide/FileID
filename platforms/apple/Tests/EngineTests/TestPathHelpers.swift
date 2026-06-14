import Foundation

/// Resolve a URL to its REAL filesystem path via `realpath(3)`.
///
/// Unlike Foundation's `URL.resolvingSymlinksInPath()` — which applies a macOS
/// special case that STRIPS a leading `/private` — `realpath` returns the fully
/// resolved path INCLUDING `/private` (e.g. `/var/folders/…` → `/private/var/folders/…`).
/// That matters in tests that use `FileManager.temporaryDirectory` (under the
/// `/var` → `/private/var` symlink): the `FileManager` directory enumerator emits
/// `/private/var/…` paths, so a test root resolved with `resolvingSymlinksInPath`
/// (`/var/…`) would NOT match the enumerated paths and the incremental skip-set
/// range/lookup would silently miss. Real scan roots (`/Users/…`, `/Volumes/…`)
/// never hit `/private`, so this only affects the temp-dir test environment.
func realResolved(_ url: URL) -> URL {
    guard let resolved = realpath(url.path, nil) else { return url }
    defer { free(resolved) }
    return URL(fileURLWithPath: String(cString: resolved))
}
