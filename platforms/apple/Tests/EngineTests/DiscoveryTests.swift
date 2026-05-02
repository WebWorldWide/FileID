// Discovery smoke test. Creates a temporary directory tree of taggable +
// non-taggable files, runs Discovery.walk, asserts the right files were
// returned in the right (sorted) order with the right kinds.
import Testing
import Foundation
@testable import FileIDEngine

@Suite("Discovery")
struct DiscoveryTests {

    @Test("Walks a small tree and returns sorted, filtered files")
    func smallTree() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDDiscoveryTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let sub = tmp.appendingPathComponent("photos")
        try FileManager.default.createDirectory(at: sub, withIntermediateDirectories: true)

        // Files we expect to be discovered.
        let goodFiles = [
            sub.appendingPathComponent("a.jpg"),
            sub.appendingPathComponent("b.png"),
            tmp.appendingPathComponent("c.pdf"),
            tmp.appendingPathComponent("d.mp4")
        ]
        // Files we expect to be filtered out.
        let badFiles = [
            tmp.appendingPathComponent(".hidden.jpg"),     // hidden
            tmp.appendingPathComponent("notes.xyz"),        // unknown ext
            tmp.appendingPathComponent("README")            // no ext
        ]
        let payload = Data("hello".utf8)
        for url in goodFiles + badFiles {
            try payload.write(to: url)
        }

        let discovery = Discovery()
        let result = await discovery.walk(root: tmp)

        // Expected: the 4 good files. Order sorted by path lexicographically.
        // macOS resolves /var → /private/var via the enumerator, so resolve
        // the same symlinks on the expected side before comparing.
        let resultPaths = result.map { $0.url.resolvingSymlinksInPath().path }
        let expectedPaths = goodFiles.map { $0.resolvingSymlinksInPath().path }.sorted()
        #expect(resultPaths == expectedPaths)

        // Spot-check kinds.
        let byExt: [String: DiscoveredFile.Kind] = Dictionary(
            uniqueKeysWithValues: result.map { ($0.url.pathExtension.lowercased(), $0.kind) }
        )
        #expect(byExt["jpg"] == .image)
        #expect(byExt["png"] == .image)
        #expect(byExt["pdf"] == .pdf)
        #expect(byExt["mp4"] == .video)
    }

    @Test("Skips files larger than the size cap")
    func skipsLargeFiles() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDLargeTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        // Tiny taggable file.
        let small = tmp.appendingPathComponent("small.jpg")
        try Data("x".utf8).write(to: small)
        // 2 MB "video" with size cap 1 MB.
        let big = tmp.appendingPathComponent("big.mp4")
        try Data(repeating: 0, count: 2 * 1024 * 1024).write(to: big)

        let discovery = Discovery()
        let result = await discovery.walk(root: tmp, maxSizeMB: 1)

        #expect(result.count == 1)
        #expect(result.first?.url.lastPathComponent == "small.jpg")
    }
}
