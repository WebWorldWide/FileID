// Database manager — owns the single GRDB DatabasePool for the engine.
//
// Schema lives at ~/Library/Application Support/FileID/fileid.sqlite (WAL).
// Migrations are versioned via DatabaseMigrator; new migrations appended,
// never edited (GRDB tracks applied versions in the `grdb_migrations` table).
//
// Vector index (vectorlite) lands in M3 alongside the embeddings — for M2
// we ship the metadata schema + FTS5 only. CLIP/SigLIP virtual tables are
// added in a later migration so existing M2 DBs upgrade cleanly.
import Foundation
import GRDB

/// Single owner of the database. Constructed once in main; passed to the DB
/// Writer task as the only writer. The SwiftUI app reads via its own
/// read-only DatabaseQueue (M4) — never via this writer.
public final class Database: @unchecked Sendable {
    public let pool: DatabasePool

    public init(at url: URL) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        var config = Configuration()
        config.qos = .userInitiated
        // GRDB sets journal_mode=WAL by default, but be explicit so a
        // future config tweak doesn't silently flip it.
        config.prepareDatabase { db in
            try db.execute(sql: "PRAGMA journal_mode = WAL")
            try db.execute(sql: "PRAGMA synchronous = NORMAL")
            try db.execute(sql: "PRAGMA temp_store = MEMORY")
            try db.execute(sql: "PRAGMA mmap_size = 268435456")     // 256 MB
            try db.execute(sql: "PRAGMA cache_size = -65536")        // 64 MB
            try db.execute(sql: "PRAGMA wal_autocheckpoint = 10000") // ~40 MB
        }
        self.pool = try DatabasePool(path: url.path, configuration: config)
        try Self.migrator.migrate(pool)
    }

    /// Default location: ~/Library/Application Support/FileID/fileid.sqlite.
    public static var defaultURL: URL {
        let base = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first!
            .appendingPathComponent("FileID", isDirectory: true)
        return base.appendingPathComponent("fileid.sqlite")
    }

    // MARK: - Migrations

    static var migrator: DatabaseMigrator {
        var m = DatabaseMigrator()

        // v1 — core scan tables: files, tags, ocr text + FTS5, face prints,
        // persons, scan_sessions. Non-vector (no embedder yet in M2).
        m.registerMigration("v1_core_tables") { db in
            try db.create(table: "files") { t in
                t.autoIncrementedPrimaryKey("id")
                t.column("path_text",   .text).notNull().unique(onConflict: .replace)
                t.column("path_hash",   .integer).notNull().indexed()
                t.column("bookmark",    .blob)              // optional security-scoped
                t.column("size_bytes",  .integer).notNull()
                t.column("created_at",  .double)
                t.column("modified_at", .double)
                t.column("scanned_at",  .double).notNull()
                t.column("kind",        .text).notNull().indexed()
                t.column("extension",   .text).notNull()
                t.column("phash",       .integer)
                t.column("aesthetic",   .double)
                t.column("has_faces",   .integer).notNull().defaults(to: 0)
                t.column("has_text",    .integer).notNull().defaults(to: 0)
                t.column("camera_model",  .text)
                t.column("location_lat",  .double)
                t.column("location_lon",  .double)
                t.column("failed",        .integer).notNull().defaults(to: 0)
                t.column("error_message", .text)
            }
            // Partial index: only files with a phash get indexed; saves ~30%
            // index size on libraries with lots of failed/non-image files.
            try db.execute(sql:
                "CREATE INDEX idx_files_phash ON files(phash) WHERE phash IS NOT NULL"
            )
            try db.create(index: "idx_files_scanned",
                          on: "files", columns: ["scanned_at"])

            try db.create(table: "tags") { t in
                t.column("file_id", .integer).notNull()
                    .references("files", onDelete: .cascade)
                t.column("tag",     .text).notNull()
                t.column("source",  .text).notNull()        // vision|clip|exif|user
                t.column("score",   .double)
                t.primaryKey(["file_id", "tag", "source"])
            }
            try db.create(index: "idx_tags_tag", on: "tags", columns: ["tag"])

            try db.create(table: "ocr_text") { t in
                t.column("file_id", .integer).primaryKey()
                    .references("files", onDelete: .cascade)
                t.column("text",    .text).notNull()
            }
            // External-content FTS5: avoids duplicating the OCR text. The
            // `text` column lives in ocr_text; FTS5 indexes it transparently.
            try db.execute(sql: """
                CREATE VIRTUAL TABLE ocr_fts USING fts5(
                    text,
                    content='ocr_text',
                    content_rowid='file_id',
                    tokenize='porter unicode61'
                )
                """)

            // persons before face_prints — face_prints.person_id references it.
            try db.create(table: "persons") { t in
                t.autoIncrementedPrimaryKey("id")
                t.column("name", .text)
                // representative_face_id is a soft FK (face_prints created
                // after this table); we add an index but no constraint, since
                // SQLite forbids forward FK references in the same migration.
                t.column("representative_face_id", .integer)
                t.column("file_count", .integer).notNull().defaults(to: 0)
                t.column("created_at", .double).notNull()
            }

            try db.create(table: "face_prints") { t in
                t.autoIncrementedPrimaryKey("id")
                t.column("file_id",    .integer).notNull()
                    .references("files", onDelete: .cascade)
                t.column("person_id",  .integer)
                    .references("persons")
                t.column("print_data", .blob).notNull()      // 512-d float32, 2048 bytes
                t.column("bbox",       .text).notNull()      // "x,y,w,h" normalized
            }
            try db.create(index: "idx_face_person", on: "face_prints", columns: ["person_id"])
            try db.create(index: "idx_face_file",   on: "face_prints", columns: ["file_id"])
            try db.create(index: "idx_person_repface", on: "persons", columns: ["representative_face_id"])

            try db.create(table: "scan_sessions") { t in
                t.column("id",              .text).primaryKey()    // UUID
                t.column("root_path",       .text).notNull()
                t.column("started_at",      .double).notNull()
                t.column("completed_at",    .double)
                t.column("last_file_index", .integer)              // resume cursor
                t.column("total_files",     .integer)
                t.column("status",          .text).notNull()       // running|completed|crashed|cancelled
            }
        }

        // v2 — CLIP image embeddings. Raw float32 BLOB (length = 512 * 4 = 2048 B).
        // No vector index in M3 (v2's vectorlite extension lands later); brute-force
        // k-NN at scan-time scale (≤500K files) is fine, and the M4 read path
        // doesn't query similarity yet (Library is filename + tags + FTS5).
        m.registerMigration("v2_clip_embeddings") { db in
            try db.create(table: "clip_embeddings") { t in
                t.column("file_id",   .integer).primaryKey()
                    .references("files", onDelete: .cascade)
                t.column("embedding", .blob).notNull()              // 2048 B = 512 × float32 LE
                t.column("model",     .text).notNull()              // "mobileclip_s2" — for future model swaps
            }
        }

        // v3 — Deep Analyze captions + proposed renames. Stored on the
        // files row directly: 1:1, no need for a join table.
        //   - vlm_description: 1-2 sentence human-readable caption
        //   - vlm_proposed_name: machine-readable suggested basename
        //   - vlm_model: which model produced it (so re-runs with a
        //     different model don't silently mix outputs)
        //   - vlm_analyzed_at: when it ran (lets the UI sort by recency
        //     and re-analyze stale entries when models change)
        m.registerMigration("v3_deep_analyze") { db in
            try db.alter(table: "files") { t in
                t.add(column: "vlm_description", .text)
                t.add(column: "vlm_proposed_name", .text)
                t.add(column: "vlm_model",       .text)
                t.add(column: "vlm_analyzed_at", .double)
            }
        }

        // v4: VLM-verified merge suggestions. After face clustering's
        // L2-distance pass, the 0.45-0.70 borderline band is sent to the
        // local VLM (Qwen2.5-VL or similar) which compares face crops
        // pairwise. Results land here. The app's Suggested Merges sheet
        // joins this with ClusterSuggestions output to rank by VLM
        // confidence rather than just L2 distance — fewer false splits,
        // less manual naming work.
        m.registerMigration("v4_face_verifications") { db in
            try db.create(table: "face_verifications") { t in
                t.column("person_a", .integer).notNull()
                t.column("person_b", .integer).notNull()
                t.column("same_person", .integer).notNull()
                t.column("confidence", .double).notNull()
                t.column("vlm_model", .text).notNull()
                t.column("verified_at", .double).notNull()
                t.primaryKey(["person_a", "person_b"])
            }
            try db.create(index: "idx_face_verify_a",
                          on: "face_verifications", columns: ["person_a"])
            try db.create(index: "idx_face_verify_b",
                          on: "face_verifications", columns: ["person_b"])
        }

        // v5: structured person naming. The original `persons.name` was a
        // single string ("Adam"). Real-world naming is more nuanced —
        // users want title (Uncle / Grandma), first, optional middle,
        // last, suffix (Jr). All optional. `is_unknown` marks clusters
        // the user has explicitly opted out of: those go into a single
        // "Unknown" bucket for the People tab and are excluded from VLM
        // re-clustering AND from Deep Analyze prompts.
        m.registerMigration("v5_person_naming_structured") { db in
            try db.alter(table: "persons") { t in
                t.add(column: "title", .text)         // "Uncle", "Grandma"
                t.add(column: "first_name", .text)
                t.add(column: "middle_name", .text)
                t.add(column: "last_name", .text)
                t.add(column: "suffix", .text)        // "Jr", "III"
                t.add(column: "is_unknown", .integer).defaults(to: 0)
            }
            // Backfill: anything in the old `name` column gets parsed
            // into first_name as a best-effort migration. Users can
            // re-edit on the People tab. Lossy by design; the new
            // schema is the source of truth going forward.
            try db.execute(sql: """
                UPDATE persons
                SET first_name = name
                WHERE name IS NOT NULL AND name != '' AND first_name IS NULL
                """)
        }

        // v6: ArcFace face-clustering rewrite. New embedding column for
        // ArcFace iResNet50 (or MobileFace) outputs (L2-normalized 512-d
        // float32 = 2048 bytes). Quality + excluded flags so the new
        // pipeline can drop blurry / profile / tiny faces from clustering
        // without losing the row entirely (the user can still see the
        // face in PersonDetailSheet under "Maybe this person").
        //
        // The legacy `print_data` column (Apple Vision feature print) is
        // kept dormant during migration so we have a rollback path. A
        // future minor version drops it once the new pipeline is proven.
        m.registerMigration("v6_arcface_embeddings") { db in
            try db.alter(table: "face_prints") { t in
                t.add(column: "arcface_embedding", .blob)
                t.add(column: "face_quality", .double)        // 0..1, Vision-provided
                t.add(column: "excluded", .integer).defaults(to: 0)
            }
            // Index for the migration job's "find rows still needing
            // ArcFace embedding" query — cheap to maintain, dramatically
            // speeds up the existence scan during rolling re-embed.
            try db.execute(sql: """
                CREATE INDEX IF NOT EXISTS idx_face_prints_arcface_null
                ON face_prints(id)
                WHERE arcface_embedding IS NULL
                """)
        }

        // v7: identity anchors. Each cluster's L2-normalized centroid
        // and the 90th-percentile cosine distance from members to that
        // centroid (= anchor_radius) are persisted on the persons row.
        // On the next clustering run, new clusters whose centroids fall
        // within an old anchor's radius inherit the old structured-name
        // fields. Lets named people survive re-clustering without the
        // app needing to track cluster identity manually.
        //
        //   centroid:        2048-byte blob (512-d float32 L2-normalized)
        //   anchor_radius:   cosine sim cutoff for membership match
        //   last_clustered_at: timestamp of the run that wrote this anchor
        m.registerMigration("v7_identity_anchors") { db in
            try db.alter(table: "persons") { t in
                t.add(column: "centroid", .blob)
                t.add(column: "anchor_radius", .double)
                t.add(column: "last_clustered_at", .double)
            }
        }

        m.registerMigration("v8_content_identity") { db in
            try db.execute(sql: "ALTER TABLE files ADD COLUMN content_hash BLOB")
            try db.execute(sql: "ALTER TABLE files ADD COLUMN file_ref INTEGER")
            try db.execute(sql: """
                CREATE INDEX IF NOT EXISTS idx_files_content_hash
                    ON files(content_hash)
                    WHERE content_hash IS NOT NULL
                """)
            try db.execute(sql: """
                CREATE INDEX IF NOT EXISTS idx_files_file_ref
                    ON files(file_ref)
                    WHERE file_ref IS NOT NULL
                """)
        }

        m.registerMigration("v9_usn_state") { db in
            try db.execute(sql: """
                CREATE TABLE IF NOT EXISTS usn_state (
                    volume_id       TEXT    PRIMARY KEY,
                    journal_id      INTEGER NOT NULL,
                    next_usn        INTEGER NOT NULL,
                    last_polled_at  DOUBLE  NOT NULL
                )
                """)
        }

        m.registerMigration("v10_doc_text") { db in
            try db.execute(sql: """
                CREATE TABLE IF NOT EXISTS doc_text (
                    file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                    text    TEXT    NOT NULL
                )
                """)
            try db.execute(sql: """
                CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts USING fts5(
                    text,
                    content='doc_text',
                    content_rowid='file_id',
                    tokenize='porter unicode61'
                )
                """)
        }

        m.registerMigration("v11_text_embeddings") { db in
            try db.execute(sql: """
                CREATE TABLE IF NOT EXISTS text_embeddings (
                    file_id   INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                    embedding BLOB    NOT NULL,
                    model     TEXT    NOT NULL
                )
                """)
        }

        // v12: face-model reset for the commercial-clean swap. SFace (128-d)
        // replaces ArcFace (512-d); the old `arcface_embedding` blobs are
        // dimensionally incomparable, so wipe all face state and let the next
        // scan re-detect + re-embed with SFace. Child→parent delete order keeps
        // the face_prints.person_id FK happy. Mirrors the Windows
        // "v12_face_model_reset" migration so both platforms reset in lockstep.
        m.registerMigration("v12_face_model_reset") { db in
            try db.execute(sql: "DELETE FROM face_verifications")
            try db.execute(sql: "DELETE FROM face_prints")
            try db.execute(sql: "DELETE FROM persons")
        }

        return m
    }

    // MARK: - Convenience reads

    /// Total file count — used by the read-side UI (M4) and quick health checks.
    public func totalFileCount() throws -> Int {
        try pool.read { db in
            try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files") ?? 0
        }
    }

    // MARK: - Engine-side person merging
    //
    // Mirror of `ReadStore.mergePersons` but inside the engine's own DB
    // pool — the VLM-driven face clustering pass calls this directly
    // instead of round-tripping through the app, since the engine is
    // already iterating clusters.

    /// Reassign every face_print of `sources` to `target`, delete source
    /// persons, recount target's file_count. Returns the new file_count.
    public func mergePersons(target: Int64, sources: [Int64]) async throws -> Int {
        let validSources = sources.filter { $0 != target }
        guard !validSources.isEmpty else {
            return try await pool.read { db in
                try Int.fetchOne(db, sql:
                    "SELECT file_count FROM persons WHERE id = ?",
                    arguments: [target]) ?? 0
            }
        }
        return try await pool.write { db in
            let placeholders = validSources.map { _ in "?" }.joined(separator: ",")
            var args: [DatabaseValueConvertible] = [target]
            args.append(contentsOf: validSources.map { Int($0) })
            try db.execute(
                sql: "UPDATE face_prints SET person_id = ? WHERE person_id IN (\(placeholders))",
                arguments: StatementArguments(args)
            )
            try db.execute(
                sql: "DELETE FROM persons WHERE id IN (\(placeholders))",
                arguments: StatementArguments(validSources.map { Int($0) })
            )
            try db.execute(sql: """
                UPDATE persons SET file_count = (
                    SELECT COUNT(DISTINCT file_id)
                    FROM face_prints
                    WHERE person_id = ?
                )
                WHERE id = ?
                """, arguments: [target, target])
            return try Int.fetchOne(db, sql:
                "SELECT file_count FROM persons WHERE id = ?",
                arguments: [target]) ?? 0
        }
    }
}
