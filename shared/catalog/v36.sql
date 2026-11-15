CREATE TABLE scan_failures (file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,size_bytes INTEGER NOT NULL,modified_at REAL,engine_build TEXT NOT NULL,attempts INTEGER NOT NULL CHECK(attempts >= 1),failed_at REAL NOT NULL);
INSERT INTO scan_failures(file_id,size_bytes,modified_at,engine_build,attempts,failed_at) SELECT file_id,size_bytes,modified_at,COALESCE(CASE WHEN json_valid(vision) THEN json_extract(vision,'$.failedBy') ELSE vision END,''),MAX(attempts,1),created_at FROM scan_catchup WHERE stages=4;
DELETE FROM scan_catchup WHERE stages=4;
