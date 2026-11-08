use super::*;
use crate::commands::face_review::{list, resolve, undo};
use crate::db::test_support::{insert_file, memory_db};
use crate::ipc::UndoFaceReviewPayload;

fn vector(x: f32, y: f32) -> Vec<u8> {
    let mut values = vec![0.0_f32; 128];
    values[0] = x;
    values[1] = y;
    values[2] = 0.05;
    values.into_iter().flat_map(f32::to_le_bytes).collect()
}

fn db() -> Connection {
    let conn = memory_db();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    for file in 1..=40 {
        insert_file(&conn, file, &format!("/photos/{file}.jpg"), "image");
    }
    conn
}

fn person(conn: &Connection, id: i64, name: Option<&str>, unknown: bool) {
    conn.execute(
        "INSERT INTO persons(id,name,is_unknown,created_at,file_count) VALUES(?1,?2,?3,0,0)",
        params![id, name, unknown as i64],
    )
    .unwrap();
}

fn face(conn: &Connection, id: i64, file: i64, owner: i64, xy: (f32, f32)) {
    conn.execute(
        "INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,face_quality,embedding_model,processing_version,source_revision) \
         VALUES(?1,?2,?3,X'00',?4,?5,0.9,'m','p','rev')",
        params![id, file, owner, format!("box{id}"), vector(xy.0, xy.1)],
    )
    .unwrap();
    conn.execute("UPDATE persons SET representative_face_id=COALESCE(representative_face_id,?1) WHERE id=?2", params![id, owner]).unwrap();
}

fn refresh_all(conn: &Connection) -> (usize, usize) {
    refresh(conn, "m", "p", &QueueSettings::default(), 10.0).unwrap()
}

