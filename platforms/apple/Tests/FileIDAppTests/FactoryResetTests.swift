import Foundation
import Testing
@testable import FileID

@Suite("Factory reset keeps description models")
struct FactoryResetTests {
    private func makeTree() throws -> (root: URL, kept: URL, cleanup: () -> Void) {
        let base = FileManager.default.temporaryDirectory.appending(component: "fileid-reset-\(UUID().uuidString)", directoryHint: .isDirectory)
        let root = base.appending(component: "FileID", directoryHint: .isDirectory)
        let kept = root.appending(path: "Models/huggingface", directoryHint: .isDirectory)
        let fm = FileManager.default
        try fm.createDirectory(at: kept.appending(path: "models/org/Model-4bit"), withIntermediateDirectories: true)
        try Data("weights".utf8).write(to: kept.appending(path: "models/org/Model-4bit/model.safetensors"))
        try fm.createDirectory(at: root.appending(path: "Models/clip_text"), withIntermediateDirectories: true)
        try Data("x".utf8).write(to: root.appending(path: "Models/clip_text/model.onnx"))
        try Data("db".utf8).write(to: root.appending(path: "fileid.sqlite"))
        return (root, kept, { try? fm.removeItem(at: base) })
    }

    @Test("everything but the description models is erased")
    func keepsOnlyTheModelFolder() throws {
        let tree = try makeTree()
        defer { tree.cleanup() }
        FactoryReset.erase(root: tree.root, keeping: [tree.kept])
        let fm = FileManager.default
        #expect(fm.fileExists(atPath: tree.kept.appending(path: "models/org/Model-4bit/model.safetensors").path))
        #expect(!fm.fileExists(atPath: tree.root.appending(path: "fileid.sqlite").path))
        #expect(!fm.fileExists(atPath: tree.root.appending(path: "Models/clip_text").path))
    }

    @Test("a model folder outside the data folder leaves nothing behind")
    func removesTheWholeFolderWhenNothingInsideIsKept() throws {
        let tree = try makeTree()
        defer { tree.cleanup() }
        let elsewhere = FileManager.default.temporaryDirectory.appending(component: "fileid-reset-other-\(UUID().uuidString)")
        FactoryReset.erase(root: tree.root, keeping: [elsewhere])
        #expect(!FileManager.default.fileExists(atPath: tree.root.path))
    }

    @Test("every key of the defaults domain is erased")
    func erasesEveryDefaultsKey() throws {
        let domain = "com.fileid.tests.reset-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: domain))
        defer { defaults.removePersistentDomain(forName: domain) }
        defaults.set(1, forKey: "zzMarker")
        defaults.set("Library", forKey: "activeTabRawValue")
        FactoryReset.eraseDefaults(domain: domain, defaults: defaults)
        #expect(defaults.persistentDomain(forName: domain)?.isEmpty ?? true)
        #expect(defaults.object(forKey: "zzMarker") == nil)
    }
}
