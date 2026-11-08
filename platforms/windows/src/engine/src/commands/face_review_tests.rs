use super::*;
use crate::db::test_support::{insert_file, memory_db};

fn vector(x: f32, y: f32) -> Vec<u8> {
    let mut values = vec![0.0_f32; 128];
    values[0] = x;
    values[1] = y;
    values.into_iter().flat_map(f32::to_le_bytes).collect()
}

fn db() -> Connection {
    let conn = memory_db();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    conn
}

fn photo(conn: &Connection, id: i64) {
    insert_file(conn, id, &format!("/photos/{id}.jpg"), "image");
    conn.execute(
        "INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?1,'rev','p',0)",
        [id],
    )
    .unwrap();
}

fn person(conn: &Connection, id: i64, name: Option<&str>, unknown: bool) {
    conn.execute(
        "INSERT INTO persons(id,name,is_unknown,created_at,file_count) VALUES(?1,?2,?3,0,0)",
        params![id, name, unknown as i64],
    )
    .unwrap();
}

fn face(conn: &Connection, id: i64, file: i64, owner: Option<i64>, xy: (f32, f32)) {
    conn.execute(
        "INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,face_quality,embedding_model,processing_version,source_revision) \
         VALUES(?1,?2,?3,X'00',?4,?5,0.9,'m','p','rev')",
        params![id, file, owner, format!("box{id}"), vector(xy.0, xy.1)],
    )
    .unwrap();
}

fn held(conn: &Connection) -> HashSet<i64> {
    holdout(conn, "m", "p", 0.0).unwrap()
}

type Row = (i64, i64, f64, String, Option<i64>);

