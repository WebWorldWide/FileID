CREATE TABLE catalog_tag_vocab(tag TEXT PRIMARY KEY) WITHOUT ROWID;
INSERT OR IGNORE INTO catalog_tag_vocab(tag) SELECT DISTINCT tag FROM tags;
CREATE TRIGGER catalog_tag_vocab_insert AFTER INSERT ON tags BEGIN
 INSERT OR IGNORE INTO catalog_tag_vocab(tag) VALUES(new.tag);
END;
CREATE TRIGGER catalog_tag_vocab_update AFTER UPDATE OF tag ON tags BEGIN
 INSERT OR IGNORE INTO catalog_tag_vocab(tag) VALUES(new.tag);
END;
CREATE INDEX catalog_files_twins ON files(content_hash,size_bytes,failed) WHERE content_hash IS NOT NULL;
CREATE INDEX catalog_files_size ON files(size_bytes);
CREATE INDEX catalog_files_taken ON files(COALESCE(created_at,modified_at));
CREATE INDEX catalog_files_kind_totals ON files(kind,failed,size_bytes,COALESCE(created_at,modified_at));
