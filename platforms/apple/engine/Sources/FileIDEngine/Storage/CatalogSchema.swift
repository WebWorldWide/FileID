enum CatalogSchema {
    static let v21 = #"""
CREATE TABLE catalog_revisions (
 file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
 revision TEXT NOT NULL, processing_version TEXT NOT NULL, updated_at REAL NOT NULL
);
CREATE TABLE catalog_chapters (
 id TEXT PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 start_seconds REAL NOT NULL CHECK(start_seconds >= 0), end_seconds REAL NOT NULL CHECK(end_seconds >= start_seconds),
 title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 200), summary TEXT NOT NULL DEFAULT '',
 source_revision TEXT NOT NULL, model_version TEXT NOT NULL, confidence REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1),
 user_edited INTEGER NOT NULL DEFAULT 0 CHECK(user_edited IN (0,1)), stale INTEGER NOT NULL DEFAULT 0 CHECK(stale IN (0,1))
);
CREATE INDEX catalog_chapters_file ON catalog_chapters(file_id,start_seconds);
CREATE TABLE catalog_coverage (
 id TEXT PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 start_seconds REAL NOT NULL CHECK(start_seconds >= 0), end_seconds REAL NOT NULL CHECK(end_seconds > start_seconds),
 status TEXT NOT NULL CHECK(status IN ('sampled','verified','incomplete','stale')),
 source_revision TEXT NOT NULL, model_version TEXT NOT NULL
);
CREATE TABLE catalog_passages (
 id TEXT PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 page INTEGER CHECK(page > 0), start_seconds REAL CHECK(start_seconds >= 0), end_seconds REAL,
 text TEXT NOT NULL, source_revision TEXT NOT NULL, model_version TEXT NOT NULL,
 confidence REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1), stale INTEGER NOT NULL DEFAULT 0,
 CHECK(end_seconds IS NULL OR end_seconds >= start_seconds)
);
CREATE INDEX catalog_passages_file ON catalog_passages(file_id);
CREATE VIRTUAL TABLE catalog_evidence_fts USING fts5(evidence_id UNINDEXED,file_id UNINDEXED,kind UNINDEXED,text,tokenize='unicode61');
CREATE TRIGGER catalog_chapter_insert AFTER INSERT ON catalog_chapters BEGIN
 INSERT INTO catalog_evidence_fts(evidence_id,file_id,kind,text) VALUES(new.id,new.file_id,'chapter',new.title || ' ' || new.summary);
END;
CREATE TRIGGER catalog_chapter_delete AFTER DELETE ON catalog_chapters BEGIN
 DELETE FROM catalog_evidence_fts WHERE evidence_id=old.id AND kind='chapter';
END;
CREATE TRIGGER catalog_chapter_update AFTER UPDATE ON catalog_chapters BEGIN
 DELETE FROM catalog_evidence_fts WHERE evidence_id=old.id AND kind='chapter';
 INSERT INTO catalog_evidence_fts(evidence_id,file_id,kind,text) VALUES(new.id,new.file_id,'chapter',new.title || ' ' || new.summary);
END;
CREATE TRIGGER catalog_passage_insert AFTER INSERT ON catalog_passages BEGIN
 INSERT INTO catalog_evidence_fts(evidence_id,file_id,kind,text) VALUES(new.id,new.file_id,'passage',new.text);
END;
CREATE TRIGGER catalog_passage_delete AFTER DELETE ON catalog_passages BEGIN
 DELETE FROM catalog_evidence_fts WHERE evidence_id=old.id AND kind='passage';
END;
CREATE TRIGGER catalog_passage_update AFTER UPDATE ON catalog_passages BEGIN
 DELETE FROM catalog_evidence_fts WHERE evidence_id=old.id AND kind='passage';
 INSERT INTO catalog_evidence_fts(evidence_id,file_id,kind,text) VALUES(new.id,new.file_id,'passage',new.text);
END;
CREATE TABLE catalog_observations (
 id TEXT PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 person_id INTEGER REFERENCES persons(id) ON DELETE SET NULL, track_id TEXT,
 start_seconds REAL CHECK(start_seconds >= 0), end_seconds REAL, region_json TEXT,
 source_revision TEXT NOT NULL, model_version TEXT NOT NULL, confidence REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1),
 user_edited INTEGER NOT NULL DEFAULT 0, stale INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX catalog_observations_person ON catalog_observations(person_id,file_id,start_seconds);
