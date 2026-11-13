//! Undo journal for ordinary People merges (mergeClusters). Each merge is one
//! catalog operation of kind person_merge, written in the merge's transaction
//! and undone through undoFaceReview. The JSON matches the Swift engine's
//! `PersonMergeJournal` so either engine can undo the other's merge.

use std::collections::BTreeMap;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::face_queues::{anchor, blank, id_list, journal, names, ordered, person_exists, person_json, queue_page, queue_refused};
use super::face_review::recount;
use crate::ipc::FaceReviewPage;

pub(crate) const KIND: &str = "person_merge";
pub(crate) const QUEUE: &str = "mergeUndo";
pub(crate) const LAST_TOKEN: &str = "lastPersonMerge";
const WINDOW: f64 = 86_400.0;
pub(crate) const CHANGED: &str = "This merge can't be undone any more because these people changed since.";

const VERDICT_COLUMNS: &str = "person_a, person_b, same_person, confidence, vlm_model, verified_at, face_a, face_b, file_a, bbox_a, file_b, bbox_b";

type Verdicts = BTreeMap<(i64, i64), Value>;

/// The verdicts a merge can replace or drop: every "different people" row, the
/// pair's own row and any row naming the person being merged in.
fn verdicts(conn: &Connection, pair: (i64, i64), source: i64) -> rusqlite::Result<Verdicts> {
    let mut statement = conn.prepare(&format!(
        "SELECT {VERDICT_COLUMNS} FROM face_verifications \
         WHERE same_person=0 OR (person_a=?1 AND person_b=?2) OR person_a=?3 OR person_b=?3"
    ))?;
    statement
        .query_map(params![pair.0, pair.1, source], |row| {
            let (a, b): (i64, i64) = (row.get(0)?, row.get(1)?);
            Ok((
                (a, b),
                json!({
                    "personA": a,
                    "personB": b,
                    "samePerson": row.get::<_, i64>(2)?,
                    "confidence": row.get::<_, f64>(3)?,
                    "model": row.get::<_, String>(4)?,
                    "verifiedAt": row.get::<_, f64>(5)?,
                    "faceA": row.get::<_, Option<i64>>(6)?,
                    "faceB": row.get::<_, Option<i64>>(7)?,
                    "fileA": row.get::<_, Option<i64>>(8)?,
                    "bboxA": row.get::<_, Option<String>>(9)?,
                    "fileB": row.get::<_, Option<i64>>(10)?,
                    "bboxB": row.get::<_, Option<String>>(11)?,
                }),
            ))
        })?
        .collect()
}

/// What a merge looked like before it ran. Take it first, run the merge, then
/// `record` it in the same transaction.
pub(crate) struct Pending {
    target: i64,
    source: i64,
    keep: Value,
    other: Value,
    faces: Vec<i64>,
    anchor_file: Option<i64>,
    verdicts: Verdicts,
}

