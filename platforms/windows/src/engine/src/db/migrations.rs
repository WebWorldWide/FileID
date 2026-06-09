//! SQLite migration stack.
//!
//! Tracks applied migrations in `grdb_migrations` (same table name + format
//! as GRDB) so a database built by either engine can be opened by the
//! other. Identifiers MUST match the canonical strings byte-for-byte:
//!
//!   v1_core_tables, v2_clip_embeddings, v3_deep_analyze,
//!   v4_face_verifications, v5_person_naming_structured,
//!   v6_arcface_embeddings, v7_identity_anchors, v8_content_identity,
//!   v9_usn_state, v10_doc_text, v11_text_embeddings, v12_face_model_reset,
//!   v13_face_verification_anchors, v14_files_kind_scanned_index
//!
//! Migrations are append-only. NEVER edit a registered migration once
//! committed; add a new vN+1 migration instead.

use anyhow::{Context, Result};
use rusqlite::Connection;

const MIGRATION_TABLE_DDL: &str = r#"
    CREATE TABLE IF NOT EXISTS grdb_migrations (
        identifier TEXT NOT NULL PRIMARY KEY
    )
"#;

/// (identifier, sql) pairs in registration order. Identifiers must match
/// the Swift `m.registerMigration(...)` strings exactly.
fn registry() -> Vec<(&'static str, &'static str)> {
    vec![
        ("v1_core_tables",              V1_CORE_TABLES),
        ("v2_clip_embeddings",          V2_CLIP_EMBEDDINGS),
        ("v3_deep_analyze",             V3_DEEP_ANALYZE),
        ("v4_face_verifications",       V4_FACE_VERIFICATIONS),
        ("v5_person_naming_structured", V5_PERSON_NAMING_STRUCTURED),
        ("v6_arcface_embeddings",       V6_ARCFACE_EMBEDDINGS),
        ("v7_identity_anchors",         V7_IDENTITY_ANCHORS),
        ("v8_content_identity",         V8_CONTENT_IDENTITY),
        ("v9_usn_state",                V9_USN_STATE),
        ("v10_doc_text",                V10_DOC_TEXT),
        ("v11_text_embeddings",         V11_TEXT_EMBEDDINGS),
        ("v12_face_model_reset",        V12_FACE_MODEL_RESET),
        ("v13_face_verification_anchors", V13_FACE_VERIFICATION_ANCHORS),
        ("v14_files_kind_scanned_index", V14_FILES_KIND_SCANNED_INDEX),
    ]
}

/// v12: commercial-clean face-model swap (ArcFace 512-d → SFace 128-d). The
/// stored face embeddings are a different model AND dimension, so mixing them in
/// a clustering run is invalid. The user chose a clean reset over name-preserving
/// re-attach: wipe the face graph (verifications + prints + persons); the next
/// scan re-detects (YuNet), re-embeds (SFace), and re-clusters from scratch.
/// File/image rows are untouched. Children deleted before parents (FK-safe).
/// NOTE: macOS must register an identical `v12_face_model_reset` identifier with
/// equivalent SQL for cross-platform DB parity.
const V12_FACE_MODEL_RESET: &str = "
DELETE FROM face_verifications;
DELETE FROM face_prints;
DELETE FROM persons;
";

/// v13: stable face-anchor keys for user "different people" verdicts. The
/// face_verifications PK (person_a, person_b) churns on every re-cluster
/// (persons are dropped + re-inserted with fresh rowids), so a person-keyed
/// verdict silently stops suppressing a pair after the next clustering pass.
/// Add nullable face_a/face_b columns holding the (min,max) anchor face_print
/// ids — face_prints.id is stable across re-clustering — so
/// findMergeSuggestions can filter on a key that survives. Existing rows keep
/// NULL face ids and fall back to the legacy person-pair filter.
/// NOTE: macOS must register an identical `v13_face_verification_anchors`
/// identifier with equivalent SQL for cross-platform DB parity.
const V13_FACE_VERIFICATION_ANCHORS: &str = "
ALTER TABLE face_verifications ADD COLUMN face_a INTEGER;
ALTER TABLE face_verifications ADD COLUMN face_b INTEGER;
";