CREATE TABLE catalog_events (id TEXT PRIMARY KEY, title TEXT NOT NULL, goal TEXT NOT NULL DEFAULT '', user_edited INTEGER NOT NULL DEFAULT 0);
CREATE TABLE catalog_event_files (event_id TEXT NOT NULL REFERENCES catalog_events(id) ON DELETE CASCADE,file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,PRIMARY KEY(event_id,file_id));
CREATE TABLE catalog_take_scores (
 event_id TEXT NOT NULL REFERENCES catalog_events(id) ON DELETE CASCADE,file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 outcome_score REAL CHECK(outcome_score BETWEEN 0 AND 1), quality_score REAL CHECK(quality_score BETWEEN 0 AND 1),
 confidence REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1), explanation TEXT NOT NULL,
 source_revision TEXT NOT NULL, model_version TEXT NOT NULL, preferred INTEGER NOT NULL DEFAULT 0, stale INTEGER NOT NULL DEFAULT 0 CHECK(stale IN (0,1)),
 PRIMARY KEY(event_id,file_id)
);
CREATE TABLE catalog_assets (
 original_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE, derived_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 role TEXT NOT NULL CHECK(role IN ('proxy','conversion','export','enhancement')), recipe_json TEXT NOT NULL,
 PRIMARY KEY(original_id,derived_id), CHECK(original_id != derived_id)
);
CREATE TABLE catalog_jobs (
 id TEXT PRIMARY KEY, kind TEXT NOT NULL, file_ids_json TEXT NOT NULL, recipe_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('queued','running','paused','completed','cancelled','failed')),
 priority INTEGER NOT NULL DEFAULT 0, progress REAL NOT NULL DEFAULT 0 CHECK(progress BETWEEN 0 AND 1),
 checkpoint_json TEXT NOT NULL DEFAULT '{}', error TEXT, created_at REAL NOT NULL, updated_at REAL NOT NULL
);
CREATE INDEX catalog_jobs_dispatch ON catalog_jobs(state,priority DESC,created_at);
CREATE TABLE catalog_operations (
 id TEXT PRIMARY KEY, plan_json TEXT NOT NULL, inverse_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('preview','running','completed','undone','failed')), created_at REAL NOT NULL
);
CREATE TABLE catalog_recipes (id TEXT PRIMARY KEY,title TEXT NOT NULL,recipe_json TEXT NOT NULL,updated_at REAL NOT NULL);
CREATE TABLE catalog_corrections (id TEXT PRIMARY KEY,file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,kind TEXT NOT NULL,before_json TEXT NOT NULL,after_json TEXT NOT NULL,created_at REAL NOT NULL);
CREATE TABLE catalog_chat (id TEXT PRIMARY KEY,conversation_id TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('user','assistant')),text TEXT NOT NULL,created_at REAL NOT NULL);
CREATE INDEX catalog_chat_conversation ON catalog_chat(conversation_id,created_at);
CREATE TRIGGER catalog_invalidate AFTER UPDATE OF size_bytes,modified_at ON files
 WHEN old.size_bytes != new.size_bytes OR old.modified_at IS NOT new.modified_at BEGIN
 UPDATE catalog_chapters SET stale=1 WHERE file_id=new.id;
 UPDATE catalog_passages SET stale=1 WHERE file_id=new.id;
 UPDATE catalog_observations SET stale=1 WHERE file_id=new.id;
 UPDATE catalog_coverage SET status='stale' WHERE file_id=new.id;
 UPDATE catalog_take_scores SET stale=1 WHERE file_id=new.id;
 DELETE FROM catalog_revisions WHERE file_id=new.id;
 DELETE FROM catalog_embeddings WHERE file_id=new.id;
 UPDATE files SET vlm_description=NULL,vlm_proposed_name=NULL,vlm_analyzed_at=NULL,vlm_model=NULL,vlm_full_model=NULL WHERE id=new.id;
END;
CREATE VIRTUAL TABLE catalog_file_fts USING fts5(path,description,tokenize='unicode61');
INSERT INTO catalog_file_fts(rowid,path,description) SELECT id,path_text,COALESCE(vlm_description,'') FROM files;
CREATE TRIGGER catalog_file_insert AFTER INSERT ON files BEGIN
 INSERT INTO catalog_file_fts(rowid,path,description) VALUES(new.id,new.path_text,COALESCE(new.vlm_description,''));
END;
CREATE TRIGGER catalog_file_delete AFTER DELETE ON files BEGIN
 DELETE FROM catalog_file_fts WHERE rowid=old.id;
END;
CREATE TRIGGER catalog_file_update AFTER UPDATE OF path_text,vlm_description ON files BEGIN
 DELETE FROM catalog_file_fts WHERE rowid=old.id;
 INSERT INTO catalog_file_fts(rowid,path,description) VALUES(new.id,new.path_text,COALESCE(new.vlm_description,''));
END;
CREATE TABLE catalog_embeddings (
 entity_id TEXT NOT NULL, entity_kind TEXT NOT NULL CHECK(entity_kind IN ('file','moment','passage','person','observation')),
 file_id INTEGER REFERENCES files(id) ON DELETE CASCADE, model TEXT NOT NULL CHECK(length(model) BETWEEN 1 AND 200),
 dimension INTEGER NOT NULL CHECK(dimension BETWEEN 1 AND 65536), vector BLOB NOT NULL,
 source_revision TEXT NOT NULL, CHECK(length(vector)=dimension*4),
 PRIMARY KEY(entity_kind,entity_id,model,dimension)
);
"""#
    static let v22 = #"""
