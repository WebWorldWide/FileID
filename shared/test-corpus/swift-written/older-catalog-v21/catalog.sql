-- The catalog the Swift engine wrote, as SQL (see README.md); placeholders expand when a case seeds it.
BEGIN;
CREATE TABLE grdb_migrations (identifier TEXT NOT NULL PRIMARY KEY);
CREATE TABLE "files" ("id" INTEGER PRIMARY KEY AUTOINCREMENT, "path_text" TEXT NOT NULL UNIQUE ON CONFLICT REPLACE, "path_hash" INTEGER NOT NULL, "bookmark" BLOB, "size_bytes" INTEGER NOT NULL, "created_at" DOUBLE, "modified_at" DOUBLE, "scanned_at" DOUBLE NOT NULL, "kind" TEXT NOT NULL, "extension" TEXT NOT NULL, "phash" INTEGER, "aesthetic" DOUBLE, "has_faces" INTEGER NOT NULL DEFAULT 0, "has_text" INTEGER NOT NULL DEFAULT 0, "camera_model" TEXT, "location_lat" DOUBLE, "location_lon" DOUBLE, "failed" INTEGER NOT NULL DEFAULT 0, "error_message" TEXT, "vlm_description" TEXT, "vlm_proposed_name" TEXT, "vlm_model" TEXT, "vlm_analyzed_at" DOUBLE, content_hash BLOB, file_ref INTEGER, path_search TEXT, text_stage_done INTEGER NOT NULL DEFAULT 0, vlm_full_model TEXT);
CREATE TABLE "tags" ("file_id" INTEGER NOT NULL REFERENCES "files"("id") ON DELETE CASCADE, "tag" TEXT NOT NULL, "source" TEXT NOT NULL, "score" DOUBLE, PRIMARY KEY ("file_id", "tag", "source"));
CREATE TABLE "ocr_text" ("file_id" INTEGER PRIMARY KEY REFERENCES "files"("id") ON DELETE CASCADE, "text" TEXT NOT NULL);
CREATE VIRTUAL TABLE ocr_fts USING fts5(
    text,
    content='ocr_text',
    content_rowid='file_id',
    tokenize='porter unicode61'
);
CREATE TABLE "persons" ("id" INTEGER PRIMARY KEY AUTOINCREMENT, "name" TEXT, "representative_face_id" INTEGER, "file_count" INTEGER NOT NULL DEFAULT 0, "created_at" DOUBLE NOT NULL, "title" TEXT, "first_name" TEXT, "middle_name" TEXT, "last_name" TEXT, "suffix" TEXT, "is_unknown" INTEGER DEFAULT 0, "centroid" BLOB, "anchor_radius" DOUBLE, "last_clustered_at" DOUBLE);
CREATE TABLE "face_prints" ("id" INTEGER PRIMARY KEY AUTOINCREMENT, "file_id" INTEGER NOT NULL REFERENCES "files"("id") ON DELETE CASCADE, "person_id" INTEGER REFERENCES "persons"("id"), "print_data" BLOB NOT NULL, "bbox" TEXT NOT NULL, "arcface_embedding" BLOB, "face_quality" DOUBLE, "excluded" INTEGER DEFAULT 0);
CREATE TABLE "scan_sessions" ("id" TEXT PRIMARY KEY, "root_path" TEXT NOT NULL, "started_at" DOUBLE NOT NULL, "completed_at" DOUBLE, "last_file_index" INTEGER, "total_files" INTEGER, "status" TEXT NOT NULL);
CREATE TABLE "clip_embeddings" ("file_id" INTEGER PRIMARY KEY REFERENCES "files"("id") ON DELETE CASCADE, "embedding" BLOB NOT NULL, "model" TEXT NOT NULL);
CREATE TABLE "face_verifications" ("person_a" INTEGER NOT NULL, "person_b" INTEGER NOT NULL, "same_person" INTEGER NOT NULL, "confidence" DOUBLE NOT NULL, "vlm_model" TEXT NOT NULL, "verified_at" DOUBLE NOT NULL, face_a INTEGER, face_b INTEGER, file_a INTEGER, bbox_a TEXT, file_b INTEGER, bbox_b TEXT, PRIMARY KEY ("person_a", "person_b"));
CREATE TABLE usn_state (
    volume_id       TEXT    PRIMARY KEY,
    journal_id      INTEGER NOT NULL,
    next_usn        INTEGER NOT NULL,
    last_polled_at  DOUBLE  NOT NULL
);
CREATE TABLE doc_text (
    file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    text    TEXT    NOT NULL
);
CREATE VIRTUAL TABLE doc_fts USING fts5(
    text,
    content='doc_text',
    content_rowid='file_id',
    tokenize='porter unicode61'
);
CREATE TABLE text_embeddings (
    file_id   INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    embedding BLOB    NOT NULL,
    model     TEXT    NOT NULL
);
CREATE TABLE restructure_feedback (
    token      TEXT    NOT NULL,
    folder     TEXT    NOT NULL,
    weight     INTEGER NOT NULL DEFAULT 1,
    updated_at DOUBLE  NOT NULL DEFAULT 0,
    PRIMARY KEY (token, folder)
);
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
CREATE VIRTUAL TABLE catalog_evidence_fts USING fts5(evidence_id UNINDEXED,file_id UNINDEXED,kind UNINDEXED,text,tokenize='unicode61');
CREATE TABLE catalog_observations (
 id TEXT PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 person_id INTEGER REFERENCES persons(id) ON DELETE SET NULL, track_id TEXT,
 start_seconds REAL CHECK(start_seconds >= 0), end_seconds REAL, region_json TEXT,
 source_revision TEXT NOT NULL, model_version TEXT NOT NULL, confidence REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1),
 user_edited INTEGER NOT NULL DEFAULT 0, stale INTEGER NOT NULL DEFAULT 0
);
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
CREATE TABLE catalog_operations (
 id TEXT PRIMARY KEY, plan_json TEXT NOT NULL, inverse_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('preview','running','completed','undone','failed')), created_at REAL NOT NULL
);
CREATE TABLE catalog_recipes (id TEXT PRIMARY KEY,title TEXT NOT NULL,recipe_json TEXT NOT NULL,updated_at REAL NOT NULL);
CREATE TABLE catalog_corrections (id TEXT PRIMARY KEY,file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,kind TEXT NOT NULL,before_json TEXT NOT NULL,after_json TEXT NOT NULL,created_at REAL NOT NULL);
CREATE TABLE catalog_chat (id TEXT PRIMARY KEY,conversation_id TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('user','assistant')),text TEXT NOT NULL,created_at REAL NOT NULL);
CREATE VIRTUAL TABLE catalog_file_fts USING fts5(path,description,tokenize='unicode61');
CREATE TABLE catalog_embeddings (
 entity_id TEXT NOT NULL, entity_kind TEXT NOT NULL CHECK(entity_kind IN ('file','moment','passage','person','observation')),
 file_id INTEGER REFERENCES files(id) ON DELETE CASCADE, model TEXT NOT NULL CHECK(length(model) BETWEEN 1 AND 200),
 dimension INTEGER NOT NULL CHECK(dimension BETWEEN 1 AND 65536), vector BLOB NOT NULL,
 source_revision TEXT NOT NULL, CHECK(length(vector)=dimension*4),
 PRIMARY KEY(entity_kind,entity_id,model,dimension)
);
DELETE FROM "ocr_fts_config";
INSERT INTO "ocr_fts_config"("k","v") VALUES('version',4);
DELETE FROM "doc_fts_config";
INSERT INTO "doc_fts_config"("k","v") VALUES('version',4);
DELETE FROM "catalog_evidence_fts_config";
INSERT INTO "catalog_evidence_fts_config"("k","v") VALUES('version',4);
DELETE FROM "catalog_file_fts_config";
INSERT INTO "catalog_file_fts_config"("k","v") VALUES('version',4);
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(1,'v1_core_tables');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(2,'v2_clip_embeddings');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(3,'v3_deep_analyze');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(4,'v4_face_verifications');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(5,'v5_person_naming_structured');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(6,'v6_arcface_embeddings');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(7,'v7_identity_anchors');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(8,'v8_content_identity');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(9,'v9_usn_state');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(10,'v10_doc_text');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(11,'v11_text_embeddings');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(12,'v12_face_model_reset');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(13,'v13_face_verification_anchors');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(14,'v14_files_kind_scanned_index');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(15,'v15_fts_sync_triggers');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(16,'v16_path_search');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(17,'v17_face_verification_stable_keys');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(18,'v18_restructure_feedback');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(19,'v19_files_text_stage_done');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(20,'v20_vlm_full_model');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(21,'v21_catalog');
INSERT INTO "files"("id","path_text","path_hash","bookmark","size_bytes","created_at","modified_at","scanned_at","kind","extension","phash","aesthetic","has_faces","has_text","camera_model","location_lat","location_lon","failed","error_message","vlm_description","vlm_proposed_name","vlm_model","vlm_analyzed_at","content_hash","file_ref","path_search","text_stage_done","vlm_full_model") VALUES(1,'{root}/Pictures/Picture.png',{path_hash:Pictures/Picture.png},NULL,460,1700000060.0,1700000060.0,1700000300.0,'image','png',4671237767210991616,NULL,1,0,'Generated Camera',NULL,NULL,0,NULL,'A generated picture',NULL,NULL,NULL,X'a18e4a7120666385c96f2ce2d8457c067cd68a2edecbcc0fb37b1e32da538417',{inode:Pictures/Picture.png},'{root}/Pictures/Picture.png',0,NULL);
INSERT INTO "files"("id","path_text","path_hash","bookmark","size_bytes","created_at","modified_at","scanned_at","kind","extension","phash","aesthetic","has_faces","has_text","camera_model","location_lat","location_lon","failed","error_message","vlm_description","vlm_proposed_name","vlm_model","vlm_analyzed_at","content_hash","file_ref","path_search","text_stage_done","vlm_full_model") VALUES(2,'{root}/Documents/Notes.txt',{path_hash:Documents/Notes.txt},NULL,43,1700000120.0,1700000120.0,1700000300.0,'doc','txt',NULL,NULL,0,1,NULL,NULL,NULL,0,NULL,NULL,NULL,NULL,NULL,X'fb9a16f9df00ba7344afcfe50c677116bb1e3c9635f9bca7d4d1b4db6247bc24',{inode:Documents/Notes.txt},'{root}/Documents/Notes.txt',1,NULL);
INSERT INTO "files"("id","path_text","path_hash","bookmark","size_bytes","created_at","modified_at","scanned_at","kind","extension","phash","aesthetic","has_faces","has_text","camera_model","location_lat","location_lon","failed","error_message","vlm_description","vlm_proposed_name","vlm_model","vlm_analyzed_at","content_hash","file_ref","path_search","text_stage_done","vlm_full_model") VALUES(3,'{root}/Documents/Report.pdf',{path_hash:Documents/Report.pdf},NULL,590,1700000180.0,1700000180.0,1700000300.0,'doc','pdf',NULL,NULL,0,1,NULL,NULL,NULL,0,NULL,NULL,NULL,NULL,NULL,X'6bbe6d721b1b112083d1088e760abb7e4ebaeb9c232dc3a51db85f7f680f0c10',{inode:Documents/Report.pdf},'{root}/Documents/Report.pdf',1,NULL);
INSERT INTO "tags"(rowid,"file_id","tag","source","score") VALUES(1,1,'outdoor','clip',0.91);
INSERT INTO "tags"(rowid,"file_id","tag","source","score") VALUES(2,1,'generated','user',NULL);
INSERT INTO "tags"(rowid,"file_id","tag","source","score") VALUES(3,2,'notes','auto',0.5);
INSERT INTO "ocr_text"("file_id","text") VALUES(2,'Quarterly notes from the generated library');
INSERT INTO "persons"("id","name","representative_face_id","file_count","created_at","title","first_name","middle_name","last_name","suffix","is_unknown","centroid","anchor_radius","last_clustered_at") VALUES(1,'Alex',1,1,1700000200.0,NULL,'Alex',NULL,NULL,NULL,0,NULL,NULL,NULL);
INSERT INTO "face_prints"("id","file_id","person_id","print_data","bbox","arcface_embedding","face_quality","excluded") VALUES(1,1,1,X'00010203','0.1,0.1,0.4,0.4',NULL,0.8,0);
INSERT INTO "scan_sessions"(rowid,"id","root_path","started_at","completed_at","last_file_index","total_files","status") VALUES(1,'generated-scan','{root}',1700000290.0,1700000300.0,NULL,3,'completed');
INSERT INTO "restructure_feedback"(rowid,"token","folder","weight","updated_at") VALUES(1,'picture','Pictures',2,0.0);
INSERT INTO "catalog_revisions"("file_id","revision","processing_version","updated_at") VALUES(2,'generated-revision','generated',1700000300.0);
INSERT INTO "catalog_chapters"(rowid,"id","file_id","start_seconds","end_seconds","title","summary","source_revision","model_version","confidence","user_edited","stale") VALUES(1,'generated-chapter',3,0.0,1.0,'Generated chapter','','generated','user',1.0,1,0);
INSERT INTO "catalog_evidence_fts"(rowid,"evidence_id","file_id","kind","text") VALUES(1,'generated-chapter',3,'chapter','Generated chapter ');
INSERT INTO "catalog_file_fts"(rowid,"path","description") VALUES(1,'{root}/Pictures/Picture.png','A generated picture');
INSERT INTO "catalog_file_fts"(rowid,"path","description") VALUES(2,'{root}/Documents/Notes.txt','');
INSERT INTO "catalog_file_fts"(rowid,"path","description") VALUES(3,'{root}/Documents/Report.pdf','');
INSERT INTO "ocr_fts"("ocr_fts") VALUES('rebuild');
INSERT INTO "doc_fts"("doc_fts") VALUES('rebuild');
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence(rowid,name,seq) VALUES(1,'files',3);
INSERT INTO sqlite_sequence(rowid,name,seq) VALUES(2,'persons',1);
INSERT INTO sqlite_sequence(rowid,name,seq) VALUES(3,'face_prints',1);
CREATE INDEX "files_on_path_hash" ON "files"("path_hash");
CREATE INDEX "files_on_kind" ON "files"("kind");
CREATE INDEX idx_files_phash ON files(phash) WHERE phash IS NOT NULL;
CREATE INDEX "idx_files_scanned" ON "files"("scanned_at");
CREATE INDEX "idx_tags_tag" ON "tags"("tag");
CREATE INDEX "idx_face_person" ON "face_prints"("person_id");
CREATE INDEX "idx_face_file" ON "face_prints"("file_id");
CREATE INDEX "idx_person_repface" ON "persons"("representative_face_id");
CREATE INDEX "idx_face_verify_a" ON "face_verifications"("person_a");
CREATE INDEX "idx_face_verify_b" ON "face_verifications"("person_b");
CREATE INDEX idx_face_prints_arcface_null
ON face_prints(id)
WHERE arcface_embedding IS NULL;
CREATE INDEX idx_files_content_hash
    ON files(content_hash)
    WHERE content_hash IS NOT NULL;
