// SEC-7 containment guard (restructure apply, engine + app). Regression cover
// for F-C3-021: a valid in-root destination whose intermediate folder doesn't
// exist yet was wrongly rejected when the root resolved through macOS's
// `/private` shortening (it only strips `/private` when the path EXISTS, so an
// existing root and a not-yet-created child landed in different canonical
// forms). Found via an isolated on-hardware restructure-apply run.
import Testing
import Foundation
@testable import FileIDShared

@Suite("pathIsContained (SEC-7)")
struct PathContainmentTests {

    private func makeTmpRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDContain-\(UUID().uuidString)/lib")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }

    /// The regression: root resolved (as the apply call site does), destination
    /// parent NOT yet created, root path given in the `/private`-prefixed
    /// resolved form. Must still be recognized as in-root.
    @Test("not-yet-created child of a /private-resolved root is contained")
    func nonexistentChildContained() throws {
        let root = try makeTmpRoot()
        defer { try? FileManager.default.removeItem(at: root.deletingLastPathComponent()) }
        // Resolve like Restructure.apply does.
        let resolvedRoot = root.resolvingSymlinksInPath().path
        let dest = root.appendingPathComponent("Photos/IMG (1).jpg")
        let parent = dest.deletingLastPathComponent()      // .../lib/Photos — does not exist
        #expect(pathIsContained(parent, inResolvedRoot: resolvedRoot))
    }

    @Test("the root itself is contained")
    func rootContained() throws {
        let root = try makeTmpRoot()
        defer { try? FileManager.default.removeItem(at: root.deletingLastPathComponent()) }
        #expect(pathIsContained(root, inResolvedRoot: root.resolvingSymlinksInPath().path))
    }

    @Test("a sibling that shares a name prefix is NOT contained")
    func siblingPrefixNotContained() throws {
        let root = try makeTmpRoot()
        defer { try? FileManager.default.removeItem(at: root.deletingLastPathComponent()) }
        let resolvedRoot = root.resolvingSymlinksInPath().path
        // …/libBackup must not prefix-match …/lib
        let sibling = URL(fileURLWithPath: resolvedRoot + "Backup/x")
        #expect(!pathIsContained(sibling, inResolvedRoot: resolvedRoot))
    }

    @Test("a ../ escape out of the root is NOT contained")
    func dotDotEscapeNotContained() throws {
        let root = try makeTmpRoot()
        defer { try? FileManager.default.removeItem(at: root.deletingLastPathComponent()) }
        let resolvedRoot = root.resolvingSymlinksInPath().path
        let escape = root.appendingPathComponent("..")     // the parent of lib
        #expect(!pathIsContained(escape, inResolvedRoot: resolvedRoot))
    }

    /// SEC-7's actual vector: an EXISTING symlinked component pointing outside
    /// the root must be resolved and rejected (the security point the guard
    /// must never lose while fixing the /private asymmetry).
    @Test("an existing symlink that escapes the root is rejected")
    func symlinkEscapeRejected() throws {
        let root = try makeTmpRoot()
        defer { try? FileManager.default.removeItem(at: root.deletingLastPathComponent()) }
        let resolvedRoot = root.resolvingSymlinksInPath().path
        // Create an out-of-root target and a symlink to it inside the root.
        let outside = root.deletingLastPathComponent().appendingPathComponent("outside")
        try FileManager.default.createDirectory(at: outside, withIntermediateDirectories: true)
        let link = root.appendingPathComponent("escapeLink")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: outside)
        // A move targeting …/lib/escapeLink/sub resolves through the symlink to
        // …/outside/sub — outside the root.
        let viaLink = link.appendingPathComponent("sub")
        #expect(!pathIsContained(viaLink, inResolvedRoot: resolvedRoot))
    }
}
