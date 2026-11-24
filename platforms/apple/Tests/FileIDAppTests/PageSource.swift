import Foundation

/// The text of one app source file, for tests that pin how a page is wired.
func appSource(_ relativePath: String) throws -> String {
    var root = URL(fileURLWithPath: #filePath)
    for _ in 0..<5 { root.deleteLastPathComponent() }
    return try String(contentsOf: root.appendingPathComponent("platforms/apple/app/Sources/FileID").appendingPathComponent(relativePath), encoding: .utf8)
}