/// v14: composite index on (kind, scanned_at). The default Library grid and
/// any saved kind filter run `WHERE kind = ? ORDER BY scanned_at`, which the
/// single-column `index_files_on_kind` (v1) can satisfy for the filter but
/// not the order, so SQLite materialises a TEMP B-TREE to sort each refresh
/// (~55 ms at 50K rows). A composite key lets the planner walk the index in
/// scanned_at order within the kind partition, eliminating the sort.
/// NOTE: macOS must register an identical `v14_files_kind_scanned_index`
/// identifier with equivalent SQL for cross-platform DB parity.
const V14_FILES_KIND_SCANNED_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_files_kind_scanned ON files(kind, scanned_at);
";

/// Apply every registered migration that hasn't been applied yet, in
/// registration order, each in its own transaction.
pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(MIGRATION_TABLE_DDL).context("creating grdb_migrations")?;

    for (id, sql) in registry() {
        let already: bool = conn
            .query_row(
                "SELECT 1 FROM grdb_migrations WHERE identifier = ?1 LIMIT 1",
                [id],
                |_row| Ok(true),
            )
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(other),
            })
            .with_context(|| format!("checking migration {id}"))?;
        if already {
            continue;
        }
        tracing::info!(migration = id, "applying migration");
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)
            .with_context(|| format!("running migration {id}"))?;
        tx.execute(
            "INSERT INTO grdb_migrations (identifier) VALUES (?1)",
            [id],
        )?;
        tx.commit().with_context(|| format!("committing migration {id}"))?;
    }
    Ok(())
}

// ─── Migration SQL ──────────────────────────────────────────────────────────
//
// Each constant below mirrors the SQL the corresponding GRDB Swift
// migration produces. GRDB's `Database.ColumnType` enum holds UPPERCASE
// affinity strings (`INTEGER`, `TEXT`, `BLOB`, `DOUBLE`), and its
// `TableDefinition` DSL emits them verbatim — so we keep UPPERCASE here
// to match what `sqlite_master.sql` reads back as on a macOS-created DB.
// SQLite affinity matching is case-insensitive (column INTEGER and
// integer produce the same storage class) but the literal text in
// sqlite_master MUST agree across platforms so an ORM doing
// schema-text comparisons doesn't see drift.
//
// Verified against engine/Sources/FileIDEngine/Storage/Database.swift v7.

const V1_CORE_TABLES: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    path_text     TEXT    NOT NULL UNIQUE ON CONFLICT REPLACE,
    path_hash     INTEGER NOT NULL,
    bookmark      BLOB,
    size_bytes    INTEGER NOT NULL,
    created_at    DOUBLE,
    modified_at   DOUBLE,
    scanned_at    DOUBLE  NOT NULL,
    kind          TEXT    NOT NULL,
    extension     TEXT    NOT NULL,
    phash         INTEGER,
    aesthetic     DOUBLE,
    has_faces     INTEGER NOT NULL DEFAULT 0,
    has_text      INTEGER NOT NULL DEFAULT 0,
    camera_model  TEXT,
    location_lat  DOUBLE,
    location_lon  DOUBLE,
    failed        INTEGER NOT NULL DEFAULT 0,
    error_message TEXT
);

CREATE INDEX IF NOT EXISTS index_files_on_path_hash ON files(path_hash);
CREATE INDEX IF NOT EXISTS index_files_on_kind      ON files(kind);
CREATE INDEX IF NOT EXISTS idx_files_phash          ON files(phash) WHERE phash IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_files_scanned        ON files(scanned_at);

CREATE TABLE IF NOT EXISTS tags (
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    tag     TEXT    NOT NULL,
    source  TEXT    NOT NULL,
    score   DOUBLE,
    PRIMARY KEY (file_id, tag, source)
);
CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);

