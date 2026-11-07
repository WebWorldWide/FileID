use super::*;

fn blob(first: f32, second: f32) -> Vec<u8> {
    let mut vector = vec![0.0_f32; DIMENSION];
    vector[0] = first;
    vector[1] = second;
    vector.into_iter().flat_map(f32::to_le_bytes).collect()
}

fn file(conn: &Connection, id: i64, path: &str, time: f64, camera: Option<&str>, vector: (f32, f32)) {
    let kind = if path.rsplit('.').next() == Some("mov") { "video" } else { "image" };
    conn.execute(
        "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension,camera_model,content_hash) VALUES(?1,?2,?1,100,?3,10,0,?4,'x',?5,?6)",
        params![id, path, time, kind, camera, vec![id as u8]],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?1,?2,'mobileclip_s2')",
        params![id, blob(vector.0, vector.1)],
    )
    .unwrap();
}

fn request(action: &str) -> CatalogRequest {
    serde_json::from_value(serde_json::json!({"requestID": "r", "action": action})).unwrap()
}

fn rerun(conn: &mut Connection) {
    enqueue(conn, &request("enqueueTakeGrouping")).unwrap();
    run_blocking(conn).unwrap();
}

fn stored(conn: &Connection) -> Vec<(String, String, String)> {
    let mut statement =
        conn.prepare("SELECT relation,file_ids_json,evidence_json FROM take_proposals ORDER BY file_ids_json").unwrap();
    statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn library(conn: &Connection) {
    file(conn, 1, "/shoot/a1.jpg", 1000.0, Some("Phone A"), (1.0, 0.0));
    file(conn, 2, "/shoot/a2.jpg", 1030.0, Some("Phone A"), (0.96, 0.28));
    file(conn, 3, "/shoot/a3.jpg", 1060.0, Some("Phone A"), (0.0, 1.0));
    file(conn, 4, "/party/b1.jpg", 9000.0, Some("Phone A"), (1.0, 0.0));
    file(conn, 5, "/party/b2.jpg", 9001.0, Some("Camera B"), (0.96, 0.28));
    file(conn, 6, "/walk/c1.jpg", 20000.0, None, (1.0, 0.0));
    file(conn, 7, "/walk/c2.jpg", 20040.0, Some("Phone A"), (0.96, 0.28));
}

#[test]
fn groups_need_similar_content_and_name_the_relation() {
    let mut conn = crate::db::test_support::memory_db();
    library(&conn);
    rerun(&mut conn);
    assert_eq!(
        stored(&conn),
        vec![
            ("repeatedAttempts".into(), "[1,2]".into(), r#"["captureSession","visualSimilarity","sameCamera"]"#.into()),
            ("alternateViews".into(), "[4,5]".into(), r#"["captureSession","visualSimilarity","simultaneousCameras"]"#.into()),
            ("uncertain".into(), "[6,7]".into(), r#"["captureSession","visualSimilarity","cameraDetailsMissing"]"#.into()),
        ]
    );
    let job: (String, f64) =
        conn.query_row("SELECT state,progress FROM catalog_jobs WHERE id=?1", [JOB_ID], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(job, ("completed".into(), 1.0));
    let events: i64 = conn.query_row("SELECT COUNT(*) FROM catalog_events", [], |r| r.get(0)).unwrap();
    assert_eq!(events, 0);
    let response = proposals(&conn, &request("takeGroupProposals")).unwrap();
    let groups = response.take_groups.unwrap();
    assert_eq!(groups.len(), 3);
    assert!(groups.iter().all(|g| g.status == "open" && g.kind == "photos"));
    assert_eq!(response.jobs.len(), 1);
}

#[test]
fn reruns_are_idempotent_and_only_touch_changed_sessions() {
    let mut conn = crate::db::test_support::memory_db();
    library(&conn);
    rerun(&mut conn);
    let before: Vec<(String, f64)> = conn
        .prepare("SELECT group_key,proposed_at FROM take_proposals ORDER BY group_key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    rerun(&mut conn);
    let after: Vec<(String, f64)> = conn
        .prepare("SELECT group_key,proposed_at FROM take_proposals ORDER BY group_key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(before, after);
    file(&conn, 8, "/shoot/a4.jpg", 1090.0, Some("Phone A"), (0.98, 0.2));
    conn.execute("DELETE FROM files WHERE id=6", []).unwrap();
    rerun(&mut conn);
    let rows = stored(&conn);
    assert_eq!(rows.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(), vec!["[1,2,8]", "[4,5]"]);
    let party: f64 = conn.query_row("SELECT proposed_at FROM take_proposals WHERE file_ids_json='[4,5]'", [], |r| r.get(0)).unwrap();
    assert!(before.iter().any(|(_, at)| at.to_bits() == party.to_bits()));
    let tracked: i64 = conn.query_row("SELECT COUNT(*) FROM take_proposal_files", [], |r| r.get(0)).unwrap();
    assert_eq!(tracked, 7);
}

#[test]
fn user_choices_events_and_people_are_respected() {
    let mut conn = crate::db::test_support::memory_db();
    library(&conn);
    file(&conn, 9, "/game/d1.jpg", 40000.0, Some("Phone A"), (1.0, 0.0));
    file(&conn, 10, "/game/d2.jpg", 40020.0, Some("Phone A"), (1.0, 0.0));
    file(&conn, 11, "/game/d3.jpg", 40040.0, Some("Phone A"), (0.87, 0.493));
    file(&conn, 12, "/game/d4.jpg", 40060.0, Some("Phone A"), (0.87, 0.493));
    conn.execute("INSERT INTO persons(id,first_name,created_at) VALUES(1,'Sam',0),(2,'Alex',0),(3,'Person 7',0)", []).unwrap();
    for (id, file, person) in [(1, 9, 1), (2, 10, 2), (3, 11, 1), (4, 12, 3)] {
        conn.execute(
            "INSERT INTO catalog_observations(id,file_id,person_id,source_revision,model_version,confidence,user_edited) VALUES(?1,?2,?3,?4,'user',1,1)",
            params![format!("o{id}"), file, person, revision(100, Some(10.0))],
        )
        .unwrap();
    }
    conn.execute("INSERT INTO catalog_events(id,title,user_edited) VALUES('e','Saved',1)", []).unwrap();
    conn.execute("INSERT INTO catalog_event_files(event_id,file_id) VALUES('e',6)", []).unwrap();
    rerun(&mut conn);
    let rows = stored(&conn);
    assert_eq!(rows.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(), vec!["[1,2]", "[4,5]", "[9,11]"]);
    assert!(rows[2].2.contains("sharedSubject"));
    let key = auto::key(&[1, 2]);
    let decision: crate::ipc::CatalogTakeDecision = serde_json::from_value(
        serde_json::json!({"groupKey": key, "decision": "keepAll", "fileIDs": [1, 2]}),
    )
    .unwrap();
    auto::decide(&conn, &decision).unwrap();
    let skip: crate::ipc::CatalogTakeDecision = serde_json::from_value(
        serde_json::json!({"groupKey": auto::key(&[4, 5]), "decision": "skip", "fileIDs": [4, 5]}),
    )
    .unwrap();
    auto::decide(&conn, &skip).unwrap();
    rerun(&mut conn);
    let groups = proposals(&conn, &request("takeGroupProposals")).unwrap().take_groups.unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups.last().unwrap().status, "skipped");
    assert_eq!(groups.last().unwrap().relation.as_deref(), Some("alternateViews"));
    let decisions: i64 = conn.query_row("SELECT COUNT(*) FROM take_group_decisions", [], |r| r.get(0)).unwrap();
    assert_eq!(decisions, 2);
    conn.execute("INSERT INTO catalog_event_files(event_id,file_id) VALUES('e',9)", []).unwrap();
    let groups = proposals(&conn, &request("takeGroupProposals")).unwrap().take_groups.unwrap();
    assert_eq!(groups.len(), 1);
}

#[test]
fn interrupted_jobs_pause_and_resume_without_redoing_sessions() {
    let mut conn = crate::db::test_support::memory_db();
    library(&conn);
    enqueue(&conn, &request("enqueueTakeGrouping")).unwrap();
    conn.execute("UPDATE catalog_jobs SET state='running' WHERE id=?1", [JOB_ID]).unwrap();
    let plan = plan(&conn).unwrap();
    process(&mut conn, &plan.dirty[0], 1, plan.dirty.len()).unwrap();
    recover(&conn).unwrap();
    let state: String = conn.query_row("SELECT state FROM catalog_jobs WHERE id=?1", [JOB_ID], |r| r.get(0)).unwrap();
    assert_eq!(state, "paused");
    assert!(process(&mut conn, &plan.dirty[1], 2, plan.dirty.len()).unwrap_err().is::<Interrupted>());
    assert_eq!(super::plan(&conn).unwrap().dirty.len(), plan.dirty.len() - 1);
    enqueue(&conn, &request("enqueueTakeGrouping")).unwrap();
    run_blocking(&mut conn).unwrap();
    assert_eq!(stored(&conn).len(), 3);
    assert!(super::plan(&conn).unwrap().dirty.is_empty());
}
