import Foundation
import Accelerate
import Testing
@testable import FileIDEngine

@Suite("HNSW clustered retrieval")
struct HNSWRecallTests {
    @Test("dense clusters retain routes to exact nearest neighbours")
    func clusteredRecall() {
        let count = 16_000
        let dimension = 128
        var random: UInt64 = 0xF11E1D12345
        func sample() -> Float {
            random ^= random << 13
            random ^= random >> 7
            random ^= random << 17
            return Float(random >> 40) / Float(1 << 24) - 0.5
        }
        func normalize(_ values: [Float]) -> [Float] {
            let norm = sqrt(values.reduce(Float(0)) { $0 + $1 * $1 })
            return values.map { $0 / norm }
        }
        let centers = (0..<64).map { _ in normalize((0..<dimension).map { _ in sample() }) }
        func vector(_ id: Int) -> [Float] {
            normalize(centers[id % 64].map { $0 + sample() * 0.08 })
        }
        let index = HNSWIndex(dim: dimension)
        var vectors: [[Float]] = []
        vectors.reserveCapacity(count)
        for id in 0..<count {
            let value = vector(id)
            vectors.append(value)
            index.insert(value)
        }
        var recalls: [Double] = []
        for id in 0..<20 {
            let query = vector(id * 29)
            let approximate = index.search(query, k: 10, ef: 64)
            var exact: [(Int32, Float)] = []
            for (candidateID, candidate) in vectors.enumerated() {
                var distance: Float = 0
                vDSP_distancesq(query, 1, candidate, 1, &distance, vDSP_Length(dimension))
                if exact.count < 10 || distance < exact.last!.1 {
                    exact.append((Int32(candidateID), distance))
                    exact.sort { $0.1 < $1.1 }
                    if exact.count > 10 { exact.removeLast() }
                }
            }
            let expected = Set(exact.map(\.0))
            recalls.append(Double(approximate.filter { expected.contains($0.0) }.count) / 10)
        }
        #expect(recalls.min()! >= 0.9)
        #expect(recalls.reduce(0,+) / Double(recalls.count) >= 0.98)
    }
}
