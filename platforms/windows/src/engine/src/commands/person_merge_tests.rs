use super::*;
use crate::commands::face_review::now;
use crate::commands::bulk::merge_clusters;
use crate::commands::face_review::{list as list_review, undo as undo_review};
use crate::db::test_support::{insert_file, memory_db};
use crate::ipc::{ListFaceReviewPayload, UndoFaceReviewPayload};

fn db() -> Connection {
    let conn = memory_db();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    conn
}

/// A person with `faces` faces, one file each; the last face is the representative.
fn person(conn: &Connection, first: Option<&str>, title: Option<&str>, unknown: bool, faces: i64) -> (i64, Vec<i64>) {
    conn.execute(
        "INSERT INTO persons(name,first_name,title,file_count,created_at,is_unknown) VALUES(?1,?1,?2,?3,100.0,?4)",
        params![first, title, faces, unknown as i64],
    )
    .unwrap();
    let id = conn.last_insert_rowid();
    let mut made = Vec::new();
    for index in 0..faces {
        let file = id * 1000 + index;
        insert_file(conn, file, &format!("/fixture/p{id}_f{index}.jpg"), "image");
        conn.execute(
            "INSERT INTO face_prints(file_id,person_id,print_data,bbox,arcface_embedding,face_quality) VALUES(?1,?2,X'',?3,X'0000803f',?4)",
            params![file, id, "0,0,1,1", 0.9 - index as f64 * 0.1],
        )
        .unwrap();
        made.push(conn.last_insert_rowid());
    }
    conn.execute("UPDATE persons SET representative_face_id=?1 WHERE id=?2", params![made.last(), id]).unwrap();
    (id, made)
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    first: Option<String>,
    title: Option<String>,
    unknown: i64,
    count: i64,
    representative: Option<i64>,
    faces: Vec<i64>,
}

