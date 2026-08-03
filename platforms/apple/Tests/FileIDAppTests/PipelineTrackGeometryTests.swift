import Testing
@testable import FileID

@Suite("Pipeline progress track geometry")
struct PipelineTrackGeometryTests {
    @Test("stage centers divide the track into equal columns")
    func stageCenters() {
        let geometry = PipelineTrackGeometry(width: 500, stageCount: 5)

        #expect(geometry.center(for: 0) == 50)
        #expect(geometry.center(for: 1) == 150)
        #expect(geometry.center(for: 2) == 250)
        #expect(geometry.center(for: 3) == 350)
        #expect(geometry.center(for: 4) == 450)
    }

    @Test("fill ends exactly at every stage center")
    func fillEndpoints() {
        let geometry = PipelineTrackGeometry(width: 500, stageCount: 5)
        let start = geometry.center(for: 0)

        for index in 0..<5 {
            let endpoint = start + geometry.filledWidth(through: index)
            #expect(abs(endpoint - geometry.center(for: index)) < 0.0001)
        }
    }
}
