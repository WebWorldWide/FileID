import Testing
import Foundation
import GRDB
import FileIDShared
@testable import FileIDEngine

@Suite("Restructure plan keeps the respect stage's moves")
struct RestructureRespectPlanTests {
    @Test func looseEventsAndFormatCorrectionsSurviveTheAnchorStrip() async throws {
        let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDRespectPlan-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        let sep19: Double = 1_789_812_000
        let oct4 = sep19 + 15 * 86_400
        var files: [(String, String, String, Double)] = []
        for i in 0..<5 { files.append(("DCIM/100MSDCF/DSC0\(i).JPG", "image", "jpg", sep19 + Double(i) * 900)) }
        for i in 0..<3 { files.append(("Eckerts 10-4-26/Photos/JPEGs/DSC1\(i).ARW", "image", "arw", oct4 + Double(i) * 600)) }
        for i in 0..<3 { files.append(("Eckerts 10-4-26/Photos/JPEGs/DSC2\(i).JPG", "image", "jpg", oct4 + Double(i) * 600)) }
        for i in 0..<3 { files.append(("Eckerts 10-4-26/Photos/RAWs/DSC3\(i).ARW", "image", "arw", oct4 + Double(i) * 600)) }
        for (relative, _, _, _) in files {
            let url = root.appendingPathComponent(relative)
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data("x".utf8).write(to: url)
        }
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        let db = try FileIDEngine.Database(at: tmp.appendingPathComponent("catalog.sqlite"))
        let rows = files
        try await db.pool.write { d in
            for (index, (relative, kind, ext, time)) in rows.enumerated() {
                let path = root.appendingPathComponent(relative).path
                try d.execute(sql: """
                    INSERT INTO files (id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension,failed)
                    VALUES (?,?,?,1,?,?,?,?,?,0)
                    """, arguments: [index + 1, path, StablePathHash.hash(path), time, time, time, kind, ext])
            }
        }

        let plan = try await Restructure.proposeAll(database: db, libraryRoot: root)
        let byName = Dictionary(plan.proposals.map { (($0.oldPath as NSString).lastPathComponent, $0.newPath) },
                                uniquingKeysWith: { a, _ in a })
        for i in 0..<5 {
            #expect(byName["DSC0\(i).JPG"]?.contains("/Photos/2026/2026-09-19/") == true, "loose camera photo \(i)")
        }
        for i in 0..<3 {
            #expect(byName["DSC1\(i).ARW"] == root.appendingPathComponent("Eckerts 10-4-26/Photos/RAWs/DSC1\(i).ARW").path,
                    "misfiled RAW \(i)")
        }
        #expect(plan.proposals.count == 8, "organized files stay put")
        let dcim = root.appendingPathComponent("DCIM/100MSDCF").path
        let jpegs = root.appendingPathComponent("Eckerts 10-4-26/Photos/JPEGs").path
        #expect(plan.tierByFolder[dcim] != "Anchor")
        #expect(plan.tierByFolder[jpegs] != "Anchor")
    }

    @Test func libraryRootIsNeverAnAnchor() {
        let root = "/Volumes/Library"
        let moves = (0..<4).map {
            RestructureProposal(fileID: Int64($0), oldPath: "\(root)/IMG_\($0).JPG",
                                newPath: "\(root)/Photos/2026/IMG_\($0).JPG", bucket: "2026", confidence: "auto")
        }
        #expect(Restructure.classifyFolders(moves, libraryRoot: root).first?.classification == .mixed)
        #expect(Restructure.classifyFolders(moves).first?.classification == .anchor)
    }
}

@Suite("Accelerate kNN matches brute force")
struct ExactNeighborsTests {
    private func vectors(_ n: Int, _ d: Int) -> [[Float]] {
        var state: UInt64 = 0x9E37_79B9_7F4A_7C15
        func next() -> Float {
            state = state &* 6364136223846793005 &+ 1442695040888963407
            return Float(Int64(bitPattern: state >> 11) % 2_000_001) / 1_000_000 - 1
        }
        var out = (0..<n).map { _ -> [Float] in
            let v = (0..<d).map { _ in next() }
            let norm = sqrt(v.reduce(0) { $0 + $1 * $1 })
            return v.map { $0 / norm }
        }
        out[n - 1] = out[0]
        out[n - 2] = out[1]
        return out
    }

    private struct Neighbor { let index: Int; let similarity: Float }

    private func dot(_ a: [Float], _ b: [Float]) -> Float {
        var sum: Float = 0
        for j in 0..<a.count { sum += a[j] * b[j] }
        return sum
    }

    private func bruteForce(_ data: [[Float]], _ i: Int, _ k: Int) -> [Neighbor] {
        var all: [Neighbor] = []
        for j in 0..<data.count where j != i { all.append(Neighbor(index: j, similarity: dot(data[i], data[j]))) }
        all.sort { (a: Neighbor, b: Neighbor) -> Bool in a.similarity != b.similarity ? a.similarity > b.similarity : a.index < b.index }
        return Array(all.prefix(k))
    }

    @Test func matchesReferenceForSeveralK() throws {
        let n = 300, d = 48
        let data = vectors(n, d)
        for k in [1, 5, n - 1] {
            let fast = try #require(RestructureSemantic.exactNeighbors(data, k: k))
            for i in 0..<n {
                let reference = bruteForce(data, i, k)
                let got = fast[i]
                #expect(got.count == reference.count)
                for (a, b) in zip(got, reference) {
                    #expect(abs(a.similarity - b.similarity) < 1e-5)
                }
                if k == n - 1 {
                    let gotIDs = Set(got.map { (hit: (neighbor: Int, similarity: Float)) -> Int in hit.neighbor })
                    let refIDs = Set(reference.map { (hit: Neighbor) -> Int in hit.index })
                    #expect(gotIDs == refIDs)
                } else if reference.count >= 2, reference[0].similarity - reference[1].similarity > 1e-4 {
                    #expect(got.first?.neighbor == reference[0].index)
                }
            }
        }
    }

    @Test func rejectsRaggedAndTinyInputs() {
        #expect(RestructureSemantic.exactNeighbors([[1, 0]], k: 1) == nil)
        #expect(RestructureSemantic.exactNeighbors([[1, 0], [1, 0, 0]], k: 1) == nil)
    }
}
