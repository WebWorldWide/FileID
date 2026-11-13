use super::*;
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> (Directory, Arc<CatalogIndex>) {
    let directory = Directory(std::env::temp_dir().join(format!("fileid-catalog-index-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("catalog.sqlite");
    let writer = Arc::new(Mutex::new(db::open_writer(&path).unwrap()));
    let scheduler = ResourceScheduler::new(|| (16384, 12000), 1, 1);
    let index = CatalogIndex::with_scheduler(writer, path, directory.0.join("index"), scheduler);
    for (id, axis, model, failed) in [
        (1, 0, clip_space::model_id(), false),
        (2, 0, clip_space::model_id(), false),
        (3, 1, clip_space::model_id(), false),
        (4, 0, clip_space::model_id(), true),
        (5, 0, "mobileclip_s2", false),
    ] {
        let conn = index.writer.lock();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,vlm_description,failed) VALUES(?1,?2,?1,100,10,0,'image','png',?3,?4)",params![id,format!("/internal/{id}.png"),if id==2 {"birthday gifts"} else {"fixture"},failed]).unwrap();
        let blob = vector(axis).iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
        conn.execute(
            "INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?1,?2,?3)",
            params![id, blob, model],
        )
        .unwrap();
    }
    (directory, index)
}
fn vector(axis: usize) -> Vec<f32> {
    let mut vector = vec![0.0; 512];
    vector[axis] = 1.0;
    vector
}
fn request(value: serde_json::Value) -> CatalogRequest {
    serde_json::from_value(value).unwrap()
}
fn seed() -> CatalogRequest {
    request(serde_json::json!({"requestID":"semantic","action":"search","searchMode":"semantic","fileID":1}))
}
fn control(action: &str) -> CatalogRequest {
    request(serde_json::json!({"requestID":"control","action":action,"jobID":CatalogIndex::job_id()}))
}
fn wait(index: &CatalogIndex) {
    // The Intel macOS runner needed more than 20 s under the full parallel suite.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut state = index.state.lock();
    while state.worker.is_some() {
        assert!(std::time::Instant::now() < deadline, "catalog worker timed out");
        index.changed.wait_for(&mut state, std::time::Duration::from_millis(20));
    }
}
fn state(index: &CatalogIndex) -> String {
    index
        .writer
        .lock()
        .query_row(
            "SELECT state FROM catalog_jobs WHERE id=?1",
            [CatalogIndex::job_id()],
            |row| row.get(0),
        )
        .unwrap()
}
fn ready(index: &Arc<CatalogIndex>) {
    assert_eq!(index.search(&seed()).unwrap().status, "error");
    wait(index);
    assert_eq!(state(index), "completed");
}
#[test]
fn semantic_hybrid_scope_exclusions_and_changed_sources_are_real() {
    let (_directory, index) = fixture();
    ready(&index);
    assert_eq!(
        index
            .search(&seed())
            .unwrap()
            .hits
            .iter()
            .map(|h| h.file_id)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    index.writer.lock().execute("INSERT INTO catalog_passages(id,file_id,text,source_revision,model_version,confidence) VALUES('chapter',2,'birthday gifts','fixture','user',1)",[]).unwrap();
    let mut hybrid = request(
        serde_json::json!({"requestID":"hybrid","action":"search","query":"birthday","searchMode":"hybrid","queryVector":vector(0),"embeddingModel":clip_space::model_id()}),
    );
    let hits = index.search(&hybrid).unwrap().hits;
    assert_eq!(hits[0].file_id, 2);
    assert_eq!(hits.iter().filter(|h| h.file_id == 2).count(), 2);
    hybrid.result_scope = Some("files".into());
    assert_eq!(
        index
            .search(&hybrid)
            .unwrap()
            .hits
            .iter()
            .filter(|h| h.file_id == 2)
            .count(),
        1
    );
    let mut legacy = seed();
    legacy.file_id = Some(5);
    assert!(index.search(&legacy).is_err());
    index
        .writer
        .lock()
        .execute("UPDATE files SET size_bytes=200 WHERE id=2", [])
        .unwrap();
    ready(&index);
    assert_eq!(
        index
            .search(&seed())
            .unwrap()
            .hits
            .iter()
            .map(|h| h.file_id)
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert!(crate::commands::catalog::search(&index.writer.lock(), "birthday", &[], None)
        .unwrap()
        .iter()
        .all(|h| h.evidence_id.is_none()));
    index.writer.lock().execute("DELETE FROM files WHERE id=3", []).unwrap();
    ready(&index);
    assert!(index.search(&seed()).unwrap().hits.is_empty());
    index.shutdown();
}
#[test]
fn graph_roundtrip_is_restored_and_corruption_rebuilds() {
    let (_directory, index) = fixture();
    ready(&index);
    let expected = index
        .search(&seed())
        .unwrap()
        .hits
        .iter()
        .map(|h| h.file_id)
        .collect::<Vec<_>>();
    let manifest = std::fs::read(index.manifest_path()).unwrap();
    let checkpoint: String = index
        .writer
        .lock()
        .query_row(
            "SELECT checkpoint_json FROM catalog_jobs WHERE id=?1",
            [CatalogIndex::job_id()],
            |row| row.get(0),
        )
        .unwrap();
    let restarted = CatalogIndex::with_scheduler(
        index.writer.clone(),
        index.database.clone(),
        index.directory.clone(),
        index.scheduler.clone(),
    );
    ready(&restarted);
    assert_eq!(std::fs::read(index.manifest_path()).unwrap(), manifest);
    assert_eq!(
        restarted
            .search(&seed())
            .unwrap()
            .hits
            .iter()
            .map(|h| h.file_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        restarted
            .writer
            .lock()
            .query_row::<String, _, _>(
                "SELECT checkpoint_json FROM catalog_jobs WHERE id=?1",
                [CatalogIndex::job_id()],
                |row| row.get(0)
            )
            .unwrap(),
        checkpoint
    );
    let manifest: Manifest = serde_json::from_slice(&manifest).unwrap();
    let graph_path = index.directory.join(format!("{}.graph", manifest.graph_hash));
    std::fs::write(&graph_path, b"corrupt").unwrap();
    let rebuilding = CatalogIndex::with_scheduler(
        index.writer.clone(),
        index.database.clone(),
        index.directory.clone(),
        index.scheduler.clone(),
    );
    ready(&rebuilding);
    assert_eq!(
        clip_space::digest(&std::fs::read(graph_path).unwrap()),
        manifest.graph_hash
    );
    assert_eq!(
        rebuilding
            .search(&seed())
            .unwrap()
            .hits
            .iter()
            .map(|h| h.file_id)
            .collect::<Vec<_>>(),
        expected
    );
    rebuilding.shutdown();
    restarted.shutdown();
    index.shutdown();
}
#[test]
fn incompatible_model_nonce_dimension_and_topology_never_restore() {
    let (_directory, index) = fixture();
    ready(&index);
    let conn = db::open_read(&index.database).unwrap();
    let latest = revision(&conn).unwrap();
    let original = std::fs::read(index.manifest_path()).unwrap();
    for field in ["model", "dimension", "revision"] {
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value[field] = match field {
            "model" => serde_json::json!("mobileclip_s2"),
            "dimension" => serde_json::json!(128),
            _ => {
                serde_json::json!({"instance":latest.instance,"generation":latest.generation,"nonce":"wrong"})
            }
        };
        std::fs::write(index.manifest_path(), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(index.load(&conn, &latest).is_err());
    }
    std::fs::write(index.manifest_path(), &original).unwrap();
    let manifest: Manifest = serde_json::from_slice(&original).unwrap();
    assert_eq!(manifest.format, Some(snapshot::VERSION));
    let graph_path = index.directory.join(format!("{}.graph", manifest.graph_hash));
    let bytes = std::fs::read(graph_path).unwrap();
    let shape = CatalogIndex::shape();
    let read = |bytes: &[u8]| snapshot::read_graph(bytes, bytes.len() as u64, &clip_space::digest(bytes), &shape);
    assert!(read(&bytes).is_ok());
    let header = 8 + 4 + 4 + 4 + clip_space::model_id().len() + 8;
    let first_neighbour = header + 8 + 8 + 3 * (8 + 4 * 512) + 8;
    let mut topology = bytes.clone();
    topology[first_neighbour..first_neighbour + 4].copy_from_slice(&200_000u32.to_le_bytes());
    assert!(read(&topology).err().unwrap().to_string().contains("out of bounds"));
    let mut dimension = bytes.clone();
    dimension[12..16].copy_from_slice(&1u32.to_le_bytes());
    assert!(read(&dimension).is_err());
    let mut vector = bytes.clone();
    let first_value = header + 8 + 8 + 8;
    vector[first_value..first_value + 4].copy_from_slice(&9.0f32.to_le_bytes());
    assert!(read(&vector).err().unwrap().to_string().contains("vector"));

    let cache = index.state.lock().cache.clone().unwrap();
    let json = serde_json::to_vec(&*cache.graph).unwrap();
    assert!(validate_graph(&json).is_ok());
    let mut graph: serde_json::Value = serde_json::from_slice(&json).unwrap();
    graph["hnsw"]["zero"][0][0] = serde_json::json!(200000);
    assert!(validate_graph(&serde_json::to_vec(&graph).unwrap()).is_err());
    graph["hnsw"]["points"][0] = serde_json::json!([1.0]);
    assert!(validate_graph(&serde_json::to_vec(&graph).unwrap()).is_err());
    index.shutdown();
}
#[test]
fn coalesced_jobs_pause_resume_cancel_and_restart_without_writer_lock() {
    let (_directory, index) = fixture();
    let lease = index
        .scheduler
        .reserve(
            Demand {
                memory_mb: 0,
                cpu: 1,
                io: 1,
            },
            Priority::Background,
            || false,
        )
        .unwrap();
    assert_eq!(index.search(&seed()).unwrap().status, "error");
    let worker = index.state.lock().worker.clone().unwrap();
    assert_eq!(index.search(&seed()).unwrap().status, "error");
    assert!(Arc::ptr_eq(&worker, index.state.lock().worker.as_ref().unwrap()));
    assert!(index.writer.try_lock().is_some());
    assert!(index.control(&control("resumeJob")).is_err());
    index.control(&control("pauseJob")).unwrap();
    assert_eq!(state(&index), "paused");
    assert!(index.search(&seed()).is_err());
    drop(lease);
    let keyword = request(serde_json::json!({"requestID":"keyword","action":"search","query":"birthday"}));
    assert_eq!(index.search(&keyword).unwrap().hits.len(), 1);
    let lease = index
        .scheduler
        .reserve(
            Demand {
                memory_mb: 0,
                cpu: 1,
                io: 1,
            },
            Priority::Background,
            || false,
        )
        .unwrap();
    index.control(&control("resumeJob")).unwrap();
    index.control(&control("cancelJob")).unwrap();
    assert_eq!(state(&index), "cancelled");
    index.control(&control("resumeJob")).unwrap();
    drop(lease);
    wait(&index);
    assert_eq!(state(&index), "completed");
    index
        .writer
        .lock()
        .execute(
            "UPDATE catalog_jobs SET state='running',progress=0.5 WHERE id=?1",
            [CatalogIndex::job_id()],
        )
        .unwrap();
    CatalogIndex::recover(&index.writer.lock()).unwrap();
    assert_eq!(state(&index), "paused");
    index.state.lock().cache = None;
    assert!(index.search(&seed()).is_err());
    index.control(&control("resumeJob")).unwrap();
    wait(&index);
    assert_eq!(state(&index), "completed");
    index.shutdown();
}
#[test]
fn delta_gap_rebuilds_invalid_vectors_are_excluded_and_snapshots_are_bounded() {
    let (_directory, index) = fixture();
    ready(&index);
    index
        .writer
        .lock()
        .execute(
            "UPDATE clip_embeddings SET embedding=?1 WHERE file_id=2",
            [vec![0u8; 2048]],
        )
        .unwrap();
    index
        .writer
        .lock()
        .execute("DELETE FROM catalog_vector_changes", [])
        .unwrap();
    ready(&index);
    assert_eq!(
        index
            .search(&seed())
            .unwrap()
            .hits
            .iter()
            .map(|h| h.file_id)
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert!(bounded_read(&index.manifest_path(), 1).is_err());
    index.shutdown();
}

#[test]
fn fingerprint_eligibility_and_terminal_close_are_revalidated() {
    let (_directory, index) = fixture();
    ready(&index);
    index
        .writer
        .lock()
        .execute("UPDATE files SET failed=1 WHERE id=2", [])
        .unwrap();
    ready(&index);
    assert_eq!(
        index
            .search(&seed())
            .unwrap()
            .hits
            .iter()
            .map(|h| h.file_id)
            .collect::<Vec<_>>(),
        vec![3]
    );
    index
        .writer
        .lock()
        .execute("UPDATE files SET failed=0 WHERE id=2", [])
        .unwrap();
    ready(&index);
    {
        let mut state = index.state.lock();
        let cache = Arc::get_mut(state.cache.as_mut().unwrap()).unwrap();
        Arc::get_mut(&mut cache.graph)
            .unwrap()
            .values
            .iter_mut()
            .find(|entry| entry.file_id == 2)
            .unwrap()
            .fingerprint = "0".repeat(64);
    }
    assert_eq!(
        index
            .search(&seed())
            .unwrap()
            .hits
            .iter()
            .map(|h| h.file_id)
            .collect::<Vec<_>>(),
        vec![3]
    );
    index.close();
    assert!(index.prepare().is_err());
    assert!(index.state.lock().worker.is_none());
}

#[test]
fn admission_defers_durably_without_false_completion_or_keyword_loss() {
    let (_directory, index) = fixture();
    let unavailable = CatalogIndex::with_scheduler(
        index.writer.clone(),
        index.database.clone(),
        index.directory.clone(),
        ResourceScheduler::new(|| (16384, 1), 1, 1),
    );
    assert_eq!(unavailable.search(&seed()).unwrap().status, "error");
    wait(&unavailable);
    assert_eq!(state(&unavailable), "paused");
    assert!(unavailable.state.lock().cache.is_none());
    let keyword = request(serde_json::json!({"requestID":"keyword","action":"search","query":"birthday"}));
    assert_eq!(unavailable.search(&keyword).unwrap().hits.len(), 1);
    assert!(!unavailable.manifest_path().exists());
    unavailable.shutdown();
    index.shutdown();
}

#[test]
fn cold_responses_match_schema_and_do_not_claim_similarity_completion() {
    let (_directory, index) = fixture();
    let hybrid = request(
        serde_json::json!({"requestID":"hybrid","action":"search","searchMode":"hybrid","query":"birthday","queryVector":vector(0),"embeddingModel":clip_space::model_id()}),
    );
    let response = index.search(&hybrid).unwrap();
    assert_eq!(response.status, "ok");
    assert_eq!(response.hits.len(), 1);
    assert_eq!(response.hits[0].file_id, 2);
    assert!(response.message.as_deref().unwrap().contains("Keyword results only"));
    assert!(response.jobs.iter().any(|job| job.kind == "catalogIndex"));
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../../../shared/ipc-schema/ipc.schema.json")).unwrap();
    let statuses = schema["$defs"]["CatalogResponse"]["properties"]["status"]["enum"]
        .as_array()
        .unwrap();
    assert!(statuses.contains(&serde_json::to_value(&response).unwrap()["status"]));
    wait(&index);
    index.close();
    let (_directory, semantic) = fixture();
    let response = semantic.search(&seed()).unwrap();
    assert_eq!(response.status, "error");
    assert!(response.hits.is_empty());
    assert!(response.message.as_deref().unwrap().contains("not available yet"));
    assert!(!response.jobs.is_empty());
    assert!(statuses.contains(&serde_json::to_value(&response).unwrap()["status"]));
    semantic.close();
}
#[test]
fn retained_memory_refresh_defers_then_cancel_resume_can_release_and_rebuild() {
    let (_directory, original) = fixture();
    ready(&original);
    let index = CatalogIndex::with_scheduler(
        original.writer.clone(),
        original.database.clone(),
        original.directory.clone(),
        ResourceScheduler::new(|| (3072, 3072), 1, 1),
    );
    ready(&index);
    {
        let mut state = index.state.lock();
        let cache = Arc::get_mut(state.cache.as_mut().unwrap()).unwrap();
        cache.lease = None;
        cache.lease = Some(
            index
                .scheduler
                .reserve(
                    Demand {
                        memory_mb: 1000,
                        cpu: 0,
                        io: 0,
                    },
                    Priority::Background,
                    || false,
                )
                .unwrap(),
        );
    }
    index
        .writer
        .lock()
        .execute("UPDATE files SET failed=1 WHERE id=2", [])
        .unwrap();
    assert_eq!(index.search(&seed()).unwrap().status, "error");
    wait(&index);
    assert_eq!(state(&index), "paused");
    assert!(index.state.lock().worker.is_none());
    let error: String = index
        .writer
        .lock()
        .query_row(
            "SELECT error FROM catalog_jobs WHERE id=?1",
            [CatalogIndex::job_id()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(error.contains("resident memory"));
    index.control(&control("cancelJob")).unwrap();
    index.control(&control("resumeJob")).unwrap();
    wait(&index);
    assert_eq!(state(&index), "completed");
    assert_eq!(
        index
            .search(&seed())
            .unwrap()
            .hits
            .iter()
            .map(|hit| hit.file_id)
            .collect::<Vec<_>>(),
        vec![3]
    );
    index.close();
    original.close();
}
#[test]
fn invalid_cache_candidates_are_rejected_before_constrained_memory_admission() {
    let (_directory, original) = fixture();
    ready(&original);
    for invalid in ["oversized", "model", "dimension", "revision"] {
        let mut manifest: Manifest = serde_json::from_slice(&std::fs::read(original.manifest_path()).unwrap()).unwrap();
        let graph = original.directory.join(format!("{}.graph", manifest.graph_hash));
        std::fs::OpenOptions::new()
            .write(true)
            .open(graph)
            .unwrap()
            .set_len(if invalid == "oversized" {
                MAX_GRAPH_BYTES + 1
            } else {
                MAX_GRAPH_BYTES
            })
            .unwrap();
        match invalid {
            "model" => manifest.model = "legacy".into(),
            "dimension" => manifest.dimension = 128,
            "revision" => manifest.revision.nonce = "wrong".into(),
            _ => {}
        }
        std::fs::write(original.manifest_path(), serde_json::to_vec(&manifest).unwrap()).unwrap();
        let index = CatalogIndex::with_scheduler(
            original.writer.clone(),
            original.database.clone(),
            original.directory.clone(),
            ResourceScheduler::new(|| (8192, 4096), 1, 1),
        );
        ready(&index);
        assert_eq!(
            index
                .search(&seed())
                .unwrap()
                .hits
                .iter()
                .map(|hit| hit.file_id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(
            std::fs::metadata(original.directory.join(format!("{}.graph", manifest.graph_hash)))
                .unwrap()
                .len()
                < MAX_GRAPH_BYTES
        );
        index.close();
    }
    original.close();
}

#[test]
fn bounded_corrupt_graph_with_compatible_manifest_rebuilds_under_constrained_memory() {
    let (_directory, original) = fixture();
    ready(&original);
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(original.manifest_path()).unwrap()).unwrap();
    let graph = original.directory.join(format!("{}.graph", manifest.graph_hash));
    std::fs::OpenOptions::new()
        .write(true)
        .open(&graph)
        .unwrap()
        .set_len(MAX_GRAPH_BYTES)
        .unwrap();
    assert_eq!(std::fs::metadata(&graph).unwrap().len(), MAX_GRAPH_BYTES);
    let conn = db::open_read(&original.database).unwrap();
    assert!(original.cache_candidate(&conn, &revision(&conn).unwrap()).is_ok());
    drop(conn);
    let index = CatalogIndex::with_scheduler(
        original.writer.clone(),
        original.database.clone(),
        original.directory.clone(),
        ResourceScheduler::new(|| (8192, 4096), 1, 1),
    );
    ready(&index);
    assert_eq!(
        index
            .search(&seed())
            .unwrap()
            .hits
            .iter()
            .map(|hit| hit.file_id)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(std::fs::metadata(&graph).unwrap().len() < MAX_GRAPH_BYTES);
    assert_eq!(clip_space::digest(&std::fs::read(&graph).unwrap()), manifest.graph_hash);
    index.close();
    original.close();
}
#[test]
fn capture_window_keeps_files_inside_it_and_rejects_backwards_ranges() {
    let (_directory, index) = fixture();
    {
        let conn = index.writer.lock();
        conn.execute("UPDATE files SET created_at=1000 WHERE id=1", []).unwrap();
        conn.execute("UPDATE files SET created_at=2000 WHERE id=3", []).unwrap();
    }
    let ids = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"requestID":"window","action":"search","query":"fixture"});
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let mut hits = index.search(&request(body)).unwrap().hits.iter().map(|h| h.file_id).collect::<Vec<_>>();
        hits.sort_unstable();
        hits
    };
    assert_eq!(ids(serde_json::json!({})), vec![1, 3, 5]);
    assert_eq!(ids(serde_json::json!({"capturedFrom": 500, "capturedTo": 1500})), vec![1]);
    assert_eq!(ids(serde_json::json!({"capturedTo": 1500})), vec![1, 5]);
    assert_eq!(ids(serde_json::json!({"capturedFrom": 1500})), vec![3]);
    assert_eq!(ids(serde_json::json!({"capturedFrom": 0, "capturedTo": 9999, "limit": 2})).len(), 2);
    let backwards = request(serde_json::json!({"requestID":"bad","action":"search","query":"fixture","capturedFrom":5000,"capturedTo":1000}));
    assert!(index.search(&backwards).is_err());
}
#[test]
fn capture_window_reaches_files_beyond_the_ranked_hundred() {
    let (_directory, index) = fixture();
    {
        let conn = index.writer.lock();
        for id in 100..400 {
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,created_at,vlm_description) VALUES(?1,?2,?1,1,0,'image','jpg',?3,'harbor boats')",
                rusqlite::params![id, format!("/lib/harbor_{id}.jpg"), if id < 380 { 5_000.0 } else { 100.0 }],
            )
            .unwrap();
        }
    }
    let body = serde_json::json!({"requestID":"old","action":"search","query":"harbor","capturedTo":200});
    let hits = index.search(&request(body)).unwrap().hits;
    assert_eq!(hits.len(), 20, "all 20 old harbor photos, not only those in the ranked hundred");
    assert!(hits.iter().all(|hit| hit.file_id >= 380));
}
#[test]
fn text_search_puts_good_matches_first_and_keyword_evidence_makes_a_file_good() {
    let directory = Directory(std::env::temp_dir().join(format!("fileid-catalog-strength-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("catalog.sqlite");
    let writer = Arc::new(Mutex::new(db::open_writer(&path).unwrap()));
    let index = CatalogIndex::with_scheduler(writer, path, directory.0.join("index"), ResourceScheduler::new(|| (16384, 12000), 1, 1));
    let fixture: serde_json::Value = serde_json::from_str(include_str!("../../../../../shared/test-corpus/search-strength.json")).unwrap();
    let case = &fixture["cases"][0];
    let dense = |sparse: &serde_json::Value| {
        let mut vector = vec![0f32; 512];
        for (key, value) in sparse.as_object().unwrap() {
            vector[key.parse::<usize>().unwrap()] = value.as_f64().unwrap() as f32;
        }
        vector
    };
    let ids = |key: &str| case[key].as_array().unwrap().iter().map(|id| id.as_i64().unwrap()).collect::<Vec<_>>();
    for photo in case["photos"].as_array().unwrap() {
        let id = photo["id"].as_i64().unwrap();
        let blob = dense(&photo["vector"]).iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
        let conn = index.writer.lock();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,vlm_description) VALUES(?1,?2,?1,10,100,100,'image','jpg',?3)", params![id, format!("/lib/{id}.jpg"), if id == 2 { "a red dog" } else { "" }]).unwrap();
        conn.execute("INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?1,?2,?3)", params![id, blob, clip_space::model_id()]).unwrap();
    }
    let query = dense(&case["query"]);
    let semantic = request(serde_json::json!({"requestID":"s","action":"search","query":"zebra","searchMode":"semantic","queryVector":query,"embeddingModel":clip_space::model_id(),"limit":10}));
    assert_eq!(index.judged(&semantic, &|_| None).unwrap().response.status, "error");
    wait(&index);
    let found = index.judged(&semantic, &|_| None).unwrap();
    assert_eq!(found.response.hits.iter().map(|hit| hit.file_id).collect::<Vec<_>>(), ids("order"));
    let strong = ids("strong");
    let weak: std::collections::HashSet<i64> = ids("order").into_iter().filter(|id| !strong.contains(id)).collect();
    assert_eq!(found.weak, Some(weak));
    assert_eq!(found.response.message.as_deref(), Some("3 good matches, 1 weaker."));
    let hybrid = request(serde_json::json!({"requestID":"h","action":"search","query":"dog","searchMode":"hybrid","queryVector":query,"embeddingModel":clip_space::model_id(),"limit":10,"resultScope":"files"}));
    let mixed = index.judged(&hybrid, &|_| None).unwrap();
    assert_eq!(mixed.weak, Some(std::collections::HashSet::new()));
    assert_eq!(mixed.response.message.as_deref(), Some("4 good matches."));
    let seeded = index.judged(&request(serde_json::json!({"requestID":"f","action":"search","searchMode":"semantic","fileID":1})), &|_| None).unwrap();
    assert!(seeded.weak.is_none());
    assert_eq!(seeded.response.message.as_deref(), Some("Visual similarity results."));
}

fn unit(seed: u64, dimension: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut vector: Vec<f32> = (0..dimension)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % 2001) as f32 / 1000.0 - 1.0
        })
        .collect();
    normalize(&mut vector);
    vector
}
fn add_photo(index: &CatalogIndex, id: i64, vector: &[f32]) {
    let conn = index.writer.lock();
    conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,vlm_description) VALUES(?1,?2,?1,100,10,0,'image','png','extra')", params![id, format!("/internal/{id}.png")]).unwrap();
    let blob = vector.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
    conn.execute("INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?1,?2,?3)", params![id, blob, clip_space::model_id()]).unwrap();
}
fn seeded_ids(index: &Arc<CatalogIndex>) -> Vec<i64> {
    index.search(&seed()).unwrap().hits.iter().map(|hit| hit.file_id).collect()
}
fn graph_entries(graph: &HnswMap<Embedding, Entry>) -> Vec<(Vec<u32>, i64, String)> {
    graph
        .iter()
        .zip(&graph.values)
        .map(|((_, point), entry)| (point.0.iter().map(|v| v.to_bits()).collect(), entry.file_id, entry.fingerprint.clone()))
        .collect()
}

#[test]
fn small_changes_ride_beside_the_graph_and_survive_a_restart() {
    let (_directory, index) = fixture();
    ready(&index);
    let built = index.state.lock().cache.clone().unwrap();
    assert_eq!(built.pending(), 0);

    let mut near = vector(0);
    near[1] = 0.05;
    normalize(&mut near);
    add_photo(&index, 50, &near);
    ready(&index);
    let advanced = index.state.lock().cache.clone().unwrap();
    assert!(Arc::ptr_eq(&advanced.graph, &built.graph), "one new photo must not rebuild the graph");
    assert_eq!(advanced.added.len(), 1);
    assert_eq!(seeded_ids(&index), vec![2, 50, 3]);

    index.writer.lock().execute("DELETE FROM files WHERE id=2", []).unwrap();
    ready(&index);
    let advanced = index.state.lock().cache.clone().unwrap();
    assert!(Arc::ptr_eq(&advanced.graph, &built.graph));
    assert_eq!(advanced.removed, HashSet::from([2]));
    assert_eq!(seeded_ids(&index), vec![50, 3]);

    let manifest: Manifest = serde_json::from_slice(&std::fs::read(index.manifest_path()).unwrap()).unwrap();
    assert_eq!(Some(&manifest.graph_hash), built.saved.lock().graph_hash.as_ref(), "the graph file is not rewritten for a small change");
    let delta = manifest.delta_hash.clone().expect("changes are saved beside the graph");
    let files = |extension: &str| {
        std::fs::read_dir(&index.directory)
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|found| found == extension))
            .count()
    };
    assert_eq!((files("graph"), files("delta")), (1, 1), "superseded snapshots are removed");

    let restarted = CatalogIndex::with_scheduler(index.writer.clone(), index.database.clone(), index.directory.clone(), index.scheduler.clone());
    ready(&restarted);
    let restored = restarted.state.lock().cache.clone().unwrap();
    assert_eq!(restored.added.len(), 1);
    assert_eq!(restored.removed, HashSet::from([2]));
    assert_eq!(seeded_ids(&restarted), vec![50, 3]);
    assert_eq!(std::fs::read(index.manifest_path()).unwrap(), serde_json::to_vec(&manifest).unwrap(), "a restart at the same revision writes nothing");

    let delta_path = index.directory.join(format!("{delta}.delta"));
    let mut bytes = std::fs::read(&delta_path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&delta_path, &bytes).unwrap();
    let rebuilt = CatalogIndex::with_scheduler(index.writer.clone(), index.database.clone(), index.directory.clone(), index.scheduler.clone());
    ready(&rebuilt);
    assert_eq!(rebuilt.state.lock().cache.as_ref().unwrap().pending(), 0, "a damaged change file falls back to a rebuild");
    assert_eq!(seeded_ids(&rebuilt), vec![50, 3]);
    rebuilt.close();
    restarted.close();
    index.close();
}

#[test]
fn a_change_that_returns_to_the_indexed_vector_needs_no_extra_entry() {
    let (_directory, index) = fixture();
    ready(&index);
    index.writer.lock().execute("UPDATE files SET failed=1 WHERE id=2", []).unwrap();
    ready(&index);
    assert_eq!(index.state.lock().cache.as_ref().unwrap().removed, HashSet::from([2]));
    index.writer.lock().execute("UPDATE files SET failed=0 WHERE id=2", []).unwrap();
    ready(&index);
    let cache = index.state.lock().cache.clone().unwrap();
    assert_eq!(cache.pending(), 0);
    assert_eq!(seeded_ids(&index), vec![2, 3]);
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(index.manifest_path()).unwrap()).unwrap();
    assert!(manifest.delta_hash.is_none());
    index.close();
}

#[test]
fn many_changes_rebuild_the_graph_once_and_fold_them_in() {
    assert!(!needs_rebuild(3, MIN_PENDING_BEFORE_REBUILD));
    assert!(needs_rebuild(3, MIN_PENDING_BEFORE_REBUILD + 1));
    assert!(!needs_rebuild(100_000, 10_000));
    assert!(needs_rebuild(100_000, 10_001));

    let (_directory, index) = fixture();
    ready(&index);
    let built = index.state.lock().cache.clone().unwrap();
    for id in 1000..=1000 + MIN_PENDING_BEFORE_REBUILD as i64 {
        add_photo(&index, id, &unit(id as u64, 512));
    }
    ready(&index);
    let rebuilt = index.state.lock().cache.clone().unwrap();
    assert!(!Arc::ptr_eq(&rebuilt.graph, &built.graph));
    assert_eq!(rebuilt.pending(), 0);
    assert_eq!(rebuilt.graph.values.len(), 3 + MIN_PENDING_BEFORE_REBUILD + 1);
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(index.manifest_path()).unwrap()).unwrap();
    assert!(manifest.delta_hash.is_none());
    assert_ne!(Some(&manifest.graph_hash), built.saved.lock().graph_hash.as_ref());
    let hits = seeded_ids(&index);
    assert_eq!(hits[0], 2, "the identical photo stays the closest after a rebuild");
    index.close();
}