fn candidates(conn: &Connection) -> Vec<Row> {
    let mut statement = conn
        .prepare("SELECT face_id,person_id,score,reason,runner_up_person_id FROM face_review_candidates ORDER BY face_id")
        .unwrap();
    statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn outcome(conn: &Connection, face: i64) -> Option<String> {
    conn.query_row("SELECT outcome FROM face_assignment_assessed WHERE face_id=?1", [face], |r| r.get(0))
        .optional()
        .unwrap()
}

fn owner(conn: &Connection, face: i64) -> Option<i64> {
    conn.query_row("SELECT person_id FROM face_prints WHERE id=?1", [face], |r| r.get(0)).unwrap()
}

/// Two people (1 along x, 2 along y), each with one assigned face, and new faces to place.
fn library() -> Connection {
    let conn = db();
    for file in 1..=6 {
        photo(&conn, file);
    }
    person(&conn, 1, Some("Alex"), false);
    person(&conn, 2, None, false);
    face(&conn, 1, 1, Some(1), (1.0, 0.0));
    face(&conn, 2, 2, Some(2), (0.0, 1.0));
    mark_pool(&conn, &[1, 2], 1.0).unwrap();
    conn
}

fn run(conn: &Connection, settings: &Settings) -> AssessStats {
    let held = held(conn);
    assess(conn, &held, "m", "p", settings, 5.0).unwrap()
}

fn scored(entries: &[(i64, f64, bool)]) -> Vec<Scored> {
    entries.iter().map(|&(person, score, unknown)| Scored { person, score, unknown }).collect()
}

#[test]
fn decide_classifies_by_score_and_margin() {
    let settings = Settings::default();
    let none = HashSet::new();
    assert_eq!(
        decide(&scored(&[(1, 0.7, false), (2, 0.2, false)]), &none, &settings),
        Decision::Queue { person: 1, score: 0.7, reason: "suggested", runner: Some((2, 0.2)) }
    );
    assert!(matches!(
        decide(&scored(&[(1, 0.5, false)]), &none, &settings),
        Decision::Queue { reason: "uncertain", runner: None, .. }
    ));
    assert!(matches!(
        decide(&scored(&[(1, 0.7, false), (2, 0.68, false)]), &none, &settings),
        Decision::Queue { reason: "ambiguous", .. }
    ));
    assert_eq!(decide(&scored(&[(1, 0.39, false)]), &none, &settings), Decision::Unmatched);
    assert_eq!(decide(&scored(&[(3, 0.9, true), (1, 0.7, false)]), &none, &settings), Decision::Unmatched);
    assert_eq!(decide(&[], &none, &settings), Decision::Unmatched);
    let barred: HashSet<i64> = [1].into();
    assert!(matches!(
        decide(&scored(&[(1, 0.9, false), (2, 0.6, false)]), &barred, &settings),
        Decision::Queue { person: 2, reason: "ambiguous", runner: Some((1, _)), .. }
    ));
    let auto = Settings { auto_assign: true, ..settings };
    assert_eq!(decide(&scored(&[(1, 0.7, false)]), &none, &auto), Decision::Assign { person: 1 });
}

#[test]
fn holdout_takes_new_faces_only_once_people_exist() {
    let conn = db();
    photo(&conn, 1);
    photo(&conn, 2);
    face(&conn, 1, 1, None, (1.0, 0.0));
    assert!(held(&conn).is_empty(), "a fresh library clusters new faces normally");
    person(&conn, 1, None, false);
    face(&conn, 2, 2, Some(1), (1.0, 0.0));
    assert_eq!(held(&conn), [1].into());
    upsert_outcome(&conn, 1, "m|p", "baseline", 0.0).unwrap();
    assert!(held(&conn).is_empty(), "baseline faces are never revisited");
    upsert_outcome(&conn, 1, "m|p", "rejected", 0.0).unwrap();
    assert_eq!(held(&conn), [1].into());
    assert!(holdout(&conn, "m", "p", 0.95).unwrap().is_empty(), "the cluster quality gate applies");
    assert!(holdout(&conn, "other", "p", 0.0).unwrap().is_empty());
}

#[test]
fn new_faces_become_suggestions_without_changing_assignments() {
    let conn = library();
    face(&conn, 3, 3, None, (0.9, 0.1));
    face(&conn, 4, 4, None, (0.6, 0.58));
    face(&conn, 5, 5, None, (0.1, -1.0));
    face(&conn, 6, 6, None, (0.5, 0.15));
    let stats = run(&conn, &Settings::default());
    assert_eq!(stats, AssessStats { queued: 3, assigned: 0, unmatched: 1 });
    let rows = candidates(&conn);
    assert_eq!(
        rows.iter().map(|r| (r.0, r.1, r.3.as_str(), r.4)).collect::<Vec<_>>(),
        vec![(3, 1, "suggested", Some(2)), (4, 1, "ambiguous", Some(2)), (6, 1, "suggested", Some(2))]
    );
    assert!((rows[0].2 - 0.9939).abs() < 1e-9);
    assert_eq!(outcome(&conn, 5).as_deref(), Some("unmatched"));
    for face in 3..=6 {
        assert_eq!(owner(&conn, face), None);
    }
    assert_eq!(pending_count(&conn).unwrap(), 3);
    let again = held(&conn);
    assert!(again.contains(&3) && !again.contains(&5));
}

#[test]
fn unknown_people_same_photo_and_rejections_never_receive() {
    let conn = library();
    person(&conn, 3, None, true);
    face(&conn, 10, 3, Some(3), (0.0, -1.0));
    face(&conn, 11, 4, None, (0.0, -1.0));
    face(&conn, 12, 1, None, (1.0, 0.6));
    face(&conn, 13, 5, None, (1.0, 0.6));
    conn.execute(
        "INSERT INTO face_identity_constraints(file_id,bbox,anchor_file_id,anchor_bbox,source,created_at) VALUES(5,'box13',1,'box1','review',0)",
        [],
    )
    .unwrap();
    run(&conn, &Settings::default());
    assert_eq!(outcome(&conn, 11).as_deref(), Some("unmatched"), "best match is an unknown person");
    let rows = candidates(&conn);
    assert!(rows.iter().all(|r| r.0 != 11));
    let twelve = rows.iter().find(|r| r.0 == 12).unwrap();
    assert_eq!((twelve.1, twelve.3.as_str()), (2, "ambiguous"), "Alex is already in photo 1");
    let thirteen = rows.iter().find(|r| r.0 == 13).unwrap();
    assert_eq!(thirteen.1, 2, "Alex was rejected for this face");
}

#[test]
fn different_people_verdicts_bar_the_other_person() {
    let conn = library();
    face(&conn, 3, 3, None, (1.0, 0.6));
    conn.execute(
        "INSERT INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,file_a,bbox_a,file_b,bbox_b) \
         VALUES(1,9,0,1,'user',0,1,'box1',3,'box3')",
        [],
    )
    .unwrap();
    run(&conn, &Settings::default());
    assert_eq!(candidates(&conn)[0].1, 2);
}

#[test]
fn best_score_claims_the_person_for_its_photo() {
    let conn = library();
    face(&conn, 20, 3, None, (0.95, 0.6));
    face(&conn, 21, 3, None, (1.0, 0.0));
    run(&conn, &Settings::default());
    let rows = candidates(&conn);
    assert_eq!(rows.iter().find(|r| r.0 == 21).unwrap().1, 1);
    assert_eq!(rows.iter().find(|r| r.0 == 20).unwrap().1, 2);
}

#[test]
fn opt_in_auto_assign_assigns_confident_matches_only() {
    let conn = library();
    face(&conn, 3, 3, None, (1.0, 0.0));
    face(&conn, 4, 4, None, (0.6, 0.58));
    let stats = run(&conn, &Settings { auto_assign: true, ..Settings::default() });
    assert_eq!((stats.assigned, stats.queued), (1, 1));
    assert_eq!(owner(&conn, 3), Some(1));
    assert_eq!(outcome(&conn, 3).as_deref(), Some("assigned"));
    assert_eq!(owner(&conn, 4), None);
    let count: i64 = conn.query_row("SELECT file_count FROM persons WHERE id=1", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 2);
}

#[test]
fn a_rejected_face_without_another_match_stays_rejected_and_held() {
    let conn = library();
    face(&conn, 3, 3, None, (1.0, 0.0));
    upsert_outcome(&conn, 3, "m|p", "rejected", 0.0).unwrap();
    conn.execute("UPDATE persons SET is_unknown=1 WHERE id=2", []).unwrap();
    conn.execute(
        "INSERT INTO face_identity_constraints(file_id,bbox,anchor_file_id,anchor_bbox,source,created_at) VALUES(3,'box3',1,'box1','review',0)",
        [],
    )
    .unwrap();
    run(&conn, &Settings::default());
    assert_eq!(outcome(&conn, 3).as_deref(), Some("rejected"));
    assert_eq!(candidates(&conn), Vec::<Row>::new());
    assert!(held(&conn).contains(&3));
}

fn list_page(conn: &Connection, person: Option<i64>, offset: i64, limit: i64) -> FaceReviewPage {
    let payload = ListFaceReviewPayload { request_id: "r".into(), person_id: person, offset: Some(offset), limit: Some(limit), queue: None };
    list(conn, &payload).unwrap()
}

#[test]
fn list_pages_best_first_and_filters_by_person() {
    let conn = library();
    face(&conn, 3, 3, None, (0.9, 0.1));
    face(&conn, 4, 4, None, (0.1, 0.9));
    face(&conn, 5, 5, None, (1.0, 0.0));
    run(&conn, &Settings::default());
    let all = list_page(&conn, None, 0, 2);
    assert_eq!((all.status.as_str(), all.total), ("ok", 3));
    assert_eq!(all.items.iter().map(|i| i.face_id).collect::<Vec<_>>(), vec![5, 3]);
    assert_eq!(all.items[0].person_name.as_deref(), Some("Alex"));
    assert_eq!(all.items[0].path, "/photos/5.jpg");
    assert_eq!(all.items[0].runner_up_name, None);
    assert_eq!(list_page(&conn, None, 2, 2).items.len(), 1);
    let unnamed = list_page(&conn, Some(2), 0, 50);
    assert_eq!((unnamed.total, unnamed.items[0].face_id, unnamed.items[0].person_name.clone()), (1, 4, None));
    for (offset, limit) in [(-1, 10), (0, 0), (0, 201)] {
        assert_eq!(list_page(&conn, None, offset, limit).status, "refused");
    }
}

fn resolve_face(conn: &Connection, face: i64, person: i64, resolution: &str) -> FaceReviewPage {
    let payload = ResolveFaceReviewPayload { request_id: "r".into(), face_id: Some(face), person_id: person, resolution: resolution.into(), other_person_id: None };
    resolve(conn, &payload).unwrap()
}

fn undo_op(conn: &Connection, id: &str) -> FaceReviewPage {
    undo(conn, &UndoFaceReviewPayload { request_id: "r".into(), operation_id: id.into() }).unwrap()
}

#[test]
fn accept_assigns_logs_and_undoes() {
    let conn = library();
    face(&conn, 3, 3, None, (0.9, 0.1));
    run(&conn, &Settings::default());
    assert_eq!(resolve_face(&conn, 3, 2, "accept").status, "refused", "person must match the suggestion");
    assert_eq!(resolve_face(&conn, 3, 1, "maybe").status, "refused");
    let reply = resolve_face(&conn, 3, 1, "accept");
    assert_eq!((reply.status.as_str(), reply.person_id, reply.total), ("ok", Some(1), 0));
    assert_eq!(owner(&conn, 3), Some(1));
    assert_eq!(outcome(&conn, 3).as_deref(), Some("accepted"));
    let operation = reply.operation_id.unwrap();
    let kind: String = conn
        .query_row("SELECT kind FROM catalog_corrections WHERE id=?1", [&operation], |r| r.get(0))
        .unwrap();
    assert_eq!(kind, "face_review");
    assert_eq!(resolve_face(&conn, 3, 1, "accept").status, "refused", "already resolved");
    let undone = undo_op(&conn, &operation);
    assert_eq!((undone.status.as_str(), undone.total), ("ok", 1));
    assert_eq!(owner(&conn, 3), None);
    assert_eq!(outcome(&conn, 3).as_deref(), Some("suggested"));
    assert_eq!(candidates(&conn)[0].1, 1);
    assert_eq!(undo_op(&conn, &operation).status, "refused", "an operation undoes once");
}

#[test]
fn accept_is_refused_when_the_person_is_in_the_photo() {
    let conn = library();
    face(&conn, 3, 3, None, (0.9, 0.1));
    run(&conn, &Settings::default());
    face(&conn, 4, 3, Some(1), (1.0, 0.0));
    let reply = resolve_face(&conn, 3, 1, "accept");
    assert_eq!(
        (reply.status.as_str(), reply.message.as_deref()),
        ("refused", Some("That person is already in this photo."))
    );
    assert_eq!(owner(&conn, 3), None);
}

#[test]
fn reject_records_a_constraint_that_undo_forgets() {
    let conn = library();
    face(&conn, 3, 3, None, (0.9, 0.6));
    run(&conn, &Settings::default());
    let reply = resolve_face(&conn, 3, 1, "reject");
    assert_eq!((reply.status.as_str(), reply.person_id), ("ok", None));
    assert_eq!(outcome(&conn, 3).as_deref(), Some("rejected"));
    let anchors: (i64, String) = conn
        .query_row("SELECT anchor_file_id, anchor_bbox FROM face_identity_constraints WHERE file_id=3", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(anchors, (1, "box1".into()));
    run(&conn, &Settings::default());
    assert_eq!(candidates(&conn)[0].1, 2, "the next run never suggests Alex again");
    conn.execute("DELETE FROM face_review_candidates", []).unwrap();
    upsert_outcome(&conn, 3, "m|p", "rejected", 0.0).unwrap();
    assert_eq!(undo_op(&conn, reply.operation_id.as_deref().unwrap()).status, "ok");
    let constraints: i64 = conn.query_row("SELECT COUNT(*) FROM face_identity_constraints", [], |r| r.get(0)).unwrap();
    assert_eq!(constraints, 0);
    assert_eq!(candidates(&conn)[0].1, 1);
}

#[test]
fn ignore_files_the_face_under_a_new_unknown_person() {
    let conn = library();
    face(&conn, 3, 3, None, (0.9, 0.1));
    run(&conn, &Settings::default());
    let reply = resolve_face(&conn, 3, 1, "ignore");
    let unknown = reply.person_id.unwrap();
    let flag: i64 = conn.query_row("SELECT is_unknown FROM persons WHERE id=?1", [unknown], |r| r.get(0)).unwrap();
    assert_eq!((flag, owner(&conn, 3)), (1, Some(unknown)));
    undo_op(&conn, reply.operation_id.as_deref().unwrap());
    let gone: bool = conn
        .query_row("SELECT NOT EXISTS(SELECT 1 FROM persons WHERE id=?1)", [unknown], |r| r.get(0))
        .unwrap();
    assert!(gone);
    assert_eq!(owner(&conn, 3), None);
}

#[test]
fn undo_is_refused_after_the_face_moved() {
    let conn = library();
    face(&conn, 3, 3, None, (0.9, 0.1));
    run(&conn, &Settings::default());
    let reply = resolve_face(&conn, 3, 1, "accept");
    conn.execute("UPDATE face_prints SET person_id=2 WHERE id=3", []).unwrap();
    let undone = undo_op(&conn, reply.operation_id.as_deref().unwrap());
    assert_eq!(undone.message.as_deref(), Some(MOVED));
    assert_eq!(owner(&conn, 3), Some(2));
}

#[test]
fn deleting_a_person_removes_its_suggestions() {
    let conn = library();
    face(&conn, 3, 3, None, (0.9, 0.1));
    run(&conn, &Settings::default());
    conn.execute("UPDATE face_prints SET person_id=NULL WHERE person_id=1", []).unwrap();
    conn.execute("DELETE FROM persons WHERE id=1", []).unwrap();
    assert_eq!(candidates(&conn), Vec::<Row>::new());
    assert!(held(&conn).contains(&3), "the face is assessed again next run");
}

#[test]
fn manual_reassignment_records_a_constraint_and_settles_the_face() {
    let conn = library();
    face(&conn, 3, 3, Some(1), (1.0, 0.0));
    conn.execute("UPDATE persons SET representative_face_id=1 WHERE id=1", []).unwrap();
    conn.execute("UPDATE face_prints SET person_id=2 WHERE id=3", []).unwrap();
    record_reassignment(&conn, 3, Some(1), Some(2)).unwrap();
    let constraint: (String, i64, String) = conn
        .query_row("SELECT bbox, anchor_file_id, source FROM face_identity_constraints", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(constraint, ("box3".into(), 1, "reassign".into()));
    let (kind, after): (String, String) = conn
        .query_row("SELECT kind, after_json FROM catalog_corrections", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(kind, "face_reassign");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&after).unwrap()["personID"], 2);
    assert_eq!(outcome(&conn, 3).as_deref(), Some("manual"));
}

#[test]
fn request_ids_follow_the_nonce_pattern() {
    assert!(valid_request_id("review-1_A"));
    assert!(!valid_request_id(""));
    assert!(!valid_request_id("a/b"));
    assert!(!valid_request_id(&"x".repeat(129)));
}

fn accept_all(conn: &Connection, person: i64) -> FaceReviewPage {
    let payload = ResolveFaceReviewPayload {
        request_id: "r".into(),
        face_id: None,
        person_id: person,
        resolution: "acceptAll".into(),
        other_person_id: None,
    };
    resolve(conn, &payload).unwrap()
}

#[test]
fn accept_all_accepts_suggested_faces_in_one_operation_and_skips_photos_with_the_person() {
    let conn = library();
    face(&conn, 3, 3, None, (1.0, 0.0));
    face(&conn, 4, 4, None, (0.95, 0.05));
    face(&conn, 5, 5, None, (0.5, 0.5));
    run(&conn, &Settings::default());
    assert_eq!(candidates(&conn).iter().filter(|row| row.3 == "suggested" && row.1 == 1).count(), 2);
    face(&conn, 6, 4, Some(1), (1.0, 0.0));
    let reply = accept_all(&conn, 1);
    assert_eq!(reply.status, "ok");
    assert_eq!(reply.accepted_face_ids.as_deref(), Some(&[3][..]));
    assert_eq!(reply.skipped_face_ids.as_deref(), Some(&[4][..]));
    assert_eq!((owner(&conn, 3), owner(&conn, 4), owner(&conn, 5)), (Some(1), None, None));
    assert_eq!(outcome(&conn, 3).as_deref(), Some("accepted"));
    let operation = reply.operation_id.unwrap();
    let kind: String = conn.query_row("SELECT kind FROM catalog_corrections WHERE id=?1", [&operation], |r| r.get(0)).unwrap();
    assert_eq!(kind, "face_review_bulk");
    let undone = undo_op(&conn, &operation);
    assert_eq!((undone.status.as_str(), undone.accepted_face_ids.as_deref()), ("ok", Some(&[3][..])));
    assert_eq!(owner(&conn, 3), None);
    assert_eq!(outcome(&conn, 3).as_deref(), Some("suggested"));
    assert!(candidates(&conn).iter().any(|row| row.0 == 3 && row.1 == 1));
    assert_eq!(undo_op(&conn, &operation).status, "refused");
}

#[test]
fn accept_all_refuses_when_nothing_can_be_accepted() {
    let conn = library();
    assert_eq!(accept_all(&conn, 1).message.as_deref(), Some(NOTHING_TO_ACCEPT));
    face(&conn, 3, 3, None, (1.0, 0.0));
    run(&conn, &Settings::default());
    face(&conn, 6, 3, Some(1), (1.0, 0.0));
    let reply = accept_all(&conn, 1);
    assert_eq!((reply.status.as_str(), reply.message.as_deref()), ("refused", Some(ALL_IN_PHOTO)));
    assert_eq!(reply.skipped_face_ids.as_deref(), Some(&[3][..]));
    assert_eq!(owner(&conn, 3), None);
}

#[test]
fn accept_all_undo_is_refused_once_a_face_moved() {
    let conn = library();
    face(&conn, 3, 3, None, (1.0, 0.0));
    face(&conn, 4, 4, None, (0.95, 0.05));
    run(&conn, &Settings::default());
    let operation = accept_all(&conn, 1).operation_id.unwrap();
    conn.execute("UPDATE face_prints SET person_id=2 WHERE id=4", []).unwrap();
    let reply = undo_op(&conn, &operation);
    assert_eq!((reply.status.as_str(), reply.message.as_deref()), ("refused", Some(MOVED_MANY)));
    assert_eq!((owner(&conn, 3), owner(&conn, 4)), (Some(1), Some(2)));
}

#[test]
fn list_marks_low_quality_faces_and_rejects_unknown_queues() {
    let conn = library();
    face(&conn, 3, 3, None, (1.0, 0.0));
    run(&conn, &Settings::default());
    conn.execute("UPDATE face_prints SET excluded=1 WHERE id=3", []).unwrap();
    assert_eq!(list_page(&conn, None, 0, 10).items[0].low_quality, Some(true));
    let payload = ListFaceReviewPayload { request_id: "r".into(), person_id: None, offset: None, limit: None, queue: Some("other".into()) };
    assert_eq!(list(&conn, &payload).unwrap().status, "refused");
    let missing_face = ResolveFaceReviewPayload { request_id: "r".into(), face_id: None, person_id: 1, resolution: "accept".into(), other_person_id: None };
    assert_eq!(resolve(&conn, &missing_face).unwrap().status, "refused");
}
