use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};

use crate::ipc::{
    sink::Sink, EngineError, EventPayload, FaceReviewItem, FaceReviewPage, IpcEvent, ListFaceReviewPayload,
    ResolveFaceReviewPayload, UndoFaceReviewPayload, Wrap,
};

type Writer = std::sync::Arc<parking_lot::Mutex<Connection>>;

const PENDING: &str = "'suggested','uncertain','ambiguous','rejected'";
pub(crate) const STALE: &str = "This suggestion changed. Refresh the review list.";
const MOVED: &str = "This face was moved since, so nothing changed.";
pub(crate) const MOVED_MANY: &str = "Some of these faces were moved since, so nothing changed.";
pub(crate) const NOTHING_TO_UNDO: &str = "There is nothing to undo for this suggestion.";
pub(crate) const NO_ANCHOR: &str = "This person has no face to compare with.";
const RESOLUTIONS: &str = "Choose accept, reject, ignore, acceptAll, merge, notSamePerson, dismiss or split.";
pub(crate) const DISPLAY_P: &str = "COALESCE(NULLIF(TRIM(p.name),''), NULLIF(TRIM(COALESCE(p.first_name,'') || ' ' || COALESCE(p.last_name,'')),''))";
const DISPLAY_RP: &str = "COALESCE(NULLIF(TRIM(rp.name),''), NULLIF(TRIM(COALESCE(rp.first_name,'') || ' ' || COALESCE(rp.last_name,'')),''))";
const PENDING_WHERE: &str = "fp.person_id IS NULL AND c.namespace = fp.embedding_model || '|' || fp.processing_version";

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Settings {
    pub suggest: f64,
    pub min: f64,
    pub margin: f64,
    pub auto_assign: bool,
}

impl Settings {
    pub(crate) fn from_env() -> Self {
        Self {
            suggest: env_cos("FILEID_FACE_REVIEW_SUGGEST_COS", 0.55),
            min: env_cos("FILEID_FACE_REVIEW_MIN_COS", 0.40),
            margin: env_cos("FILEID_FACE_REVIEW_MARGIN", 0.05),
            auto_assign: std::env::var("FILEID_FACE_REVIEW_AUTO_ASSIGN")
                .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true")),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self { suggest: 0.55, min: 0.40, margin: 0.05, auto_assign: false }
    }
}

fn env_cos(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map_or(default, |value| value.clamp(0.0, 1.0))
}

pub(crate) fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub(crate) fn round4(value: f64) -> f64 {
    (value * 10000.0).round() / 10000.0
}

pub(crate) fn json_ids<'a>(ids: impl IntoIterator<Item = &'a i64>) -> String {
    let mut sorted: Vec<i64> = ids.into_iter().copied().collect();
    sorted.sort_unstable();
    serde_json::to_string(&sorted).unwrap_or_else(|_| "[]".into())
}

pub(crate) fn unit_vector(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.is_empty() || blob.len() % 4 != 0 {
        return None;
    }
    let mut vector: Vec<f32> = blob.as_chunks::<4>().0.iter().map(|chunk| f32::from_le_bytes(*chunk)).collect();
    if vector.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::MIN_POSITIVE {
        return None;
    }
    for value in &mut vector {
        *value /= norm;
    }
    Some(vector)
}

pub(crate) fn dot(a: &[f32], b: &[f32]) -> f64 {
    f64::from(a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>())
}

/// Faces kept out of the full clustering pool for review: pending suggestions, and new faces once
/// the space already has assigned faces to compare them with.
pub(crate) fn holdout(conn: &Connection, model: &str, processing: &str, min_quality: f64) -> rusqlite::Result<HashSet<i64>> {
    let incremental: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM face_prints WHERE person_id IS NOT NULL AND embedding_model=?1 AND processing_version=?2)",
        params![model, processing],
        |row| row.get(0),
    )?;
    let new_faces = if incremental { "a.face_id IS NULL OR " } else { "" };
    let sql = format!(
        "SELECT fp.id FROM face_prints fp LEFT JOIN face_assignment_assessed a ON a.face_id=fp.id \
         JOIN catalog_revisions r ON r.file_id=fp.file_id \
         WHERE fp.person_id IS NULL AND COALESCE(fp.excluded,0)=0 AND length(fp.arcface_embedding)=512 \
           AND fp.embedding_model=?1 AND fp.processing_version=?2 AND fp.source_revision=r.revision \
           AND COALESCE(fp.face_quality,0) >= ?3 AND ({new_faces}a.outcome IN ({PENDING}))"
    );
    let mut statement = conn.prepare(&sql)?;
    statement
        .query_map(params![model, processing, min_quality], |row| row.get(0))?
        .collect()
}

/// Records every face of this run's pool that has no assessment yet, so later runs treat it as known.
pub(crate) fn mark_pool<'a>(conn: &Connection, pool: impl IntoIterator<Item = &'a i64>, at: f64) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT OR IGNORE INTO face_assignment_assessed(face_id,namespace,outcome,assessed_at) \
         SELECT id, COALESCE(embedding_model,'') || '|' || COALESCE(processing_version,''), \
                CASE WHEN person_id IS NULL THEN 'unmatched' ELSE 'clustered' END, ?2 \
         FROM face_prints WHERE id IN (SELECT value FROM json_each(?1))",
        params![json_ids(pool), at],
    )
}

