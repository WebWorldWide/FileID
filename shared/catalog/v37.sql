CREATE INDEX IF NOT EXISTS files_on_size ON files(size_bytes);
CREATE INDEX IF NOT EXISTS files_on_taken_newest ON files((COALESCE(created_at,modified_at) IS NULL), COALESCE(created_at,modified_at) DESC, id DESC);
CREATE INDEX IF NOT EXISTS files_on_taken_oldest ON files((COALESCE(created_at,modified_at) IS NULL), COALESCE(created_at,modified_at) ASC, id ASC);
