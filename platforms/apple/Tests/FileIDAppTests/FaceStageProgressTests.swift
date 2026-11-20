import FileIDShared
import Testing
@testable import FileID

@MainActor
struct FaceStageProgressTests {
    private func queue(_ title: String?, category: JobCategory = .faceCluster) -> IPCEvent {
        let running = title.map { QueuedJob(id: "job", category: category, title: $0, etaSeconds: nil) }
        return IPCEvent(payload: .queueState(QueueState(running: running, pending: [], totalEtaSeconds: nil)))
    }

    @Test func aFaceJobTheEngineStartsItselfShowsAsRunningWithItsProgress() {
        let engine = EngineClient()
        #expect(!engine.faceClusteringInFlight)
        engine.handleEvent(queue("Finding faces… 120 of 848"))
        #expect(engine.faceClusteringInFlight)
        #expect(engine.faceStageProgress == "Finding faces… 120 of 848")
        engine.handleEvent(queue("Group faces"))
        #expect(engine.faceStageProgress == nil)
        engine.handleEvent(IPCEvent(payload: .faceClusteringComplete(FaceClusteringResult(personCount: 0, faceCount: 0, unmatchedFaces: 0, durationSeconds: 0))))
        #expect(!engine.faceClusteringInFlight)
    }

    @Test func otherJobsDoNotMarkFacesRunning() {
        let engine = EngineClient()
        engine.handleEvent(queue("Scan Library", category: .scan))
        #expect(!engine.faceClusteringInFlight)
        #expect(engine.faceStageProgress == nil)
    }
}
