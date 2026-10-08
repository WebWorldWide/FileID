CREATE TABLE face_refresh_failures (
    face_id INTEGER PRIMARY KEY REFERENCES face_prints(id) ON DELETE CASCADE,
    embedding_model TEXT NOT NULL CHECK(length(embedding_model) BETWEEN 1 AND 150),
    processing_version TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    source_path TEXT NOT NULL,
    source_size INTEGER NOT NULL,
    source_modified REAL,
    bbox TEXT NOT NULL,
    attempts INTEGER NOT NULL CHECK(attempts > 0),
    retry_after REAL NOT NULL,
    reason TEXT NOT NULL CHECK(reason IN ('source_unavailable_or_changed','no_embedding')),
    updated_at REAL NOT NULL
);
CREATE INDEX face_refresh_retry ON face_refresh_failures(retry_after);
