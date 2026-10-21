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
    assert!(crate::commands::catalog::search(&index.writer.lock(), "birthday", &[])
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
    let graph_path = index.directory.join(format!("{}.graph", manifest.graph_hash));
    let bytes = std::fs::read(graph_path).unwrap();
    assert!(validate_graph(&bytes).is_ok());
    let mut graph: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
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
        cache
            .graph
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
