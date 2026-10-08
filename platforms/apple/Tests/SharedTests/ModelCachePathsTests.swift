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

    @Test("Absent and relative roots do not redirect the installed-model cache")
    func invalidRoot() {
        for environment in [[:], ["FILEID_HF_CACHE_ROOT": ""],
                            ["FILEID_HF_CACHE_ROOT": "models"],
                            ["FILEID_HF_CACHE_ROOT": "~/models"]] {
            #expect(ModelCachePaths.isolatedRoot(environment: environment) == nil)
        }
    }
}
