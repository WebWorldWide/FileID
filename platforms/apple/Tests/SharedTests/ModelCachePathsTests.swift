import Foundation
import Testing
@testable import FileIDShared

@Suite("Isolated visual-model cache")
struct ModelCachePathsTests {
    @Test("An absolute cache root preserves spaces and normalizes path segments")
    func absoluteRoot() {
        let root = ModelCachePaths.isolatedRoot(environment: [
            "FILEID_HF_CACHE_ROOT": "/tmp/FileID QA/work/../huggingface"
        ])
        #expect(root?.path == "/tmp/FileID QA/huggingface")
    }

    @Test("New installs use Application Support; a non-empty legacy Documents folder is kept")
    func legacyFolderIsKeptOnlyWhenItHoldsModels() throws {
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDCache-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: base) }
        let support = base.appendingPathComponent("Support/huggingface")
        let legacy = base.appendingPathComponent("Documents/huggingface")
        #expect(ModelCachePaths.resolvedRoot(applicationSupport: support, legacy: legacy) == support)
        let models = legacy.appendingPathComponent("models")
        try FileManager.default.createDirectory(at: models, withIntermediateDirectories: true)
        try Data().write(to: models.appendingPathComponent(".DS_Store"))
        #expect(ModelCachePaths.resolvedRoot(applicationSupport: support, legacy: legacy) == support)
        try FileManager.default.createDirectory(at: models.appendingPathComponent("org"), withIntermediateDirectories: true)
        #expect(ModelCachePaths.resolvedRoot(applicationSupport: support, legacy: legacy) == legacy)
    }

    @Test("Absent and relative roots do not redirect the installed-model cache")
    func invalidRoot() {
        for environment in [[:], ["FILEID_HF_CACHE_ROOT": ""],
                            ["FILEID_HF_CACHE_ROOT": "models"],
                            ["FILEID_HF_CACHE_ROOT": "~/models"]] {
            #expect(ModelCachePaths.isolatedRoot(environment: environment) == nil)
        }
    }
}