CREATE TABLE IF NOT EXISTS ocr_text (
    file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    text    TEXT    NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS ocr_fts USING fts5(
    text,
    content='ocr_text',
    content_rowid='file_id',
    tokenize='porter unicode61'
);

CREATE TABLE IF NOT EXISTS persons (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    name                     TEXT,
    representative_face_id   INTEGER,
    file_count               INTEGER NOT NULL DEFAULT 0,
    created_at               DOUBLE  NOT NULL
);

CREATE TABLE IF NOT EXISTS face_prints (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id    INTEGER NOT NULL REFERENCES files(id)   ON DELETE CASCADE,
    person_id  INTEGER          REFERENCES persons(id),
    print_data BLOB    NOT NULL,
    bbox       TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_face_person     ON face_prints(person_id);
CREATE INDEX IF NOT EXISTS idx_face_file       ON face_prints(file_id);
CREATE INDEX IF NOT EXISTS idx_person_repface  ON persons(representative_face_id);

CREATE TABLE IF NOT EXISTS scan_sessions (
    id              TEXT PRIMARY KEY,
    root_path       TEXT    NOT NULL,
    started_at      DOUBLE  NOT NULL,
    completed_at    DOUBLE,
    last_file_index INTEGER,
    total_files     INTEGER,
    status          TEXT    NOT NULL
);
"#;

const V2_CLIP_EMBEDDINGS: &str = r#"
CREATE TABLE IF NOT EXISTS clip_embeddings (
    file_id   INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    embedding BLOB    NOT NULL,
    model     TEXT    NOT NULL
);
"#;

const V3_DEEP_ANALYZE: &str = r#"
ALTER TABLE files ADD COLUMN vlm_description    TEXT;
ALTER TABLE files ADD COLUMN vlm_proposed_name  TEXT;
ALTER TABLE files ADD COLUMN vlm_model          TEXT;
ALTER TABLE files ADD COLUMN vlm_analyzed_at    DOUBLE;
"#;

const V4_FACE_VERIFICATIONS: &str = r#"
CREATE TABLE IF NOT EXISTS face_verifications (
    person_a    INTEGER NOT NULL,
    person_b    INTEGER NOT NULL,
    same_person INTEGER NOT NULL,
    confidence  DOUBLE  NOT NULL,
    vlm_model   TEXT    NOT NULL,
    verified_at DOUBLE  NOT NULL,
    PRIMARY KEY (person_a, person_b)
);
CREATE INDEX IF NOT EXISTS idx_face_verify_a ON face_verifications(person_a);
CREATE INDEX IF NOT EXISTS idx_face_verify_b ON face_verifications(person_b);
"#;

const V5_PERSON_NAMING_STRUCTURED: &str = r#"
ALTER TABLE persons ADD COLUMN title       TEXT;
ALTER TABLE persons ADD COLUMN first_name  TEXT;
ALTER TABLE persons ADD COLUMN middle_name TEXT;
ALTER TABLE persons ADD COLUMN last_name   TEXT;
ALTER TABLE persons ADD COLUMN suffix      TEXT;
ALTER TABLE persons ADD COLUMN is_unknown  INTEGER DEFAULT 0;

UPDATE persons
SET first_name = name
WHERE name IS NOT NULL AND name != '' AND first_name IS NULL;
"#;

const V6_ARCFACE_EMBEDDINGS: &str = r#"
ALTER TABLE face_prints ADD COLUMN arcface_embedding BLOB;
ALTER TABLE face_prints ADD COLUMN face_quality      DOUBLE;
ALTER TABLE face_prints ADD COLUMN excluded          INTEGER DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_face_prints_arcface_null
    ON face_prints(id)
    WHERE arcface_embedding IS NULL;
"#;

const V7_IDENTITY_ANCHORS: &str = r#"
ALTER TABLE persons ADD COLUMN centroid           BLOB;
ALTER TABLE persons ADD COLUMN anchor_radius      DOUBLE;
ALTER TABLE persons ADD COLUMN last_clustered_at  DOUBLE;
"#;

// v11: BGE-small (or future) text embeddings for documents — parallel to
// `clip_embeddings` (image space). Distinct table because the vector dim +
// model are different; the `model` column lets multiple text-embedding
// families coexist if we add another later.
const V11_TEXT_EMBEDDINGS: &str = r#"
CREATE TABLE IF NOT EXISTS text_embeddings (
    file_id   INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    embedding BLOB    NOT NULL,
    model     TEXT    NOT NULL
);
"#;

// v10: document text extracted by `pipeline::doc_extract` for txt/md/docx/
// pptx/xlsx (and a future Phase 4b pdf step). Mirrors the `ocr_text` /
// `ocr_fts` shape so the dbwriter inserts are near copy-paste and the
// existing search ranker can search `doc_fts` with the same query syntax.
const V10_DOC_TEXT: &str = r#"
CREATE TABLE IF NOT EXISTS doc_text (
    file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    text    TEXT    NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts USING fts5(
    text,
    content='doc_text',
    content_rowid='file_id',
    tokenize='porter unicode61'
);
"#;

// v9: per-volume USN journal cursor. Provisioned by the Phase 3 foundation;
// the (Windows-only) scan-driver integration that reads
// `FSCTL_READ_USN_JOURNAL` since `next_usn` and prunes the skip-set
// accordingly is a follow-up — until then this table stays unwritten and the
// scan uses the (always-allowed) `jwalk` + timestamp-skip path. `journal_id`
// detects deletion/recreation so a stale cursor never reads garbage.
const V9_USN_STATE: &str = r#"
CREATE TABLE IF NOT EXISTS usn_state (
    volume_id       TEXT    PRIMARY KEY,
    journal_id      INTEGER NOT NULL,
    next_usn        INTEGER NOT NULL,
    last_polled_at  DOUBLE  NOT NULL
);
"#;

// v8: rename / move identity. `content_hash` is BLAKE3 (full for ≤16 MB, head+
// tail+size composite above — see util::content_hash). `file_ref` is the
// platform's volume-local file id (NTFS MFT reference on Windows via
// GetFileInformationByHandle; inode on macOS). Both are nullable so a row
// missing them (e.g. an online-only OneDrive placeholder, or a permission
// error during the metadata pass) still inserts. The partial indexes keep the
// rename-heal lookup O(log n) without paying index cost on the NULL rows.
const V8_CONTENT_IDENTITY: &str = r#"
ALTER TABLE files ADD COLUMN content_hash BLOB;
ALTER TABLE files ADD COLUMN file_ref     INTEGER;

CREATE INDEX IF NOT EXISTS idx_files_content_hash
    ON files(content_hash)
    WHERE content_hash IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_files_file_ref
    ON files(file_ref)
    WHERE file_ref IS NOT NULL;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn fresh_database_applies_all_migrations() {
        let conn = Connection::open_in_memory().unwrap();
        for pragma in crate::db::SETUP_PRAGMAS {
            // mmap_size + journal_mode are no-ops in memory; just don't error.
            let _ = conn.execute_batch(pragma);
        }
        apply(&conn).expect("migrations apply");

        // grdb_migrations has 14 rows.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM grdb_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 14, "expected 14 applied migrations");

        // v13 added face_a + face_b to face_verifications (stable anchor keys).
        let verify_cols: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('face_verifications') WHERE name IN ('face_a', 'face_b')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(verify_cols, 2, "v13 must add face_a + face_b columns");

        // Spot-check schema cardinals: files has 23 columns including v3
        // additions (vlm_*) and v8 additions (content_hash, file_ref);
        // persons has 11 columns including v5 + v7 additions.
        let files_cols: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('files')", [], |r| r.get(0))
            .unwrap();
        assert!(files_cols >= 23, "files expected >= 23 columns, got {files_cols}");

        let persons_cols: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('persons')", [], |r| r.get(0))
            .unwrap();
        assert!(persons_cols >= 11, "persons expected >= 11 columns, got {persons_cols}");

        // FTS5 virtual table is reachable (insert + query).
        conn.execute_batch(r#"
            INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension)
              VALUES ('/test/a.png', 1, 100, 1.0, 'image', 'png');
            INSERT INTO ocr_text (file_id, text)
              VALUES ((SELECT id FROM files WHERE path_text = '/test/a.png'),
                      'the quick brown fox');
            INSERT INTO ocr_fts (rowid, text) VALUES (
                (SELECT id FROM files WHERE path_text = '/test/a.png'),
                'the quick brown fox'
            );
        "#).unwrap();

        let file_id: i64 = conn
            .query_row("SELECT id FROM files WHERE path_text = '/test/a.png'", [], |r| r.get(0))
            .unwrap();

        // Hit: matching word returns the file's rowid (== file_id).
        let matched_rowid: i64 = conn
            .query_row("SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH 'fox'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(matched_rowid, file_id, "FTS rowid must match files.id");

        // Miss: a word that isn't in the indexed text returns zero hits.
        let misses: i64 = conn
            .query_row("SELECT COUNT(*) FROM ocr_fts WHERE ocr_fts MATCH 'aardvark'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(misses, 0, "FTS must not match a word that isn't indexed");
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        apply(&conn).unwrap(); // second run is a no-op
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM grdb_migrations", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 14);
    }

    /// V10 added the `doc_text` + `doc_fts` (FTS5) pair, mirroring `ocr_text`
    /// /`ocr_fts`. Verify the table + virtual table both exist and the FTS5
    /// content_rowid is wired correctly (insert → match).
    #[test]
    fn v10_doc_text_and_fts_are_searchable() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();

        conn.execute_batch(
            r#"
            INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension)
              VALUES ('/test/doc.docx', 1, 100, 1.0, 'doc', 'docx');
            INSERT INTO doc_text (file_id, text) VALUES (
                (SELECT id FROM files WHERE path_text = '/test/doc.docx'),
                'quarterly revenue report and projections'
            );
            INSERT INTO doc_fts (rowid, text) VALUES (
                (SELECT id FROM files WHERE path_text = '/test/doc.docx'),
                'quarterly revenue report and projections'
            );
            "#,
        )
        .unwrap();

        let file_id: i64 = conn
            .query_row("SELECT id FROM files WHERE path_text = '/test/doc.docx'", [], |r| r.get(0))
            .unwrap();

        let hit: i64 = conn
            .query_row("SELECT rowid FROM doc_fts WHERE doc_fts MATCH 'revenue'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hit, file_id);

        let miss: i64 = conn
            .query_row("SELECT COUNT(*) FROM doc_fts WHERE doc_fts MATCH 'aardvark'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(miss, 0);
    }

    /// V9 provisioned the per-volume USN cursor table (foundation; reader
    /// integration is a follow-up). Verify the table + the key column exist.
    #[test]
    fn v9_usn_state_table_exists() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='usn_state'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "usn_state table missing");

        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('usn_state')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();
        for name in ["volume_id", "journal_id", "next_usn", "last_polled_at"] {
            assert!(cols.iter().any(|c| c == name), "usn_state.{name} missing");
        }
    }

    /// V8 added `content_hash` (BLOB) + `file_ref` (INTEGER) on `files` for
    /// rename/move identity. Verify the columns + their partial indexes exist.
    #[test]
    fn v8_adds_content_identity_columns_and_indexes() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();

        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('files')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();
        assert!(cols.iter().any(|c| c == "content_hash"), "files.content_hash missing");
        assert!(cols.iter().any(|c| c == "file_ref"), "files.file_ref missing");

        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name IN ('idx_files_content_hash', 'idx_files_file_ref')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 2, "both v8 partial indexes must exist");
    }

    /// V14 added the composite `(kind, scanned_at)` index so the kind-filtered
    /// Library grid avoids a TEMP B-TREE sort. Verify the index exists and that
    /// the planner uses it (no SCAN / TEMP B-TREE) for the grid query shape.
    #[test]
    fn v14_adds_kind_scanned_composite_index() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();

        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name = 'idx_files_kind_scanned'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1, "v14 composite index must exist");

        let plan: Vec<String> = conn
            .prepare(
                "EXPLAIN QUERY PLAN \
                 SELECT id FROM files WHERE kind = 'image' AND failed = 0 \
                 ORDER BY scanned_at DESC LIMIT 200",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .flatten()
            .collect();
        let joined = plan.join(" | ");
        assert!(
            joined.contains("idx_files_kind_scanned"),
            "grid query should use the composite index, got: {joined}"
        );
        assert!(
            !joined.to_ascii_uppercase().contains("TEMP B-TREE"),
            "grid query must not materialise a TEMP B-TREE sort, got: {joined}"
        );
    }
}