fn merge_pairs(conn: &Connection) -> Vec<(i64, i64)> {
    let mut statement = conn.prepare("SELECT person_id, other_person_id FROM face_merge_suggestions ORDER BY person_id, other_person_id").unwrap();
    statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

fn owner(conn: &Connection, face: i64) -> Option<i64> {
    conn.query_row("SELECT person_id FROM face_prints WHERE id=?1", [face], |r| r.get(0)).unwrap()
}

fn act(conn: &Connection, person: i64, other: Option<i64>, resolution: &str) -> FaceReviewPage {
    let payload = ResolveFaceReviewPayload {
        request_id: "r".into(),
        face_id: None,
        person_id: person,
        resolution: resolution.into(),
        other_person_id: other,
    };
    resolve(conn, &payload).unwrap()
}

fn undo_op(conn: &Connection, id: &str) -> FaceReviewPage {
    undo(conn, &UndoFaceReviewPayload { request_id: "r".into(), operation_id: id.into() }).unwrap()
}

fn listed(conn: &Connection, queue: &str) -> FaceReviewPage {
    let payload = ListFaceReviewPayload { request_id: "r".into(), person_id: None, offset: None, limit: None, queue: Some(queue.into()) };
    list(conn, &payload).unwrap()
}

/// Alex (named, two faces) and an unnamed fragment of the same face.
fn pair() -> Connection {
    let conn = db();
    person(&conn, 1, None, false);
    person(&conn, 2, Some("Alex"), false);
    face(&conn, 1, 1, 1, (1.0, 0.0));
    face(&conn, 2, 2, 2, (0.95, 0.1));
    face(&conn, 3, 3, 2, (0.97, 0.05));
    conn
}

#[test]
fn merges_pair_close_people_named_first_and_skip_unknown_shared_and_far_people() {
    let conn = pair();
    person(&conn, 3, None, false);
    face(&conn, 4, 4, 3, (0.0, 1.0));
    person(&conn, 4, None, true);
    face(&conn, 5, 5, 4, (1.0, 0.0));
    person(&conn, 5, Some("Sam"), false);
    face(&conn, 6, 1, 5, (1.0, 0.02));
    face(&conn, 7, 2, 5, (1.0, 0.02));
    face(&conn, 8, 3, 5, (1.0, 0.02));
    refresh_all(&conn);
    let pairs = merge_pairs(&conn);
    assert!(pairs.contains(&(2, 1)), "{pairs:?}");
    assert!(!pairs.iter().any(|(a, b)| *a == 4 || *b == 4 || *a == 3 || *b == 3), "{pairs:?}");
    assert!(!pairs.contains(&(5, 1)) && !pairs.contains(&(5, 2)) && !pairs.contains(&(2, 5)), "shared photos: {pairs:?}");
    let page = listed(&conn, "merges");
    assert_eq!((page.status.as_str(), page.queue.as_deref(), page.total), ("ok", Some("merges"), pairs.len() as i64));
    let first = &page.merges.as_ref().unwrap()[0];
    assert_eq!((first.person_id, first.person_name.as_deref(), first.face_count, first.other_face_count), (2, Some("Alex"), 2, 1));
}

#[test]
fn merge_moves_faces_journals_and_undo_brings_the_person_back() {
    let conn = pair();
    refresh_all(&conn);
    assert_eq!(act(&conn, 1, Some(2), "merge").message.as_deref(), Some(STALE), "the kept person must match");
    let reply = act(&conn, 2, Some(1), "merge");
    assert_eq!((reply.status.as_str(), reply.person_id, reply.other_person_id, reply.total), ("ok", Some(2), Some(1), 0));
    assert_eq!(owner(&conn, 1), Some(2));
    assert!(!person_exists(&conn, 1).unwrap());
    let verdict: (i64, String) = conn
        .query_row("SELECT same_person, vlm_model FROM face_verifications WHERE person_a=1 AND person_b=2", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(verdict, (1, "user-merged".to_string()));
    let operation = reply.operation_id.unwrap();
    let undone = undo_op(&conn, &operation);
    assert_eq!((undone.status.as_str(), undone.queue.as_deref(), undone.total), ("ok", Some("merges"), 1));
    assert_eq!(owner(&conn, 1), Some(1));
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM face_verifications", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
    assert_eq!(merge_pairs(&conn), vec![(2, 1)]);
    assert_eq!(undo_op(&conn, &operation).status, "refused");
}

#[test]
fn merge_is_refused_when_the_people_now_share_a_photo() {
    let conn = pair();
    refresh_all(&conn);
    face(&conn, 9, 2, 1, (1.0, 0.0));
    let reply = act(&conn, 2, Some(1), "merge");
    assert_eq!((reply.status.as_str(), reply.message.as_deref(), reply.queue.as_deref()), ("refused", Some(SHARED_PHOTO), Some("merges")));
    assert_eq!(owner(&conn, 1), Some(1));
}

#[test]
fn not_same_person_records_a_verdict_that_keeps_the_pair_out_until_undone() {
    let conn = pair();
    refresh_all(&conn);
    let reply = act(&conn, 2, Some(1), "notSamePerson");
    assert_eq!(reply.status, "ok");
    let verdict: (i64, String, i64, i64) = conn
        .query_row("SELECT same_person, vlm_model, face_a, face_b FROM face_verifications WHERE person_a=1 AND person_b=2", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap();
    assert_eq!(verdict, (0, "user-verified".to_string(), 1, 2));
    refresh_all(&conn);
    assert!(merge_pairs(&conn).is_empty());
    assert_eq!(undo_op(&conn, &reply.operation_id.unwrap()).status, "ok");
    assert_eq!(merge_pairs(&conn), vec![(2, 1)]);
    refresh_all(&conn);
    assert_eq!(merge_pairs(&conn), vec![(2, 1)]);
}

#[test]
fn dismissal_survives_new_person_ids_and_undo_forgets_it() {
    let conn = pair();
    refresh_all(&conn);
    let reply = act(&conn, 2, Some(1), "dismiss");
    assert_eq!(reply.status, "ok");
    conn.execute("INSERT INTO persons(id,name,is_unknown,created_at,file_count) VALUES(10,NULL,0,0,0)", []).unwrap();
    conn.execute("UPDATE face_prints SET person_id=10 WHERE person_id=1", []).unwrap();
    conn.execute("DELETE FROM persons WHERE id=1", []).unwrap();
    refresh_all(&conn);
    assert!(merge_pairs(&conn).is_empty(), "the dismissal follows the faces");
    assert_eq!(undo_op(&conn, &reply.operation_id.unwrap()).status, "ok");
    refresh_all(&conn);
    assert_eq!(merge_pairs(&conn), vec![(2, 10)]);
}

/// One person holding two different faces: four along x, three along y.
fn mixed() -> Connection {
    let conn = db();
    person(&conn, 1, Some("Kim"), false);
    for (index, xy) in [(1.0, 0.0), (0.98, 0.05), (0.97, 0.0), (0.99, 0.02), (0.0, 1.0), (0.05, 0.98), (0.02, 0.97)].into_iter().enumerate() {
        let id = index as i64 + 1;
        face(&conn, id, id, 1, xy);
    }
    conn
}

#[test]
fn split_flags_two_separated_groups_and_keeps_the_representative_face() {
    let conn = mixed();
    assert_eq!(refresh_all(&conn).1, 1);
    let page = listed(&conn, "splits");
    let split = &page.splits.as_ref().unwrap()[0];
    assert_eq!((split.person_id, split.keep_count, split.split_count), (1, 4, 3));
    assert!(split.keep_face_ids.contains(&1));
    assert!(split.score < 0.2, "{}", split.score);
    let coherent = pair();
    assert_eq!(refresh_all(&coherent).1, 0);
}

#[test]
fn split_moves_the_group_to_a_new_person_and_undo_restores_it() {
    let conn = mixed();
    refresh_all(&conn);
    let reply = act(&conn, 1, None, "split");
    assert_eq!(reply.status, "ok");
    let created = reply.other_person_id.unwrap();
    assert_eq!((owner(&conn, 1), owner(&conn, 5), owner(&conn, 6), owner(&conn, 7)), (Some(1), Some(created), Some(created), Some(created)));
    let verdicts: i64 = conn.query_row("SELECT COUNT(*) FROM face_verifications WHERE same_person=0", [], |r| r.get(0)).unwrap();
    assert_eq!(verdicts, 1);
    assert_eq!(refresh_all(&conn), (0, 0), "a split pair is never offered as a merge");
    let undone = undo_op(&conn, &reply.operation_id.unwrap());
    assert_eq!((undone.status.as_str(), undone.queue.as_deref()), ("ok", Some("splits")));
    assert_eq!(owner(&conn, 5), Some(1));
    assert!(!person_exists(&conn, created).unwrap());
    assert_eq!(split_count(&conn).unwrap(), 1);
}

#[test]
fn split_undo_is_refused_after_the_new_person_was_named() {
    let conn = mixed();
    refresh_all(&conn);
    let reply = act(&conn, 1, None, "split");
    conn.execute("UPDATE persons SET name='Lee' WHERE id=?1", [reply.other_person_id.unwrap()]).unwrap();
    let undone = undo_op(&conn, &reply.operation_id.unwrap());
    assert_eq!((undone.status.as_str(), undone.message.as_deref()), ("refused", Some(RENAMED)));
}

#[test]
fn split_is_refused_when_its_faces_moved_and_dismissal_hides_it() {
    let conn = mixed();
    refresh_all(&conn);
    conn.execute("UPDATE face_prints SET person_id=NULL WHERE id=6", []).unwrap();
    assert_eq!(act(&conn, 1, None, "split").message.as_deref(), Some(STALE));
    conn.execute("UPDATE face_prints SET person_id=1 WHERE id=6", []).unwrap();
    let reply = act(&conn, 1, None, "dismiss");
    assert_eq!(reply.status, "ok");
    assert_eq!(refresh_all(&conn).1, 0);
    assert_eq!(undo_op(&conn, &reply.operation_id.unwrap()).status, "ok");
    assert_eq!(refresh_all(&conn).1, 1);
}

#[test]
fn merge_resolutions_need_the_other_person() {
    let conn = pair();
    refresh_all(&conn);
    assert_eq!(act(&conn, 2, None, "merge").message.as_deref(), Some(OTHER_REQUIRED));
}

#[test]
fn two_groups_is_deterministic_and_separates_clear_groups() {
    let a = [1.0_f32, 0.0];
    let b = [0.0_f32, 1.0];
    let vectors: Vec<&[f32]> = vec![&a, &a, &b, &b];
    let (groups, first, second) = two_groups(&vectors).unwrap();
    assert_eq!(groups[0], groups[1]);
    assert_ne!(groups[0], groups[2]);
    assert!(dot(&first, &second).abs() < 1e-6);
    assert!(two_groups(&[&a, &a]).is_none());
}