#[test]
fn a_failed_save_keeps_search_working_and_is_retried() {
    let (_directory, index) = fixture();
    std::fs::write(&index.directory, b"in the way").unwrap();
    ready(&index);
    assert_eq!(seeded_ids(&index), vec![2, 3], "search runs from memory when the snapshot cannot be written");
    let cache = index.state.lock().cache.clone().unwrap();
    assert!(cache.saved.lock().retry_at.is_some());
    assert!(cache.saved.lock().graph_hash.is_none());

    assert_eq!(seeded_ids(&index), vec![2, 3]);
    assert!(index.state.lock().worker.is_none(), "no retry before its time");

    std::fs::remove_file(&index.directory).unwrap();
    cache.saved.lock().retry_at = Some(std::time::Instant::now());
    assert_eq!(seeded_ids(&index), vec![2, 3]);
    wait(&index);
    assert!(index.manifest_path().exists());
    assert!(cache.saved.lock().retry_at.is_none());
    assert_eq!(state(&index), "completed");

    let restarted = CatalogIndex::with_scheduler(index.writer.clone(), index.database.clone(), index.directory.clone(), index.scheduler.clone());
    ready(&restarted);
    assert_eq!(seeded_ids(&restarted), vec![2, 3]);
    restarted.close();
    index.close();
}

