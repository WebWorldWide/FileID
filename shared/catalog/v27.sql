CREATE VIRTUAL TABLE catalog_tag_fts USING fts5(tags,tokenize='unicode61');
INSERT INTO catalog_tag_fts(rowid,tags) SELECT file_id,group_concat(REPLACE(tag,'_',' '),' ') FROM tags WHERE source IN ('auto','user','vlm') GROUP BY file_id;
CREATE TRIGGER catalog_tag_insert AFTER INSERT ON tags BEGIN
 DELETE FROM catalog_tag_fts WHERE rowid=new.file_id;
 INSERT INTO catalog_tag_fts(rowid,tags) SELECT file_id,group_concat(REPLACE(tag,'_',' '),' ') FROM tags WHERE file_id=new.file_id AND source IN ('auto','user','vlm') GROUP BY file_id;
END;
CREATE TRIGGER catalog_tag_delete AFTER DELETE ON tags BEGIN
 DELETE FROM catalog_tag_fts WHERE rowid=old.file_id;
 INSERT INTO catalog_tag_fts(rowid,tags) SELECT file_id,group_concat(REPLACE(tag,'_',' '),' ') FROM tags WHERE file_id=old.file_id AND source IN ('auto','user','vlm') GROUP BY file_id;
END;
CREATE TRIGGER catalog_tag_update AFTER UPDATE ON tags BEGIN
 DELETE FROM catalog_tag_fts WHERE rowid IN (old.file_id,new.file_id);
 INSERT INTO catalog_tag_fts(rowid,tags) SELECT file_id,group_concat(REPLACE(tag,'_',' '),' ') FROM tags WHERE file_id IN (old.file_id,new.file_id) AND source IN ('auto','user','vlm') GROUP BY file_id;
END;
CREATE TRIGGER catalog_tag_file_delete AFTER DELETE ON files BEGIN
 DELETE FROM catalog_tag_fts WHERE rowid=old.id;
END;
CREATE INDEX catalog_files_twins ON files(content_hash,size_bytes,failed) WHERE content_hash IS NOT NULL;
CREATE INDEX catalog_files_size ON files(size_bytes);
CREATE INDEX catalog_files_taken ON files(COALESCE(created_at,modified_at));
CREATE INDEX catalog_files_kind_totals ON files(kind,failed,size_bytes,COALESCE(created_at,modified_at));
