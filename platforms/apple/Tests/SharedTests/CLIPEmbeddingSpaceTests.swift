import Foundation
import Testing
@testable import FileIDShared

@Suite("Verified CLIP embedding space")
struct CLIPEmbeddingSpaceTests {
    private func blob(_ values: [Float]) -> Data {
        values.withUnsafeBufferPointer { Data(buffer: $0) }
    }

    @Test("space identity includes pinned artifacts and remains bounded")
    func identity() {
        #expect(CLIPEmbeddingSpace.modelID.hasPrefix("clip-vitb32-openai-v1:"))
        #expect(CLIPEmbeddingSpace.modelID != "mobileclip_s2")
        #expect(CLIPEmbeddingSpace.modelID.utf8.count < 200)
        for id in ["clip_vitb32_image", "clip_vitb32_text", "clip_bpe_vocab", "clip_bpe_merges"] {
            #expect(ModelManifest.artifacts.contains { $0.id == id })
        }
    }

    @Test("vectors require exact dimensions, finite values and normalization")
    func vectors() {
        var vector = [Float](repeating: 0, count: 512)
        vector[0] = 1
        #expect(CLIPEmbeddingSpace.vector(from: blob(vector)) == vector)
        #expect(CLIPEmbeddingSpace.vector(from: Data()) == nil)
        #expect(CLIPEmbeddingSpace.vector(from: blob(Array(vector.dropLast()))) == nil)
        #expect(CLIPEmbeddingSpace.vector(from: blob(vector) + Data([0])) == nil)
        let invalidValues: [Float] = [0, 2, .infinity, .nan]
        for invalid in invalidValues {
            vector[0] = invalid
            #expect(CLIPEmbeddingSpace.vector(from: blob(vector)) == nil)
        }
    }

    @Test("artifact verification reads only regular local files with an exact hash")
    func artifacts() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDModelHash-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appendingPathComponent("model.bin")
        try Data("abc".utf8).write(to: url)
        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        #expect(ModelArtifactVerification.verifyFile(at: url, sha256: digest))
        #expect(!ModelArtifactVerification.verifyFile(at: root, sha256: digest))
        #expect(!ModelArtifactVerification.verifyFile(at: root.appendingPathComponent("missing"), sha256: digest))
        #expect(!ModelArtifactVerification.verifyFile(at: URL(string: "https://huggingface.co/model")!, sha256: digest))
        #expect(!CLIPEmbeddingSpace.verifyArtifact(at: url, id: "clip_vitb32_image"))
        #expect(!CLIPEmbeddingSpace.verifyArtifact(at: url, id: "unknown"))
        try Data("abd".utf8).write(to: url)
        #expect(!ModelArtifactVerification.verifyFile(at: url, sha256: digest))
    }
}
