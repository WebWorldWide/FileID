//! SQLite migration stack, tracked in `grdb_migrations` (same table + identifier format as GRDB) so catalogs written by
//! the Swift engine still open.
//!
//! Registered identifiers must match the canonical list byte-for-byte, and migrations are append-only: never edit one
//! once committed, add vN+1.

use anyhow::{Context, Result};
use rusqlite::Connection;

const MIGRATION_TABLE_DDL: &str = r#"
    CREATE TABLE IF NOT EXISTS grdb_migrations (
        identifier TEXT NOT NULL PRIMARY KEY
    )
"#;

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
        ("v15_fts_sync_triggers",        V15_FTS_SYNC_TRIGGERS),
        ("v16_path_search",              V16_PATH_SEARCH),
        ("v17_face_verification_stable_keys", V17_FACE_VERIFICATION_STABLE_KEYS),
        ("v18_restructure_feedback",     V18_RESTRUCTURE_FEEDBACK),
        ("v19_files_text_stage_done",    V19_FILES_TEXT_STAGE_DONE),
        ("v20_vlm_full_model",           V20_VLM_FULL_MODEL),
        ("v21_catalog", include_str!("../../../../../../shared/catalog/v21.sql")),
        ("v22_face_cache_provenance", include_str!("../../../../../../shared/catalog/v22.sql")),
        ("v23_vector_change_tracking", include_str!("../../../../../../shared/catalog/v23.sql")),
        ("v24_face_refresh_failures", include_str!("../../../../../../shared/catalog/v24.sql")),
        ("v25_ocr_text_source", include_str!("../../../../../../shared/catalog/v25.sql")),
        ("v26_vlm_inherited_from", include_str!("../../../../../../shared/catalog/v26.sql")),
        ("v27_chat_retrieval", include_str!("../../../../../../shared/catalog/v27.sql")),
        ("v28_take_quality", include_str!("../../../../../../shared/catalog/v28.sql")),
        ("v29_library_roots", include_str!("../../../../../../shared/catalog/v29.sql")),
        ("v30_files_dir_index", include_str!("../../../../../../shared/catalog/v30.sql")),
        ("v31_take_proposals", include_str!("../../../../../../shared/catalog/v31.sql")),
        ("v32_face_review", include_str!("../../../../../../shared/catalog/v32.sql")),
        ("v33_face_queues", include_str!("../../../../../../shared/catalog/v33.sql")),
        ("v34_scan_catchup", include_str!("../../../../../../shared/catalog/v34.sql")),
        ("v35_foreign_key_indexes", include_str!("../../../../../../shared/catalog/v35.sql")),
        ("v36_scan_failures", include_str!("../../../../../../shared/catalog/v36.sql")),
        ("v37_files_size_index", include_str!("../../../../../../shared/catalog/v37.sql")),
        ("v38_files_name_sort", include_str!("../../../../../../shared/catalog/v38.sql")),
        ("v39_files_search_fts", include_str!("../../../../../../shared/catalog/v39.sql")),
    ]
}

/// v12: face-model swap (ArcFace 512-d → SFace 128-d). The two models' embeddings are incompatible, so the face graph
/// is wiped (children before parents); file and image rows are untouched.
const V12_FACE_MODEL_RESET: &str = "
DELETE FROM face_verifications;
DELETE FROM face_prints;
DELETE FROM persons;
";

/// v13: face_a/face_b hold stable face_print ids for "different people" verdicts; person rowids churn on every
/// re-cluster.
const V13_FACE_VERIFICATION_ANCHORS: &str = "
ALTER TABLE face_verifications ADD COLUMN face_a INTEGER;
ALTER TABLE face_verifications ADD COLUMN face_b INTEGER;
";

/// v14: composite (kind, scanned_at) index so kind-filtered Library queries avoid a TEMP B-TREE sort.
const V14_FILES_KIND_SCANNED_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_files_kind_scanned ON files(kind, scanned_at);
";

