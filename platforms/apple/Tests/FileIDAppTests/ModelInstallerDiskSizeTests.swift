import Foundation
import Testing
@testable import FileID

@Suite("Model folder size on disk")
struct ModelInstallerDiskSizeTests {
    @Test("adds every file in nested folders, hidden ones included, and one file by itself")
    func sumsNestedFolders() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("disk-size-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let deep = root.appendingPathComponent("a/b", isDirectory: true)
        try FileManager.default.createDirectory(at: deep, withIntermediateDirectories: true)
        try Data(count: 1000).write(to: root.appendingPathComponent("top.bin"))
        try Data(count: 200).write(to: root.appendingPathComponent("a/mid.bin"))
        try Data(count: 30).write(to: deep.appendingPathComponent(".hidden.bin"))
        #expect(ModelInstaller.diskSize(of: root) == 1230)
        #expect(ModelInstaller.diskSize(of: root.appendingPathComponent("top.bin")) == 1000)
        #expect(ModelInstaller.diskSize(of: root.appendingPathComponent("missing")) == 0)
    }

    @Test("a folder it cannot read counts as zero and the rest still adds up")
    func unreadableFolderIsSkipped() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("disk-size-\(UUID().uuidString)")
        let locked = root.appendingPathComponent("locked", isDirectory: true)
        try FileManager.default.createDirectory(at: locked, withIntermediateDirectories: true)
        try Data(count: 500).write(to: locked.appendingPathComponent("x.bin"))
        try Data(count: 70).write(to: root.appendingPathComponent("y.bin"))
        try FileManager.default.setAttributes([.posixPermissions: 0o000], ofItemAtPath: locked.path)
        defer {
            try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: locked.path)
            try? FileManager.default.removeItem(at: root)
        }
        #expect(ModelInstaller.diskSize(of: root) == 70)
    }
}