CREATE INDEX idx_files_file_ref
    ON files(file_ref)
    WHERE file_ref IS NOT NULL;
CREATE INDEX idx_files_kind_scanned ON files(kind, scanned_at);
CREATE TRIGGER ocr_text_ai AFTER INSERT ON ocr_text BEGIN
    INSERT INTO ocr_fts(rowid, text) VALUES (new.file_id, new.text);
END;
CREATE TRIGGER ocr_text_ad AFTER DELETE ON ocr_text BEGIN
    INSERT INTO ocr_fts(ocr_fts, rowid, text) VALUES ('delete', old.file_id, old.text);
END;
CREATE TRIGGER ocr_text_au AFTER UPDATE ON ocr_text BEGIN
    INSERT INTO ocr_fts(ocr_fts, rowid, text) VALUES ('delete', old.file_id, old.text);
    INSERT INTO ocr_fts(rowid, text) VALUES (new.file_id, new.text);
END;
CREATE TRIGGER doc_text_ai AFTER INSERT ON doc_text BEGIN
    INSERT INTO doc_fts(rowid, text) VALUES (new.file_id, new.text);
END;
CREATE TRIGGER doc_text_ad AFTER DELETE ON doc_text BEGIN
    INSERT INTO doc_fts(doc_fts, rowid, text) VALUES ('delete', old.file_id, old.text);
END;
CREATE TRIGGER doc_text_au AFTER UPDATE ON doc_text BEGIN
    INSERT INTO doc_fts(doc_fts, rowid, text) VALUES ('delete', old.file_id, old.text);
    INSERT INTO doc_fts(rowid, text) VALUES (new.file_id, new.text);
END;
CREATE INDEX idx_restructure_feedback_token ON restructure_feedback(token);
CREATE INDEX catalog_chapters_file ON catalog_chapters(file_id,start_seconds);
CREATE INDEX catalog_passages_file ON catalog_passages(file_id);
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
CREATE INDEX catalog_observations_person ON catalog_observations(person_id,file_id,start_seconds);
CREATE INDEX catalog_jobs_dispatch ON catalog_jobs(state,priority DESC,created_at);
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
COMMIT;
