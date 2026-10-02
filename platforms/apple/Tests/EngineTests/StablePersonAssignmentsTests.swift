import Foundation
import Testing
@testable import FileIDEngine

@Suite("Stable person assignment fixtures")
struct StablePersonAssignmentsTests {
    struct Fixture: Decodable { let cases: [Case] }
    struct Case: Decodable {
        let name: String
        let priors: [StablePersonAssignments.Prior]
        let clusters: [StablePersonAssignments.Cluster]
        let fixed: [Int64?]
        let preserved: [Int64]
        let expected: [Int64?]?
        let reject: Bool?
    }

    @Test func sharedFixtures() throws {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let fixture = try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/stable-person-ids.json")))
        for item in fixture.cases {
            if item.reject == true {
                #expect(throws: StablePersonAssignments.Failure.self) {
                    try StablePersonAssignments.resolve(priors: item.priors, clusters: item.clusters, fixed: item.fixed, preserved: Set(item.preserved))
                }
            } else {
                let result = try StablePersonAssignments.resolve(priors: item.priors, clusters: item.clusters, fixed: item.fixed, preserved: Set(item.preserved))
                #expect(result == item.expected, "\(item.name)")
            }
        }
    }

    @Test func sharedProtectedPartitions() throws {
        struct Owner: Decodable { let face: Int64; let person: Int64 }
        struct Item: Decodable {
            let name: String
            let raw: [[Int64]]
            let owners: [Owner]
            let different: [[Int64]]
            let excluded: [Int64]
            let expected: [[Int64]]
        }
        struct Fixtures: Decodable { let cases: [Item] }
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let fixtures = try JSONDecoder().decode(Fixtures.self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/protected-face-partitions.json")))
        for item in fixtures.cases {
            let faceIDs = Set(item.raw.flatMap { $0 }).sorted()
            let dense = Dictionary(uniqueKeysWithValues: faceIDs.enumerated().map { ($0.element,$0.offset) })
            let raw = Dictionary(uniqueKeysWithValues: item.raw.enumerated().map { ($0.offset,$0.element.map { dense[$0]! }) })
            let result = FaceClustering.partitionProtectedClusters(raw, denseToFaceID: faceIDs,
                bucketOwnerByFaceID: Dictionary(uniqueKeysWithValues: item.owners.map { ($0.face,$0.person) }),
                differentPairs: Set(item.different.map { .init($0[0],$0[1]) }), excludedFaceIDs: Set(item.excluded))
            let buckets = result.clusters.keys.sorted().map { result.clusters[$0]!.map { faceIDs[$0] } }
            #expect(buckets == item.expected, "\(item.name)")
        }
    }
}