pub(crate) fn begin(conn: &Connection, target: i64, source: i64) -> rusqlite::Result<Option<Pending>> {
    let (Some(keep), Some(other)) = (person_json(conn, target)?, person_json(conn, source)?) else { return Ok(None) };
    let faces = {
        let mut statement = conn.prepare("SELECT id FROM face_prints WHERE person_id=?1 ORDER BY id")?;
        statement.query_map([source], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    Ok(Some(Pending {
        target,
        source,
        keep,
        other,
        faces,
        anchor_file: anchor(conn, target)?.map(|found| found.file),
        verdicts: verdicts(conn, ordered(target, source), source)?,
    }))
}

impl Pending {
    /// Journals the finished merge and returns the operation to undo.
    pub(crate) fn record(self, conn: &Connection, at: f64) -> rusqlite::Result<String> {
        let now = verdicts(conn, ordered(self.target, self.source), self.source)?;
        let replaced: Vec<Value> =
            self.verdicts.into_iter().filter(|(key, verdict)| now.get(key) != Some(verdict)).map(|(_, verdict)| verdict).collect();
        let before = json!({
            "resolution": "merge",
            "personID": self.target,
            "otherPersonID": self.source,
            "other": self.other,
            "keepNames": names(&self.keep),
            "keepUnknown": self.keep["is_unknown"],
            "keepRepresentative": self.keep["representative_face_id"],
            "verifications": replaced,
        });
        let after = json!({"resolution": "merge", "personID": self.target, "otherPersonID": self.source, "faceIDs": self.faces});
        journal(conn, KIND, self.anchor_file, &before, &after, at)
    }
}

fn movable(conn: &Connection, faces: &[i64], keep: i64) -> rusqlite::Result<Vec<i64>> {
    let mut statement =
        conn.prepare("SELECT id FROM face_prints WHERE person_id=?1 AND id IN (SELECT value FROM json_each(?2)) ORDER BY id")?;
    statement
        .query_map(params![keep, serde_json::to_string(faces).unwrap_or_else(|_| "[]".into())], |row| row.get(0))?
        .collect()
}

/// The people and the faces that can still go back, or None when the merged-in
/// person's id is in use again or none of its faces can return.
fn undoable(conn: &Connection, after: &Value) -> rusqlite::Result<Option<(i64, i64, Vec<i64>)>> {
    let (Some(keep), Some(other)) = (after["personID"].as_i64(), after["otherPersonID"].as_i64()) else { return Ok(None) };
    if person_exists(conn, other)? {
        return Ok(None);
    }
    let faces = id_list(&after["faceIDs"]);
    let back = movable(conn, &faces, keep)?;
    if !faces.is_empty() && back.is_empty() {
        return Ok(None);
    }
    Ok(Some((keep, other, back)))
}

/// The newest merge from the last day that can still be undone.
pub(crate) fn latest(conn: &Connection, at: f64) -> rusqlite::Result<Option<(String, i64, i64)>> {
    let rows: Vec<(String, String)> = {
        let mut statement = conn.prepare(
            "SELECT o.id, c.after_json FROM catalog_operations o JOIN catalog_corrections c ON c.id=o.id \
             WHERE o.state='completed' AND c.kind=?1 AND o.created_at>=?2 ORDER BY o.created_at DESC, o.rowid DESC LIMIT 50",
        )?;
        statement.query_map(params![KIND, at - WINDOW], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (id, after) in rows {
        let Ok(after) = serde_json::from_str::<Value>(&after) else { continue };
        if let Some((keep, other, _)) = undoable(conn, &after)? {
            return Ok(Some((id, keep, other)));
        }
    }
    Ok(None)
}

pub(crate) fn offered(conn: &Connection, at: f64) -> rusqlite::Result<i64> {
    Ok(latest(conn, at)?.is_some() as i64)
}

pub(crate) fn list(conn: &Connection, request: &str, at: f64) -> rusqlite::Result<FaceReviewPage> {
    let found = latest(conn, at)?;
    Ok(FaceReviewPage {
        total: found.is_some() as i64,
        person_id: found.as_ref().map(|found| found.1),
        other_person_id: found.as_ref().map(|found| found.2),
        operation_id: found.map(|found| found.0),
        ..queue_page(request, "list", "ok", QUEUE)
    })
}

pub(crate) fn undo(conn: &Connection, request: &str, before: &Value, after: &Value, at: f64) -> rusqlite::Result<FaceReviewPage> {
    let Some((keep, other, faces)) = undoable(conn, after)? else {
        return Ok(FaceReviewPage { total: offered(conn, at)?, ..queue_refused(request, "undo", QUEUE, CHANGED) });
    };
    let row = &before["other"];
    conn.execute(
        "INSERT INTO persons(id,name,title,first_name,middle_name,last_name,suffix,is_unknown,created_at,representative_face_id,file_count,anchor_radius,last_clustered_at) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0,?11,?12)",
        params![
            other,
            row["name"].as_str(),
            row["title"].as_str(),
            row["first_name"].as_str(),
            row["middle_name"].as_str(),
            row["last_name"].as_str(),
            row["suffix"].as_str(),
            row["is_unknown"].as_i64().unwrap_or(0),
            row["created_at"].as_f64().unwrap_or(at),
            row["representative_face_id"].as_i64(),
            row["anchor_radius"].as_f64(),
            row["last_clustered_at"].as_f64()
        ],
    )?;
    conn.execute(
        "UPDATE face_prints SET person_id=?1 WHERE person_id=?2 AND id IN (SELECT value FROM json_each(?3))",
        params![other, keep, serde_json::to_string(&faces).unwrap_or_else(|_| "[]".into())],
    )?;
    let kept: Vec<Option<String>> = before["keepNames"].as_array().into_iter().flatten().map(|name| name.as_str().map(str::to_string)).collect();
    if blank(&kept)
        && let Some(current) = person_json(conn, keep)?
        && names(&current) == names(row)
    {
        conn.execute(
            "UPDATE persons SET name=NULL,title=NULL,first_name=NULL,middle_name=NULL,last_name=NULL,suffix=NULL,is_unknown=?1 WHERE id=?2",
            params![before["keepUnknown"].as_i64().unwrap_or(0), keep],
        )?;
    }
    let pair = ordered(keep, other);
    conn.execute(
        "DELETE FROM face_verifications WHERE person_a=?1 AND person_b=?2 AND same_person=1 AND vlm_model='user-merged'",
        params![pair.0, pair.1],
    )?;
    for verdict in before["verifications"].as_array().into_iter().flatten() {
        conn.execute(
            &format!("INSERT OR IGNORE INTO face_verifications({VERDICT_COLUMNS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"),
            params![
                verdict["personA"].as_i64(),
                verdict["personB"].as_i64(),
                verdict["samePerson"].as_i64().unwrap_or(0),
                verdict["confidence"].as_f64().unwrap_or(1.0),
                verdict["model"].as_str().unwrap_or_default(),
                verdict["verifiedAt"].as_f64().unwrap_or(0.0),
                verdict["faceA"].as_i64(),
                verdict["faceB"].as_i64(),
                verdict["fileA"].as_i64(),
                verdict["bboxA"].as_str(),
                verdict["fileB"].as_i64(),
                verdict["bboxB"].as_str()
            ],
        )?;
    }
    if let Some(representative) = before["keepRepresentative"].as_i64() {
        conn.execute(
            "UPDATE persons SET representative_face_id=?1 WHERE id=?2 AND EXISTS(SELECT 1 FROM face_prints WHERE id=?1 AND person_id=?2)",
            params![representative, keep],
        )?;
    }
    recount(conn, keep)?;
    recount(conn, other)?;
    Ok(FaceReviewPage {
        person_id: Some(keep),
        other_person_id: Some(other),
        total: offered(conn, at)?,
        ..queue_page(request, "undo", "ok", QUEUE)
    })
}

#[cfg(test)]
#[path = "person_merge_tests.rs"]
mod tests;
