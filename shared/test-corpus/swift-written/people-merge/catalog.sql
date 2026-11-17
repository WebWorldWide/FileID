-- The catalog the Swift engine wrote, as SQL (see README.md); placeholders expand when a case seeds it.
BEGIN;
CREATE TABLE grdb_migrations (identifier TEXT NOT NULL PRIMARY KEY);
CREATE TABLE "files" ("id" INTEGER PRIMARY KEY AUTOINCREMENT, "path_text" TEXT NOT NULL UNIQUE ON CONFLICT REPLACE, "path_hash" INTEGER NOT NULL, "bookmark" BLOB, "size_bytes" INTEGER NOT NULL, "created_at" DOUBLE, "modified_at" DOUBLE, "scanned_at" DOUBLE NOT NULL, "kind" TEXT NOT NULL, "extension" TEXT NOT NULL, "phash" INTEGER, "aesthetic" DOUBLE, "has_faces" INTEGER NOT NULL DEFAULT 0, "has_text" INTEGER NOT NULL DEFAULT 0, "camera_model" TEXT, "location_lat" DOUBLE, "location_lon" DOUBLE, "failed" INTEGER NOT NULL DEFAULT 0, "error_message" TEXT, "vlm_description" TEXT, "vlm_proposed_name" TEXT, "vlm_model" TEXT, "vlm_analyzed_at" DOUBLE, content_hash BLOB, file_ref INTEGER, path_search TEXT, text_stage_done INTEGER NOT NULL DEFAULT 0, vlm_full_model TEXT, vlm_inherited_from INTEGER);
CREATE TABLE "tags" ("file_id" INTEGER NOT NULL REFERENCES "files"("id") ON DELETE CASCADE, "tag" TEXT NOT NULL, "source" TEXT NOT NULL, "score" DOUBLE, PRIMARY KEY ("file_id", "tag", "source"));
CREATE TABLE "ocr_text" ("file_id" INTEGER PRIMARY KEY REFERENCES "files"("id") ON DELETE CASCADE, "text" TEXT NOT NULL, source TEXT NOT NULL DEFAULT 'ocr' CHECK(source IN ('ocr','vlm')), handwritten INTEGER NOT NULL DEFAULT 0 CHECK(handwritten IN (0,1)));
CREATE VIRTUAL TABLE ocr_fts USING fts5(
    text,
    content='ocr_text',
    content_rowid='file_id',
    tokenize='porter unicode61'
);
CREATE TABLE "persons" ("id" INTEGER PRIMARY KEY AUTOINCREMENT, "name" TEXT, "representative_face_id" INTEGER, "file_count" INTEGER NOT NULL DEFAULT 0, "created_at" DOUBLE NOT NULL, "title" TEXT, "first_name" TEXT, "middle_name" TEXT, "last_name" TEXT, "suffix" TEXT, "is_unknown" INTEGER DEFAULT 0, "centroid" BLOB, "anchor_radius" DOUBLE, "last_clustered_at" DOUBLE);
CREATE TABLE "face_prints" ("id" INTEGER PRIMARY KEY AUTOINCREMENT, "file_id" INTEGER NOT NULL REFERENCES "files"("id") ON DELETE CASCADE, "person_id" INTEGER REFERENCES "persons"("id"), "print_data" BLOB NOT NULL, "bbox" TEXT NOT NULL, "arcface_embedding" BLOB, "face_quality" DOUBLE, "excluded" INTEGER DEFAULT 0, embedding_model TEXT, processing_version TEXT, source_revision TEXT);
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
CREATE TABLE catalog_vector_state (
 namespace TEXT PRIMARY KEY CHECK(namespace IN ('clip','text','catalog')),
 instance_id TEXT NOT NULL CHECK(length(instance_id)=32),
 generation INTEGER NOT NULL DEFAULT 0 CHECK(generation >= 0),
 nonce TEXT NOT NULL CHECK(length(nonce)=32)
);
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
CREATE TABLE catalog_tag_vocab(tag TEXT PRIMARY KEY) WITHOUT ROWID;
CREATE TABLE take_quality (file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,source_revision TEXT NOT NULL,version TEXT NOT NULL,signals_json TEXT NOT NULL,computed_at REAL NOT NULL);
CREATE TABLE take_group_decisions (group_key TEXT PRIMARY KEY,decision TEXT NOT NULL CHECK(decision IN ('keepAll','skip')),file_ids_json TEXT NOT NULL,decided_at REAL NOT NULL);
CREATE TABLE library_roots (id INTEGER PRIMARY KEY,path TEXT NOT NULL UNIQUE,display TEXT NOT NULL,kind TEXT NOT NULL DEFAULT 'internal' CHECK(kind IN ('internal','external','network')),enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),added_at REAL NOT NULL,last_scan_at REAL,last_status TEXT);
CREATE TABLE take_proposals (group_key TEXT PRIMARY KEY,session TEXT NOT NULL,media TEXT NOT NULL CHECK(media IN ('photos','videos')),relation TEXT NOT NULL CHECK(relation IN ('repeatedAttempts','alternateViews','uncertain')),file_ids_json TEXT NOT NULL,evidence_json TEXT NOT NULL,similarity REAL NOT NULL CHECK(similarity BETWEEN 0 AND 1),captured_at REAL NOT NULL,proposed_at REAL NOT NULL);
CREATE TABLE take_proposal_files (file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,session TEXT NOT NULL,fingerprint TEXT NOT NULL);
CREATE TABLE face_assignment_assessed (face_id INTEGER PRIMARY KEY REFERENCES face_prints(id) ON DELETE CASCADE,namespace TEXT NOT NULL,outcome TEXT NOT NULL CHECK(outcome IN ('baseline','clustered','unmatched','suggested','uncertain','ambiguous','rejected','accepted','ignored','assigned','manual')),assessed_at REAL NOT NULL);
CREATE TABLE face_review_candidates (face_id INTEGER PRIMARY KEY REFERENCES face_prints(id) ON DELETE CASCADE,person_id INTEGER NOT NULL REFERENCES persons(id) ON DELETE CASCADE,score REAL NOT NULL,reason TEXT NOT NULL CHECK(reason IN ('suggested','uncertain','ambiguous')),runner_up_person_id INTEGER REFERENCES persons(id) ON DELETE SET NULL,runner_up_score REAL,namespace TEXT NOT NULL,created_at REAL NOT NULL);
CREATE TABLE face_identity_constraints (id INTEGER PRIMARY KEY AUTOINCREMENT,file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,bbox TEXT NOT NULL,anchor_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,anchor_bbox TEXT NOT NULL,source TEXT NOT NULL CHECK(source IN ('review','reassign')),created_at REAL NOT NULL,UNIQUE(file_id,bbox,anchor_file_id,anchor_bbox));
CREATE TABLE face_merge_suggestions (person_id INTEGER NOT NULL REFERENCES persons(id) ON DELETE CASCADE,other_person_id INTEGER NOT NULL REFERENCES persons(id) ON DELETE CASCADE,score REAL NOT NULL,namespace TEXT NOT NULL,created_at REAL NOT NULL,PRIMARY KEY(person_id,other_person_id));
CREATE TABLE face_split_suggestions (person_id INTEGER PRIMARY KEY REFERENCES persons(id) ON DELETE CASCADE,score REAL NOT NULL,keep_json TEXT NOT NULL,split_json TEXT NOT NULL,namespace TEXT NOT NULL,created_at REAL NOT NULL);
CREATE TABLE face_queue_dismissals (id INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL CHECK(kind IN ('merge','split')),file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,bbox TEXT NOT NULL,other_file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,other_bbox TEXT,created_at REAL NOT NULL);
CREATE TABLE face_refresh_backlog (face_id INTEGER NOT NULL REFERENCES face_prints(id) ON DELETE CASCADE,embedding_model TEXT NOT NULL,processing_version TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','done','missing','offline','failed')),attempts INTEGER NOT NULL DEFAULT 0,reason TEXT,updated_at REAL NOT NULL,PRIMARY KEY(face_id,embedding_model,processing_version));
CREATE TABLE scan_catchup (file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,stages INTEGER NOT NULL CHECK(stages BETWEEN 1 AND 7),size_bytes INTEGER NOT NULL,modified_at REAL,vision TEXT,face_count INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,created_at REAL NOT NULL);
CREATE TABLE scan_failures (file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,size_bytes INTEGER NOT NULL,modified_at REAL,engine_build TEXT NOT NULL,attempts INTEGER NOT NULL CHECK(attempts >= 1),failed_at REAL NOT NULL);
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
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(22,'v22_face_cache_provenance');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(23,'v23_vector_change_tracking');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(24,'v24_face_refresh_failures');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(25,'v25_ocr_text_source');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(26,'v26_vlm_inherited_from');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(27,'v27_chat_retrieval');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(28,'v28_take_quality');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(29,'v29_library_roots');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(30,'v30_files_dir_index');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(31,'v31_take_proposals');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(32,'v32_face_review');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(33,'v33_face_queues');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(34,'v34_scan_catchup');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(35,'v35_foreign_key_indexes');
INSERT INTO "grdb_migrations"(rowid,"identifier") VALUES(36,'v36_scan_failures');
INSERT INTO "files"("id","path_text","path_hash","bookmark","size_bytes","created_at","modified_at","scanned_at","kind","extension","phash","aesthetic","has_faces","has_text","camera_model","location_lat","location_lon","failed","error_message","vlm_description","vlm_proposed_name","vlm_model","vlm_analyzed_at","content_hash","file_ref","path_search","text_stage_done","vlm_full_model","vlm_inherited_from") VALUES(1,'{root}/1.jpg',1,NULL,1,NULL,NULL,0.0,'image','jpg',NULL,NULL,0,0,NULL,NULL,NULL,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,NULL,NULL);
INSERT INTO "files"("id","path_text","path_hash","bookmark","size_bytes","created_at","modified_at","scanned_at","kind","extension","phash","aesthetic","has_faces","has_text","camera_model","location_lat","location_lon","failed","error_message","vlm_description","vlm_proposed_name","vlm_model","vlm_analyzed_at","content_hash","file_ref","path_search","text_stage_done","vlm_full_model","vlm_inherited_from") VALUES(2,'{root}/2.jpg',2,NULL,1,NULL,NULL,0.0,'image','jpg',NULL,NULL,0,0,NULL,NULL,NULL,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,NULL,NULL);
INSERT INTO "files"("id","path_text","path_hash","bookmark","size_bytes","created_at","modified_at","scanned_at","kind","extension","phash","aesthetic","has_faces","has_text","camera_model","location_lat","location_lon","failed","error_message","vlm_description","vlm_proposed_name","vlm_model","vlm_analyzed_at","content_hash","file_ref","path_search","text_stage_done","vlm_full_model","vlm_inherited_from") VALUES(3,'{root}/3.jpg',3,NULL,1,NULL,NULL,0.0,'image','jpg',NULL,NULL,0,0,NULL,NULL,NULL,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,NULL,NULL);
INSERT INTO "persons"("id","name","representative_face_id","file_count","created_at","title","first_name","middle_name","last_name","suffix","is_unknown","centroid","anchor_radius","last_clustered_at") VALUES(1,'Alex',2,3,123.0,'Aunt','Alex',NULL,NULL,NULL,0,NULL,NULL,NULL);
INSERT INTO "face_prints"("id","file_id","person_id","print_data","bbox","arcface_embedding","face_quality","excluded","embedding_model","processing_version","source_revision") VALUES(1,1,1,X'00','[0,0,1,1]',X'0000803f',0.4,0,NULL,NULL,NULL);
INSERT INTO "face_prints"("id","file_id","person_id","print_data","bbox","arcface_embedding","face_quality","excluded","embedding_model","processing_version","source_revision") VALUES(2,2,1,X'00','[0,0,1,1]',X'0000803f',0.9,0,NULL,NULL,NULL);
INSERT INTO "face_prints"("id","file_id","person_id","print_data","bbox","arcface_embedding","face_quality","excluded","embedding_model","processing_version","source_revision") VALUES(3,3,1,X'00','[0,0,1,1]',X'0000803f',0.5,0,NULL,NULL,NULL);
INSERT INTO "face_verifications"(rowid,"person_a","person_b","same_person","confidence","vlm_model","verified_at","face_a","face_b","file_a","bbox_a","file_b","bbox_b") VALUES(1,1,2,1,1.0,'user-merged',1794900923.5878038,1,3,1,'[0,0,1,1]',3,'[0,0,1,1]');
INSERT INTO "catalog_operations"(rowid,"id","plan_json","inverse_json","state","created_at") VALUES(1,'03fdb6cc-cb79-4d3f-acb6-ce526481442e','{"personID":1,"resolution":"merge","type":"person_merge"}','{"keepNames":[null,null,null,null,null,null],"keepRepresentative":1,"keepUnknown":1,"other":{"anchor_radius":null,"created_at":123,"first_name":"Alex","is_unknown":0,"last_clustered_at":null,"last_name":null,"middle_name":null,"name":"Alex","representative_face_id":3,"suffix":null,"title":"Aunt"},"otherPersonID":2,"personID":1,"resolution":"merge","verifications":[{"bboxA":null,"bboxB":null,"confidence":1,"faceA":1,"faceB":2,"fileA":null,"fileB":null,"model":"user-verified","personA":1,"personB":2,"samePerson":0,"verifiedAt":5}]}','completed',1794900923.586216);
INSERT INTO "catalog_corrections"(rowid,"id","file_id","kind","before_json","after_json","created_at") VALUES(1,'03fdb6cc-cb79-4d3f-acb6-ce526481442e',1,'person_merge','{"keepNames":[null,null,null,null,null,null],"keepRepresentative":1,"keepUnknown":1,"other":{"anchor_radius":null,"created_at":123,"first_name":"Alex","is_unknown":0,"last_clustered_at":null,"last_name":null,"middle_name":null,"name":"Alex","representative_face_id":3,"suffix":null,"title":"Aunt"},"otherPersonID":2,"personID":1,"resolution":"merge","verifications":[{"bboxA":null,"bboxB":null,"confidence":1,"faceA":1,"faceB":2,"fileA":null,"fileB":null,"model":"user-verified","personA":1,"personB":2,"samePerson":0,"verifiedAt":5}]}','{"faceIDs":[2,3],"otherPersonID":2,"personID":1,"resolution":"merge"}',1794900923.586216);
INSERT INTO "catalog_vector_state"(rowid,"namespace","instance_id","generation","nonce") VALUES(1,'clip','BE2016F575B1C51AECA712E28508D944',0,'BE2016F575B1C51AECA712E28508D944');
INSERT INTO "catalog_vector_state"(rowid,"namespace","instance_id","generation","nonce") VALUES(2,'text','B9433DA3A050F2C539E28D07FCB0F012',0,'B9433DA3A050F2C539E28D07FCB0F012');
INSERT INTO "catalog_vector_state"(rowid,"namespace","instance_id","generation","nonce") VALUES(3,'catalog','9C4D9C1A250EC80A96308DEF6D29DF42',0,'9C4D9C1A250EC80A96308DEF6D29DF42');
INSERT INTO "catalog_file_fts"(rowid,"path","description") VALUES(1,'{root}/1.jpg','');
INSERT INTO "catalog_file_fts"(rowid,"path","description") VALUES(2,'{root}/2.jpg','');
INSERT INTO "catalog_file_fts"(rowid,"path","description") VALUES(3,'{root}/3.jpg','');
INSERT INTO "ocr_fts"("ocr_fts") VALUES('rebuild');
INSERT INTO "doc_fts"("doc_fts") VALUES('rebuild');
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence(rowid,name,seq) VALUES(1,'files',3);
INSERT INTO sqlite_sequence(rowid,name,seq) VALUES(2,'persons',2);
INSERT INTO sqlite_sequence(rowid,name,seq) VALUES(3,'face_prints',3);
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
CREATE INDEX face_refresh_retry ON face_refresh_failures(retry_after);
CREATE TRIGGER ocr_text_vlm_invalidate AFTER UPDATE OF size_bytes,modified_at ON files
 WHEN old.size_bytes != new.size_bytes OR old.modified_at IS NOT new.modified_at BEGIN
 DELETE FROM ocr_text WHERE file_id=new.id AND source='vlm';
END;
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
CREATE INDEX catalog_files_dir ON files(rtrim(replace(path_text,char(92),'/'),replace(replace(path_text,char(92),'/'),'/','')),failed,size_bytes);
CREATE INDEX take_proposals_session ON take_proposals(session);
CREATE INDEX face_assignment_assessed_outcome ON face_assignment_assessed(outcome);
CREATE INDEX face_review_candidates_person ON face_review_candidates(person_id,score DESC);
CREATE INDEX face_merge_suggestions_other ON face_merge_suggestions(other_person_id);
CREATE UNIQUE INDEX face_queue_dismissals_key ON face_queue_dismissals(kind,file_id,bbox,COALESCE(other_file_id,0),COALESCE(other_bbox,''));
CREATE INDEX face_refresh_backlog_state ON face_refresh_backlog(embedding_model,processing_version,state,face_id);
CREATE INDEX catalog_observations_file ON catalog_observations(file_id);
CREATE INDEX catalog_embeddings_file ON catalog_embeddings(file_id) WHERE file_id IS NOT NULL;
CREATE INDEX catalog_coverage_file ON catalog_coverage(file_id);
CREATE INDEX catalog_take_scores_file ON catalog_take_scores(file_id);
CREATE INDEX catalog_event_files_file ON catalog_event_files(file_id);
CREATE INDEX catalog_assets_derived ON catalog_assets(derived_id);
CREATE INDEX catalog_corrections_file ON catalog_corrections(file_id) WHERE file_id IS NOT NULL;
CREATE INDEX face_identity_constraints_anchor ON face_identity_constraints(anchor_file_id);
CREATE INDEX face_queue_dismissals_file ON face_queue_dismissals(file_id);
CREATE INDEX face_queue_dismissals_other ON face_queue_dismissals(other_file_id) WHERE other_file_id IS NOT NULL;
CREATE INDEX face_review_candidates_runner_up ON face_review_candidates(runner_up_person_id) WHERE runner_up_person_id IS NOT NULL;
COMMIT;
