import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Related takes start after a scan", .serialized)
struct CatalogTakeAutoStartTests {
    private func makeDatabase() throws -> (FileIDEngine.Database, URL) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        return (try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite")), directory)
    }

    private func jobState(_ database: FileIDEngine.Database) async throws -> String? {
        try await database.pool.read { db in
            try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [CatalogTakeGrouping.jobID])
        }
    }

    private func addJob(_ database: FileIDEngine.Database, id: String, kind: String, state: String) async throws {
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,priority,created_at,updated_at) VALUES(?,?,'[]','{}',?,0,0,0)",
                           arguments: [id, kind, state])
        }
    }

    private func waitForState(_ database: FileIDEngine.Database, _ wanted: String) async throws -> Bool {
        for _ in 0..<100 {
            if try await jobState(database) == wanted { return true }
            try await Task.sleep(for: .milliseconds(50))
        }
        return false
    }

    @Test func idleLibraryStartsTheJobOnceAndItCompletes() async throws {
        let (database, directory) = try makeDatabase()
        defer { try? FileManager.default.removeItem(at: directory) }
        let task = await CatalogTakeGrouping().startAfterScan(database: database, sink: IPCSink(), poll: .milliseconds(10), attempts: 3)
        await task?.value
        #expect(try await waitForState(database, "completed"))
    }

    @Test func aBusyCatalogJobKeepsItFromStarting() async throws {
        let (database, directory) = try makeDatabase()
        defer { try? FileManager.default.removeItem(at: directory) }
        try await addJob(database, id: "timeline-x", kind: "timelineSample", state: "running")
        let task = await CatalogTakeGrouping().startAfterScan(database: database, sink: IPCSink(), poll: .milliseconds(10), attempts: 3)
        await task?.value
        #expect(try await jobState(database) == nil)
        try await database.pool.write { db in try db.execute(sql: "UPDATE catalog_jobs SET state='completed'") }
        let again = await CatalogTakeGrouping().startAfterScan(database: database, sink: IPCSink(), poll: .milliseconds(10), attempts: 3)
        await again?.value
        #expect(try await waitForState(database, "completed"))
    }

    @Test func aPausedOrRunningJobIsLeftAlone() async throws {
        for state in ["paused", "running", "queued"] {
            let (database, directory) = try makeDatabase()
            defer { try? FileManager.default.removeItem(at: directory) }
            try await addJob(database, id: CatalogTakeGrouping.jobID, kind: CatalogTakeGrouping.kind, state: state)
            let task = await CatalogTakeGrouping().startAfterScan(database: database, sink: IPCSink(), poll: .milliseconds(10), attempts: 2)
            await task?.value
            try await Task.sleep(for: .milliseconds(100))
            #expect(try await jobState(database) == state)
        }
    }

    @Test func theScanCoordinatorCountsOnlyAnalysedMedia() async throws {
        let coordinator = ScanCoordinator()
        _ = await coordinator.startSession(rootDisplayPath: "/x", epoch: 0)
        #expect(await coordinator.mediaAnalyzed == 0)
        await coordinator.bumpProcessed(by: 5)
        #expect(await coordinator.mediaAnalyzed == 0)
        await coordinator.bumpMediaAnalyzed(by: 2)
        #expect(await coordinator.mediaAnalyzed == 2)
    }
}
