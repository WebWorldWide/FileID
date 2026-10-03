import Foundation
import Accelerate

@main
struct Benchmark {
    static func main() throws {
        let count = Int(CommandLine.arguments.dropFirst().first ?? "100000") ?? 100000
        let dimension = Int(CommandLine.arguments.dropFirst(2).first ?? "512") ?? 512
        let ef = Int(CommandLine.arguments.dropFirst(3).first ?? "256") ?? 256
        precondition((100...100_000).contains(count) && (16...512).contains(dimension) && (16...1024).contains(ef))
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
        func progress(_ value: String) { FileHandle.standardError.write(Data((value + "\n").utf8)) }
        let index = HNSWIndex(dim: dimension, M: 16, efConstruction: 200, efSearch: 128)
        var vectors: [[Float]] = []
        vectors.reserveCapacity(count)
        let buildStart = Date()
        for id in 0..<count {
            let value = vector(id)
            vectors.append(value)
            index.insert(value)
            if id % 10_000 == 0 { progress("inserted \(id), elapsed \(Date().timeIntervalSince(buildStart))") }
        }
        let buildSeconds = Date().timeIntervalSince(buildStart)
        var durations: [Double] = []
        var recall: [Double] = []
        var queries: [[Float]] = []
        for id in 0..<20 {
            let query = vector(id * 29)
            queries.append(query)
            let start = Date()
            let approximate = index.search(query, k: 10, ef: ef)
            durations.append(Date().timeIntervalSince(start))
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
            recall.append(Double(approximate.filter { expected.contains($0.0) }.count) / 10)
        }
        let url = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent().appendingPathComponent("index.bin")
        let saveStart = Date()
        try index.writeSnapshot(to: url, modelID: "synthetic-clusters-\(dimension)-v1", sourceRevision: "fixture-\(count)-\(dimension)")
        let saveSeconds = Date().timeIntervalSince(saveStart)
        let loadStart = Date()
        let restored = try HNSWIndex.readSnapshot(from: url, modelID: "synthetic-clusters-\(dimension)-v1", sourceRevision: "fixture-\(count)-\(dimension)")
        let loadSeconds = Date().timeIntervalSince(loadStart)
        let stable = queries.allSatisfy { query in
            restored.search(query, k: 10, ef: ef).map(\.0) == index.search(query, k: 10, ef: ef).map(\.0)
        }
        let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize!
        let output: [String: Any] = ["vectors":count,"dimension":dimension,"queries":20,"ef":ef,
            "buildSeconds":buildSeconds,"queryP95Milliseconds":durations.sorted()[18] * 1000,
            "recallAt10Mean":recall.reduce(0,+) / Double(recall.count),"recallAt10Min":recall.min()!,
            "snapshotBytes":size,"saveSeconds":saveSeconds,"loadSeconds":loadSeconds,"restoredResultsIdentical":stable]
        print(String(decoding: try JSONSerialization.data(withJSONObject: output, options: [.sortedKeys]), as: UTF8.self))
    }
}
