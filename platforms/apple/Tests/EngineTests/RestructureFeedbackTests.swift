// Learn-from-corrections parity (R3): the macOS RestructureFeedback must behave
// identically to the Windows engine's restructure_feedback — record() credits an
// applied move's filename tokens toward its destination folder, and boost() upgrades
// a planned move to Auto once the summed weight clears FEEDBACK_AUTO_WEIGHT (3).
// Mirrors the Rust tests in pipeline/restructure_feedback.rs.
import Testing
import Foundation
import GRDB
@testable import FileIDEngine
private typealias Database = FileIDEngine.Database

@Suite("Restructure learn-from-corrections (R3)")
struct RestructureFeedbackTests {

    private func makeDB() throws -> (Database, URL) {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("FeedbackTest-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return (try Database(at: dir.appendingPathComponent("t.sqlite")), dir)
    }

    // The user applied three "acme invoice" files into /lib/Invoices; a NEW similar
    // file the planner only marked Review must be upgraded to Auto on the learned
    // filing habit (acme + invoice each weigh 3 → score 6 ≥ 3).
    @Test("record then boost upgrades a matching move to Auto")
    func recordThenBoostUpgrades() async throws {
        let (db, dir) = try makeDB()
        defer { try? FileManager.default.removeItem(at: dir) }

        let applied: [(source: String, destination: String)] = (0..<3).map { i in
            (source: "/in/acme_invoice_\(i).pdf",
             destination: "/lib/Invoices/acme_invoice_\(i).pdf")
        }
        await RestructureFeedback.record(database: db, moves: applied, now: 0)

        let move = RestructureProposal(
            fileID: 99, oldPath: "/in/acme_invoice_new.pdf",
            newPath: "/lib/Invoices/acme_invoice_new.pdf",
            bucket: "Invoices", confidence: "review", reason: nil)
        let boosted = await RestructureFeedback.boost(database: db, proposals: [move])

        #expect(boosted[0].confidence == "auto")
        #expect(boosted[0].reason?.contains("filed files like this") == true)
    }

    // A move to a DIFFERENT folder with no feedback stays Review.
    @Test("boost leaves an unrelated move alone")
    func boostLeavesUnrelatedAlone() async throws {
        let (db, dir) = try makeDB()
        defer { try? FileManager.default.removeItem(at: dir) }

        await RestructureFeedback.record(
            database: db,
            moves: [(source: "/in/acme_invoice_0.pdf",
                     destination: "/lib/Invoices/acme_invoice_0.pdf")],
            now: 0)

        let move = RestructureProposal(
            fileID: 1, oldPath: "/in/trip_hawaii.mp4",
            newPath: "/lib/Videos/trip_hawaii.mp4",
            bucket: "Videos", confidence: "review", reason: nil)
        let boosted = await RestructureFeedback.boost(database: db, proposals: [move])

        #expect(boosted[0].confidence == "review")
    }

    // Re-recording the same token→folder bumps the weight (UPSERT, not duplicate).
    @Test("record accumulates weight on the same token→folder")
    func recordAccumulatesWeight() async throws {
        let (db, dir) = try makeDB()
        defer { try? FileManager.default.removeItem(at: dir) }

        let one: [(source: String, destination: String)] =
            [(source: "/in/report_q1.pdf", destination: "/lib/Reports/report_q1.pdf")]
        await RestructureFeedback.record(database: db, moves: one, now: 0)
        await RestructureFeedback.record(database: db, moves: one, now: 1)

        let weight = try await db.pool.read { conn in
            try Int.fetchOne(conn, sql:
                "SELECT weight FROM restructure_feedback WHERE token = ? AND folder = ?",
                arguments: ["report", "Reports"])
        }
        #expect(weight == 2)
    }
}
