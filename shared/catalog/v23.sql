CREATE TABLE catalog_vector_state (
 namespace TEXT PRIMARY KEY CHECK(namespace IN ('clip','text','catalog')),
 instance_id TEXT NOT NULL CHECK(length(instance_id)=32),
 generation INTEGER NOT NULL DEFAULT 0 CHECK(generation >= 0),
 nonce TEXT NOT NULL CHECK(length(nonce)=32)
);
INSERT INTO catalog_vector_state(namespace,instance_id,generation,nonce)
 SELECT 'clip',hex(randomblob(16)),0,hex(randomblob(16))
 UNION ALL SELECT 'text',hex(randomblob(16)),0,hex(randomblob(16))
 UNION ALL SELECT 'catalog',hex(randomblob(16)),0,hex(randomblob(16));
UPDATE catalog_vector_state SET nonce=instance_id;
CREATE TABLE catalog_vector_changes (
 namespace TEXT NOT NULL REFERENCES catalog_vector_state(namespace),
 generation INTEGER NOT NULL CHECK(generation > 0),
 nonce TEXT NOT NULL CHECK(length(nonce)=32),
 entity_id TEXT NOT NULL,
 entity_kind TEXT NOT NULL,
 file_id INTEGER,
 model TEXT NOT NULL,
 dimension INTEGER NOT NULL,
 PRIMARY KEY(namespace,generation)
);
CREATE TRIGGER catalog_vector_clip_insert AFTER INSERT ON clip_embeddings BEGIN
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='clip';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,CAST(new.file_id AS TEXT),'file',new.file_id,new.model,512
 FROM catalog_vector_state WHERE namespace='clip';
 DELETE FROM catalog_vector_changes WHERE namespace='clip' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='clip')-100000;
END;
CREATE TRIGGER catalog_vector_clip_delete AFTER DELETE ON clip_embeddings BEGIN
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='clip';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,CAST(old.file_id AS TEXT),'file',old.file_id,old.model,512
 FROM catalog_vector_state WHERE namespace='clip';
 DELETE FROM catalog_vector_changes WHERE namespace='clip' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='clip')-100000;
END;
CREATE TRIGGER catalog_vector_clip_update AFTER UPDATE ON clip_embeddings BEGIN
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='clip';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,CAST(old.file_id AS TEXT),'file',old.file_id,old.model,512
 FROM catalog_vector_state WHERE namespace='clip';
 DELETE FROM catalog_vector_changes WHERE namespace='clip' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='clip')-100000;
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='clip';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,CAST(new.file_id AS TEXT),'file',new.file_id,new.model,512
 FROM catalog_vector_state WHERE namespace='clip';
 DELETE FROM catalog_vector_changes WHERE namespace='clip' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='clip')-100000;
END;
CREATE TRIGGER catalog_vector_text_insert AFTER INSERT ON text_embeddings BEGIN
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='text';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,CAST(new.file_id AS TEXT),'file',new.file_id,new.model,384
 FROM catalog_vector_state WHERE namespace='text';
 DELETE FROM catalog_vector_changes WHERE namespace='text' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='text')-100000;
END;
CREATE TRIGGER catalog_vector_text_delete AFTER DELETE ON text_embeddings BEGIN
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='text';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,CAST(old.file_id AS TEXT),'file',old.file_id,old.model,384
 FROM catalog_vector_state WHERE namespace='text';
 DELETE FROM catalog_vector_changes WHERE namespace='text' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='text')-100000;
END;
CREATE TRIGGER catalog_vector_text_update AFTER UPDATE ON text_embeddings BEGIN
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='text';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,CAST(old.file_id AS TEXT),'file',old.file_id,old.model,384
 FROM catalog_vector_state WHERE namespace='text';
 DELETE FROM catalog_vector_changes WHERE namespace='text' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='text')-100000;
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='text';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,CAST(new.file_id AS TEXT),'file',new.file_id,new.model,384
 FROM catalog_vector_state WHERE namespace='text';
 DELETE FROM catalog_vector_changes WHERE namespace='text' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='text')-100000;
END;
CREATE TRIGGER catalog_vector_catalog_insert AFTER INSERT ON catalog_embeddings BEGIN
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='catalog';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,new.entity_id,new.entity_kind,new.file_id,new.model,new.dimension
 FROM catalog_vector_state WHERE namespace='catalog';
 DELETE FROM catalog_vector_changes WHERE namespace='catalog' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='catalog')-100000;
END;
CREATE TRIGGER catalog_vector_catalog_delete AFTER DELETE ON catalog_embeddings BEGIN
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='catalog';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,old.entity_id,old.entity_kind,old.file_id,old.model,old.dimension
 FROM catalog_vector_state WHERE namespace='catalog';
 DELETE FROM catalog_vector_changes WHERE namespace='catalog' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='catalog')-100000;
END;
CREATE TRIGGER catalog_vector_catalog_update AFTER UPDATE ON catalog_embeddings BEGIN
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='catalog';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,old.entity_id,old.entity_kind,old.file_id,old.model,old.dimension
 FROM catalog_vector_state WHERE namespace='catalog';
 DELETE FROM catalog_vector_changes WHERE namespace='catalog' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='catalog')-100000;
 UPDATE catalog_vector_state SET generation=generation+1,nonce=hex(randomblob(16)) WHERE namespace='catalog';
 INSERT INTO catalog_vector_changes(namespace,generation,nonce,entity_id,entity_kind,file_id,model,dimension)
 SELECT namespace,generation,nonce,new.entity_id,new.entity_kind,new.file_id,new.model,new.dimension
 FROM catalog_vector_state WHERE namespace='catalog';
 DELETE FROM catalog_vector_changes WHERE namespace='catalog' AND generation < (SELECT generation FROM catalog_vector_state WHERE namespace='catalog')-100000;
END;
CREATE TRIGGER catalog_vector_source_revision AFTER UPDATE OF size_bytes,modified_at ON files
 WHEN old.size_bytes IS NOT new.size_bytes OR old.modified_at IS NOT new.modified_at
 BEGIN
 DELETE FROM clip_embeddings WHERE file_id=new.id;
 DELETE FROM text_embeddings WHERE file_id=new.id;
 END;

CREATE TRIGGER catalog_vector_file_eligibility AFTER UPDATE OF failed ON files
WHEN old.failed IS NOT new.failed
BEGIN
    UPDATE clip_embeddings SET embedding=embedding WHERE file_id=new.id;
    UPDATE text_embeddings SET embedding=embedding WHERE file_id=new.id;
END;
