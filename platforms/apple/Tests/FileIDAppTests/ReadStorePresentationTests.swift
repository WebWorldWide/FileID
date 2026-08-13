import Foundation
import GRDB
import Testing
@testable import FileID

@Suite("Read store presentation data", .serialized)
struct ReadStorePresentationTests {
    private func makeStore(seed: (Database) throws -> Void) throws -> (store: ReadStore, root: URL) {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-read-store-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        let databaseURL = root.appending(component: "fileid.sqlite")
        let queue = try DatabaseQueue(path: databaseURL.path)
        try queue.write { db in
            try db.execute(sql: """
                CREATE TABLE files (
                    id INTEGER PRIMARY KEY,
                    path_text TEXT NOT NULL DEFAULT '',
                    kind TEXT NOT NULL DEFAULT '',
                    failed INTEGER NOT NULL DEFAULT 0,
                    content_hash BLOB,
                    size_bytes INTEGER NOT NULL DEFAULT 0,
                    aesthetic REAL,
                    created_at REAL,
                    vlm_proposed_name TEXT,
                    vlm_full_model TEXT
                )
                """)
            try db.execute(sql: """
                CREATE TABLE tags (
                    file_id INTEGER NOT NULL,
                    tag TEXT NOT NULL,
                    source TEXT NOT NULL,
                    score REAL
                )
                """)
            try db.execute(sql: """
                CREATE TABLE persons (
                    id INTEGER PRIMARY KEY,
                    name TEXT,
                    title TEXT,
                    first_name TEXT,
                    middle_name TEXT,
                    last_name TEXT,
                    suffix TEXT,
                    is_unknown INTEGER NOT NULL DEFAULT 0,
                    representative_face_id INTEGER,
                    file_count INTEGER NOT NULL DEFAULT 0,
                    centroid BLOB
                )
                """)
            try db.execute(sql: """
                CREATE TABLE face_prints (
                    id INTEGER PRIMARY KEY,
                    person_id INTEGER,
                    excluded INTEGER NOT NULL DEFAULT 0,
                    bbox TEXT,
                    file_id INTEGER,
                    arcface_embedding BLOB
                )
                """)
            try db.execute(sql: """
                CREATE TABLE face_verifications (
                    person_a INTEGER NOT NULL,
                    person_b INTEGER NOT NULL,
                    same_person INTEGER NOT NULL,
                    confidence REAL NOT NULL,
                    vlm_model TEXT NOT NULL,
                    verified_at REAL NOT NULL,
                    face_a INTEGER,
                    face_b INTEGER,
                    file_a INTEGER,
                    bbox_a TEXT,
                    file_b INTEGER,
                    bbox_b TEXT,
                    PRIMARY KEY (person_a, person_b)
                )
                """)
            try seed(db)
        }
        let store = ReadStore(dbURL: databaseURL)
        store.openIfPossible()
        return (store, root)
    }

    @Test("generic tags do not consume a file's visible tag slots")
    func genericTagsDoNotConsumeVisibleSlots() throws {
        let fixture = try makeStore { db in
            try db.execute(sql: "INSERT INTO files (id) VALUES (1)")
            try db.execute(sql: """
                INSERT INTO tags (file_id, tag, source, score) VALUES
                    (1, ' favorite ', 'user', NULL),
                    (1, 'image', 'vlm', 1.0),
                    (1, 'sunset', 'vlm', NULL),
                    (1, 'Sunset', 'auto', 0.99),
                    (1, 'cat', 'auto', 0.9),
                    (1, 'CAT', 'auto', 0.8)
                """)
        }
        defer {
            fixture.store.close()
            try? FileManager.default.removeItem(at: fixture.root)
        }

        #expect(fixture.store.topVisionTagsBulk(forFileIDs: [1]) == [1: ["favorite", "sunset"]])
        #expect(fixture.store.tags(forFileID: 1) == ["favorite", "sunset", "cat"])
    }

    @Test("Deep Analyze counts mirror every engine-supported kind")
    func deepAnalyzeCountsCoverAllKinds() throws {
        let fixture = try makeStore { db in
            let rows = [
                (1, "image", "model", "photo-name"),
                (2, "pdf", "model", "report-name"),
                (3, "video", nil, nil),
                (4, "doc", "older", nil),
                (5, "audio", nil, nil),
                (6, "model", nil, nil),
                (7, "other", nil, nil),
            ] as [(Int, String, String?, String?)]
            for row in rows {
                try db.execute(sql: """
                    INSERT INTO files
                      (id, kind, failed, vlm_full_model, vlm_proposed_name)
                    VALUES (?, ?, 0, ?, ?)
                    """, arguments: [row.0, row.1, row.2, row.3])
            }
            try db.execute(sql: "INSERT INTO files (id, kind, failed) VALUES (8, 'audio', 1)")
        }
        defer {
            fixture.store.close()
            try? FileManager.default.removeItem(at: fixture.root)
        }

        #expect(fixture.store.totalAnalyzableFiles() == 6)
        #expect(fixture.store.filesAnalysisStats() == (analyzable: 6, captioned: 2))
        #expect(fixture.store.deepAnalyzePending(modelKey: "model") == (total: 6, pending: 4))
    }

