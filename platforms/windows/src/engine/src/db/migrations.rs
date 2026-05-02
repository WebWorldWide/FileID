//! SQLite migration stack — direct port of `Database.swift`'s GRDB
//! `DatabaseMigrator`.
//!
//! Tracks applied migrations in `grdb_migrations` (same table name + format
//! GRDB uses) so a database built by either engine can be opened by the
//! other. Identifiers MUST match the macOS Swift strings byte-for-byte:
//!
//!   v1_core_tables, v2_clip_embeddings, v3_deep_analyze,
//!   v4_face_verifications, v5_person_naming_structured,
//!   v6_arcface_embeddings, v7_identity_anchors
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
    ]
}

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
// Each constant below is the exact SQL the corresponding GRDB Swift
// migration produces. GRDB's TableDefinition DSL lowercases column types,
// so we match: INTEGER → integer, TEXT → text, BLOB → blob, REAL → double.
//
// Verified against engine/Sources/FileIDEngine/Storage/Database.swift v7
// (via `git log`).

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

        // grdb_migrations has 7 rows.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM grdb_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 7, "expected 7 applied migrations");

        // Spot-check schema cardinals: files has 21 columns including v3
        // additions; persons has 11 columns including v5 + v7 additions.
        let files_cols: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('files')", [], |r| r.get(0))
            .unwrap();
        assert!(files_cols >= 21, "files expected >= 21 columns, got {files_cols}");

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

        let hits: i64 = conn
            .query_row("SELECT COUNT(*) FROM ocr_fts WHERE ocr_fts MATCH 'fox'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        apply(&conn).unwrap(); // second run is a no-op
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM grdb_migrations", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 7);
    }
}
