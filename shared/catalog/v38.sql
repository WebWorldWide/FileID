ALTER TABLE files ADD COLUMN name_sort TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS files_on_name ON files(name_sort, id);
CREATE INDEX IF NOT EXISTS files_on_kind_name ON files(kind, name_sort, id);
CREATE INDEX IF NOT EXISTS files_on_kind_desc_name ON files(kind DESC, name_sort, id);
CREATE INDEX IF NOT EXISTS files_unkeyed ON files(id) WHERE name_sort = '';
CREATE TRIGGER IF NOT EXISTS files_name_sort_stale AFTER UPDATE OF path_text ON files
WHEN new.name_sort = old.name_sort AND new.path_text <> old.path_text AND substr(replace(new.path_text,'\','/'), length(rtrim(replace(new.path_text,'\','/'), replace(replace(new.path_text,'\','/'),'/',''))) + 1) <> substr(replace(old.path_text,'\','/'), length(rtrim(replace(old.path_text,'\','/'), replace(replace(old.path_text,'\','/'),'/',''))) + 1)
BEGIN
    UPDATE files SET name_sort = '' WHERE id = new.id;
END;
CREATE INDEX IF NOT EXISTS files_taken_missing ON files(id) WHERE COALESCE(created_at, modified_at) IS NULL;
