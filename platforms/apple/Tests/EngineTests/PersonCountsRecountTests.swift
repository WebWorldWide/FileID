import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Person file counts")
struct PersonCountsRecountTests {
    private func makeDB() throws -> (FileIDEngine.Database, URL) {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("PersonCounts-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return (try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite")), root)
    }

    private func seed(_ db: FileIDEngine.Database) async throws {
        try await db.pool.write { d in
            for (id, path) in [(1, "/lib/1.jpg"), (2, "/lib/2.jpg"), (3, "/lib/3.jpg")] {
                try d.execute(sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES (?,?,?,1,0,'image','jpg')",
                              arguments: [id, path, id])
            }
            for id in 1...4 {
                try d.execute(sql: "INSERT INTO persons (id, created_at, file_count) VALUES (?, 0, 99)", arguments: [id])
            }
            // Person 1: two counted faces on file 1, one on file 2, one excluded on file 3.
            // Person 2 has only an excluded face, person 3 none, person 4 one counted face.
            for (id, file, person, excluded) in [(1, 1, 1, 0), (2, 1, 1, 0), (3, 2, 1, 0), (4, 3, 1, 1), (5, 3, 2, 1), (6, 3, 4, 0)] {
                try d.execute(sql: "INSERT INTO face_prints (id, file_id, person_id, print_data, bbox, excluded) VALUES (?,?,?,X'00','0,0,1,1',?)",
                              arguments: [id, file, person, excluded])
            }
        }
    }

    private func counts(_ db: FileIDEngine.Database) async throws -> [Int] {
        try await db.pool.read { try Int.fetchAll($0, sql: "SELECT file_count FROM persons ORDER BY id") }
    }

    @Test func recountStaleCorrectsWrongExcludedAndEmptyCounts() async throws {
        let (db, root) = try makeDB()
        defer { try? FileManager.default.removeItem(at: root) }
        try await seed(db)
        let fixed = try await db.pool.write { try PersonCounts.recountStale($0) }
        #expect(fixed == 4)
        #expect(try await counts(db) == [2, 0, 0, 1])
        let again = try await db.pool.write { try PersonCounts.recountStale($0) }
        #expect(again == 0)
    }

    @Test func recountStaleLeavesCorrectRowsAlone() async throws {
        let (db, root) = try makeDB()
        defer { try? FileManager.default.removeItem(at: root) }
        try await seed(db)
        try await db.pool.write { try $0.execute(sql: "UPDATE persons SET file_count = 1 WHERE id = 4") }
        let fixed = try await db.pool.write { try PersonCounts.recountStale($0) }
        #expect(fixed == 3)
        #expect(try await counts(db) == [2, 0, 0, 1])
    }

    @Test func trashingAFileRecountsItsPersons() async throws {
        let (db, root) = try makeDB()
        defer { try? FileManager.default.removeItem(at: root) }
        let bin = root.appendingPathComponent("FakeTrash")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        let photo = root.appendingPathComponent("photo.jpg")
        try Data("pixels".utf8).write(to: photo)
        let ref = Discovery.inode(of: photo).map { Int64(bitPattern: $0) }
        try await db.pool.write { d in
            try d.execute(sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, file_ref) VALUES (1,?,?,1,0,'image','jpg',?)",
                          arguments: [photo.path, StablePathHash.hash(photo.path), ref])
            try d.execute(sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES (2,'/lib/other.jpg',2,1,0,'image','jpg')")
            try d.execute(sql: "INSERT INTO persons (id, created_at, file_count) VALUES (1, 0, 2)")
            try d.execute(sql: "INSERT INTO face_prints (file_id, person_id, print_data, bbox) VALUES (1,1,X'00','0,0,1,1'),(2,1,X'00','0,0,1,1')")
        }
        let journal = root.appendingPathComponent("trash-journal.json")
        let result = await FileIDEngineMain.trashFiles(database: db, fileIDs: [1], journal: journal) { url in
            let landed = bin.appendingPathComponent(url.lastPathComponent)
            try FileManager.default.moveItem(at: url, to: landed)
            return landed
        }
        #expect(result.succeeded == 1)
        #expect(try await counts(db) == [1])
    }
}