#[test]
fn a_json_snapshot_from_an_older_engine_is_read_once_and_rewritten_as_binary() {
    let (_directory, index) = fixture();
    ready(&index);
    let cache = index.state.lock().cache.clone().unwrap();
    let binary: Manifest = serde_json::from_slice(&std::fs::read(index.manifest_path()).unwrap()).unwrap();
    let json = serde_json::to_vec(&*cache.graph).unwrap();
    let legacy_hash = clip_space::digest(&json);
    std::fs::remove_file(index.directory.join(format!("{}.graph", binary.graph_hash))).unwrap();
    std::fs::write(index.directory.join(format!("{legacy_hash}.graph")), &json).unwrap();
    let legacy = serde_json::json!({"model": binary.model, "dimension": binary.dimension, "revision": binary.revision, "graph_hash": legacy_hash});
    std::fs::write(index.manifest_path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
    index.close();

    let migrated = CatalogIndex::with_scheduler(index.writer.clone(), index.database.clone(), index.directory.clone(), index.scheduler.clone());
    ready(&migrated);
    assert_eq!(seeded_ids(&migrated), vec![2, 3]);
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(index.manifest_path()).unwrap()).unwrap();
    assert_eq!(manifest.format, Some(snapshot::VERSION));
    assert_eq!(manifest.graph_hash, binary.graph_hash, "the migrated graph is the one the JSON held");
    assert!(!index.directory.join(format!("{legacy_hash}.graph")).exists(), "the JSON snapshot is removed after migration");
    let restored = migrated.state.lock().cache.clone().unwrap();
    assert_eq!(graph_entries(&restored.graph), graph_entries(&cache.graph));
    migrated.close();
}

