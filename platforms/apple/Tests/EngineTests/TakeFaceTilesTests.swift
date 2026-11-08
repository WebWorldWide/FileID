import Foundation
import Testing
@testable import FileIDEngine

@Suite("Take scoring small-face tiles (shared fixture)")
struct TakeFaceTilesTests {
    struct Fixture: Decodable {
        let tiles: [TileCase]
        let wantsTiles: [WantsCase]
        let placing: [PlacingCase]
        let merge: [MergeCase]
        let scoring: ScoringCase
    }
    struct TileCase: Decodable { let width: Int; let height: Int; let expect: [TakeScoring.Tile] }
    struct WantsCase: Decodable {
        let name: String
        let sourceLongSide: Int
        let width: Int
        let height: Int
        let faces: [TakeScoring.Face]
        let people: [TakeScoring.Box]?
        let expect: Bool
    }
    struct PlacingCase: Decodable { let face: TakeScoring.Face; let tile: TakeScoring.Tile; let width: Int; let height: Int; let expect: TakeScoring.Face }
    struct MergeCase: Decodable { let name: String; let base: [TakeScoring.Face]; let tiled: [TakeScoring.Face]; let expect: [TakeScoring.Face] }
    struct ScoringCase: Decodable { let name: String; let shots: [ShotCase]; let expect: [Expect] }
    struct ShotCase: Decodable { let primaryFileID: Int64; let fileIDs: [Int64]; let path: String; let signals: TakeScoring.Signals }
    struct Expect: Decodable { let primaryFileID: Int64; let eyesClosed: Bool; let faceCount: Int }

    static func fixture() throws -> Fixture {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        return try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/take-face-tiles.json")))
    }

    @Test func sharedTilesTriggersPlacementAndMerge() throws {
        let fixture = try Self.fixture()
        for item in fixture.tiles { #expect(TakeScoring.tiles(width: item.width, height: item.height) == item.expect) }
        for item in fixture.wantsTiles {
            let wants = TakeScoring.wantsTiles(sourceLongSide: item.sourceLongSide, width: item.width, height: item.height, faces: item.faces, people: item.people)
            #expect(wants == item.expect, "\(item.name)")
        }
        for item in fixture.placing { #expect(TakeScoring.placing(item.face, in: item.tile, width: item.width, height: item.height) == item.expect) }
        for item in fixture.merge { #expect(TakeScoring.mergeFaces(base: item.base, tiled: item.tiled) == item.expect, "\(item.name)") }
    }

    @Test func faceSizesAreJudgedAtTheTileScale() throws {
        let scoring = try Self.fixture().scoring
        let shots = scoring.shots.map { TakeScoring.Shot(primaryFileID: $0.primaryFileID, fileIDs: $0.fileIDs, path: $0.path, signals: $0.signals) }
        let qualities = TakeScoring.score(shots)
        for expect in scoring.expect {
            let quality = try #require(qualities.first { $0.primaryFileID == expect.primaryFileID })
            #expect(quality.eyesClosed == expect.eyesClosed, "\(scoring.name) \(expect.primaryFileID)")
            #expect(quality.faceCount == expect.faceCount)
        }
    }
}
