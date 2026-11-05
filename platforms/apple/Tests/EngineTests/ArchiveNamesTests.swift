import Foundation
import Testing
@testable import FileIDEngine

@Suite("Zip entry names")
struct ArchiveNamesTests {
    private func makeZip(in dir: URL, files: [String: String], comment: String? = nil, zip64: Bool = false) throws -> URL {
        for (name, body) in files {
            let url = dir.appendingPathComponent("src").appendingPathComponent(name)
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data(body.utf8).write(to: url)
        }
        let zip = dir.appendingPathComponent("bundle.zip")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/zip")
        process.currentDirectoryURL = dir.appendingPathComponent("src")
        process.arguments = ["-qr"] + (zip64 ? ["-fz"] : []) + [zip.path, "."] + (comment == nil ? [] : ["-z"])
        let input = Pipe()
        process.standardInput = input
        try process.run()
        if let comment { input.fileHandleForWriting.write(Data((comment + "\n.\n").utf8)) }
        try input.fileHandleForWriting.close()
        process.waitUntilExit()
        return zip
    }

    @Test("names inside a zip are listed; folders and hidden entries are not")
    func listsNames() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDZip-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let zip = try makeZip(in: dir, files: ["notes/jeff-rule-changes.txt": "a", "photo.jpg": "b", ".DS_Store": "c"],
                              comment: "made for a test")
        let names = Set(try #require(ArchiveNames.list(url: zip)).split(separator: "\n").map(String.init))
        #expect(names.contains("notes/jeff-rule-changes.txt") && names.contains("photo.jpg"))
        #expect(!names.contains(where: { $0.hasSuffix("/") || $0.contains("DS_Store") }))
    }

    @Test("ZIP64 archives are listed too")
    func listsZip64() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDZip64-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let zip = try makeZip(in: dir, files: ["big/report.rpt": "r", "notes.txt": "n"], zip64: true)
        let raw = try Data(contentsOf: zip)
        #expect(raw.range(of: Data([0x50, 0x4B, 0x06, 0x06])) != nil, "fixture must carry a ZIP64 end record")
        let names = Set(try #require(ArchiveNames.list(url: zip)).split(separator: "\n").map(String.init))
        #expect(names == ["big/report.rpt", "notes.txt"])
    }

    @Test("corrupt or truncated zips return nil without crashing")
    func survivesCorruption() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDZip-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let zip = try makeZip(in: dir, files: ["a.txt": "a", "b/c.txt": "c"])
        let original = [UInt8](try Data(contentsOf: zip))
        let broken = dir.appendingPathComponent("broken.zip")
        var state: UInt64 = 0x2545_F491_4F6C_DD1D
        for round in 0..<300 {
            state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            var bytes = original
            if round % 3 == 0 {
                bytes = Array(bytes.prefix(Int(state >> 33) % bytes.count))
            } else {
                for k in 0..<4 { bytes[Int((state >> UInt64(8 * k)) % UInt64(bytes.count))] = UInt8(truncatingIfNeeded: state >> 40) }
            }
            try Data(bytes).write(to: broken)
            _ = ArchiveNames.list(url: broken)
        }
        try Data("not a zip".utf8).write(to: broken)
        #expect(ArchiveNames.list(url: broken) == nil)
    }
}