#[test]
fn truncated_and_damaged_binary_snapshots_are_refused_without_panicking() {
    let (_directory, index) = fixture();
    ready(&index);
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(index.manifest_path()).unwrap()).unwrap();
    let bytes = std::fs::read(index.directory.join(format!("{}.graph", manifest.graph_hash))).unwrap();
    let shape = CatalogIndex::shape();
    assert!(snapshot::read_graph(&bytes[..], bytes.len() as u64, &manifest.graph_hash, &shape).is_ok());
    for cut in [0usize, 7, 8, 20, 40, 60, 100, bytes.len() / 3, bytes.len() / 2, bytes.len() - 80, bytes.len() - 1] {
        let part = &bytes[..cut];
        assert!(snapshot::read_graph(part, part.len() as u64, &clip_space::digest(part), &shape).is_err(), "cut at {cut}");
        assert!(snapshot::read_graph(part, bytes.len() as u64, &manifest.graph_hash, &shape).is_err(), "short read at {cut}");
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(snapshot::read_graph(&longer[..], longer.len() as u64, &clip_space::digest(&longer), &shape).is_err());
    assert!(snapshot::read_graph(&longer[..], bytes.len() as u64, &manifest.graph_hash, &shape).is_err());

    let mut state = 0x1234_5678_9ABC_DEF1u64;
    for _ in 0..400 {
        let mut damaged = bytes.clone();
        for _ in 0..3 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let at = (state as usize) % damaged.len();
            damaged[at] ^= 1 << (state >> 61);
        }
        assert!(snapshot::read_graph(&damaged[..], damaged.len() as u64, &manifest.graph_hash, &shape).is_err());
        if let Ok(graph) = snapshot::read_graph(&damaged[..], damaged.len() as u64, &clip_space::digest(&damaged), &shape) {
            let _ = Searcher::default().top_k(&graph, &vector(0), 10);
        }
    }
    let huge = {
        let mut claim = bytes.clone();
        let count_at = 8 + 4 + 4 + 4 + clip_space::model_id().len();
        claim[count_at..count_at + 8].copy_from_slice(&(MAX_FILES as u64).to_le_bytes());
        claim
    };
    assert!(snapshot::read_graph(&huge[..], huge.len() as u64, &clip_space::digest(&huge), &shape).is_err());
    index.close();
}