/// v15: external-content FTS5 sync triggers. Writers use explicit DELETE + INSERT: REPLACE's implicit delete skips
/// AFTER DELETE triggers unless recursive_triggers is on.
const V15_FTS_SYNC_TRIGGERS: &str = "
INSERT INTO ocr_fts(ocr_fts) VALUES('rebuild');
INSERT INTO doc_fts(doc_fts) VALUES('rebuild');
CREATE TRIGGER IF NOT EXISTS ocr_text_ai AFTER INSERT ON ocr_text BEGIN
    INSERT INTO ocr_fts(rowid, text) VALUES (new.file_id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS ocr_text_ad AFTER DELETE ON ocr_text BEGIN
    INSERT INTO ocr_fts(ocr_fts, rowid, text) VALUES ('delete', old.file_id, old.text);
END;
CREATE TRIGGER IF NOT EXISTS ocr_text_au AFTER UPDATE ON ocr_text BEGIN
    INSERT INTO ocr_fts(ocr_fts, rowid, text) VALUES ('delete', old.file_id, old.text);
    INSERT INTO ocr_fts(rowid, text) VALUES (new.file_id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS doc_text_ai AFTER INSERT ON doc_text BEGIN
    INSERT INTO doc_fts(rowid, text) VALUES (new.file_id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS doc_text_ad AFTER DELETE ON doc_text BEGIN
    INSERT INTO doc_fts(doc_fts, rowid, text) VALUES ('delete', old.file_id, old.text);
END;
CREATE TRIGGER IF NOT EXISTS doc_text_au AFTER UPDATE ON doc_text BEGIN
    INSERT INTO doc_fts(doc_fts, rowid, text) VALUES ('delete', old.file_id, old.text);
    INSERT INTO doc_fts(rowid, text) VALUES (new.file_id, new.text);
END;
";

/// v16: LIKE compares bytes, so NFD-stored names never match an NFC query; `path_search` holds the NFC form of
/// `path_text`.
const V16_PATH_SEARCH: &str = "
ALTER TABLE files ADD COLUMN path_search TEXT;
UPDATE files SET path_search = path_text;
";

/// v17: face_print ids are re-inserted on every faces_evaluated re-scan; (file_id, bbox) identifies the same physical
/// face.
const V17_FACE_VERIFICATION_STABLE_KEYS: &str = "
ALTER TABLE face_verifications ADD COLUMN file_a INTEGER;
ALTER TABLE face_verifications ADD COLUMN bbox_a TEXT;
ALTER TABLE face_verifications ADD COLUMN file_b INTEGER;
ALTER TABLE face_verifications ADD COLUMN bbox_b TEXT;
UPDATE face_verifications SET
    file_a = (SELECT file_id FROM face_prints WHERE id = face_a),
    bbox_a = (SELECT bbox    FROM face_prints WHERE id = face_a),
    file_b = (SELECT file_id FROM face_prints WHERE id = face_b),
    bbox_b = (SELECT bbox    FROM face_prints WHERE id = face_b)
WHERE face_a IS NOT NULL AND face_b IS NOT NULL;
";

/// v18: token→folder co-occurrence memory for restructure learning; applied moves credit each filename token toward the
/// destination folder.
const V18_RESTRUCTURE_FEEDBACK: &str = "
CREATE TABLE IF NOT EXISTS restructure_feedback (
    token      TEXT    NOT NULL,
    folder     TEXT    NOT NULL,
    weight     INTEGER NOT NULL DEFAULT 1,
    updated_at DOUBLE  NOT NULL DEFAULT 0,
    PRIMARY KEY (token, folder)
);
CREATE INDEX IF NOT EXISTS idx_restructure_feedback_token ON restructure_feedback(token);
";

// v19: marks a doc/pdf text-extraction stage as run, so the BGE backfill stops re-walking docs with no embeddable text.
const V19_FILES_TEXT_STAGE_DONE: &str = "
ALTER TABLE files ADD COLUMN text_stage_done INTEGER NOT NULL DEFAULT 0;
";

const V20_VLM_FULL_MODEL: &str = "
ALTER TABLE files ADD COLUMN vlm_full_model TEXT;
";

#[derive(Debug)]
pub struct NewerThanEngine {
    pub unknown: Vec<String>,
}

impl std::fmt::Display for NewerThanEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "db_newer_than_engine: database was migrated by a newer FileID \
             version (unknown migrations: {}) — refusing to open for writing",
            self.unknown.join(", ")
        )
    }
}

impl std::error::Error for NewerThanEngine {}

