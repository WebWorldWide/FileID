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
    ]

    @Test("Registered migration identifiers match the canonical cross-platform list")
    func identifiersMatchCanonicalList() {
        #expect(Database.migrator.migrations == Self.canonicalIdentifiers)
    }

    // L7: a DB stamped by a newer engine (identifiers beyond this
    // registry) must refuse to open rather than silently write into a
    // schema it doesn't understand. Windows mirror:
    // migrations.rs newer_db_with_unknown_migration_is_refused.
    @Test("DB migrated beyond this engine's registry refuses to open")
    func newerDatabaseRefusesToOpen() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-l7-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let dbURL = dir.appendingPathComponent("fileid.sqlite")

        // FileIDEngine.Database — bare `Database` is ambiguous with
        // GRDB.Database under @testable import.
        var db: FileIDEngine.Database? = try FileIDEngine.Database(at: dbURL)
        try db!.pool.write { conn in
            try conn.execute(
                sql: "INSERT INTO grdb_migrations (identifier) VALUES (?)",
                arguments: ["v99_from_the_future"]
            )
        }
        db = nil

        #expect(throws: DatabaseOpenError.self) {
            _ = try FileIDEngine.Database(at: dbURL)
        }
    }
}
