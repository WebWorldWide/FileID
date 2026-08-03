import Foundation
import Testing
@testable import FileIDShared

@Suite("Model license acceptance")
struct ModelLicenseAcceptanceTests {
    @Test("restricted models require an exact current marker")
    func restrictedMarker() throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-license-\(UUID().uuidString)", directoryHint: .isDirectory)
        defer { try? FileManager.default.removeItem(at: root) }
        let kind = AIModelKind.gemma3_4B

        #expect(!ModelLicenseAcceptance.isAccepted(for: kind, root: root))
        try ModelLicenseAcceptance.recordAcceptance(for: kind, root: root)
        #expect(ModelLicenseAcceptance.isAccepted(for: kind, root: root))

        let marker = try ModelLicenseAcceptance.markerURL(for: kind, root: root)
        try "policy=Gemma\nreviewedAt=2025-01-01\nterms=https://ai.google.dev/gemma/terms\n"
            .write(to: marker, atomically: true, encoding: .utf8)
        #expect(!ModelLicenseAcceptance.isAccepted(for: kind, root: root))
    }

    @Test("permissive models do not require a marker")
    func permissiveModel() {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-license-\(UUID().uuidString)", directoryHint: .isDirectory)
        #expect(ModelLicenseAcceptance.isAccepted(for: .qwen3VL4B, root: root))
        #expect(ModelLicenseAcceptance.isAccepted(for: .qwen3VL8B, root: root))
    }
}