/// Applies every registered migration not yet applied, in registration order, each in its own transaction.
pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(MIGRATION_TABLE_DDL).context("creating grdb_migrations")?;

    // A DB stamped by a NEWER engine carries unknown identifiers; writing could silently break its schema invariants.
    // Refuse to open.
    let known: Vec<&str> = registry().iter().map(|(id, _)| *id).collect();
    let applied: Vec<String> = conn
        .prepare("SELECT identifier FROM grdb_migrations")?
        .query_map([], |row| row.get(0))?
        .collect::<std::result::Result<_, _>>()
        .context("listing applied migrations")?;
    let unknown: Vec<String> = applied
        .iter()
        .filter(|id| !known.contains(&id.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(NewerThanEngine { unknown }.into());
    }

    for (id, sql) in registry() {
        if applied.iter().any(|a| a.as_str() == id) {
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
    super::name_sort::fill_missing(conn).context("keying file names for sorting")?;
    Ok(())
}

// Uppercase affinity names: sqlite_master text must match the GRDB-created schema byte-for-byte across platforms.

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

// Own table: the text vector dim and model differ from clip_embeddings; `model` lets several text families coexist.
const V11_TEXT_EMBEDDINGS: &str = r#"
CREATE TABLE IF NOT EXISTS text_embeddings (
    file_id   INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    embedding BLOB    NOT NULL,
    model     TEXT    NOT NULL
);
"#;

// v10: doc text shares the ocr_text/ocr_fts shape so dbwriter inserts and the search ranker reuse the same query syntax.
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

// v9: per-volume USN journal cursor. Never written: scans use the `jwalk` +
// timestamp-skip path. Kept because applied migrations are immutable.
const V9_USN_STATE: &str = r#"
CREATE TABLE IF NOT EXISTS usn_state (
    volume_id       TEXT    PRIMARY KEY,
    journal_id      INTEGER NOT NULL,
    next_usn        INTEGER NOT NULL,
    last_polled_at  DOUBLE  NOT NULL
);
"#;

// v8: rename/move identity. content_hash is the cross-platform recipe in util::content_hash; the rename-heal also matches the pre-interior-sample recipe-v1 digest and re-stamps it. file_ref is the volume-local file id; both nullable, partial indexes skip NULL rows.
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
    fn library_roots_are_upserted_once_per_path_and_stamped_on_finish() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        for (session, started) in [("s1", 10.0), ("s2", 20.0)] {
            conn.execute("INSERT INTO scan_sessions (id, root_path, started_at, status) VALUES (?1, '/Volumes/Adlon', ?2, 'running')", rusqlite::params![session, started]).unwrap();
            conn.execute("INSERT INTO library_roots(path,display,kind,added_at,last_status) VALUES(?1,?1,'external',?2,'running') ON CONFLICT(path) DO UPDATE SET last_status='running'", rusqlite::params!["/Volumes/Adlon", started]).unwrap();
        }
        conn.execute("UPDATE library_roots SET last_scan_at=?1,last_status=?2 WHERE path=(SELECT root_path FROM scan_sessions WHERE id=?3)", rusqlite::params![30.0, "completed", "s2"]).unwrap();
        let (count, added, status): (i64, f64, String) = conn
            .query_row("SELECT COUNT(*), MIN(added_at), MIN(last_status) FROM library_roots", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!((count, added, status.as_str()), (1, 10.0, "completed"));
    }

    #[test]
    fn a_catalog_written_before_the_name_sort_key_opens_and_every_row_gets_one() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_TABLE_DDL).unwrap();
        for (id, sql) in registry().into_iter().take_while(|(id, _)| *id != "v38_files_name_sort") {
            conn.execute_batch(sql).unwrap();
            conn.execute("INSERT INTO grdb_migrations (identifier) VALUES (?1)", [id]).unwrap();
        }
        for id in 1..=12_000_i64 {
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?1,?2,?1,1,0,'image','jpg')",
                rusqlite::params![id, format!("/Photos/IMG_{id}.jpg")],
            )
            .unwrap();
        }
        apply(&conn).unwrap();
        let unkeyed: i64 = conn.query_row("SELECT COUNT(*) FROM files WHERE name_sort = ''", [], |r| r.get(0)).unwrap();
        assert_eq!(unkeyed, 0, "the migration keys every existing row");
        let first: i64 = conn
            .query_row("SELECT id FROM files ORDER BY name_sort, id LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(first, 1, "IMG_1 sorts before IMG_2 and IMG_10 sorts after IMG_9");
        let plan: String = conn
            .prepare("EXPLAIN QUERY PLAN SELECT id FROM files WHERE failed = 0 ORDER BY name_sort, id LIMIT 200")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join(" | ");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }

    #[test]
    fn the_file_search_index_finds_substrings_and_follows_inserts_renames_and_deletes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_TABLE_DDL).unwrap();
        for (id, sql) in registry().into_iter().take_while(|(id, _)| *id != "v39_files_search_fts") {
            conn.execute_batch(sql).unwrap();
            conn.execute("INSERT INTO grdb_migrations (identifier) VALUES (?1)", [id]).unwrap();
        }
        let add = |id: i64, path: &str, description: Option<&str>| {
            conn.execute(
                "INSERT INTO files(id,path_text,path_search,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(?1,?2,?2,?1,1,0,'image','jpg',?3)",
                rusqlite::params![id, path, description],
            )
            .unwrap();
        };
        add(1, "/lib/SunsetBeach.jpg", None);
        add(2, "/lib/other.jpg", Some("two dogs on the beach at dusk"));
        apply(&conn).unwrap();
        let hits = |needle: &str| -> Vec<i64> {
            conn.prepare("SELECT rowid FROM files_search_fts WHERE files_search_fts MATCH ?1 ORDER BY rowid")
                .unwrap()
                .query_map([format!("\"{needle}\"")], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(hits("beach"), vec![1, 2], "a partial name and a description both match, whatever the case");
        add(3, "/lib/Beachball.png", None);
        assert_eq!(hits("each"), vec![1, 2, 3]);
        conn.execute("UPDATE files SET path_search = '/lib/harbor.jpg', path_text = '/lib/harbor.jpg' WHERE id = 1", []).unwrap();
        assert_eq!(hits("beach"), vec![2, 3]);
        conn.execute("DELETE FROM files WHERE id = 2", []).unwrap();
        assert_eq!(hits("beach"), vec![3]);
    }

    #[test]
    fn faces_already_in_a_catalog_are_marked_baseline_so_they_never_come_back_as_new() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_TABLE_DDL).unwrap();
        for (id, sql) in registry().into_iter().take_while(|(id, _)| *id != "v32_face_review") {
            conn.execute_batch(sql).unwrap();
            conn.execute("INSERT INTO grdb_migrations (identifier) VALUES (?1)", [id]).unwrap();
        }
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,'/a.jpg',1,1,0,'image','jpg')", []).unwrap();
        conn.execute(
            "INSERT INTO face_prints(id,file_id,print_data,bbox,embedding_model,processing_version) VALUES(1,1,X'00','0,0,1,1','sface','macos-v3'),(2,1,X'00','0,0,.5,.5',NULL,NULL)",
            [],
        )
        .unwrap();
        apply(&conn).unwrap();
        let rows: Vec<(i64, String, String)> = conn
            .prepare("SELECT face_id,namespace,outcome FROM face_assignment_assessed ORDER BY face_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![(1, "sface|macos-v3".into(), "baseline".into()), (2, "|".into(), "baseline".into())]);
        conn.execute("INSERT INTO face_prints(id,file_id,print_data,bbox) VALUES(3,1,X'00','0,0,.2,.2')", []).unwrap();
        let assessed: i64 = conn.query_row("SELECT COUNT(*) FROM face_assignment_assessed WHERE face_id=3", [], |r| r.get(0)).unwrap();
        assert_eq!(assessed, 0, "a face found after the migration is new");
    }

    #[test]
    fn fresh_database_applies_all_migrations() {
        let conn = Connection::open_in_memory().unwrap();
        for pragma in crate::db::SETUP_PRAGMAS {
            // mmap_size + journal_mode are no-ops in memory; just don't error.
            let _ = conn.execute_batch(pragma);
        }
        apply(&conn).expect("migrations apply");

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM grdb_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 39, "expected 39 applied migrations");

        let full_model_column: (String, i64, Option<String>) = conn
            .query_row(
                "SELECT type, [notnull], dflt_value \
                 FROM pragma_table_info('files') WHERE name='vlm_full_model'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(full_model_column, ("TEXT".into(), 0, None));

        let verify_cols: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('face_verifications') WHERE name IN ('face_a', 'face_b')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(verify_cols, 2, "v13 must add face_a + face_b columns");

        let stable_cols: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('face_verifications') \
                 WHERE name IN ('file_a', 'bbox_a', 'file_b', 'bbox_b')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stable_cols, 4, "v17 must add file_a/bbox_a/file_b/bbox_b columns");

        let files_cols: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('files')", [], |r| r.get(0))
            .unwrap();
        assert!(files_cols >= 23, "files expected >= 23 columns, got {files_cols}");

        let persons_cols: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('persons')", [], |r| r.get(0))
            .unwrap();
        assert!(persons_cols >= 11, "persons expected >= 11 columns, got {persons_cols}");

        conn.execute_batch(r#"
            INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension)
              VALUES ('/test/a.png', 1, 100, 1.0, 'image', 'png');
            INSERT INTO ocr_text (file_id, text)
              VALUES ((SELECT id FROM files WHERE path_text = '/test/a.png'),
                      'the quick brown fox');
        "#).unwrap();

        let file_id: i64 = conn
            .query_row("SELECT id FROM files WHERE path_text = '/test/a.png'", [], |r| r.get(0))
            .unwrap();

        let matched_rowid: i64 = conn
            .query_row("SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH 'fox'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(matched_rowid, file_id, "FTS rowid must match files.id");

        let misses: i64 = conn
            .query_row("SELECT COUNT(*) FROM ocr_fts WHERE ocr_fts MATCH 'aardvark'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(misses, 0, "FTS must not match a word that isn't indexed");
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        apply(&conn).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM grdb_migrations", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 39);
    }

    /// A "different people" verdict's churn-stable (file_id, bbox) key must still resolve to the current face after a
    /// faces_evaluated re-scan DELETE+re-INSERTs face_prints with fresh ids (the face_a/face_b id is then dangling).
    #[test]
    fn verdict_stable_key_survives_face_print_churn() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES \
             (1, '/a.jpg', 1, 100, 0, 'image', 'jpg'), \
             (2, '/b.jpg', 2, 100, 0, 'image', 'jpg')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO face_prints (id, file_id, print_data, bbox) VALUES \
             (10, 1, x'00', '0.1,0.1,0.2,0.2'), (20, 2, x'00', '0.3,0.3,0.2,0.2')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO face_verifications \
                (person_a, person_b, same_person, confidence, vlm_model, verified_at, \
                 face_a, face_b, file_a, bbox_a, file_b, bbox_b) \
             VALUES (1, 2, 0, 1.0, 'user-verified', 0, 10, 20, 1, '0.1,0.1,0.2,0.2', 2, '0.3,0.3,0.2,0.2')",
            [],
        ).unwrap();
        conn.execute("DELETE FROM face_prints", []).unwrap();
        conn.execute(
            "INSERT INTO face_prints (id, file_id, print_data, bbox) VALUES \
             (101, 1, x'00', '0.1,0.1,0.2,0.2'), (202, 2, x'00', '0.3,0.3,0.2,0.2')",
            [],
        ).unwrap();
        let legacy_resolves: i64 = conn
            .query_row("SELECT COUNT(*) FROM face_prints WHERE id IN (10, 20)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(legacy_resolves, 0, "churn must invalidate the legacy face ids");
        let new_a: i64 = conn
            .query_row(
                "SELECT id FROM face_prints WHERE file_id = \
                   (SELECT file_a FROM face_verifications) \
                 AND bbox = (SELECT bbox_a FROM face_verifications) LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new_b: i64 = conn
            .query_row(
                "SELECT id FROM face_prints WHERE file_id = \
                   (SELECT file_b FROM face_verifications) \
                 AND bbox = (SELECT bbox_b FROM face_verifications) LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(new_a, 101, "stable key A must resolve to the re-scanned face");
        assert_eq!(new_b, 202, "stable key B must resolve to the re-scanned face");
    }

    /// A DB stamped by a newer engine must refuse to open rather than silently write into a schema this build doesn't
    /// know.
    #[test]
    fn newer_db_with_unknown_migration_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        conn.execute(
            "INSERT INTO grdb_migrations (identifier) VALUES ('v99_from_the_future')",
            [],
        )
        .unwrap();
        let err = apply(&conn).unwrap_err();
        assert!(err.to_string().contains("db_newer_than_engine"));
        assert!(err.to_string().contains("v99_from_the_future"));
        assert!(err.downcast_ref::<NewerThanEngine>().is_some());
    }

    /// The migration chains once forked at v14 ("v14_fts_sync_triggers" vs "v14_files_kind_scanned_index"), which made a
    /// library touched on macOS fail every Windows scan with SQLITE_CORRUPT. The canonical list is pinned here and
    /// against a catalog the Swift engine wrote (see `catalog_the_swift_engine_wrote_matches_the_migrations`).
    #[test]
    fn migration_identifiers_match_canonical_list() {
        const CANONICAL: [&str; 39] = [
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
            "v21_catalog",
            "v22_face_cache_provenance",
            "v23_vector_change_tracking",
            "v24_face_refresh_failures",
            "v25_ocr_text_source",
            "v26_vlm_inherited_from",
            "v27_chat_retrieval",
            "v28_take_quality",
            "v29_library_roots",
            "v30_files_dir_index",
            "v31_take_proposals",
            "v32_face_review",
            "v33_face_queues",
            "v34_scan_catchup",
            "v35_foreign_key_indexes",
            "v36_scan_failures",
            "v37_files_size_index",
            "v38_files_name_sort",
            "v39_files_search_fts",
        ];
        let ids: Vec<&str> = registry().iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, CANONICAL, "migration identifiers must match the canonical cross-platform list");
    }

    /// A catalog the retired Swift engine wrote (GRDB, its 36 migrations) names
    /// the first migrations registered here and holds the same tables and
    /// indexes (index names differ for `files_on_kind`/`files_on_path_hash`).
    #[test]
    fn catalog_the_swift_engine_wrote_matches_the_migrations() {
        let swift = crate::db::test_support::swift_catalog("swift-written-chat-history");
        let names = |conn: &Connection, sql: &str| -> Vec<String> {
            let mut statement = conn.prepare(sql).unwrap();
            statement.query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        let swift_migrations = names(&swift.conn, "SELECT identifier FROM grdb_migrations ORDER BY rowid");
        let ids: Vec<String> = registry().iter().map(|(id, _)| (*id).to_string()).collect();
        assert_eq!(swift_migrations, ids[..swift_migrations.len()]);

        let fresh = Connection::open_in_memory().unwrap();
        apply(&fresh).unwrap();
        let tables = "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name";
        assert_eq!(names(&swift.conn, tables), names(&fresh, tables));
    }

    #[test]
    fn library_size_sort_and_duplicate_size_classes_walk_an_index() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        let plan = |sql: &str| -> String {
            let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let details: Vec<String> =
                statement.query_map([], |row| row.get::<_, String>(3)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
            details.join(" | ")
        };
        let sorted = plan("SELECT id FROM files WHERE failed = 0 ORDER BY size_bytes DESC, id DESC LIMIT 200");
        assert!(sorted.contains("files_on_size") && !sorted.contains("TEMP B-TREE"), "{sorted}");
        let added = plan("SELECT id FROM files WHERE failed = 0 ORDER BY scanned_at DESC, id DESC LIMIT 200");
        assert!(added.contains("idx_files_scanned") && !added.contains("TEMP B-TREE"), "{added}");
        for (direction, index) in [("DESC", "files_on_taken_newest"), ("ASC", "files_on_taken_oldest")] {
            let taken = plan(&format!(
                "SELECT id FROM files WHERE failed = 0 ORDER BY (COALESCE(created_at, modified_at) IS NULL), \
                 COALESCE(created_at, modified_at) {direction}, id {direction} LIMIT 200"
            ));
            assert!(taken.contains(index) && !taken.contains("TEMP B-TREE"), "{taken}");
        }
        let classes =
            plan("SELECT size_bytes, COUNT(*) FROM files WHERE failed = 0 AND size_bytes >= 0 GROUP BY size_bytes HAVING COUNT(*) > 1");
        assert!(classes.contains("INDEX files_on_size"), "{classes}");
    }

    #[test]
    fn v15_installs_fts_sync_triggers() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();

        let triggers: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name IN \
                 ('ocr_text_ai','ocr_text_ad','ocr_text_au', \
                  'doc_text_ai','doc_text_ad','doc_text_au')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(triggers, 6, "all six v15 sync triggers must exist");
    }

    #[test]
    fn v16_adds_and_backfills_path_search() {
        let conn = Connection::open_in_memory().unwrap();
        for (id, sql) in registry() {
            if id == "v16_path_search" {
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute(
            "INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension) \
             VALUES ('C:\\test\\legacy.png', 1, 100, 1.0, 'image', 'png')",
            [],
        )
        .unwrap();
        conn.execute_batch(V16_PATH_SEARCH).unwrap();

        let backfilled: String = conn
            .query_row(
                "SELECT path_search FROM files WHERE path_text = 'C:\\test\\legacy.png'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(backfilled, "C:\\test\\legacy.png", "v16 must backfill path_search from path_text");
    }

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

    fn uncovered_foreign_keys(conn: &Connection) -> Vec<String> {
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND sql NOT LIKE 'CREATE VIRTUAL%' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        let mut missing = Vec::new();
        for table in tables {
            let keys: Vec<String> = conn
                .prepare("SELECT \"from\" FROM pragma_foreign_key_list(?1) WHERE seq=0")
                .unwrap()
                .query_map([&table], |r| r.get(0))
                .unwrap()
                .flatten()
                .collect();
            let mut leading: Vec<String> = conn
                .prepare(
                    "SELECT i.name FROM pragma_index_list(?1) l, pragma_index_info(l.name) i WHERE i.seqno=0",
                )
                .unwrap()
                .query_map([&table], |r| r.get::<_, Option<String>>(0))
                .unwrap()
                .flatten()
                .flatten()
                .collect();
            let rowid: Vec<String> = conn
                .prepare("SELECT name FROM pragma_table_info(?1) WHERE pk=1 AND type='INTEGER' AND (SELECT COUNT(*) FROM pragma_table_info(?1) WHERE pk>0)=1")
                .unwrap()
                .query_map([&table], |r| r.get(0))
                .unwrap()
                .flatten()
                .collect();
            leading.extend(rowid);
            for key in keys {
                if !leading.contains(&key) {
                    missing.push(format!("{table}.{key}"));
                }
            }
        }
        missing
    }

    #[test]
    fn v35_indexes_every_foreign_key_column() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        assert_eq!(uncovered_foreign_keys(&conn), Vec::<String>::new());
    }

    fn plan(conn: &Connection, sql: &str) -> Vec<String> {
        conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .flatten()
            .collect()
    }

    #[test]
    fn v35_invalidation_and_file_deletes_search_instead_of_scanning() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        let trigger: String = conn
            .query_row("SELECT sql FROM sqlite_master WHERE type='trigger' AND name='catalog_invalidate'", [], |r| r.get(0))
            .unwrap();
        let body = trigger.split_once("BEGIN").unwrap().1.rsplit_once("END").unwrap().0;
        let statements: Vec<String> = body
            .split(';')
            .map(|s| s.trim().replace("new.id", "7"))
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(statements.len(), 8, "catalog_invalidate changed; re-check its indexes");
        for statement in &statements {
            let steps = plan(&conn, statement);
            assert!(!steps.is_empty(), "{statement}");
            for step in steps {
                assert!(step.starts_with("SEARCH"), "{statement} => {step}");
            }
        }

        let children: Vec<(String, String)> = conn
            .prepare(
                "SELECT m.name, f.\"from\" FROM sqlite_master m, pragma_foreign_key_list(m.name) f \
                 WHERE m.type='table' AND f.\"table\" IN ('files','persons','face_prints') ORDER BY 1,2",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .flatten()
            .collect();
        assert!(children.len() > 30, "{children:?}");
        for (table, column) in children {
            for step in plan(&conn, &format!("SELECT 1 FROM {table} WHERE {column}=7")) {
                assert!(step.starts_with("SEARCH"), "{table}.{column} => {step}");
            }
        }

        for (name, table) in [
            ("catalog_observations_file", "catalog_observations"),
            ("catalog_embeddings_file", "catalog_embeddings"),
            ("catalog_coverage_file", "catalog_coverage"),
            ("catalog_take_scores_file", "catalog_take_scores"),
            ("catalog_event_files_file", "catalog_event_files"),
            ("catalog_assets_derived", "catalog_assets"),
            ("catalog_corrections_file", "catalog_corrections"),
            ("face_identity_constraints_anchor", "face_identity_constraints"),
            ("face_queue_dismissals_file", "face_queue_dismissals"),
            ("face_queue_dismissals_other", "face_queue_dismissals"),
            ("face_review_candidates_runner_up", "face_review_candidates"),
        ] {
            let found: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1 AND tbl_name=?2",
                    [name, table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "{name}");
        }
    }

    #[test]
    fn v35_deleting_a_file_still_cascades() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             INSERT INTO files (id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES
              (1,'/a.jpg',1,100,0,'image','jpg'),(2,'/b.jpg',2,100,0,'image','jpg');
             INSERT INTO catalog_events(id,title) VALUES('e','t');
             INSERT INTO catalog_event_files VALUES('e',1),('e',2);
             INSERT INTO catalog_assets VALUES(2,1,'proxy','{}');
             INSERT INTO catalog_corrections VALUES('c',1,'k','{}','{}',0);
             INSERT INTO catalog_embeddings VALUES('1','file',1,'m',1,x'00000000','r');
             INSERT INTO face_identity_constraints(file_id,bbox,anchor_file_id,anchor_bbox,source,created_at) VALUES(2,'b',1,'a','review',0);
             INSERT INTO face_queue_dismissals(kind,file_id,bbox,other_file_id,other_bbox,created_at) VALUES('merge',2,'b',1,'a',0);
             DELETE FROM files WHERE id=1;",
        )
        .unwrap();
        let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count("SELECT COUNT(*) FROM catalog_event_files"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM catalog_assets"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM catalog_corrections WHERE file_id IS NULL"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM catalog_embeddings"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM face_identity_constraints"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM face_queue_dismissals"), 0);
    }

    /// A v35 catalog holding failure-memory rows (`stages = 4`), owed rows and
    /// a row of each kind is migrated by v36 into `scan_failures`.
    #[test]
    fn v36_moves_failure_memory_out_of_scan_catchup() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        let mut v36 = "";
        for (id, sql) in registry() {
            if id == "v36_scan_failures" {
                v36 = sql;
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO files (id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed) VALUES
              (1,'/a.jpg',1,100,0,'image','jpg',1),(2,'/b.jpg',2,200,0,'image','jpg',0),
              (3,'/c.jpg',3,300,0,'image','jpg',1),(4,'/d.jpg',4,400,0,'image','jpg',0);
             INSERT INTO scan_catchup(file_id,stages,size_bytes,modified_at,vision,face_count,attempts,created_at) VALUES
              (1,4,100,10.5,'{\"failedBy\":\"fileid-engine 1.2.3 apple\"}',0,3,77.0),
              (2,1,200,20.0,'{\"k\":[],\"c\":[],\"u\":false}',2,1,5.0),
              (3,4,300,NULL,'not json',0,2,78.0),
              (4,6,400,40.0,NULL,0,0,6.0);",
        )
        .unwrap();
        conn.execute_batch(v36).unwrap();
        let failures: Vec<(i64, i64, Option<f64>, String, i64, f64)> = conn
            .prepare("SELECT file_id,size_bytes,modified_at,engine_build,attempts,failed_at FROM scan_failures ORDER BY file_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            failures,
            vec![
                (1, 100, Some(10.5), "fileid-engine 1.2.3 apple".to_string(), 3, 77.0),
                (3, 300, None, "not json".to_string(), 2, 78.0),
            ]
        );
        let owed: Vec<(i64, i64)> = conn
            .prepare("SELECT file_id,stages FROM scan_catchup ORDER BY file_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(owed, vec![(2, 1), (4, 6)], "owed rows stay exactly as they were");
        conn.execute("DELETE FROM files WHERE id = 1", []).unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM scan_failures", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    /// The heal lookup searches `files` by index (`file_ref`/size and
    /// `content_hash`), never by scanning it.
    #[test]
    fn heal_lookup_uses_the_file_ref_index() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        conn.execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 20000)
             INSERT INTO files (path_text,path_hash,size_bytes,scanned_at,kind,extension,file_ref,content_hash)
             SELECT '/p/'||i||'.jpg', i, i, 0, 'image', 'jpg', CASE WHEN i % 10 = 0 THEN NULL ELSE i END, randomblob(32) FROM n;
             ANALYZE;",
        )
        .unwrap();
        let sql = "EXPLAIN QUERY PLAN SELECT id, path_text, (?1 IS NOT NULL AND file_ref IS NOT NULL AND file_ref = ?1 AND size_bytes = ?5) AS by_ref \
            FROM files WHERE path_text != ?3 AND ((file_ref IS NOT NULL AND file_ref = ?1 AND size_bytes = ?5) \
            OR (content_hash IS NOT NULL AND content_hash IN (?2, ?4, ?6))) ORDER BY by_ref DESC LIMIT 32";
        let plan: Vec<String> = conn
            .prepare(sql)
            .unwrap()
            .query_map(rusqlite::params![5i64, vec![0u8; 32], "/x", vec![1u8; 32], 5i64, Option::<Vec<u8>>::None], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let plan = plan.join(" | ");
        assert!(plan.contains("USING INDEX") && !plan.contains("SCAN files"), "{plan}");
    }
}
