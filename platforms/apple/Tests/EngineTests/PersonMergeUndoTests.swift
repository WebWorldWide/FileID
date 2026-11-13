import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

private typealias Database = FileIDEngine.Database

@Suite("People merge undo")
struct PersonMergeUndoTests {
    private func makePath() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("MergeUndo-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("t.sqlite")
    }

    private func insertPerson(_ db: Database, firstName: String? = nil, title: String? = nil, unknown: Bool = false,
                              faces: Int) async throws -> (person: Int64, faceIDs: [Int64]) {
        try await db.pool.write { d -> (Int64, [Int64]) in
            try d.execute(sql: "INSERT INTO persons (name, first_name, title, file_count, created_at, is_unknown) VALUES (?, ?, ?, ?, ?, ?)",
                          arguments: [firstName, firstName, title, faces, 100.0, unknown ? 1 : 0])
            let pid = d.lastInsertedRowID
            var faceIDs: [Int64] = []
            for k in 0..<faces {
                try d.execute(sql: """
                    INSERT INTO files (path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension)
                    VALUES (?, ?, 1, 1, 1, 'image', 'jpg')
                    """, arguments: ["/fixture/p\(pid)_f\(k).jpg", pid * 1000 + Int64(k)])
                let fileID = d.lastInsertedRowID
                try d.execute(sql: """
                    INSERT INTO face_prints (file_id, person_id, print_data, bbox, arcface_embedding, face_quality)
                    VALUES (?, ?, ?, '0,0,1,1', ?, ?)
                    """, arguments: [fileID, pid, Data(), FloatBlob.encode([Float](repeating: 0.1, count: 128)), 0.9 - Double(k) * 0.1])
                faceIDs.append(d.lastInsertedRowID)
            }
            try d.execute(sql: "UPDATE persons SET representative_face_id = ? WHERE id = ?", arguments: [faceIDs.last, pid])
            return (pid, faceIDs)
        }
    }

    private struct Snapshot: Equatable {
        var first: String?
        var title: String?
        var unknown: Int
        var count: Int
        var representative: Int64?
        var faces: [Int64]
    }

    private func snapshot(_ db: Database, _ id: Int64) async throws -> Snapshot? {
        try await db.pool.read { d in
            guard let row = try Row.fetchOne(d, sql: "SELECT first_name, title, COALESCE(is_unknown,0) AS u, file_count, representative_face_id FROM persons WHERE id=?",
                                             arguments: [id]) else { return nil }
            let faces = try Int64.fetchAll(d, sql: "SELECT id FROM face_prints WHERE person_id=? ORDER BY id", arguments: [id])
            return Snapshot(first: row["first_name"], title: row["title"], unknown: row["u"], count: row["file_count"],
                            representative: row["representative_face_id"], faces: faces)
        }
    }

    private func merge(_ db: Database, _ source: Int64, into target: Int64) async throws -> String {
        let result = await FileIDEngineMain.mergeClusters(database: db, source: source, destination: target)
        #expect(result.succeeded == 1 && result.failed == 0)
        return try #require(result.messages.first?.operationID)
    }

    private func undo(_ db: Database, _ operation: String) async -> FaceReviewPage {
        await FaceReview.reply(.undo(requestID: "r", operationID: operation), database: db)
    }

    private func list(_ db: Database) async -> FaceReviewPage {
        await FaceReview.reply(.list(requestID: "r", personID: nil, offset: nil, limit: nil, queue: "mergeUndo"), database: db)
    }

    @Test func undoRestoresNamesFacesCountsAndVerdicts() async throws {
        let db = try Database(at: try makePath())
        let keep = try await insertPerson(db, faces: 2)
        let other = try await insertPerson(db, firstName: "Bo", title: "Uncle", unknown: false, faces: 3)
        try await db.pool.write { d in
            try d.execute(sql: """
                INSERT INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,face_a,face_b)
                VALUES(?,?,0,1.0,'user-verified',5,?,?)
                """, arguments: [keep.person, other.person, keep.faceIDs[0], other.faceIDs[0]])
        }
        let keepBefore = try #require(try await snapshot(db, keep.person))
        let otherBefore = try #require(try await snapshot(db, other.person))

        let operation = try await merge(db, other.person, into: keep.person)
        let merged = try #require(try await snapshot(db, keep.person))
        #expect(merged.first == "Bo" && merged.count == 5 && merged.faces.count == 5)
        #expect(try await snapshot(db, other.person) == nil)
        let offered = await list(db)
        #expect(offered.status == "ok" && offered.total == 1 && offered.operationID == operation)
        #expect(offered.personID == keep.person && offered.otherPersonID == other.person)

        let page = await undo(db, operation)
        #expect(page.status == "ok" && page.operationID == operation && page.personID == keep.person && page.otherPersonID == other.person)
        #expect(try await snapshot(db, keep.person) == keepBefore)
        #expect(try await snapshot(db, other.person) == otherBefore)
        let verdict = try await db.pool.read { d in
            try String.fetchOne(d, sql: "SELECT same_person || ':' || vlm_model FROM face_verifications WHERE person_a=? AND person_b=?",
                                arguments: [min(keep.person, other.person), max(keep.person, other.person)])
        }
        #expect(verdict == "0:user-verified")
        #expect(await list(db).total == 0)
    }

    @Test func namedTargetKeepsItsNameAndSourceNameReturns() async throws {
        let db = try Database(at: try makePath())
        let keep = try await insertPerson(db, firstName: "Ada", faces: 1)
        let other = try await insertPerson(db, firstName: "Bo", unknown: true, faces: 1)
        let operation = try await merge(db, other.person, into: keep.person)
        #expect(try await snapshot(db, keep.person)?.first == "Ada")
        #expect(await undo(db, operation).status == "ok")
        #expect(try await snapshot(db, keep.person)?.first == "Ada")
        let back = try #require(try await snapshot(db, other.person))
        #expect(back.first == "Bo" && back.unknown == 1 && back.faces == other.faceIDs)
    }

