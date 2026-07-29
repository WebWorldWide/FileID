// C12 regression: the migration chains forked at v14 (macOS registered
// "v14_fts_sync_triggers" while Windows registered
// "v14_files_kind_scanned_index"), which made a macOS-touched library fail
// every Windows scan with SQLITE_CORRUPT. Both platforms now pin the same
// canonical identifier list — the Windows mirror lives in
// platforms/windows/src/engine/src/db/migrations.rs
// (migration_identifiers_match_canonical_list). Update BOTH or the chains
// fork again.
import Testing
import Foundation
import GRDB
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database

@Suite("Migration chain parity (C12)")
struct MigrationParityTests {

    static let canonicalIdentifiers = [
        "v1_core_tables",
        "v2_clip_embeddings",
        "v3_deep_analyze",
        "v4_face_verifications",
        "v5_person_naming_structured",
        "v6_arcface_embeddings",
        "v7_identity_anchors",
        "v8_content_identity",
        "v9_usn_state",
        "v10_doc_text",
        "v11_text_embeddings",
        "v12_face_model_reset",
        "v13_face_verification_anchors",
        "v14_files_kind_scanned_index",
        "v15_fts_sync_triggers",
        "v16_path_search",
        "v17_face_verification_stable_keys",
        "v18_restructure_feedback",
        "v19_files_text_stage_done",
        "v20_vlm_full_model",
    ]

    @Test("Registered migration identifiers match the canonical cross-platform list")
    func identifiersMatchCanonicalList() {
        #expect(Database.migrator.migrations == Self.canonicalIdentifiers)
    }

    @Test("v20 adds nullable TEXT full-model marker without rewriting provenance")
    func vlmFullModelMigrationIsNullableAndPreservesProvenance() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-v20-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let q = try DatabaseQueue(path: dir.appendingPathComponent("fileid.sqlite").path)
        let migrator = FileIDEngine.Database.migrator

        try migrator.migrate(q, upTo: "v19_files_text_stage_done")
        try q.write { db in
            try db.execute(sql: """
                INSERT INTO files
                    (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, vlm_model)
                VALUES (1, '/a.jpg', 1, 100, 0, 'image', 'jpg', 'qwen3-vl-4b')
                """)
        }
        let v19Count = try q.read { db in
            try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM grdb_migrations") ?? -1
        }
        #expect(v19Count == 19)

        try migrator.migrate(q)
        try migrator.migrate(q)
        let state: (
            migrationCount: Int,
            provenance: String?,
            fullModel: String?,
            declaredType: String,
            notNull: Int,
            defaultValue: String?
        )? = try q.read { db in
            guard let file = try Row.fetchOne(db, sql: """
                    SELECT vlm_model, vlm_full_model FROM files WHERE id = 1
                    """),
                  let column = try Row.fetchOne(db, sql: """
                    SELECT type, "notnull", dflt_value
                    FROM pragma_table_info('files')
                    WHERE name = 'vlm_full_model'
                    """)
            else {
                return nil
            }
            let migrationCount =
                try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM grdb_migrations") ?? -1
            let provenance: String? = file["vlm_model"]
            let fullModel: String? = file["vlm_full_model"]
            let declaredType: String = column["type"] ?? ""
            let notNull: Int = column["notnull"] ?? -1
            let defaultValue: String? = column["dflt_value"]
            return (
                migrationCount, provenance, fullModel,
                declaredType, notNull, defaultValue
            )
        }
        let state = try #require(state)

        #expect(state.migrationCount == 20)
        #expect(state.provenance == "qwen3-vl-4b")
        #expect(state.fullModel == nil)
        #expect(state.declaredType.uppercased() == "TEXT")
        #expect(state.notNull == 0)
        #expect(state.defaultValue == nil)
        try q.close()
    }

    // R3-15 regression: a "different people" verdict's churn-stable (file_id, bbox)
    // key must still resolve to the CURRENT face after a faces_evaluated re-scan
    // DELETE+re-INSERTs face_prints with fresh ids (the legacy face_a/face_b id is
    // then dangling). Mirrors the Windows verdict_stable_key_survives_face_print_churn.
    @Test("v17 stable verdict key survives a face_print id churn")
    func verdictStableKeySurvivesFaceChurn() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-r315-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let q = try DatabaseQueue(path: dir.appendingPathComponent("fileid.sqlite").path)
        try FileIDEngine.Database.migrator.migrate(q)
        try q.write { db in
            try db.execute(sql: """
                INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES
                (1, '/a.jpg', 1, 100, 0, 'image', 'jpg'),
                (2, '/b.jpg', 2, 100, 0, 'image', 'jpg')
                """)
            try db.execute(sql: """
                INSERT INTO face_prints (id, file_id, print_data, bbox) VALUES
                (10, 1, x'00', '0.1,0.1,0.2,0.2'), (20, 2, x'00', '0.3,0.3,0.2,0.2')
                """)
            try db.execute(sql: """
                INSERT INTO face_verifications
                    (person_a, person_b, same_person, confidence, vlm_model, verified_at,
                     face_a, face_b, file_a, bbox_a, file_b, bbox_b)
                VALUES (1, 2, 0, 1.0, 'user-verified', 0, 10, 20, 1, '0.1,0.1,0.2,0.2', 2, '0.3,0.3,0.2,0.2')
                """)
            // CHURN: re-scan drops + re-inserts faces with FRESH ids (same file+bbox).
            try db.execute(sql: "DELETE FROM face_prints")
            try db.execute(sql: """
                INSERT INTO face_prints (id, file_id, print_data, bbox) VALUES
                (101, 1, x'00', '0.1,0.1,0.2,0.2'), (202, 2, x'00', '0.3,0.3,0.2,0.2')
                """)
            let legacyGone = try Int.fetchOne(db,
                sql: "SELECT COUNT(*) FROM face_prints WHERE id IN (10, 20)") ?? -1
            #expect(legacyGone == 0)
            let newA = try Int64.fetchOne(db, sql: """
                SELECT id FROM face_prints WHERE file_id = (SELECT file_a FROM face_verifications)
                AND bbox = (SELECT bbox_a FROM face_verifications) LIMIT 1
                """)
            let newB = try Int64.fetchOne(db, sql: """
                SELECT id FROM face_prints WHERE file_id = (SELECT file_b FROM face_verifications)
                AND bbox = (SELECT bbox_b FROM face_verifications) LIMIT 1
                """)
            #expect(newA == 101)
            #expect(newB == 202)
        }
        try q.close()
    }

    // L7: a DB stamped by a newer engine (identifiers beyond this
    // registry) must refuse to open rather than silently write into a
    // schema it doesn't understand. Windows mirror:
    // migrations.rs newer_db_with_unknown_migration_is_refused.
    @Test("DB migrated beyond this engine's registry refuses to open")
    func newerDatabaseRefusesToOpen() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-l7-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let dbURL = dir.appendingPathComponent("fileid.sqlite")

        // Build the "newer" DB with a plain DatabaseQueue — opening a
        // second DatabasePool on the same file inside one process is
        // exactly what Database's own header warns against.
        do {
            let q = try DatabaseQueue(path: dbURL.path)
            try FileIDEngine.Database.migrator.migrate(q)
            try q.write { conn in
                try conn.execute(
                    sql: "INSERT INTO grdb_migrations (identifier) VALUES (?)",
                    arguments: ["v99_from_the_future"]
                )
            }
            try q.close()
        }

        // Bare `Database` is ambiguous with GRDB.Database here.
        #expect(throws: DatabaseOpenError.self) {
            _ = try FileIDEngine.Database(at: dbURL)
        }
    }
}
