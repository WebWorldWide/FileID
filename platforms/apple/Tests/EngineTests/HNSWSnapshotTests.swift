import Foundation
import CryptoKit
import Testing
@testable import FileIDEngine

@Suite("HNSW snapshots")
struct HNSWSnapshotTests {
    private func vector(_ seed: Int) -> [Float] {
        (0..<16).map { Float(sin(Double(seed * 17 + $0 * 11))) }
    }

    @Test("restoration preserves graph, tombstones, search and incremental insertion")
    func roundTrip() throws {
        let original = HNSWIndex(dim: 16)
        for seed in 0..<400 { original.insert(vector(seed)) }
        for id in stride(from: 0, to: 400, by: 7) { original.remove(id: Int32(id)) }
        let data = try original.snapshot(modelID: "fixture-model", sourceRevision: "catalog-42")
        let restored = try HNSWIndex.restoreSnapshot(data, modelID: "fixture-model", sourceRevision: "catalog-42")
        #expect(restored.count == original.count)
        #expect(restored.rawCount == original.rawCount)
        for seed in [0, 27, 398] {
            let expected = original.search(vector(seed), k: 20)
            let actual = restored.search(vector(seed), k: 20)
            #expect(actual.map(\.0) == expected.map(\.0))
            #expect(actual.map(\.1) == expected.map(\.1))
            #expect(actual.allSatisfy { $0.0 % 7 != 0 })
        }
        for seed in 400..<420 {
            #expect(original.insert(vector(seed)) == restored.insert(vector(seed)))
        }
        #expect(try original.snapshot(modelID: "fixture-model", sourceRevision: "catalog-43") == restored.snapshot(modelID: "fixture-model", sourceRevision: "catalog-43"))
    }

    @Test("empty graphs persist and incompatible or stale snapshots are rejected")
    func compatibility() throws {
        let index = HNSWIndex(dim: 16)
        let data = try index.snapshot(modelID: "model-a", sourceRevision: "revision-a")
        let restored = try HNSWIndex.restoreSnapshot(data, modelID: "model-a", sourceRevision: "revision-a")
        #expect(restored.count == 0)
        #expect(restored.search(vector(0), k: 4).isEmpty)
        #expect(throws: HNSWIndex.SnapshotError.self) {
            try HNSWIndex.restoreSnapshot(data, modelID: "model-b", sourceRevision: "revision-a")
        }
        #expect(throws: HNSWIndex.SnapshotError.self) {
            try HNSWIndex.restoreSnapshot(data, modelID: "model-a", sourceRevision: "revision-b")
        }
    }

    @Test("corruption, truncation and bounded malformed fields are rejected")
    func malformed() throws {
        let index = HNSWIndex(dim: 16)
        index.insert(vector(0))
        let data = try index.snapshot(modelID: "m", sourceRevision: "r")
        var corrupt = data
        corrupt[20] ^= 1
        #expect(throws: HNSWIndex.SnapshotError.self) {
            try HNSWIndex.restoreSnapshot(corrupt, modelID: "m", sourceRevision: "r")
        }
        for count in stride(from: 0, to: data.count, by: 11) {
            #expect(throws: HNSWIndex.SnapshotError.self) {
                try HNSWIndex.restoreSnapshot(Data(data.prefix(count)), modelID: "m", sourceRevision: "r")
            }
        }
        let header = 13 + 5 + 5
        for offset in [header, header + 4, header + 24, header + 32, header + 36, header + 38] {
            var payload = Data(data.dropLast(32))
            payload[offset] = 255
            payload[offset + 1] = 255
            payload[offset + 2] = 255
            payload[offset + 3] = 255
            payload.append(contentsOf: SHA256.hash(data: payload))
            #expect(throws: HNSWIndex.SnapshotError.self) {
                try HNSWIndex.restoreSnapshot(payload, modelID: "m", sourceRevision: "r")
            }
        }
    }

    @Test("atomic disk snapshots reject protected destinations")
    func disk() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDIndex-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appendingPathComponent("index.bin")
        let index = HNSWIndex(dim: 16)
        index.insert(vector(8))
        try index.writeSnapshot(to: url, modelID: "m", sourceRevision: "r")
        let restored = try HNSWIndex.readSnapshot(from: url, modelID: "m", sourceRevision: "r")
        #expect(restored.search(vector(8), k: 1).first?.0 == 0)
        #expect(throws: (any Error).self) {
            try index.writeSnapshot(to: URL(fileURLWithPath: "/Volumes/Adlon/FileID-index-test.bin"), modelID: "m", sourceRevision: "r")
        }
        #expect(throws: HNSWIndex.SnapshotError.self) {
            try HNSWIndex.readSnapshot(from: root, modelID: "m", sourceRevision: "r")
        }
    }
}
