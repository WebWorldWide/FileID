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
}
