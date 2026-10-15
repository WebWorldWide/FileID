import Foundation
import Testing
@testable import FileIDShared

@Suite("Face refresh coverage wire compatibility")
struct FaceClusteringCoverageCompatibilityTests {
    @Test func legacyResultOmitsOptionalCoverageAndCurrentResultRoundTripsIt() throws {
        let legacy = Data(#"{"personCount":2,"faceCount":6,"unmatchedFaces":1,"durationSeconds":0.5}"#.utf8)
        let decoded = try JSONDecoder().decode(FaceClusteringResult.self, from: legacy)
        #expect(decoded.deferredFaces == nil)
        #expect(decoded.personCount == 2)
        let encoded = try JSONEncoder().encode(decoded)
        let object = try #require(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
        #expect(object["deferredFaces"] == nil)
        let current = Data(#"{"personCount":2,"faceCount":6,"unmatchedFaces":1,"durationSeconds":0.5,"deferredFaces":4}"#.utf8)
        let result = try JSONDecoder().decode(FaceClusteringResult.self, from: current)
        #expect(result.deferredFaces == 4)
        let roundTrip = try JSONDecoder().decode(FaceClusteringResult.self, from: JSONEncoder().encode(result))
        #expect(roundTrip.deferredFaces == 4)
        #expect(roundTrip.faceCount == result.faceCount)
    }
}
