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
