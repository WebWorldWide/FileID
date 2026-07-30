import Foundation
import Testing
@testable import FileIDShared

@Suite("Shared database row semantics")
struct DBTypesTests {
    @Test("Only the full-model marker establishes Deep Analyze completion")
    func fullAnalysisUsesDedicatedMarker() {
        let legacy = file(vlmModel: "model-a", vlmFullModel: nil)
        let complete = file(vlmModel: "model-a", vlmFullModel: "model-a")

        #expect(!legacy.isFullyAnalyzed(by: "model-a"))
        #expect(complete.isFullyAnalyzed(by: "model-a"))
        #expect(!complete.isFullyAnalyzed(by: "model-b"))
    }

    private func file(vlmModel: String?, vlmFullModel: String?) -> FileRow {
        FileRow(
            id: 1,
            pathText: "/library/a.jpg",
            sizeBytes: 1,
            createdAt: nil,
            modifiedAt: nil,
            scannedAt: Date(timeIntervalSince1970: 0),
            kind: "image",
            extension: "jpg",
            phash: nil,
            aesthetic: nil,
            hasFaces: false,
            hasText: false,
            cameraModel: nil,
            locationLat: nil,
            locationLon: nil,
            failed: false,
            errorMessage: nil,
            vlmModel: vlmModel,
            vlmFullModel: vlmFullModel)
    }
}
