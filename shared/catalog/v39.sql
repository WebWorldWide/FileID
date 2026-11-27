CREATE VIRTUAL TABLE IF NOT EXISTS files_search_fts USING fts5(path_search, vlm_proposed_name, vlm_description, content='files', content_rowid='id', tokenize='trigram');
CREATE TRIGGER IF NOT EXISTS files_search_fts_insert AFTER INSERT ON files BEGIN
    INSERT INTO files_search_fts(rowid, path_search, vlm_proposed_name, vlm_description) VALUES (new.id, new.path_search, new.vlm_proposed_name, new.vlm_description);
END;
CREATE TRIGGER IF NOT EXISTS files_search_fts_delete AFTER DELETE ON files BEGIN
    INSERT INTO files_search_fts(files_search_fts, rowid, path_search, vlm_proposed_name, vlm_description) VALUES ('delete', old.id, old.path_search, old.vlm_proposed_name, old.vlm_description);
END;
CREATE TRIGGER IF NOT EXISTS files_search_fts_update AFTER UPDATE OF path_search, vlm_proposed_name, vlm_description ON files BEGIN
    INSERT INTO files_search_fts(files_search_fts, rowid, path_search, vlm_proposed_name, vlm_description) VALUES ('delete', old.id, old.path_search, old.vlm_proposed_name, old.vlm_description);
    INSERT INTO files_search_fts(rowid, path_search, vlm_proposed_name, vlm_description) VALUES (new.id, new.path_search, new.vlm_proposed_name, new.vlm_description);
END;
INSERT INTO files_search_fts(files_search_fts) VALUES ('rebuild');