    @Test("all active clusters remain visible regardless of size")
    func allClusterSizesRemainVisible() throws {
        let fixture = try makeStore { db in
            try db.execute(sql: """
                INSERT INTO persons (id, last_name, is_unknown, file_count) VALUES
                    (1, 'Doe', 0, 1),
                    (2, NULL, 0, 1),
                    (3, NULL, 1, 1)
                """)
            try db.execute(sql: """
                INSERT INTO face_prints (id, person_id, file_id) VALUES
                    (1, 1, NULL),
                    (2, 2, NULL),
                    (3, 3, NULL)
                """)
        }
        defer {
            fixture.store.close()
            try? FileManager.default.removeItem(at: fixture.root)
        }

        #expect(fixture.store.persons().map(\.id) == [1, 2])
        #expect(fixture.store.persons(includeUnknown: true).map(\.id) == [1, 2, 3])
    }

    @Test("merge suggestions reject people who appear in the same file")
    func mergeSuggestionsRejectCooccurringPeople() throws {
        let blob = { (values: [Float]) in
            values.withUnsafeBytes { Data($0) }
        }
        let fixture = try makeStore { db in
            try db.execute(sql: """
                INSERT INTO persons (id, representative_face_id) VALUES
                    (1, 1), (2, 2), (3, 3)
                """)
            try db.execute(
                sql: "INSERT INTO face_prints (id, person_id, file_id, arcface_embedding) VALUES (?, ?, ?, ?)",
                arguments: [1, 1, 10, blob([1.0, 0.0])]
            )
            try db.execute(
                sql: "INSERT INTO face_prints (id, person_id, file_id, arcface_embedding) VALUES (?, ?, ?, ?)",
                arguments: [2, 2, 10, blob([0.6, 0.8])]
            )
            try db.execute(
                sql: "INSERT INTO face_prints (id, person_id, file_id, arcface_embedding) VALUES (?, ?, ?, ?)",
                arguments: [3, 3, 11, blob([0.6, 0.8])]
            )
        }
        defer {
            fixture.store.close()
            try? FileManager.default.removeItem(at: fixture.root)
        }

        let candidates = ClusterSuggestions.findCandidates(
            dbPath: fixture.root.appending(component: "fileid.sqlite").path
        )
        #expect(!candidates.contains { ($0.personA, $0.personB) == (1, 2) })
        #expect(candidates.contains { ($0.personA, $0.personB) == (1, 3) })
    }

    @Test("merge suggestions honor a persisted different-people verdict")
    func mergeSuggestionsHonorDifferentVerdict() throws {
        let blob = { (values: [Float]) in values.withUnsafeBytes { Data($0) } }
        let fixture = try makeStore { db in
            try db.execute(sql: """
                INSERT INTO persons (id, representative_face_id) VALUES (1, 1), (2, 2)
                """)
            try db.execute(
                sql: "INSERT INTO face_prints (id, person_id, file_id, bbox, arcface_embedding) VALUES (?, ?, ?, ?, ?)",
                arguments: [1, 1, 10, "0,0,0.2,0.2", blob([1.0, 0.0])]
            )
            try db.execute(
                sql: "INSERT INTO face_prints (id, person_id, file_id, bbox, arcface_embedding) VALUES (?, ?, ?, ?, ?)",
                arguments: [2, 2, 11, "0.5,0.5,0.2,0.2", blob([0.6, 0.8])]
            )
            try db.execute(sql: """
                INSERT INTO face_verifications
                    (person_a, person_b, same_person, confidence, vlm_model, verified_at,
                     face_a, face_b, file_a, bbox_a, file_b, bbox_b)
                VALUES (1, 2, 0, 1, 'user-verified', 1, 1, 2,
                        10, '0,0,0.2,0.2', 11, '0.5,0.5,0.2,0.2')
                """)
        }
        defer {
            fixture.store.close()
            try? FileManager.default.removeItem(at: fixture.root)
        }

        let candidates = ClusterSuggestions.findCandidates(
            dbPath: fixture.root.appending(component: "fileid.sqlite").path
        )
        #expect(candidates.isEmpty)
    }

    @Test("merge suggestions stay bounded")
    func mergeSuggestionsStayBounded() throws {
        let blob = { (values: [Float]) in
            values.withUnsafeBytes { Data($0) }
        }
        let fixture = try makeStore { db in
            for id in 1...30 {
                var vector = [Float](repeating: 0, count: 31)
                vector[0] = Float(0.6).squareRoot()
                vector[id] = Float(0.4).squareRoot()
                try db.execute(
                    sql: "INSERT INTO persons (id, representative_face_id) VALUES (?, ?)",
                    arguments: [id, id]
                )
                try db.execute(
                    sql: "INSERT INTO face_prints (id, person_id, file_id, arcface_embedding) VALUES (?, ?, ?, ?)",
                    arguments: [id, id, id, blob(vector)]
                )
            }
        }
        defer {
            fixture.store.close()
            try? FileManager.default.removeItem(at: fixture.root)
        }

        let candidates = ClusterSuggestions.findCandidates(
            dbPath: fixture.root.appending(component: "fileid.sqlite").path
        )
        #expect(candidates.count == ClusterSuggestions.resultLimit)
    }
}
