ALTER TABLE files ADD COLUMN name_sort TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS files_on_name ON files(name_sort, id);
CREATE INDEX IF NOT EXISTS files_on_kind_name ON files(kind, name_sort, id);
CREATE INDEX IF NOT EXISTS files_on_kind_desc_name ON files(kind DESC, name_sort, id);
CREATE INDEX IF NOT EXISTS files_unkeyed ON files(id) WHERE name_sort = '';
CREATE TRIGGER IF NOT EXISTS files_name_sort_stale AFTER UPDATE OF path_text ON files
WHEN new.name_sort = old.name_sort AND new.path_text <> old.path_text
BEGIN
    UPDATE files SET name_sort = '' WHERE id = new.id;
END;