fn snapshot(conn: &Connection, id: i64) -> Option<Snapshot> {
    let faces = {
        let mut statement = conn.prepare("SELECT id FROM face_prints WHERE person_id=?1 ORDER BY id").unwrap();
        statement.query_map([id], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    conn.query_row(
        "SELECT first_name, title, COALESCE(is_unknown,0), file_count, representative_face_id FROM persons WHERE id=?1",
        [id],
        |row| Ok(Snapshot { first: row.get(0)?, title: row.get(1)?, unknown: row.get(2)?, count: row.get(3)?, representative: row.get(4)?, faces }),
    )
    .ok()
}

fn merge(conn: &Connection, source: i64, target: i64) -> String {
    let result = merge_clusters(conn, source, target).unwrap();
    assert_eq!((result.succeeded, result.failed), (1, 0));
    result.messages[0].operation_id.clone().expect("merge returns an operation to undo")
}

fn undo_op(conn: &Connection, operation: &str) -> FaceReviewPage {
    undo_review(conn, &UndoFaceReviewPayload { request_id: "r".into(), operation_id: operation.into() }).unwrap()
}

fn offered_page(conn: &Connection) -> FaceReviewPage {
    let payload = ListFaceReviewPayload { request_id: "r".into(), person_id: None, offset: None, limit: None, queue: Some(QUEUE.into()) };
    list_review(conn, &payload).unwrap()
}

fn verdict(conn: &Connection, a: i64, b: i64) -> Option<String> {
    conn.query_row(
        "SELECT same_person || ':' || vlm_model FROM face_verifications WHERE person_a=?1 AND person_b=?2",
        params![a.min(b), a.max(b)],
        |row| row.get(0),
    )
    .ok()
}

#[test]
fn undo_restores_names_faces_counts_and_verdicts() {
    let conn = db();
    let keep = person(&conn, None, None, true, 2);
    let other = person(&conn, Some("Bo"), Some("Uncle"), false, 3);
    conn.execute(
        "INSERT INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,face_a,face_b) VALUES(?1,?2,0,1.0,'user-verified',5,?3,?4)",
        params![keep.0, other.0, keep.1[0], other.1[0]],
    )
    .unwrap();
    let keep_before = snapshot(&conn, keep.0).unwrap();
    let other_before = snapshot(&conn, other.0).unwrap();

    let operation = merge(&conn, other.0, keep.0);
    let merged = snapshot(&conn, keep.0).unwrap();
    assert_eq!((merged.first.as_deref(), merged.unknown, merged.count, merged.faces.len()), (Some("Bo"), 0, 5, 5));
    assert_eq!(snapshot(&conn, other.0), None);
    assert_eq!(verdict(&conn, keep.0, other.0).as_deref(), Some("1:user-merged"));
    let page = offered_page(&conn);
    assert_eq!((page.status.as_str(), page.action.as_str(), page.queue.as_deref(), page.total), ("ok", "list", Some(QUEUE), 1));
    assert_eq!((page.operation_id.as_deref(), page.person_id, page.other_person_id), (Some(operation.as_str()), Some(keep.0), Some(other.0)));
    assert!(page.merges.is_none() && page.splits.is_none() && page.items.is_empty());

    let page = undo_op(&conn, &operation);
    assert_eq!((page.status.as_str(), page.action.as_str(), page.queue.as_deref(), page.total), ("ok", "undo", Some(QUEUE), 0));
    assert_eq!((page.operation_id.as_deref(), page.person_id, page.other_person_id), (Some(operation.as_str()), Some(keep.0), Some(other.0)));
    assert_eq!(snapshot(&conn, keep.0).unwrap(), keep_before);
    assert_eq!(snapshot(&conn, other.0).unwrap(), other_before);
    assert_eq!(verdict(&conn, keep.0, other.0).as_deref(), Some("0:user-verified"));
    assert_eq!(offered_page(&conn).total, 0);
}

#[test]
fn named_target_keeps_its_name_and_the_source_name_returns() {
    let conn = db();
    let keep = person(&conn, Some("Ada"), None, false, 1);
    let other = person(&conn, Some("Bo"), None, true, 1);
    let operation = merge(&conn, other.0, keep.0);
    assert_eq!(snapshot(&conn, keep.0).unwrap().first.as_deref(), Some("Ada"));
    assert_eq!(undo_op(&conn, &operation).status, "ok");
    assert_eq!(snapshot(&conn, keep.0).unwrap().first.as_deref(), Some("Ada"));
    let back = snapshot(&conn, other.0).unwrap();
    assert_eq!((back.first.as_deref(), back.unknown, &back.faces), (Some("Bo"), 1, &other.1));
    assert_eq!(verdict(&conn, keep.0, other.0), None);
}

#[test]
fn undo_after_a_face_was_deleted_moves_the_rest_back() {
    let conn = db();
    let keep = person(&conn, Some("Ada"), None, false, 2);
    let other = person(&conn, Some("Bo"), None, false, 3);
    let operation = merge(&conn, other.0, keep.0);
    conn.execute("DELETE FROM face_prints WHERE id=?1", [other.1[2]]).unwrap();
    assert_eq!(undo_op(&conn, &operation).status, "ok");
    let back = snapshot(&conn, other.0).unwrap();
    assert_eq!((&back.faces[..], back.count), (&other.1[..2], 2));
    assert!(back.representative.is_some_and(|face| back.faces.contains(&face)));
    let kept = snapshot(&conn, keep.0).unwrap();
    assert_eq!((&kept.faces, kept.count), (&keep.1, 2));
}

#[test]
fn faces_moved_elsewhere_stay_and_an_emptied_merge_is_refused() {
    let conn = db();
    let keep = person(&conn, Some("Ada"), None, false, 1);
    let other = person(&conn, Some("Bo"), None, false, 2);
    let third = person(&conn, Some("Cy"), None, false, 1);
    let operation = merge(&conn, other.0, keep.0);
    conn.execute("UPDATE face_prints SET person_id=?1 WHERE id=?2", params![third.0, other.1[0]]).unwrap();
    assert_eq!(undo_op(&conn, &operation).status, "ok");
    assert_eq!(snapshot(&conn, other.0).unwrap().faces, vec![other.1[1]]);
    assert!(snapshot(&conn, third.0).unwrap().faces.contains(&other.1[0]));

    let fourth = person(&conn, Some("Di"), None, false, 1);
    let second = merge(&conn, fourth.0, keep.0);
    conn.execute("UPDATE face_prints SET person_id=?1 WHERE id=?2", params![third.0, fourth.1[0]]).unwrap();
    assert_eq!(offered_page(&conn).total, 0);
    let refused = undo_op(&conn, &second);
    assert_eq!((refused.status.as_str(), refused.queue.as_deref(), refused.message.as_deref(), refused.total), ("refused", Some(QUEUE), Some(CHANGED), 0));
    assert_eq!(snapshot(&conn, fourth.0), None);
}

#[test]
fn undo_is_refused_when_the_old_id_is_in_use_again() {
    let conn = db();
    let keep = person(&conn, Some("Ada"), None, false, 1);
    let other = person(&conn, Some("Bo"), None, false, 1);
    let operation = merge(&conn, other.0, keep.0);
    conn.execute("INSERT INTO persons(id,is_unknown,created_at,file_count) VALUES(?1,0,1,0)", [other.0]).unwrap();
    let refused = undo_op(&conn, &operation);
    assert_eq!((refused.status.as_str(), refused.message.as_deref()), ("refused", Some(CHANGED)));
    assert_eq!(snapshot(&conn, keep.0).unwrap().faces.len(), 2);
}

#[test]
fn undo_twice_changes_nothing() {
    let conn = db();
    let keep = person(&conn, Some("Ada"), None, false, 2);
    let other = person(&conn, Some("Bo"), None, false, 2);
    let operation = merge(&conn, other.0, keep.0);
    assert_eq!(undo_op(&conn, &operation).status, "ok");
    let (keep_after, other_after) = (snapshot(&conn, keep.0), snapshot(&conn, other.0));
    let again = undo_op(&conn, &operation);
    assert_eq!((again.status.as_str(), again.message.as_deref()), ("refused", Some(crate::commands::face_review::NOTHING_TO_UNDO)));
    assert_eq!(undo_op(&conn, LAST_TOKEN).status, "refused");
    assert_eq!(snapshot(&conn, keep.0), keep_after);
    assert_eq!(snapshot(&conn, other.0), other_after);
}

#[test]
fn journal_survives_reopen_and_the_last_token_undoes_newest_first() {
    let dir = std::env::temp_dir().join(format!("fileid-merge-undo-{}", uuid::Uuid::new_v4()));
    let path = dir.join("t.sqlite");
    let (a, b, c, first, second);
    {
        let conn = crate::db::open_writer(&path).unwrap();
        a = person(&conn, Some("Ada"), None, false, 1);
        b = person(&conn, Some("Bo"), None, false, 1);
        c = person(&conn, Some("Cy"), None, false, 1);
        first = merge(&conn, b.0, a.0);
        second = merge(&conn, c.0, a.0);
    }
    let conn = crate::db::open_writer(&path).unwrap();
    assert_eq!(offered_page(&conn).operation_id.as_deref(), Some(second.as_str()));
    let newest = undo_op(&conn, LAST_TOKEN);
    assert_eq!((newest.status.as_str(), newest.operation_id.as_deref(), newest.other_person_id, newest.total), ("ok", Some(second.as_str()), Some(c.0), 1));
    assert_eq!(offered_page(&conn).operation_id.as_deref(), Some(first.as_str()));
    assert_eq!(undo_op(&conn, LAST_TOKEN).other_person_id, Some(b.0));
    assert_eq!(snapshot(&conn, a.0).unwrap().faces, a.1);
    assert_eq!(snapshot(&conn, b.0).unwrap().first.as_deref(), Some("Bo"));
    assert_eq!(snapshot(&conn, c.0).unwrap().faces, c.1);
    drop(conn);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn merges_older_than_a_day_are_not_offered_but_still_undo_by_id() {
    let conn = db();
    let keep = person(&conn, Some("Ada"), None, false, 1);
    let other = person(&conn, Some("Bo"), None, false, 1);
    let operation = merge(&conn, other.0, keep.0);
    conn.execute("UPDATE catalog_operations SET created_at=created_at-90000 WHERE id=?1", [&operation]).unwrap();
    assert_eq!(offered_page(&conn).total, 0);
    assert_eq!(undo_op(&conn, LAST_TOKEN).status, "refused");
    assert_eq!(undo_op(&conn, &operation).status, "ok");
}

#[test]
fn journal_rows_use_the_swift_field_names() {
    let conn = db();
    let keep = person(&conn, None, None, true, 2);
    let other = person(&conn, Some("Bo"), Some("Uncle"), false, 3);
    let operation = merge(&conn, other.0, keep.0);
    let (kind, file, before, after, plan, inverse, state): (String, Option<i64>, String, String, String, String, String) = conn
        .query_row(
            "SELECT c.kind, c.file_id, c.before_json, c.after_json, o.plan_json, o.inverse_json, o.state \
             FROM catalog_operations o JOIN catalog_corrections c ON c.id=o.id WHERE o.id=?1",
            [&operation],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), file, state.as_str()), (KIND, Some(keep.0 * 1000 + 1), "completed"));
    assert_eq!(inverse, before);
    let keys = |text: &str| -> Vec<String> { serde_json::from_str::<Value>(text).unwrap().as_object().unwrap().keys().cloned().collect() };
    let mut before_keys = keys(&before);
    before_keys.sort();
    assert_eq!(before_keys, ["keepNames", "keepRepresentative", "keepUnknown", "other", "otherPersonID", "personID", "resolution", "verifications"]);
    let mut after_keys = keys(&after);
    after_keys.sort();
    assert_eq!(after_keys, ["faceIDs", "otherPersonID", "personID", "resolution"]);
    let before: Value = serde_json::from_str(&before).unwrap();
    let mut other_keys: Vec<&String> = before["other"].as_object().unwrap().keys().collect();
    other_keys.sort();
    assert_eq!(
        other_keys,
        ["anchor_radius", "created_at", "first_name", "is_unknown", "last_clustered_at", "last_name", "middle_name", "name", "representative_face_id", "suffix", "title"]
    );
    assert_eq!(before["keepNames"], json!([null, null, null, null, null, null]));
    assert_eq!((before["keepUnknown"].as_i64(), before["keepRepresentative"].as_i64()), (Some(1), Some(keep.1[1])));
    assert_eq!(serde_json::from_str::<Value>(&after).unwrap()["faceIDs"], json!(other.1));
    assert_eq!(serde_json::from_str::<Value>(&plan).unwrap(), json!({"type": KIND, "resolution": "merge", "personID": keep.0}));
}

/// The three strings are what the Swift engine's `PersonMergeJournal.merge`
/// writes (JSONSerialization, sorted keys) for person 2 "Bo" merged into the
/// unnamed, hidden person 1.
#[test]
fn a_merge_journaled_by_the_swift_engine_is_undone_here() {
    const BEFORE: &str = r#"{"keepNames":[null,null,null,null,null,null],"keepRepresentative":2,"keepUnknown":1,"other":{"anchor_radius":0.41999999999999998,"created_at":100,"first_name":"Bo","is_unknown":0,"last_clustered_at":null,"last_name":null,"middle_name":null,"name":"Bo","representative_face_id":5,"suffix":null,"title":"Uncle"},"otherPersonID":2,"personID":1,"resolution":"merge","verifications":[{"bboxA":null,"bboxB":"0,0,1,1","confidence":1,"faceA":1,"faceB":3,"fileA":null,"fileB":13,"model":"user-verified","personA":1,"personB":2,"samePerson":0,"verifiedAt":5}]}"#;
    const AFTER: &str = r#"{"faceIDs":[3,4,5],"otherPersonID":2,"personID":1,"resolution":"merge"}"#;
    const PLAN: &str = r#"{"personID":1,"resolution":"merge","type":"person_merge"}"#;
    const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    let conn = db();
    for face in 1..=5 {
        insert_file(&conn, 10 + face, &format!("/fixture/{face}.jpg"), "image");
    }
    conn.execute("INSERT INTO persons(id,name,first_name,title,is_unknown,created_at,representative_face_id,file_count) VALUES(1,'Bo','Bo','Uncle',0,50,5,5)", []).unwrap();
    for face in 1..=5 {
        conn.execute(
            "INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,face_quality) VALUES(?1,?2,1,X'','0,0,1,1',X'0000803f',0.5)",
            params![face, 10 + face],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,face_a,face_b,file_a,bbox_a,file_b,bbox_b) \
         VALUES(1,2,1,1.0,'user-merged',?1,2,5,12,'0,0,1,1',15,'0,0,1,1')",
        [now()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?1,12,'person_merge',?2,?3,?4)",
        params![ID, BEFORE, AFTER, now()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?1,?2,?3,'completed',?4)",
        params![ID, PLAN, BEFORE, now()],
    )
    .unwrap();

    let page = offered_page(&conn);
    assert_eq!((page.total, page.operation_id.as_deref(), page.person_id, page.other_person_id), (1, Some(ID), Some(1), Some(2)));
    let page = undo_op(&conn, LAST_TOKEN);
    assert_eq!((page.status.as_str(), page.operation_id.as_deref(), page.person_id, page.other_person_id), ("ok", Some(ID), Some(1), Some(2)));

    assert_eq!(
        snapshot(&conn, 1).unwrap(),
        Snapshot { first: None, title: None, unknown: 1, count: 2, representative: Some(2), faces: vec![1, 2] }
    );
    assert_eq!(
        snapshot(&conn, 2).unwrap(),
        Snapshot { first: Some("Bo".into()), title: Some("Uncle".into()), unknown: 0, count: 3, representative: Some(5), faces: vec![3, 4, 5] }
    );
    let (name, created, radius): (Option<String>, f64, Option<f64>) =
        conn.query_row("SELECT name, created_at, anchor_radius FROM persons WHERE id=2", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
    assert_eq!((name.as_deref(), created, radius), (Some("Bo"), 100.0, Some(0.42)));
    let restored: (i64, String, f64, Option<i64>, Option<i64>, Option<i64>, Option<String>, Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT same_person, vlm_model, verified_at, face_a, face_b, file_a, bbox_a, file_b, bbox_b FROM face_verifications WHERE person_a=1 AND person_b=2",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
        )
        .unwrap();
    assert_eq!(restored, (0, "user-verified".into(), 5.0, Some(1), Some(3), None, None, Some(13), Some("0,0,1,1".into())));
    let state: String = conn.query_row("SELECT state FROM catalog_operations WHERE id=?1", [ID], |row| row.get(0)).unwrap();
    assert_eq!(state, "undone");
}

#[test]
fn undo_returns_an_older_verdict_that_named_the_merged_in_person() {
    let conn = db();
    let keep = person(&conn, Some("Ada"), None, false, 1);
    let other = person(&conn, Some("Bo"), None, false, 1);
    let third = person(&conn, Some("Cy"), None, false, 1);
    conn.execute(
        "INSERT INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at) VALUES(?1,?2,1,0.9,'qwen',7)",
        params![other.0, third.0],
    )
    .unwrap();
    let operation = merge(&conn, other.0, keep.0);
    assert_eq!(verdict(&conn, other.0, third.0), None);
    assert_eq!(undo_op(&conn, &operation).status, "ok");
    assert_eq!(verdict(&conn, other.0, third.0).as_deref(), Some("1:qwen"));
}

#[test]
fn a_merge_that_changes_nothing_returns_no_operation() {
    let conn = db();
    let keep = person(&conn, Some("Ada"), None, false, 1);
    let same = merge_clusters(&conn, keep.0, keep.0).unwrap();
    assert_eq!((same.succeeded, same.messages[0].operation_id.as_deref()), (1, None));
    let missing = merge_clusters(&conn, 999, keep.0).unwrap();
    assert_eq!((missing.failed, missing.messages[0].operation_id.as_deref()), (1, None));
    assert_eq!(offered_page(&conn).total, 0);
}