ALTER TABLE face_prints ADD COLUMN embedding_model TEXT;
ALTER TABLE face_prints ADD COLUMN processing_version TEXT;
ALTER TABLE face_prints ADD COLUMN source_revision TEXT;
CREATE INDEX face_prints_processing ON face_prints(embedding_model,processing_version);
CREATE TRIGGER face_cache_observation_insert AFTER INSERT ON face_prints
WHEN new.embedding_model IS NOT NULL AND new.processing_version IS NOT NULL AND new.source_revision IS NOT NULL AND length(new.arcface_embedding)=512
BEGIN
 INSERT INTO catalog_observations(id,file_id,person_id,region_json,source_revision,model_version,confidence,user_edited,stale)
 VALUES('faceprint:' || new.id,new.file_id,new.person_id,json_object('kind','face','bbox',new.bbox),new.source_revision,new.embedding_model || '|' || new.processing_version,0,0,new.excluded);
 INSERT INTO catalog_embeddings(entity_id,entity_kind,file_id,model,dimension,vector,source_revision)
 VALUES('faceprint:' || new.id,'observation',new.file_id,new.embedding_model || '|' || new.processing_version,128,new.arcface_embedding,new.source_revision);
END;
CREATE TRIGGER face_cache_observation_update AFTER UPDATE OF arcface_embedding,embedding_model,processing_version,source_revision ON face_prints
WHEN new.embedding_model IS NOT NULL AND new.processing_version IS NOT NULL AND new.source_revision IS NOT NULL AND length(new.arcface_embedding)=512
BEGIN
 INSERT INTO catalog_observations(id,file_id,person_id,region_json,source_revision,model_version,confidence,user_edited,stale)
 VALUES('faceprint:' || new.id,new.file_id,new.person_id,json_object('kind','face','bbox',new.bbox),new.source_revision,new.embedding_model || '|' || new.processing_version,0,0,new.excluded)
 ON CONFLICT(id) DO UPDATE SET file_id=excluded.file_id,person_id=excluded.person_id,region_json=excluded.region_json,source_revision=excluded.source_revision,model_version=excluded.model_version,confidence=0,stale=excluded.stale WHERE catalog_observations.user_edited=0;
 INSERT INTO catalog_embeddings(entity_id,entity_kind,file_id,model,dimension,vector,source_revision)
 VALUES('faceprint:' || new.id,'observation',new.file_id,new.embedding_model || '|' || new.processing_version,128,new.arcface_embedding,new.source_revision)
 ON CONFLICT(entity_kind,entity_id,model,dimension) DO UPDATE SET vector=excluded.vector,source_revision=excluded.source_revision;
END;
CREATE TRIGGER face_cache_person_update AFTER UPDATE OF person_id ON face_prints
BEGIN
 UPDATE catalog_observations SET person_id=new.person_id WHERE id='faceprint:' || new.id;
END;
CREATE TRIGGER face_cache_observation_delete AFTER DELETE ON face_prints
BEGIN
 UPDATE catalog_observations SET stale=1 WHERE id='faceprint:' || old.id AND user_edited=1;
 DELETE FROM catalog_observations WHERE id='faceprint:' || old.id AND user_edited=0;
 DELETE FROM catalog_embeddings WHERE entity_kind='observation' AND entity_id='faceprint:' || old.id;
END;
CREATE TRIGGER face_cache_exclusion_update AFTER UPDATE OF excluded ON face_prints
BEGIN
 UPDATE catalog_observations SET stale=CASE WHEN new.excluded=0 AND length(new.arcface_embedding)=512 AND new.embedding_model IS NOT NULL AND new.processing_version IS NOT NULL AND EXISTS(SELECT 1 FROM catalog_revisions WHERE file_id=new.file_id AND revision=new.source_revision) THEN 0 ELSE 1 END WHERE id='faceprint:' || new.id AND user_edited=0;
END;
CREATE TRIGGER face_cache_invalidate AFTER UPDATE OF arcface_embedding,embedding_model,processing_version,source_revision ON face_prints
WHEN new.embedding_model IS NULL OR new.processing_version IS NULL OR new.source_revision IS NULL OR new.arcface_embedding IS NULL OR length(new.arcface_embedding)!=512
BEGIN
 UPDATE catalog_observations SET stale=1 WHERE id='faceprint:' || new.id;
 DELETE FROM catalog_embeddings WHERE entity_kind='observation' AND entity_id='faceprint:' || new.id;
END;
CREATE TRIGGER face_cache_bbox_update AFTER UPDATE OF bbox ON face_prints
WHEN old.bbox IS NOT new.bbox
BEGIN
 UPDATE face_prints SET arcface_embedding=NULL,embedding_model=NULL,processing_version=NULL,source_revision=NULL WHERE id=new.id;
END;
"""#
    static let v23 = #"""
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
"""#
    static let v24 = #"""
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
"""#
}
