import Foundation
import Testing
@testable import FileIDEngine

@Suite("Scan root guard")
struct ScanRootGuardTests {
    @Test func missingAndEmptyRootsAreUnreachable() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("guard-\(UUID().uuidString)")
        #expect(!ScanRootGuard.isReachable(dir.path))
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        #expect(!ScanRootGuard.isReachable(dir.path))
    }

    @Test func populatedRootIsReachable() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("guard-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        try Data().write(to: dir.appendingPathComponent("a.jpg"))
        #expect(ScanRootGuard.isReachable(dir.path))
    }

    @Test func plainFileIsNotARoot() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("guard-\(UUID().uuidString).txt")
        try Data("x".utf8).write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        #expect(!ScanRootGuard.isReachable(file.path))
    }
}
