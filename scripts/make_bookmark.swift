// Tiny one-shot helper: turn a folder path into a base64-encoded
// bookmark Data, suitable for the `IPCCommand.startScan` payload.
// Used by the test harness so we can drive the engine from Python
// without needing to crack security-scoped bookmarks ourselves.
//
// Build: swiftc -O scripts/make_bookmark.swift -o scripts/make_bookmark
// Run:   ./scripts/make_bookmark /absolute/path/to/folder
//        → prints base64 to stdout
import Foundation

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write(
        "usage: make_bookmark <absolute-folder-path>\n".data(using: .utf8)!
    )
    exit(64)
}

let path = CommandLine.arguments[1]
let url = URL(fileURLWithPath: path)

do {
    // The engine resolves with `[.withSecurityScope]` first, falls back
    // to `[]` if that fails. Plain bookmarks (no scope) work in both
    // paths because the test process and the engine share the user's
    // session. So we emit a plain bookmark — simplest, no entitlements.
    let data = try url.bookmarkData(
        options: [],
        includingResourceValuesForKeys: nil,
        relativeTo: nil
    )
    print(data.base64EncodedString())
} catch {
    FileHandle.standardError.write(
        "bookmark failed for \(path): \(error)\n".data(using: .utf8)!
    )
    exit(1)
}