    @Test func undoAfterAFaceWasDeletedMovesTheRestBack() async throws {
        let db = try Database(at: try makePath())
        let keep = try await insertPerson(db, firstName: "Ada", faces: 2)
        let other = try await insertPerson(db, firstName: "Bo", faces: 3)
        let operation = try await merge(db, other.person, into: keep.person)
        try await db.pool.write { d in
            try d.execute(sql: "DELETE FROM face_prints WHERE id=?", arguments: [other.faceIDs[2]])
        }
        #expect(await undo(db, operation).status == "ok")
        let back = try #require(try await snapshot(db, other.person))
        #expect(back.faces == Array(other.faceIDs.prefix(2)) && back.count == 2)
        #expect(back.representative.map { back.faces.contains($0) } == true)
        let kept = try #require(try await snapshot(db, keep.person))
        #expect(kept.faces == keep.faceIDs && kept.count == 2)
    }

    @Test func facesMovedElsewhereStayAndAnEmptiedMergeIsRefused() async throws {
        let db = try Database(at: try makePath())
        let keep = try await insertPerson(db, firstName: "Ada", faces: 1)
        let other = try await insertPerson(db, firstName: "Bo", faces: 2)
        let third = try await insertPerson(db, firstName: "Cy", faces: 1)
        let operation = try await merge(db, other.person, into: keep.person)
        try await db.pool.write { d in
            try d.execute(sql: "UPDATE face_prints SET person_id=? WHERE id=?", arguments: [third.person, other.faceIDs[0]])
        }
        #expect(await undo(db, operation).status == "ok")
        #expect(try await snapshot(db, other.person)?.faces == [other.faceIDs[1]])
        #expect(try await snapshot(db, third.person)?.faces.contains(other.faceIDs[0]) == true)

        let fourth = try await insertPerson(db, firstName: "Di", faces: 1)
        let second = try await merge(db, fourth.person, into: keep.person)
        try await db.pool.write { d in
            try d.execute(sql: "UPDATE face_prints SET person_id=? WHERE id=?", arguments: [third.person, fourth.faceIDs[0]])
        }
        #expect(await list(db).total == 0)
        let refused = await undo(db, second)
        #expect(refused.status == "refused" && refused.message == PersonMergeJournal.changedMessage)
        #expect(try await snapshot(db, fourth.person) == nil)
    }

    @Test func undoTwiceChangesNothing() async throws {
        let db = try Database(at: try makePath())
        let keep = try await insertPerson(db, firstName: "Ada", faces: 2)
        let other = try await insertPerson(db, firstName: "Bo", faces: 2)
        let operation = try await merge(db, other.person, into: keep.person)
        #expect(await undo(db, operation).status == "ok")
        let keepAfter = try await snapshot(db, keep.person), otherAfter = try await snapshot(db, other.person)
        let again = await undo(db, operation)
        #expect(again.status == "refused")
        #expect(await undo(db, PersonMergeJournal.lastToken).status == "refused")
        #expect(try await snapshot(db, keep.person) == keepAfter)
        #expect(try await snapshot(db, other.person) == otherAfter)
    }

    @Test func journalSurvivesReopenAndLastTokenUndoesNewestFirst() async throws {
        let path = try makePath()
        var first = "", second = ""
        var a: (person: Int64, faceIDs: [Int64]) = (0, []), b = a, c = a
        do {
            let db = try Database(at: path)
            a = try await insertPerson(db, firstName: "Ada", faces: 1)
            b = try await insertPerson(db, firstName: "Bo", faces: 1)
            c = try await insertPerson(db, firstName: "Cy", faces: 1)
            first = try await merge(db, b.person, into: a.person)
            second = try await merge(db, c.person, into: a.person)
        }
        let db = try Database(at: path)
        #expect(await list(db).operationID == second)
        let newest = await undo(db, PersonMergeJournal.lastToken)
        #expect(newest.status == "ok" && newest.operationID == second && newest.otherPersonID == c.person)
        #expect(await list(db).operationID == first)
        #expect(await undo(db, PersonMergeJournal.lastToken).otherPersonID == b.person)
        #expect(try await snapshot(db, a.person)?.faces == a.faceIDs)
        #expect(try await snapshot(db, b.person)?.first == "Bo")
        #expect(try await snapshot(db, c.person)?.faces == c.faceIDs)
    }

    @Test func mergesOlderThanADayAreNotOfferedButStillUndoById() async throws {
        let db = try Database(at: try makePath())
        let keep = try await insertPerson(db, firstName: "Ada", faces: 1)
        let other = try await insertPerson(db, firstName: "Bo", faces: 1)
        let operation = try await merge(db, other.person, into: keep.person)
        try await db.pool.write { d in
            try d.execute(sql: "UPDATE catalog_operations SET created_at=created_at-90000 WHERE id=?", arguments: [operation])
        }
        #expect(await list(db).total == 0)
        #expect(await undo(db, PersonMergeJournal.lastToken).status == "refused")
        #expect(await undo(db, operation).status == "ok")
    }

    @Test func trashRefusalsCarryAReason() async throws {
        let db = try Database(at: try makePath())
        let journal = try makePath().deletingLastPathComponent().appendingPathComponent("trash.json")
        let result = await FileIDEngineMain.trashFiles(database: db, fileIDs: [424_242], journal: journal) { _ in nil }
        #expect(result.failed == 1 && result.messages.first?.reason == "missing")
    }
}
