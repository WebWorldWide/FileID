CREATE INDEX IF NOT EXISTS files_on_failed_size ON files(failed, size_bytes);
CREATE INDEX IF NOT EXISTS files_on_taken_newest ON files(failed, (COALESCE(created_at,modified_at) IS NULL), COALESCE(created_at,modified_at) DESC, id DESC);
CREATE INDEX IF NOT EXISTS files_on_taken_oldest ON files(failed, (COALESCE(created_at,modified_at) IS NULL), COALESCE(created_at,modified_at) ASC, id ASC);
CREATE INDEX IF NOT EXISTS files_on_failed_scanned ON files(failed, scanned_at);
