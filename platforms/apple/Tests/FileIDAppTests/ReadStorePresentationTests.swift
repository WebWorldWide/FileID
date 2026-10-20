import Foundation
import GRDB
import FileIDShared
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
                    modified_at REAL,
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
                    arcface_embedding BLOB,
                    embedding_model TEXT, processing_version TEXT, source_revision TEXT
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
            try db.execute(sql: "CREATE TABLE catalog_revisions(file_id INTEGER PRIMARY KEY,revision TEXT,processing_version TEXT,updated_at REAL)")
            try seed(db)
            for row in try Row.fetchAll(db, sql: "SELECT id,file_id,arcface_embedding FROM face_prints WHERE LENGTH(arcface_embedding)>0") {
                let file: Int64 = row["file_id"]
                let face: Int64 = row["id"]
                let data: Data = row["arcface_embedding"]
                var vector = data.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
                if vector.count < 128 { vector += [Float](repeating: 0, count: 128-vector.count) }
                let padded = vector.withUnsafeBytes { Data($0) }
                try db.execute(sql: "INSERT OR IGNORE INTO files(id,path_text,size_bytes,modified_at) VALUES(?,'/fixture.jpg',1,1)", arguments: [file])
                try db.execute(sql: "INSERT OR IGNORE INTO catalog_revisions(file_id,revision) VALUES(?,'1:4607182418800017408')", arguments: [file])
                try db.execute(sql: "UPDATE face_prints SET arcface_embedding=?,embedding_model='fixture-sface',processing_version=?,source_revision='1:4607182418800017408' WHERE id=?", arguments: [padded,FaceCacheProvenance.processingVersion,face])
            }
        }
        let store = ReadStore(dbURL: databaseURL)
        store.openIfPossible()
        return (store, root)
    }

    @Test("Representative thumbnail revisions change without changing the confirmed person")
    func representativeRevisions() throws {
        let fixture = try makeStore { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,size_bytes,modified_at) VALUES(1,'/tmp/photo.jpg',42,100)")
            try db.execute(sql: "INSERT INTO persons(id,name,representative_face_id) VALUES(1,'Alex',1)")
            try db.execute(sql: "INSERT INTO face_prints(id,person_id,file_id,bbox,embedding_model,processing_version,source_revision) VALUES(1,1,1,'0.1,0.2,0.3,0.4','model-a','v2','source-a')")
        }
        defer {
            fixture.store.close()
            try? FileManager.default.removeItem(at: fixture.root)
        }
        let writer = try DatabaseQueue(path: fixture.root.appending(component: "fileid.sqlite").path)
        var previous = try #require(fixture.store.persons().first)
        for sql in [
            "UPDATE face_prints SET processing_version='v3' WHERE id=1",
            "UPDATE face_prints SET embedding_model='model-b' WHERE id=1",
            "UPDATE face_prints SET source_revision='source-b' WHERE id=1",
            "UPDATE files SET modified_at=101 WHERE id=1",
            "UPDATE files SET size_bytes=43 WHERE id=1"
        ] {
            try writer.write { try $0.execute(sql: sql) }
            let current = try #require(fixture.store.persons().first)
            #expect(current.id == previous.id)
            #expect(current.displayName == "Alex")
            #expect(current.representativeFaceID == previous.representativeFaceID)
            #expect(current.representativeCacheRevision != previous.representativeCacheRevision)
            previous = current
        }
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

    @Test("retained empty identity records do not create unnamed People cards")
    func retainedIdentityVisibility() throws {
        let fixture = try makeStore { db in
            try db.execute(sql: "INSERT INTO persons(id,first_name) VALUES(7,'Alex'),(8,NULL)")
        }
        defer {
            fixture.store.close()
            try? FileManager.default.removeItem(at: fixture.root)
        }
        #expect(fixture.store.persons().map(\.id) == [7])
    }

    @Test("person names agree between display, tagging and the SQL named filter")
    func personNames() throws {
        let fixture = try makeStore { db in
            try db.execute(sql: """
                INSERT INTO persons(id,name,title,first_name,last_name,suffix) VALUES
                    (1,NULL,' Dr ','Ada',' Lovelace','Jr'),
                    (2,'Legacy Name',NULL,NULL,NULL,NULL),
                    (3,'  ',NULL,' ',NULL,NULL)
                """)
            try db.execute(sql: "INSERT INTO face_prints(id,person_id) VALUES(1,1),(2,2),(3,3)")
        }
        defer {
            fixture.store.close()
            try? FileManager.default.removeItem(at: fixture.root)
        }
        let byID = Dictionary(uniqueKeysWithValues: fixture.store.persons().map { ($0.id, $0) })
        #expect(byID[1]?.displayName == "Dr Ada Lovelace Jr")
        #expect(byID[1]?.tagName == "Dr Ada Lovelace Jr")
        #expect(byID[2]?.displayName == "Legacy Name")
        #expect(byID[3]?.displayName == "Person 3")
        #expect(byID[3]?.hasAnyName == false)
        #expect(fixture.store.namedPersonCount() == 2)
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
            dbPath: fixture.root.appending(component: "fileid.sqlite").path, selectedModel: "fixture-sface"
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
            dbPath: fixture.root.appending(component: "fileid.sqlite").path, selectedModel: "fixture-sface"
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
            dbPath: fixture.root.appending(component: "fileid.sqlite").path, selectedModel: "fixture-sface"
        )
        #expect(candidates.count == ClusterSuggestions.resultLimit)
    }
}
