import Foundation
import GRDB
import Testing
@testable import FileIDEngine

private typealias Database = FileIDEngine.Database

@Suite("reassignFace bulk action")
struct ReassignFaceTests {
    private func makeDB() throws -> Database {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("Reassign-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return try Database(at: dir.appendingPathComponent("t.sqlite"))
    }

    @discardableResult
    private func insertPerson(_ db: Database, firstName: String? = nil, faces: Int, quality: [Double]? = nil) async throws -> (person: Int64, faceIDs: [Int64]) {
        try await db.pool.write { d -> (Int64, [Int64]) in
            try d.execute(sql: "INSERT INTO persons (name, first_name, file_count, created_at, is_unknown) VALUES (?, ?, ?, ?, 0)",
                          arguments: [firstName, firstName, faces, Date().timeIntervalSince1970])
            let pid = d.lastInsertedRowID
            var faceIDs: [Int64] = []
            for k in 0..<faces {
                try d.execute(sql: """
                    INSERT INTO files (path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension)
                    VALUES (?, ?, 1, 1, ?, 'image', 'jpg')
                    """, arguments: ["/fixture/p\(pid)_f\(k).jpg", pid * 1000 + Int64(k), Date().timeIntervalSince1970])
                let fileID = d.lastInsertedRowID
                try d.execute(sql: """
                    INSERT INTO face_prints (file_id, person_id, print_data, bbox, arcface_embedding, face_quality)
                    VALUES (?, ?, ?, '0,0,1,1', ?, ?)
                    """, arguments: [fileID, pid, Data(), ArcFaceService.embeddingToBlob([Float](repeating: 0.1, count: 128)), quality?[k] ?? 0.5])
                faceIDs.append(d.lastInsertedRowID)
            }
            try d.execute(sql: "UPDATE persons SET representative_face_id = ? WHERE id = ?", arguments: [faceIDs.first, pid])
            return (pid, faceIDs)
        }
    }

    private func person(_ db: Database, _ id: Int64) async throws -> (fileCount: Int, representative: Int64?)? {
        try await db.pool.read { d in
            try Row.fetchOne(d, sql: "SELECT file_count, representative_face_id FROM persons WHERE id = ?", arguments: [id])
                .map { ($0["file_count"] as Int, $0["representative_face_id"] as Int64?) }
        }
    }

    private func owner(_ db: Database, face: Int64) async throws -> Int64? {
        try await db.pool.read { d in try Int64.fetchOne(d, sql: "SELECT person_id FROM face_prints WHERE id = ?", arguments: [face]) }
    }

    @Test func movesFaceAndReconcilesBothPeople() async throws {
        let db = try makeDB()
        let a = try await insertPerson(db, firstName: "Ada", faces: 2, quality: [0.2, 0.9])
        let b = try await insertPerson(db, firstName: "Bo", faces: 1)
        let result = await FileIDEngineMain.reassignFace(database: db, faceID: a.faceIDs[1], destinationPersonID: b.person, createNewPerson: false)
        #expect(result.succeeded == 1 && result.failed == 0)
        #expect(try await owner(db, face: a.faceIDs[1]) == b.person)
        let source = try #require(try await person(db, a.person))
        let destination = try #require(try await person(db, b.person))
        #expect(source.fileCount == 1 && source.representative == a.faceIDs[0])
        #expect(destination.fileCount == 2 && destination.representative == a.faceIDs[1])
    }

    @Test func newPersonIsCreatedAndEmptiedUnnamedSourceIsRemoved() async throws {
        let db = try makeDB()
        let anonymous = try await insertPerson(db, faces: 1)
        let named = try await insertPerson(db, firstName: "Cy", faces: 1)
        let created = await FileIDEngineMain.reassignFace(database: db, faceID: anonymous.faceIDs[0], destinationPersonID: nil, createNewPerson: true)
        #expect(created.succeeded == 1)
        #expect(try await person(db, anonymous.person) == nil)
        let newOwner = try #require(try await owner(db, face: anonymous.faceIDs[0]))
        #expect(newOwner != anonymous.person)
        #expect(try await person(db, newOwner)?.fileCount == 1)
        let detached = await FileIDEngineMain.reassignFace(database: db, faceID: named.faceIDs[0], destinationPersonID: nil, createNewPerson: false)
        #expect(detached.succeeded == 1)
        #expect(try await owner(db, face: named.faceIDs[0]) == nil)
        #expect(try await person(db, named.person)?.fileCount == 0)
    }

    @Test func refusesMissingFaceStaleDestinationAndConflictingPayload() async throws {
        let db = try makeDB()
        let a = try await insertPerson(db, firstName: "Di", faces: 1)
        let missingFace = await FileIDEngineMain.reassignFace(database: db, faceID: 999, destinationPersonID: a.person, createNewPerson: false)
        #expect(missingFace.failed == 1 && missingFace.messages.first?.message?.contains("no longer exists") == true)
        let staleDestination = await FileIDEngineMain.reassignFace(database: db, faceID: a.faceIDs[0], destinationPersonID: 999, createNewPerson: false)
        #expect(staleDestination.failed == 1 && staleDestination.messages.first?.message?.contains("destination person") == true)
        let conflicting = await FileIDEngineMain.reassignFace(database: db, faceID: a.faceIDs[0], destinationPersonID: a.person, createNewPerson: true)
        #expect(conflicting.failed == 1)
        let noop = await FileIDEngineMain.reassignFace(database: db, faceID: a.faceIDs[0], destinationPersonID: a.person, createNewPerson: false)
        #expect(noop.succeeded == 1 && noop.messages.first?.message == nil)
        #expect(try await owner(db, face: a.faceIDs[0]) == a.person)
    }
}