pub(crate) fn pending_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM face_review_candidates c JOIN face_prints fp ON fp.id=c.face_id JOIN persons p ON p.id=c.person_id WHERE {PENDING_WHERE}"),
        [],
        |row| row.get(0),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scored {
    pub person: i64,
    pub score: f64,
    pub unknown: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decision {
    Unmatched,
    Queue { person: i64, score: f64, reason: &'static str, runner: Option<(i64, f64)> },
    Assign { person: i64 },
}

pub(crate) fn decide(scored: &[Scored], barred: &HashSet<i64>, settings: &Settings) -> Decision {
    let Some(first) = scored.first() else { return Decision::Unmatched };
    if first.unknown {
        return Decision::Unmatched;
    }
    let Some(best) = scored.iter().find(|entry| !entry.unknown && !barred.contains(&entry.person)) else {
        return Decision::Unmatched;
    };
    let runner = scored.iter().find(|entry| entry.person != best.person).map(|entry| (entry.person, entry.score));
    let margin = runner.map_or(f64::INFINITY, |(_, score)| best.score - score);
    if best.score < settings.min {
        return Decision::Unmatched;
    }
    let reason = if margin < settings.margin {
        "ambiguous"
    } else if best.score >= settings.suggest {
        if settings.auto_assign {
            return Decision::Assign { person: best.person };
        }
        "suggested"
    } else {
        "uncertain"
    };
    Decision::Queue { person: best.person, score: best.score, reason, runner }
}

struct Receiver {
    person: i64,
    unknown: bool,
    centroid: Vec<f32>,
}

fn receivers(conn: &Connection, model: &str, processing: &str) -> rusqlite::Result<Vec<Receiver>> {
    let mut statement = conn.prepare(
        "SELECT fp.person_id, COALESCE(p.is_unknown,0), fp.arcface_embedding FROM face_prints fp JOIN persons p ON p.id=fp.person_id \
         WHERE COALESCE(fp.excluded,0)=0 AND length(fp.arcface_embedding)=512 AND fp.embedding_model=?1 AND fp.processing_version=?2 \
         ORDER BY fp.person_id, fp.id",
    )?;
    let mut rows = statement.query(params![model, processing])?;
    let mut out: Vec<Receiver> = Vec::new();
    while let Some(row) = rows.next()? {
        let person: i64 = row.get(0)?;
        let unknown: i64 = row.get(1)?;
        let blob: Vec<u8> = row.get(2)?;
        let Some(vector) = unit_vector(&blob) else { continue };
        match out.last_mut() {
            Some(last) if last.person == person => {
                for (sum, value) in last.centroid.iter_mut().zip(&vector) {
                    *sum += value;
                }
            }
            _ => out.push(Receiver { person, unknown: unknown != 0, centroid: vector }),
        }
    }
    out.retain_mut(|receiver| {
        let norm = receiver.centroid.iter().map(|value| value * value).sum::<f32>().sqrt();
        if !norm.is_finite() || norm <= f32::MIN_POSITIVE {
            return false;
        }
        for value in &mut receiver.centroid {
            *value /= norm;
        }
        true
    });
    Ok(out)
}

struct Candidate {
    face: i64,
    file: i64,
    previous: Option<String>,
    scored: Vec<Scored>,
    barred: HashSet<i64>,
}

fn same_file_owners(conn: &Connection, face: i64, file: i64) -> rusqlite::Result<Vec<i64>> {
    let mut statement = conn.prepare_cached(
        "SELECT DISTINCT person_id FROM face_prints WHERE file_id=?1 AND id<>?2 AND person_id IS NOT NULL",
    )?;
    statement.query_map(params![file, face], |row| row.get(0))?.collect()
}

fn constraint_owners(conn: &Connection, file: i64, bbox: &str) -> rusqlite::Result<Vec<i64>> {
    let mut statement = conn.prepare_cached(
        "SELECT a.person_id FROM face_identity_constraints k JOIN face_prints a ON a.file_id=k.anchor_file_id AND a.bbox=k.anchor_bbox \
         WHERE k.file_id=?1 AND k.bbox=?2 AND a.person_id IS NOT NULL",
    )?;
    statement.query_map(params![file, bbox], |row| row.get(0))?.collect()
}

pub(crate) fn owner_of(conn: &Connection, legacy: Option<i64>, file: Option<i64>, bbox: Option<&str>) -> rusqlite::Result<Option<i64>> {
    if let (Some(file), Some(bbox)) = (file, bbox) {
        let found: Option<Option<i64>> = conn
            .query_row(
                "SELECT person_id FROM face_prints WHERE file_id=?1 AND bbox=?2 ORDER BY id LIMIT 1",
                params![file, bbox],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = found {
            return Ok(owner);
        }
    }
    let Some(legacy) = legacy else { return Ok(None) };
    Ok(conn
        .query_row("SELECT person_id FROM face_prints WHERE id=?1", [legacy], |row| row.get::<_, Option<i64>>(0))
        .optional()?
        .flatten())
}

fn verdict_owners(conn: &Connection, face: i64, file: i64, bbox: &str) -> rusqlite::Result<Vec<i64>> {
    type Side = (Option<i64>, Option<i64>, Option<String>);
    let mut statement = conn.prepare_cached(
        "SELECT face_a, file_a, bbox_a, face_b, file_b, bbox_b FROM face_verifications WHERE same_person=0 \
         AND (face_a=?1 OR (file_a=?2 AND bbox_a=?3) OR face_b=?1 OR (file_b=?2 AND bbox_b=?3))",
    )?;
    let rows: Vec<(Side, Side)> = statement
        .query_map(params![face, file, bbox], |row| {
            Ok(((row.get(0)?, row.get(1)?, row.get(2)?), (row.get(3)?, row.get(4)?, row.get(5)?)))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let is_face = |side: &Side| side.0 == Some(face) || (side.1 == Some(file) && side.2.as_deref() == Some(bbox));
    let mut owners = Vec::new();
    for (a, b) in rows {
        let other = match (is_face(&a), is_face(&b)) {
            (true, false) => b,
            (false, true) => a,
            _ => continue,
        };
        if let Some(owner) = owner_of(conn, other.0, other.1, other.2.as_deref())? {
            owners.push(owner);
        }
    }
    Ok(owners)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AssessStats {
    pub queued: usize,
    pub assigned: usize,
    pub unmatched: usize,
}

/// Compares held faces with person centroids in their own space. By default it only queues
/// suggestions; `auto_assign` assigns confident matches instead.
pub(crate) fn assess(
    conn: &Connection,
    held: &HashSet<i64>,
    model: &str,
    processing: &str,
    settings: &Settings,
    at: f64,
) -> rusqlite::Result<AssessStats> {
    let mut stats = AssessStats::default();
    if held.is_empty() {
        return Ok(stats);
    }
    let namespace = format!("{model}|{processing}");
    let people = receivers(conn, model, processing)?;
    let mut candidates = Vec::new();
    {
        let mut statement = conn.prepare(
            "SELECT fp.id, fp.file_id, fp.bbox, fp.arcface_embedding, a.outcome FROM face_prints fp \
             LEFT JOIN face_assignment_assessed a ON a.face_id=fp.id \
             WHERE fp.id IN (SELECT value FROM json_each(?1)) AND fp.person_id IS NULL AND COALESCE(fp.excluded,0)=0 \
               AND length(fp.arcface_embedding)=512 AND fp.embedding_model=?2 AND fp.processing_version=?3 ORDER BY fp.id",
        )?;
        let rows: Vec<(i64, i64, String, Vec<u8>, Option<String>)> = statement
            .query_map(params![json_ids(held), model, processing], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (face, file, bbox, blob, previous) in rows {
            let Some(vector) = unit_vector(&blob) else { continue };
            let mut scored: Vec<Scored> = people
                .iter()
                .filter(|receiver| receiver.centroid.len() == vector.len())
                .map(|receiver| Scored { person: receiver.person, score: dot(&vector, &receiver.centroid), unknown: receiver.unknown })
                .collect();
            scored.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.person.cmp(&b.person)));
            let mut barred: HashSet<i64> = people.iter().filter(|receiver| receiver.unknown).map(|receiver| receiver.person).collect();
            barred.extend(same_file_owners(conn, face, file)?);
            barred.extend(constraint_owners(conn, file, &bbox)?);
            barred.extend(verdict_owners(conn, face, file, &bbox)?);
            candidates.push(Candidate { face, file, previous, scored, barred });
        }
    }
    let lead = |candidate: &Candidate| -> Option<f64> {
        let first = candidate.scored.first()?;
        if first.unknown {
            return None;
        }
        candidate.scored.iter().find(|entry| !candidate.barred.contains(&entry.person)).map(|entry| entry.score)
    };
    let mut ordered: Vec<(Option<f64>, Candidate)> = candidates.into_iter().map(|candidate| (lead(&candidate), candidate)).collect();
    ordered.sort_by(|(a, x), (b, y)| match (a, b) {
        (Some(a), Some(b)) => b.total_cmp(a).then(x.face.cmp(&y.face)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => x.face.cmp(&y.face),
    });
    let mut claimed: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut received: Vec<(i64, i64)> = Vec::new();
    for (_, mut candidate) in ordered {
        if let Some(taken) = claimed.get(&candidate.file) {
            candidate.barred.extend(taken);
        }
        let decision = decide(&candidate.scored, &candidate.barred, settings);
        let previous: Option<(i64, f64)> = conn
            .query_row(
                "SELECT person_id, created_at FROM face_review_candidates WHERE face_id=?1",
                [candidate.face],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        conn.execute("DELETE FROM face_review_candidates WHERE face_id=?1", [candidate.face])?;
        let outcome = match &decision {
            Decision::Unmatched => {
                stats.unmatched += 1;
                if candidate.previous.as_deref() == Some("rejected") { "rejected" } else { "unmatched" }
            }
            Decision::Assign { person } => {
                conn.execute("UPDATE face_prints SET person_id=?1 WHERE id=?2 AND person_id IS NULL", params![person, candidate.face])?;
                claimed.entry(candidate.file).or_default().insert(*person);
                received.push((*person, candidate.face));
                stats.assigned += 1;
                "assigned"
            }
            Decision::Queue { person, score, reason, runner } => {
                if *reason == "suggested" {
                    claimed.entry(candidate.file).or_default().insert(*person);
                }
                let created = previous.filter(|(owner, _)| owner == person).map_or(at, |(_, created)| created);
                conn.execute(
                    "INSERT INTO face_review_candidates(face_id,person_id,score,reason,runner_up_person_id,runner_up_score,namespace,created_at) \
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        candidate.face,
                        person,
                        round4(*score),
                        reason,
                        runner.map(|(id, _)| id),
                        runner.map(|(_, value)| round4(value)),
                        namespace,
                        created
                    ],
                )?;
                stats.queued += 1;
                reason
            }
        };
        upsert_outcome(conn, candidate.face, &namespace, outcome, at)?;
    }
    received.sort_unstable();
    for (person, face) in &received {
        conn.execute(
            "UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?1), \
             representative_face_id=COALESCE(representative_face_id, ?2) WHERE id=?1",
            params![person, face],
        )?;
    }
    Ok(stats)
}

pub(crate) fn upsert_outcome(conn: &Connection, face: i64, namespace: &str, outcome: &str, at: f64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO face_assignment_assessed(face_id,namespace,outcome,assessed_at) VALUES(?1,?2,?3,?4) \
         ON CONFLICT(face_id) DO UPDATE SET namespace=excluded.namespace,outcome=excluded.outcome,assessed_at=excluded.assessed_at",
        params![face, namespace, outcome, at],
    )?;
    Ok(())
}

pub(crate) fn face_namespace(conn: &Connection, face: i64) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT COALESCE(embedding_model,'') || '|' || COALESCE(processing_version,'') FROM face_prints WHERE id=?1",
        [face],
        |row| row.get(0),
    )
}

/// The user moved a face out of a person by hand: remember it is not that person, log the
/// correction and settle any pending suggestion for it.
pub(crate) fn record_reassignment(conn: &Connection, face: i64, previous: Option<i64>, destination: Option<i64>) -> rusqlite::Result<()> {
    let at = now();
    let (file, bbox): (i64, String) = conn.query_row("SELECT file_id, bbox FROM face_prints WHERE id=?1", [face], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if let Some(person) = previous {
        let anchor: Option<(i64, String)> = conn
            .query_row(
                "SELECT a.file_id, a.bbox FROM persons p JOIN face_prints a ON a.id=p.representative_face_id WHERE p.id=?1 AND a.id<>?2",
                params![person, face],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((anchor_file, anchor_bbox)) = anchor {
            conn.execute(
                "INSERT OR IGNORE INTO face_identity_constraints(file_id,bbox,anchor_file_id,anchor_bbox,source,created_at) VALUES(?1,?2,?3,?4,'reassign',?5)",
                params![file, bbox, anchor_file, anchor_bbox, at],
            )?;
        }
    }
    let before = serde_json::json!({"faceID": face, "personID": previous}).to_string();
    let after = serde_json::json!({"faceID": face, "personID": destination}).to_string();
    conn.execute(
        "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?1,?2,'face_reassign',?3,?4,?5)",
        params![uuid::Uuid::new_v4().to_string(), file, before, after, at],
    )?;
    conn.execute("DELETE FROM face_review_candidates WHERE face_id=?1", [face])?;
    upsert_outcome(conn, face, &face_namespace(conn, face)?, "manual", at)
}

pub(crate) fn page(request_id: &str, action: &str, status: &str) -> FaceReviewPage {
    FaceReviewPage {
        request_id: request_id.to_string(),
        action: action.to_string(),
        status: status.to_string(),
        message: None,
        operation_id: None,
        face_id: None,
        person_id: None,
        total: 0,
        offset: 0,
        items: Vec::new(),
        queue: None,
        merges: None,
        splits: None,
        refresh: None,
        other_person_id: None,
        accepted_face_ids: None,
        skipped_face_ids: None,
    }
}

pub(crate) fn refused(request_id: &str, action: &str, message: &str) -> FaceReviewPage {
    FaceReviewPage { message: Some(message.to_string()), ..page(request_id, action, "refused") }
}

pub(crate) fn failed(request_id: &str, action: &str, message: &str) -> FaceReviewPage {
    FaceReviewPage { message: Some(message.to_string()), ..page(request_id, action, "failed") }
}

pub(crate) fn valid_request_id(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

pub(crate) fn page_bounds(payload: &ListFaceReviewPayload) -> Result<(i64, i64), FaceReviewPage> {
    let offset = payload.offset.unwrap_or(0);
    let limit = payload.limit.unwrap_or(50);
    if offset < 0 || !(1..=200).contains(&limit) {
        let reply = refused(&payload.request_id, "list", "Ask for 1 to 200 suggestions from offset 0 or later.");
        return Err(FaceReviewPage { offset, ..reply });
    }
    Ok((offset, limit))
}

pub(crate) fn list(conn: &Connection, payload: &ListFaceReviewPayload) -> rusqlite::Result<FaceReviewPage> {
    match payload.queue.as_deref() {
        None | Some("faces") => list_faces(conn, payload),
        Some("merges") => super::face_queues::list_merges(conn, payload),
        Some("splits") => super::face_queues::list_splits(conn, payload),
        Some("refresh") => super::face_refresh::status_page(conn, &payload.request_id),
        Some(_) => Ok(refused(&payload.request_id, "list", "Choose the faces, merges, splits or refresh queue.")),
    }
}

fn list_faces(conn: &Connection, payload: &ListFaceReviewPayload) -> rusqlite::Result<FaceReviewPage> {
    let (offset, limit) = match page_bounds(payload) {
        Ok(bounds) => bounds,
        Err(reply) => return Ok(reply),
    };
    let filter = format!("{PENDING_WHERE} AND (?1 IS NULL OR c.person_id=?1)");
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM face_review_candidates c JOIN face_prints fp ON fp.id=c.face_id JOIN files f ON f.id=fp.file_id JOIN persons p ON p.id=c.person_id WHERE {filter}"),
        [payload.person_id],
        |row| row.get(0),
    )?;
    let sql = format!(
        "SELECT c.face_id, fp.file_id, f.path_text, fp.bbox, c.person_id, {DISPLAY_P}, p.representative_face_id, c.score, c.reason, \
                c.runner_up_person_id, {DISPLAY_RP}, c.runner_up_score, c.created_at, COALESCE(fp.excluded,0) \
         FROM face_review_candidates c JOIN face_prints fp ON fp.id=c.face_id JOIN files f ON f.id=fp.file_id \
         JOIN persons p ON p.id=c.person_id LEFT JOIN persons rp ON rp.id=c.runner_up_person_id \
         WHERE {filter} ORDER BY c.score DESC, c.face_id ASC LIMIT ?2 OFFSET ?3"
    );
    let mut statement = conn.prepare(&sql)?;
    let items = statement
        .query_map(params![payload.person_id, limit, offset], |row| {
            Ok(FaceReviewItem {
                face_id: row.get(0)?,
                file_id: row.get(1)?,
                path: row.get(2)?,
                bbox: row.get(3)?,
                person_id: row.get(4)?,
                person_name: row.get(5)?,
                representative_face_id: row.get(6)?,
                score: row.get(7)?,
                reason: row.get(8)?,
                runner_up_person_id: row.get(9)?,
                runner_up_name: row.get(10)?,
                runner_up_score: row.get(11)?,
                created_at: row.get(12)?,
                low_quality: (row.get::<_, i64>(13)? != 0).then_some(true),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(FaceReviewPage { total, offset, items, ..page(&payload.request_id, "list", "ok") })
}

struct Pending {
    person: i64,
    score: f64,
    reason: String,
    runner: Option<i64>,
    runner_score: Option<f64>,
    namespace: String,
    created: f64,
}

pub(crate) fn recount(conn: &Connection, person: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?1), \
         representative_face_id=CASE WHEN representative_face_id IS NOT NULL AND EXISTS(SELECT 1 FROM face_prints WHERE id=persons.representative_face_id AND person_id=?1) \
           THEN representative_face_id ELSE COALESCE( \
             (SELECT fp.id FROM face_prints fp WHERE fp.person_id=?1 AND fp.arcface_embedding IS NOT NULL ORDER BY COALESCE(fp.face_quality,0) DESC LIMIT 1), \
             (SELECT fp.id FROM face_prints fp WHERE fp.person_id=?1 ORDER BY COALESCE(fp.face_quality,0) DESC LIMIT 1)) END \
         WHERE id=?1",
        [person],
    )?;
    Ok(())
}

fn resolve_in(conn: &Connection, payload: &ResolveFaceReviewPayload) -> rusqlite::Result<FaceReviewPage> {
    let request = payload.request_id.as_str();
    match payload.resolution.as_str() {
        "accept" | "reject" | "ignore" => match payload.face_id {
            Some(face) => resolve_face_in(conn, request, face, payload.person_id, payload.resolution.as_str()),
            None => Ok(refused(request, "resolve", "Choose a face to resolve.")),
        },
        "acceptAll" => accept_all_in(conn, request, payload.person_id),
        "merge" | "notSamePerson" | "dismiss" | "split" => super::face_queues::resolve_in(conn, payload),
        _ => Ok(refused(request, "resolve", RESOLUTIONS)),
    }
}

fn resolve_face_in(conn: &Connection, request: &str, face_id: i64, person_id: i64, resolution: &str) -> rusqlite::Result<FaceReviewPage> {
    let row: Option<(Pending, i64, String, Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT c.person_id, c.score, c.reason, c.runner_up_person_id, c.runner_up_score, c.namespace, c.created_at, \
                    fp.file_id, fp.bbox, fp.person_id, a.outcome \
             FROM face_review_candidates c JOIN face_prints fp ON fp.id=c.face_id \
             LEFT JOIN face_assignment_assessed a ON a.face_id=c.face_id WHERE c.face_id=?1",
            [face_id],
            |row| {
                Ok((
                    Pending {
                        person: row.get(0)?,
                        score: row.get(1)?,
                        reason: row.get(2)?,
                        runner: row.get(3)?,
                        runner_score: row.get(4)?,
                        namespace: row.get(5)?,
                        created: row.get(6)?,
                    },
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )
        .optional()?;
    let Some((pending, file, bbox, owner, outcome)) = row else {
        return Ok(refused(request, "resolve", STALE));
    };
    if pending.person != person_id || owner.is_some() {
        return Ok(refused(request, "resolve", STALE));
    }
    let person = pending.person;
    let at = now();
    let mut new_owner = None;
    let mut constraint: (Option<i64>, bool) = (None, false);
    match resolution {
        "accept" => {
            let present: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM face_prints WHERE file_id=?1 AND person_id=?2 AND id<>?3)",
                params![file, person, face_id],
                |row| row.get(0),
            )?;
            if present {
                return Ok(refused(request, "resolve", "That person is already in this photo."));
            }
            conn.execute("UPDATE face_prints SET person_id=?1 WHERE id=?2", params![person, face_id])?;
            conn.execute(
                "UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?1), \
                 representative_face_id=COALESCE(representative_face_id, ?2) WHERE id=?1",
                params![person, face_id],
            )?;
            new_owner = Some(person);
        }
        "reject" => {
            let anchor: Option<(i64, String)> = conn
                .query_row(
                    "SELECT a.file_id, a.bbox FROM face_prints a WHERE a.person_id=?1 \
                     ORDER BY CASE WHEN a.id=(SELECT representative_face_id FROM persons WHERE id=?1) THEN 0 ELSE 1 END, \
                              COALESCE(a.face_quality,0) DESC, a.id LIMIT 1",
                    [person],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((anchor_file, anchor_bbox)) = anchor else {
                return Ok(refused(request, "resolve", "This person has no face to compare with."));
            };
            let created = conn.execute(
                "INSERT OR IGNORE INTO face_identity_constraints(file_id,bbox,anchor_file_id,anchor_bbox,source,created_at) VALUES(?1,?2,?3,?4,'review',?5)",
                params![file, bbox, anchor_file, anchor_bbox, at],
            )? > 0;
            let id: i64 = conn.query_row(
                "SELECT id FROM face_identity_constraints WHERE file_id=?1 AND bbox=?2 AND anchor_file_id=?3 AND anchor_bbox=?4",
                params![file, bbox, anchor_file, anchor_bbox],
                |row| row.get(0),
            )?;
            constraint = (Some(id), created);
        }
        _ => {
            conn.execute(
                "INSERT INTO persons(name,is_unknown,created_at,representative_face_id,file_count) VALUES(NULL,1,?1,?2,1)",
                params![at, face_id],
            )?;
            let unknown = conn.last_insert_rowid();
            conn.execute("UPDATE face_prints SET person_id=?1 WHERE id=?2", params![unknown, face_id])?;
            new_owner = Some(unknown);
        }
    }
    conn.execute("DELETE FROM face_review_candidates WHERE face_id=?1", [face_id])?;
    let final_outcome = match resolution {
        "accept" => "accepted",
        "reject" => "rejected",
        _ => "ignored",
    };
    upsert_outcome(conn, face_id, &face_namespace(conn, face_id)?, final_outcome, at)?;
    let id = uuid::Uuid::new_v4().to_string();
    let before = serde_json::json!({
        "faceID": face_id,
        "outcome": outcome,
        "candidate": {
            "personID": pending.person,
            "score": pending.score,
            "reason": pending.reason,
            "runnerUpPersonID": pending.runner,
            "runnerUpScore": pending.runner_score,
            "namespace": pending.namespace,
            "createdAt": pending.created,
        },
    })
    .to_string();
    let after = serde_json::json!({
        "faceID": face_id,
        "resolution": resolution,
        "personID": new_owner,
        "constraintID": constraint.0,
        "constraintCreated": constraint.1,
    })
    .to_string();
    let plan = serde_json::json!({"type": "faceReview", "faceID": face_id, "resolution": resolution}).to_string();
    conn.execute(
        "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?1,?2,'face_review',?3,?4,?5)",
        params![id, file, before, after, at],
    )?;
    conn.execute(
        "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?1,?2,?3,'completed',?4)",
        params![id, plan, before, at],
    )?;
    Ok(FaceReviewPage {
        operation_id: Some(id),
        face_id: Some(face_id),
        person_id: new_owner,
        total: pending_count(conn)?,
        ..page(request, "resolve", "ok")
    })
}

fn undo_in(conn: &Connection, payload: &UndoFaceReviewPayload) -> rusqlite::Result<FaceReviewPage> {
    let request = payload.request_id.as_str();
    let found: Option<(String, String, String)> = conn
        .query_row(
            "SELECT c.kind, c.before_json, c.after_json FROM catalog_operations o JOIN catalog_corrections c ON c.id=o.id \
             WHERE o.id=?1 AND o.state='completed' \
               AND c.kind IN ('face_review','face_review_bulk','face_merge_review','face_split_review')",
            [&payload.operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let nothing = NOTHING_TO_UNDO;
    let Some((kind, before, after)) = found else { return Ok(refused(request, "undo", nothing)) };
    let (Ok(before), Ok(after)) = (
        serde_json::from_str::<serde_json::Value>(&before),
        serde_json::from_str::<serde_json::Value>(&after),
    ) else {
        return Ok(refused(request, "undo", nothing));
    };
    let reply = match kind.as_str() {
        "face_review_bulk" => undo_accept_all_in(conn, request, &before, &after)?,
        "face_merge_review" | "face_split_review" => super::face_queues::undo_in(conn, request, &before, &after)?,
        _ => undo_face_in(conn, request, &before, &after)?,
    };
    if reply.status == "ok" {
        conn.execute("UPDATE catalog_operations SET state='undone' WHERE id=?1", [&payload.operation_id])?;
        return Ok(FaceReviewPage { operation_id: Some(payload.operation_id.clone()), ..reply });
    }
    Ok(reply)
}

fn undo_face_in(conn: &Connection, request: &str, before: &serde_json::Value, after: &serde_json::Value) -> rusqlite::Result<FaceReviewPage> {
    let nothing = NOTHING_TO_UNDO;
    let Some(face) = after["faceID"].as_i64() else { return Ok(refused(request, "undo", nothing)) };
    let owner: Option<Option<i64>> = conn
        .query_row("SELECT person_id FROM face_prints WHERE id=?1", [face], |row| row.get(0))
        .optional()?;
    let Some(owner) = owner else { return Ok(refused(request, "undo", "This face no longer exists.")) };
    let assigned = after["personID"].as_i64();
    match after["resolution"].as_str() {
        Some("accept") => {
            if owner.is_none() || owner != assigned {
                return Ok(refused(request, "undo", MOVED));
            }
            conn.execute("UPDATE face_prints SET person_id=NULL WHERE id=?1", [face])?;
            if let Some(person) = assigned {
                recount(conn, person)?;
            }
        }
        Some("ignore") => {
            if owner.is_none() || owner != assigned {
                return Ok(refused(request, "undo", MOVED));
            }
            conn.execute("UPDATE face_prints SET person_id=NULL WHERE id=?1", [face])?;
            conn.execute(
                "DELETE FROM persons WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM face_prints WHERE person_id=?1) \
                 AND COALESCE(TRIM(name),'')='' AND COALESCE(TRIM(first_name),'')='' AND COALESCE(TRIM(last_name),'')=''",
                [assigned],
            )?;
        }
        Some("reject") => {
            if owner.is_some() {
                return Ok(refused(request, "undo", MOVED));
            }
            if after["constraintCreated"].as_bool() == Some(true)
                && let Some(constraint) = after["constraintID"].as_i64()
            {
                conn.execute("DELETE FROM face_identity_constraints WHERE id=?1", [constraint])?;
            }
        }
        _ => return Ok(refused(request, "undo", nothing)),
    }
    restore_candidate(conn, face, &before["candidate"])?;
    Ok(FaceReviewPage { face_id: Some(face), total: pending_count(conn)?, ..page(request, "undo", "ok") })
}

fn restore_candidate(conn: &Connection, face: i64, candidate: &serde_json::Value) -> rusqlite::Result<()> {
    let reason = candidate["reason"].as_str().unwrap_or("suggested");
    let namespace = candidate["namespace"].as_str().unwrap_or_default();
    if let Some(person) = candidate["personID"].as_i64() {
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM persons WHERE id=?1)", [person], |row| row.get(0))?;
        if exists {
            let runner = candidate["runnerUpPersonID"]
                .as_i64()
                .filter(|id| conn.query_row("SELECT EXISTS(SELECT 1 FROM persons WHERE id=?1)", [id], |row| row.get(0)).unwrap_or(false));
            conn.execute(
                "INSERT OR REPLACE INTO face_review_candidates(face_id,person_id,score,reason,runner_up_person_id,runner_up_score,namespace,created_at) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    face,
                    person,
                    candidate["score"].as_f64().unwrap_or(0.0),
                    reason,
                    runner,
                    candidate["runnerUpScore"].as_f64(),
                    namespace,
                    candidate["createdAt"].as_f64().unwrap_or_else(now)
                ],
            )?;
        }
    }
    upsert_outcome(conn, face, namespace, reason, now())
}

fn candidate_json(conn: &Connection, face: i64) -> rusqlite::Result<Option<(i64, serde_json::Value)>> {
    conn.query_row(
        "SELECT fp.file_id, c.person_id, c.score, c.reason, c.runner_up_person_id, c.runner_up_score, c.namespace, c.created_at, a.outcome \
         FROM face_review_candidates c JOIN face_prints fp ON fp.id=c.face_id \
         LEFT JOIN face_assignment_assessed a ON a.face_id=c.face_id WHERE c.face_id=?1",
        [face],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                serde_json::json!({
                    "faceID": face,
                    "outcome": row.get::<_, Option<String>>(8)?,
                    "candidate": {
                        "personID": row.get::<_, i64>(1)?,
                        "score": row.get::<_, f64>(2)?,
                        "reason": row.get::<_, String>(3)?,
                        "runnerUpPersonID": row.get::<_, Option<i64>>(4)?,
                        "runnerUpScore": row.get::<_, Option<f64>>(5)?,
                        "namespace": row.get::<_, String>(6)?,
                        "createdAt": row.get::<_, f64>(7)?,
                    },
                }),
            ))
        },
    )
    .optional()
}

const NOTHING_TO_ACCEPT: &str = "Nothing to accept for this person.";
const ALL_IN_PHOTO: &str = "Every suggested face is in a photo that already has this person, so nothing changed.";

fn accept_all_in(conn: &Connection, request: &str, person: i64) -> rusqlite::Result<FaceReviewPage> {
    let faces: Vec<(i64, i64)> = {
        let mut statement = conn.prepare(&format!(
            "SELECT c.face_id, fp.file_id FROM face_review_candidates c JOIN face_prints fp ON fp.id=c.face_id \
             JOIN persons p ON p.id=c.person_id WHERE {PENDING_WHERE} AND c.person_id=?1 AND c.reason='suggested' \
             ORDER BY c.score DESC, c.face_id ASC"
        ))?;
        statement.query_map([person], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    if faces.is_empty() {
        return Ok(FaceReviewPage { person_id: Some(person), ..refused(request, "resolve", NOTHING_TO_ACCEPT) });
    }
    let at = now();
    let mut accepted = Vec::new();
    let mut skipped = Vec::new();
    let mut entries = Vec::new();
    let mut files: HashSet<i64> = HashSet::new();
    let mut first_file = None;
    for (face, file) in faces {
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM face_prints WHERE file_id=?1 AND person_id=?2 AND id<>?3)",
            params![file, person, face],
            |row| row.get(0),
        )?;
        if present || files.contains(&file) {
            skipped.push(face);
            continue;
        }
        let Some((_, entry)) = candidate_json(conn, face)? else { continue };
        conn.execute("UPDATE face_prints SET person_id=?1 WHERE id=?2 AND person_id IS NULL", params![person, face])?;
        conn.execute("DELETE FROM face_review_candidates WHERE face_id=?1", [face])?;
        upsert_outcome(conn, face, &face_namespace(conn, face)?, "accepted", at)?;
        files.insert(file);
        first_file.get_or_insert(file);
        accepted.push(face);
        entries.push(entry);
    }
    if accepted.is_empty() {
        return Ok(FaceReviewPage {
            person_id: Some(person),
            skipped_face_ids: Some(skipped),
            accepted_face_ids: Some(Vec::new()),
            ..refused(request, "resolve", ALL_IN_PHOTO)
        });
    }
    conn.execute(
        "UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?1), \
         representative_face_id=COALESCE(representative_face_id, ?2) WHERE id=?1",
        params![person, accepted[0]],
    )?;
    let id = uuid::Uuid::new_v4().to_string();
    let before = serde_json::json!({"personID": person, "faces": entries}).to_string();
    let after = serde_json::json!({
        "personID": person,
        "resolution": "acceptAll",
        "acceptedFaceIDs": accepted,
        "skippedFaceIDs": skipped,
    })
    .to_string();
    let plan = serde_json::json!({"type": "faceReviewBulk", "personID": person, "resolution": "acceptAll", "count": accepted.len()}).to_string();
    conn.execute(
        "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?1,?2,'face_review_bulk',?3,?4,?5)",
        params![id, first_file, before, after, at],
    )?;
    conn.execute(
        "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?1,?2,?3,'completed',?4)",
        params![id, plan, before, at],
    )?;
    Ok(FaceReviewPage {
        operation_id: Some(id),
        person_id: Some(person),
        accepted_face_ids: Some(accepted),
        skipped_face_ids: Some(skipped),
        total: pending_count(conn)?,
        ..page(request, "resolve", "ok")
    })
}

fn undo_accept_all_in(conn: &Connection, request: &str, before: &serde_json::Value, after: &serde_json::Value) -> rusqlite::Result<FaceReviewPage> {
    let (Some(person), Some(accepted)) = (after["personID"].as_i64(), after["acceptedFaceIDs"].as_array()) else {
        return Ok(refused(request, "undo", NOTHING_TO_UNDO));
    };
    let faces: Vec<i64> = accepted.iter().filter_map(serde_json::Value::as_i64).collect();
    for face in &faces {
        let owner: Option<Option<i64>> =
            conn.query_row("SELECT person_id FROM face_prints WHERE id=?1", [face], |row| row.get(0)).optional()?;
        if owner.flatten() != Some(person) {
            return Ok(refused(request, "undo", MOVED_MANY));
        }
    }
    for face in &faces {
        conn.execute("UPDATE face_prints SET person_id=NULL WHERE id=?1", [face])?;
    }
    recount(conn, person)?;
    for entry in before["faces"].as_array().into_iter().flatten() {
        if let Some(face) = entry["faceID"].as_i64() {
            restore_candidate(conn, face, &entry["candidate"])?;
        }
    }
    Ok(FaceReviewPage {
        person_id: Some(person),
        accepted_face_ids: Some(faces),
        total: pending_count(conn)?,
        ..page(request, "undo", "ok")
    })
}

fn settle(tx: rusqlite::Transaction<'_>, reply: FaceReviewPage) -> rusqlite::Result<FaceReviewPage> {
    if reply.status == "ok" {
        tx.commit()?;
        return Ok(reply);
    }
    drop(tx);
    Ok(FaceReviewPage { total: 0, ..reply })
}

pub(crate) fn resolve(conn: &Connection, payload: &ResolveFaceReviewPayload) -> rusqlite::Result<FaceReviewPage> {
    let tx = conn.unchecked_transaction()?;
    let reply = resolve_in(&tx, payload)?;
    let reply = settle(tx, reply)?;
    with_pending_total(conn, reply)
}

pub(crate) fn undo(conn: &Connection, payload: &UndoFaceReviewPayload) -> rusqlite::Result<FaceReviewPage> {
    let tx = conn.unchecked_transaction()?;
    let reply = undo_in(&tx, payload)?;
    let reply = settle(tx, reply)?;
    with_pending_total(conn, reply)
}

fn with_pending_total(conn: &Connection, reply: FaceReviewPage) -> rusqlite::Result<FaceReviewPage> {
    if reply.status == "ok" {
        return Ok(reply);
    }
    let total = match reply.queue.as_deref() {
        Some("merges") => super::face_queues::merge_count(conn)?,
        Some("splits") => super::face_queues::split_count(conn)?,
        _ => pending_count(conn)?,
    };
    Ok(FaceReviewPage { total, ..reply })
}

pub(crate) fn rejection(request_id: &str, action: &str) -> IpcEvent {
    reply_event(failed(request_id, action, "The library scan is paused, so this can't run yet. Resume or cancel the scan, then try again."))
}

fn reply_event(page: FaceReviewPage) -> IpcEvent {
    IpcEvent::now(EventPayload::FaceReview(Wrap::new(page)))
}

async fn invalid(sink: &Sink) {
    sink.send(EngineError::event(
        "invalid_face_review_request",
        "Face review requests need a 1–128 character ASCII request ID.",
    ))
    .await;
}

async fn run<F>(sink: Sink, db: Option<Writer>, request_id: String, action: &'static str, failure: &'static str, work: F)
where
    F: FnOnce(&Connection) -> rusqlite::Result<FaceReviewPage> + Send + 'static,
{
    if !valid_request_id(&request_id) {
        invalid(&sink).await;
        return;
    }
    let Some(db) = db else {
        sink.send(reply_event(failed(&request_id, action, "FileID couldn't open its library. Quit and reopen FileID."))).await;
        return;
    };
    let id = request_id.clone();
    let result = tokio::task::spawn_blocking(move || work(&db.lock())).await;
    let page = match result {
        Ok(Ok(page)) => page,
        Ok(Err(err)) => {
            tracing::warn!(?err, action, "face review failed");
            failed(&id, action, failure)
        }
        Err(err) => {
            tracing::warn!(?err, action, "face review task failed");
            failed(&id, action, failure)
        }
    };
    sink.send(reply_event(page)).await;
}

pub(crate) async fn handle_list(sink: Sink, db: Option<Writer>, payload: ListFaceReviewPayload) {
    let request_id = payload.request_id.clone();
    run(sink, db, request_id, "list", "The review list could not be read.", move |conn| list(conn, &payload)).await;
}

pub(crate) async fn handle_resolve(sink: Sink, db: Option<Writer>, payload: ResolveFaceReviewPayload) {
    let request_id = payload.request_id.clone();
    run(sink, db, request_id, "resolve", "The review could not be saved.", move |conn| resolve(conn, &payload)).await;
}

pub(crate) async fn handle_undo(sink: Sink, db: Option<Writer>, payload: UndoFaceReviewPayload) {
    let request_id = payload.request_id.clone();
    run(sink, db, request_id, "undo", "The review could not be saved.", move |conn| undo(conn, &payload)).await;
}

#[cfg(test)]
#[path = "face_review_tests.rs"]
mod tests;