#[test]
fn a_library_past_eighty_thousand_photos_round_trips_within_the_size_limit() {
    const COUNT: usize = 81_000;
    const DIMENSION: usize = 8;
    let points: Vec<(Vec<f32>, Entry)> = (0..COUNT)
        .map(|index| (unit(index as u64 + 1, DIMENSION), Entry { file_id: index as i64 + 1, fingerprint: format!("{:064x}", index) }))
        .collect();
    let graph = hnsw_index::build_search_index(points, EF_SEARCH);
    let valid = |vector: &[f32]| vector.len() == DIMENSION && vector.iter().all(|value| value.is_finite());
    let shape = snapshot::Shape { model: "test-model", dimension: DIMENSION, max_points: MAX_FILES, ef_search: EF_SEARCH as u64, valid: &valid };
    let mut bytes = Vec::new();
    let (hash, written) = snapshot::write_graph(&mut bytes, shape.model, DIMENSION, &graph, &|| false).unwrap();
    assert_eq!(written, bytes.len() as u64);
    assert_eq!(hash, clip_space::digest(&bytes));
    assert!(written <= snapshot::max_graph_bytes(DIMENSION, COUNT), "{written} bytes for {COUNT} points");
    let per_point_overhead = (written - (COUNT * 4 * DIMENSION) as u64).div_ceil(COUNT as u64);
    let at_full_size = MAX_FILES as u64 * (4 * clip_space::DIMENSION as u64 + per_point_overhead);
    assert!(at_full_size <= MAX_GRAPH_BYTES, "a full library of real vectors needs {at_full_size} bytes");
    assert!(COUNT as u64 * (4 * clip_space::DIMENSION as u64 + per_point_overhead) < LEGACY_MAX_GRAPH_BYTES);
    const { assert!(MAX_GRAPH_BYTES < 1 << 30) };

    let restored = snapshot::read_graph(&bytes[..], written, &hash, &shape).unwrap();
    assert_eq!(graph_entries(&restored), graph_entries(&graph));
    let mut again = Vec::new();
    snapshot::write_graph(&mut again, shape.model, DIMENSION, &restored, &|| false).unwrap();
    assert!(again == bytes, "the restored graph has the same neighbours");
    let query = unit(7, DIMENSION);
    let ids = |graph: &HnswMap<Embedding, Entry>| Searcher::default().top_k(graph, &query, 50).into_iter().map(|(entry, _)| entry.file_id).collect::<Vec<_>>();
    assert_eq!(ids(&restored), ids(&graph));
    assert_eq!(ids(&graph)[0], 7);

    let stopped = snapshot::write_graph(&mut Vec::new(), shape.model, DIMENSION, &graph, &|| true);
    assert!(stopped.is_err(), "a cancelled save stops writing");
}

