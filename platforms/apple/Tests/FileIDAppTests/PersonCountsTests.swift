import Foundation
import GRDB
import Testing
@testable import FileID

@Suite("Person card counts", .serialized)
struct PersonCountsTests {
    @Test("photos are counted live from the faces the card counts, not from the stored file_count")
    func liveCounts() throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-persons-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appending(component: "fileid.sqlite")
        try DatabaseQueue(path: url.path).write { db in
            try db.execute(sql: """
                CREATE TABLE files (id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, size_bytes INTEGER, modified_at REAL);
                CREATE TABLE persons (id INTEGER PRIMARY KEY, name TEXT, title TEXT, first_name TEXT, middle_name TEXT, last_name TEXT,
                    suffix TEXT, is_unknown INTEGER DEFAULT 0, representative_face_id INTEGER, file_count INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE face_prints (id INTEGER PRIMARY KEY, file_id INTEGER, person_id INTEGER, bbox TEXT, excluded INTEGER DEFAULT 0,
                    embedding_model TEXT, processing_version TEXT, source_revision TEXT);
                INSERT INTO files(id,path_text) VALUES (1,'/a'),(2,'/b'),(3,'/c'),(4,'/d');
                INSERT INTO persons(id,file_count,representative_face_id) VALUES (1,99,1),(2,1,5);
                INSERT INTO face_prints(id,file_id,person_id,excluded) VALUES
                    (1,1,1,0),(2,1,1,0),(3,2,1,0),(4,3,1,1),(5,4,2,0)
                """)
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        let people = store.persons()
        let first = try #require(people.first { $0.id == 1 })
        #expect(first.faceCount == 3)
        #expect(first.fileCount == 2)
        #expect(people.map(\.id) == [1, 2])
        #expect(try #require(people.first { $0.id == 2 }).fileCount == 1)
    }
}

@Suite struct MovePhotosOutcomeTests {
    private typealias Outcome = CatalogWrites.FaceMoveOutcome

    @Test func aPhotoCountsOnceEvenWithTwoFacesAndOnlyWhenEveryFaceMoved() {
        let faces: [(face: Int64, file: Int64)] = [(1, 10), (2, 10), (3, 11), (4, 12)]
        #expect(Outcome.settle(faceFiles: faces, movedFaces: [1, 2, 3, 4], photosAsked: 3).photosMoved == 3)
        #expect(Outcome.settle(faceFiles: faces, movedFaces: [1, 3], photosAsked: 3).photosMoved == 1)
        #expect(Outcome.settle(faceFiles: faces, movedFaces: [], photosAsked: 3).photosMoved == 0)
    }

    @Test func nothingMovedSaysSoAndPartialSaysHowMany() {
        #expect(Outcome(photosMoved: 0, photosAsked: 3).status(target: "Alex") == "The photos didn’t move. Try again in a moment.")
        #expect(Outcome(photosMoved: 3, photosAsked: 3).status(target: "Alex") == "Moved 3 photos to Alex.")
        #expect(Outcome(photosMoved: 1, photosAsked: 1).status(target: "Alex") == "Moved 1 photo to Alex.")
        #expect(Outcome(photosMoved: 2, photosAsked: 5).status(target: "Alex") == "Moved 2 photos of 5 to Alex. The rest stayed where they were.")
    }
}