#[test]
fn change_files_round_trip_and_reject_damage() {
    let valid = |vector: &[f32]| vector.len() == 4;
    let shape = snapshot::Shape { model: "m", dimension: 4, max_points: 10, ef_search: EF_SEARCH as u64, valid: &valid };
    let delta = snapshot::Delta {
        added: vec![(vec![1.0, 0.0, 0.0, 0.0], Entry { file_id: 9, fingerprint: "a".repeat(64) }), (vec![0.0, 1.0, 0.0, 0.0], Entry { file_id: 4, fingerprint: "b".repeat(64) })],
        removed: vec![4, 7],
    };
    let mut bytes = Vec::new();
    let (hash, written) = snapshot::write_delta(&mut bytes, 4, &delta, &|| false).unwrap();
    assert_eq!(written, bytes.len() as u64);
    assert!(written <= snapshot::max_delta_bytes(4, 2, 2));
    let restored = snapshot::read_delta(&bytes[..], written, &hash, &shape).unwrap();
    assert_eq!(restored.removed, vec![4, 7]);
    assert_eq!(restored.added.iter().map(|(vector, entry)| (vector.clone(), entry.file_id)).collect::<Vec<_>>(), vec![(vec![1.0, 0.0, 0.0, 0.0], 9), (vec![0.0, 1.0, 0.0, 0.0], 4)]);
    for cut in 0..bytes.len() {
        let part = &bytes[..cut];
        assert!(snapshot::read_delta(part, part.len() as u64, &clip_space::digest(part), &shape).is_err());
    }
    let mut repeated = bytes.clone();
    let second_id = 8 + 4 + 4 + 8 + 8 + 8 + 64 + 16;
    repeated[second_id..second_id + 8].copy_from_slice(&9i64.to_le_bytes());
    assert!(snapshot::read_delta(&repeated[..], repeated.len() as u64, &clip_space::digest(&repeated), &shape).is_err());
    assert!(snapshot::read_delta(&bytes[..], written, &"0".repeat(64), &shape).is_err());
}
