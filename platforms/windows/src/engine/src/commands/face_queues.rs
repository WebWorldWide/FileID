//! Durable merge and split suggestion queues for People. Mirror of the macOS
//! `FaceQueues.swift`; keep thresholds, ordering, messages and journal shapes
//! in step (cross-engine conformance reads both journals).

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};

use super::face_review::{
    dot, now, owner_of, page, page_bounds, recount, refused, round4, unit_vector, DISPLAY_P, NOTHING_TO_UNDO, NO_ANCHOR,
    STALE,
};
use crate::ipc::{FaceMergeSuggestion, FaceReviewPage, FaceSplitSuggestion, ListFaceReviewPayload, ResolveFaceReviewPayload};

const DISPLAY_O: &str = "COALESCE(NULLIF(TRIM(o.name),''), NULLIF(TRIM(COALESCE(o.first_name,'') || ' ' || COALESCE(o.last_name,'')),''))";
const SHARED_PHOTO: &str = "These two people share a photo, so they can't be one person.";
const MARKED_DIFFERENT: &str = "These two people were marked as different.";
const OTHER_REQUIRED: &str = "Choose the other person in this merge suggestion.";
const RENAMED: &str = "The new person was named since, so nothing changed.";
const MAX_MERGES: usize = 200;
const MAX_SPLITS: usize = 100;
const MAX_SPLIT_FACES: usize = 2000;
const PREVIEW: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct QueueSettings {
    pub merge_min: f64,
    pub split_max: f64,
    pub split_min_faces: usize,
    pub split_min_cohesion: f64,
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self { merge_min: 0.55, split_max: 0.45, split_min_faces: 6, split_min_cohesion: 0.5 }
    }
}

impl QueueSettings {
    pub(crate) fn from_env() -> Self {
        let read = |key: &str, default: f64| {
            std::env::var(key)
                .ok()
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite())
                .map_or(default, |value| value.clamp(-1.0, 1.0))
        };
        let defaults = Self::default();
        Self {
            merge_min: read("FILEID_FACE_MERGE_SUGGEST_COS", defaults.merge_min),
            split_max: read("FILEID_FACE_SPLIT_MAX_COS", defaults.split_max),
            split_min_faces: std::env::var("FILEID_FACE_SPLIT_MIN_FACES")
                .ok()
                .and_then(|value| value.trim().parse::<usize>().ok())
                .map_or(defaults.split_min_faces, |value| value.clamp(4, 1000)),
            split_min_cohesion: read("FILEID_FACE_SPLIT_MIN_COHESION", defaults.split_min_cohesion),
        }
    }
}

struct Member {
    face: i64,
    quality: f64,
    vector: Vec<f32>,
}

struct Person {
    id: i64,
    named: bool,
    faces: i64,
    representative: Option<i64>,
    members: Vec<Member>,
}

fn normalize(mut values: Vec<f32>) -> Option<Vec<f32>> {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::MIN_POSITIVE {
        return None;
    }
    for value in &mut values {
        *value /= norm;
    }
    Some(values)
}

fn centroid<'a>(vectors: impl IntoIterator<Item = &'a [f32]>) -> Option<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for vector in vectors {
        match sum.as_mut() {
            Some(total) if total.len() == vector.len() => {
                for (value, add) in total.iter_mut().zip(vector) {
                    *value += add;
                }
            }
            Some(_) => {}
            None => sum = Some(vector.to_vec()),
        }
    }
    normalize(sum?)
}

fn people(conn: &Connection, model: &str, processing: &str) -> rusqlite::Result<Vec<Person>> {
    let mut out: Vec<Person> = Vec::new();
    let mut statement = conn.prepare(&format!(
        "SELECT p.id, {DISPLAY_P} IS NOT NULL, (SELECT COUNT(*) FROM face_prints a WHERE a.person_id=p.id), p.representative_face_id, \
                fp.id, COALESCE(fp.face_quality,0), fp.arcface_embedding \
         FROM face_prints fp JOIN persons p ON p.id=fp.person_id \
         WHERE COALESCE(p.is_unknown,0)=0 AND COALESCE(fp.excluded,0)=0 AND length(fp.arcface_embedding)=512 \
           AND fp.embedding_model=?1 AND fp.processing_version=?2 ORDER BY p.id, fp.id"
    ))?;
    let mut rows = statement.query(params![model, processing])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let blob: Vec<u8> = row.get(6)?;
        let Some(vector) = unit_vector(&blob) else { continue };
        let member = Member { face: row.get(4)?, quality: row.get(5)?, vector };
        match out.last_mut() {
            Some(last) if last.id == id => last.members.push(member),
            _ => out.push(Person {
                id,
                named: row.get::<_, bool>(1)?,
                faces: row.get(2)?,
                representative: row.get(3)?,
                members: vec![member],
            }),
        }
    }
    Ok(out)
}

fn ordered(a: i64, b: i64) -> (i64, i64) {
    if a < b { (a, b) } else { (b, a) }
}

fn owner_at(conn: &Connection, file: Option<i64>, bbox: Option<&str>) -> rusqlite::Result<Option<i64>> {
    owner_of(conn, None, file, bbox)
}

/// Person pairs that must never be suggested: a different-people verdict, a
/// "not this person" constraint or a dismissal between faces they own now.
fn blocked_pairs(conn: &Connection) -> rusqlite::Result<HashSet<(i64, i64)>> {
    type Side = (Option<i64>, Option<i64>, Option<String>);
    let mut blocked = HashSet::new();
    let verdicts: Vec<(Side, Side)> = {
        let mut statement = conn.prepare(
            "SELECT face_a, file_a, bbox_a, face_b, file_b, bbox_b FROM face_verifications WHERE same_person=0",
        )?;
        statement
            .query_map([], |row| Ok(((row.get(0)?, row.get(1)?, row.get(2)?), (row.get(3)?, row.get(4)?, row.get(5)?))))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (a, b) in verdicts {
        let first = owner_of(conn, a.0, a.1, a.2.as_deref())?;
        let second = owner_of(conn, b.0, b.1, b.2.as_deref())?;
        if let (Some(x), Some(y)) = (first, second)
            && x != y
        {
            blocked.insert(ordered(x, y));
        }
    }
    let pairs: Vec<(i64, String, Option<i64>, Option<String>)> = {
        let mut statement = conn.prepare(
            "SELECT file_id, bbox, anchor_file_id, anchor_bbox FROM face_identity_constraints \
             UNION ALL SELECT file_id, bbox, other_file_id, other_bbox FROM face_queue_dismissals WHERE kind='merge'",
        )?;
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (file, bbox, other_file, other_bbox) in pairs {
        let first = owner_at(conn, Some(file), Some(&bbox))?;
        let second = owner_at(conn, other_file, other_bbox.as_deref())?;
        if let (Some(x), Some(y)) = (first, second)
            && x != y
        {
            blocked.insert(ordered(x, y));
        }
    }
    Ok(blocked)
}

fn files_by_person(conn: &Connection) -> rusqlite::Result<HashMap<i64, HashSet<i64>>> {
    let mut out: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut statement = conn.prepare("SELECT DISTINCT person_id, file_id FROM face_prints WHERE person_id IS NOT NULL")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        out.entry(row.get(0)?).or_default().insert(row.get(1)?);
    }
    Ok(out)
}

fn keeps_first(a: &Person, b: &Person) -> bool {
    (a.named, a.faces, -a.id) >= (b.named, b.faces, -b.id)
}

struct MergeRow {
    keep: i64,
    other: i64,
    score: f64,
}

fn merges(people: &[Person], blocked: &HashSet<(i64, i64)>, files: &HashMap<i64, HashSet<i64>>, settings: &QueueSettings) -> Vec<MergeRow> {
    let centroids: Vec<Option<Vec<f32>>> =
        people.iter().map(|person| centroid(person.members.iter().map(|member| member.vector.as_slice()))).collect();
    let empty = HashSet::new();
    let mut out = Vec::new();
    for i in 0..people.len() {
        let Some(a) = &centroids[i] else { continue };
        for j in (i + 1)..people.len() {
            let Some(b) = &centroids[j] else { continue };
            if a.len() != b.len() {
                continue;
            }
            let score = dot(a, b);
            if score < settings.merge_min || blocked.contains(&ordered(people[i].id, people[j].id)) {
                continue;
            }
            let left = files.get(&people[i].id).unwrap_or(&empty);
            let right = files.get(&people[j].id).unwrap_or(&empty);
            if !left.is_disjoint(right) {
                continue;
            }
            let (keep, other) =
                if keeps_first(&people[i], &people[j]) { (&people[i], &people[j]) } else { (&people[j], &people[i]) };
            out.push(MergeRow { keep: keep.id, other: other.id, score: round4(score) });
        }
    }
    out.sort_by(|x, y| y.score.total_cmp(&x.score).then(x.keep.cmp(&y.keep)).then(x.other.cmp(&y.other)));
    out.truncate(MAX_MERGES);
    out
}

pub(crate) struct SplitRow {
    pub person: i64,
    pub score: f64,
    pub keep: Vec<i64>,
    pub split: Vec<i64>,
}

/// Deterministic two-means over a person's unit vectors. Seeds are the face
/// farthest from the centroid and the face farthest from that one.
pub(crate) fn two_groups(vectors: &[&[f32]]) -> Option<(Vec<usize>, Vec<f32>, Vec<f32>)> {
    let whole = centroid(vectors.iter().copied())?;
    let farthest = |from: &[f32]| {
        let mut best: Option<(usize, f64)> = None;
        for (index, vector) in vectors.iter().enumerate() {
            let score = dot(vector, from);
            if best.is_none_or(|(_, low)| score < low) {
                best = Some((index, score));
            }
        }
        best.map(|(index, _)| index)
    };
    let a = farthest(&whole)?;
    let b = farthest(vectors[a])?;
    if a == b {
        return None;
    }
    let mut centers = (vectors[a].to_vec(), vectors[b].to_vec());
    let mut groups: Vec<usize> = vec![usize::MAX; vectors.len()];
    for _ in 0..20 {
        let next: Vec<usize> =
            vectors.iter().map(|vector| usize::from(dot(vector, &centers.0) < dot(vector, &centers.1))).collect();
        if next == groups {
            break;
        }
        groups = next;
        let first = centroid(vectors.iter().zip(&groups).filter(|(_, group)| **group == 0).map(|(vector, _)| *vector))?;
        let second = centroid(vectors.iter().zip(&groups).filter(|(_, group)| **group == 1).map(|(vector, _)| *vector))?;
        centers = (first, second);
    }
    Some((groups, centers.0, centers.1))
}

fn split_of(person: &Person, settings: &QueueSettings) -> Option<SplitRow> {
    let members: Vec<&Member> = person.members.iter().take(MAX_SPLIT_FACES).collect();
    let n = members.len();
    if n < settings.split_min_faces {
        return None;
    }
    let vectors: Vec<&[f32]> = members.iter().map(|member| member.vector.as_slice()).collect();
    let (groups, first, second) = two_groups(&vectors)?;
    let min_group = 3.max(n.div_ceil(10));
    let sizes = (groups.iter().filter(|group| **group == 0).count(), groups.iter().filter(|group| **group == 1).count());
    if sizes.0 < min_group || sizes.1 < min_group {
        return None;
    }
    let between = dot(&first, &second);
    if between > settings.split_max {
        return None;
    }
    let cohesion = |group: usize, center: &[f32]| {
        let scores: Vec<f64> =
            vectors.iter().zip(&groups).filter(|(_, g)| **g == group).map(|(vector, _)| dot(vector, center)).collect();
        scores.iter().sum::<f64>() / scores.len() as f64
    };
    if cohesion(0, &first).min(cohesion(1, &second)) < settings.split_min_cohesion {
        return None;
    }
    let mut parts: [Vec<&Member>; 2] = [Vec::new(), Vec::new()];
    for (member, group) in members.iter().zip(&groups) {
        parts[*group].push(member);
    }
    for part in &mut parts {
        part.sort_by(|a, b| b.quality.total_cmp(&a.quality).then(a.face.cmp(&b.face)));
    }
    let holds = |part: &Vec<&Member>| person.representative.is_some_and(|rep| part.iter().any(|member| member.face == rep));
    let keep_index = if holds(&parts[0]) {
        0
    } else if holds(&parts[1]) {
        1
    } else if parts[0].len() != parts[1].len() {
        usize::from(parts[1].len() > parts[0].len())
    } else {
        usize::from(parts[1].iter().map(|m| m.face).min() < parts[0].iter().map(|m| m.face).min())
    };
    let ids = |part: &Vec<&Member>| part.iter().map(|member| member.face).collect::<Vec<i64>>();
    Some(SplitRow { person: person.id, score: round4(between), keep: ids(&parts[keep_index]), split: ids(&parts[1 - keep_index]) })
}

fn resolve_face(conn: &Connection, legacy: Option<i64>, file: Option<i64>, bbox: Option<&str>) -> rusqlite::Result<Option<i64>> {
    if let (Some(file), Some(bbox)) = (file, bbox)
        && let Some(id) = conn
            .query_row("SELECT id FROM face_prints WHERE file_id=?1 AND bbox=?2 ORDER BY id LIMIT 1", params![file, bbox], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
    {
        return Ok(Some(id));
    }
    let Some(legacy) = legacy else { return Ok(None) };
    conn.query_row("SELECT id FROM face_prints WHERE id=?1", [legacy], |row| row.get(0)).optional()
}

/// Faces the user said belong together (a user-merged verdict) or a dismissed
/// split, so a person holding them is never offered for splitting.
fn split_blocks(conn: &Connection) -> rusqlite::Result<(Vec<(i64, i64)>, HashSet<i64>)> {
    type Side = (Option<i64>, Option<i64>, Option<String>);
    let rows: Vec<(Side, Side)> = {
        let mut statement = conn.prepare(
            "SELECT face_a, file_a, bbox_a, face_b, file_b, bbox_b FROM face_verifications WHERE same_person=1",
        )?;
        statement
            .query_map([], |row| Ok(((row.get(0)?, row.get(1)?, row.get(2)?), (row.get(3)?, row.get(4)?, row.get(5)?))))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut linked = Vec::new();
    for (a, b) in rows {
        if let (Some(x), Some(y)) =
            (resolve_face(conn, a.0, a.1, a.2.as_deref())?, resolve_face(conn, b.0, b.1, b.2.as_deref())?)
        {
            linked.push((x, y));
        }
    }
    let anchors: Vec<(i64, String)> = {
        let mut statement = conn.prepare("SELECT file_id, bbox FROM face_queue_dismissals WHERE kind='split'")?;
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let mut dismissed = HashSet::new();
    for (file, bbox) in anchors {
        if let Some(owner) = owner_at(conn, Some(file), Some(&bbox))? {
            dismissed.insert(owner);
        }
    }
    Ok((linked, dismissed))
}

/// Rebuilds both queues for one face space. Called by runFaceClustering after
/// the review assessment, inside its transaction.
pub(crate) fn refresh(conn: &Connection, model: &str, processing: &str, settings: &QueueSettings, at: f64) -> rusqlite::Result<(usize, usize)> {
    let namespace = format!("{model}|{processing}");
    let created: HashMap<(i64, i64), f64> = {
        let mut statement = conn.prepare("SELECT person_id, other_person_id, created_at FROM face_merge_suggestions")?;
        statement.query_map([], |row| Ok(((row.get(0)?, row.get(1)?), row.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let split_created: HashMap<i64, f64> = {
        let mut statement = conn.prepare("SELECT person_id, created_at FROM face_split_suggestions")?;
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    conn.execute("DELETE FROM face_merge_suggestions", [])?;
    conn.execute("DELETE FROM face_split_suggestions", [])?;
    let people = people(conn, model, processing)?;
    let rows = merges(&people, &blocked_pairs(conn)?, &files_by_person(conn)?, settings);
    for row in &rows {
        conn.execute(
            "INSERT INTO face_merge_suggestions(person_id,other_person_id,score,namespace,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![row.keep, row.other, row.score, namespace, created.get(&(row.keep, row.other)).copied().unwrap_or(at)],
        )?;
    }
    let (linked, dismissed) = split_blocks(conn)?;
    let mut splits: Vec<SplitRow> = people
        .iter()
        .filter(|person| !dismissed.contains(&person.id))
        .filter_map(|person| split_of(person, settings))
        .filter(|row| {
            let keep: HashSet<i64> = row.keep.iter().copied().collect();
            let split: HashSet<i64> = row.split.iter().copied().collect();
            !linked.iter().any(|(a, b)| (keep.contains(a) && split.contains(b)) || (keep.contains(b) && split.contains(a)))
        })
        .collect();
    splits.sort_by(|x, y| x.score.total_cmp(&y.score).then(x.person.cmp(&y.person)));
    splits.truncate(MAX_SPLITS);
    for row in &splits {
        conn.execute(
            "INSERT INTO face_split_suggestions(person_id,score,keep_json,split_json,namespace,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                row.person,
                row.score,
                serde_json::to_string(&row.keep).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&row.split).unwrap_or_else(|_| "[]".into()),
                namespace,
                split_created.get(&row.person).copied().unwrap_or(at)
            ],
        )?;
    }
    Ok((rows.len(), splits.len()))
}

const MERGE_FROM: &str = "FROM face_merge_suggestions s JOIN persons p ON p.id=s.person_id JOIN persons o ON o.id=s.other_person_id \
     WHERE COALESCE(p.is_unknown,0)=0 AND COALESCE(o.is_unknown,0)=0 AND (?1 IS NULL OR s.person_id=?1 OR s.other_person_id=?1)";
const SPLIT_FROM: &str =
    "FROM face_split_suggestions s JOIN persons p ON p.id=s.person_id WHERE COALESCE(p.is_unknown,0)=0 AND (?1 IS NULL OR s.person_id=?1)";

pub(crate) fn merge_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(&format!("SELECT COUNT(*) {MERGE_FROM}"), [None::<i64>], |row| row.get(0))
}

pub(crate) fn split_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(&format!("SELECT COUNT(*) {SPLIT_FROM}"), [None::<i64>], |row| row.get(0))
}

fn queue_page(request: &str, action: &str, status: &str, queue: &str) -> FaceReviewPage {
    FaceReviewPage { queue: Some(queue.to_string()), ..page(request, action, status) }
}

fn queue_refused(request: &str, action: &str, queue: &str, message: &str) -> FaceReviewPage {
    FaceReviewPage { queue: Some(queue.to_string()), ..refused(request, action, message) }
}

pub(crate) fn list_merges(conn: &Connection, payload: &ListFaceReviewPayload) -> rusqlite::Result<FaceReviewPage> {
    let (offset, limit) = match page_bounds(payload) {
        Ok(bounds) => bounds,
        Err(reply) => return Ok(FaceReviewPage { queue: Some("merges".into()), ..reply }),
    };
    let total: i64 = conn.query_row(&format!("SELECT COUNT(*) {MERGE_FROM}"), [payload.person_id], |row| row.get(0))?;
    let mut statement = conn.prepare(&format!(
        "SELECT s.person_id, {DISPLAY_P}, p.representative_face_id, (SELECT COUNT(*) FROM face_prints a WHERE a.person_id=s.person_id), \
                s.other_person_id, {DISPLAY_O}, o.representative_face_id, (SELECT COUNT(*) FROM face_prints a WHERE a.person_id=s.other_person_id), \
                s.score, s.created_at {MERGE_FROM} ORDER BY s.score DESC, s.person_id, s.other_person_id LIMIT ?2 OFFSET ?3"
    ))?;
    let merges = statement
        .query_map(params![payload.person_id, limit, offset], |row| {
            Ok(FaceMergeSuggestion {
                person_id: row.get(0)?,
                person_name: row.get(1)?,
                representative_face_id: row.get(2)?,
                face_count: row.get(3)?,
                other_person_id: row.get(4)?,
                other_name: row.get(5)?,
                other_representative_face_id: row.get(6)?,
                other_face_count: row.get(7)?,
                score: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(FaceReviewPage { total, offset, merges: Some(merges), ..queue_page(&payload.request_id, "list", "ok", "merges") })
}

fn ids(json: &str) -> Vec<i64> {
    serde_json::from_str(json).unwrap_or_default()
}

pub(crate) fn list_splits(conn: &Connection, payload: &ListFaceReviewPayload) -> rusqlite::Result<FaceReviewPage> {
    let (offset, limit) = match page_bounds(payload) {
        Ok(bounds) => bounds,
        Err(reply) => return Ok(FaceReviewPage { queue: Some("splits".into()), ..reply }),
    };
    let total: i64 = conn.query_row(&format!("SELECT COUNT(*) {SPLIT_FROM}"), [payload.person_id], |row| row.get(0))?;
    let mut statement = conn.prepare(&format!(
        "SELECT s.person_id, {DISPLAY_P}, p.representative_face_id, s.score, s.keep_json, s.split_json, s.created_at \
         {SPLIT_FROM} ORDER BY s.score ASC, s.person_id LIMIT ?2 OFFSET ?3"
    ))?;
    let splits = statement
        .query_map(params![payload.person_id, limit, offset], |row| {
            let keep = ids(&row.get::<_, String>(4)?);
            let split = ids(&row.get::<_, String>(5)?);
            Ok(FaceSplitSuggestion {
                person_id: row.get(0)?,
                person_name: row.get(1)?,
                representative_face_id: row.get(2)?,
                score: row.get(3)?,
                keep_count: keep.len() as i64,
                split_count: split.len() as i64,
                keep_face_ids: keep.into_iter().take(PREVIEW).collect(),
                split_face_ids: split.into_iter().take(PREVIEW).collect(),
                created_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(FaceReviewPage { total, offset, splits: Some(splits), ..queue_page(&payload.request_id, "list", "ok", "splits") })
}

struct Anchor {
    face: i64,
    file: i64,
    bbox: String,
}

fn anchor(conn: &Connection, person: i64) -> rusqlite::Result<Option<Anchor>> {
    conn.query_row(
        "SELECT fp.id, fp.file_id, fp.bbox FROM face_prints fp WHERE fp.person_id=?1 \
         ORDER BY (fp.id=(SELECT representative_face_id FROM persons WHERE id=?1)) DESC, (fp.arcface_embedding IS NOT NULL) DESC, \
                  COALESCE(fp.face_quality,0) DESC, fp.id ASC LIMIT 1",
        [person],
        |row| Ok(Anchor { face: row.get(0)?, file: row.get(1)?, bbox: row.get(2)? }),
    )
    .optional()
}

fn verification(conn: &Connection, pair: (i64, i64)) -> rusqlite::Result<serde_json::Value> {
    Ok(conn
        .query_row(
            "SELECT same_person, confidence, vlm_model, verified_at, face_a, face_b, file_a, bbox_a, file_b, bbox_b \
             FROM face_verifications WHERE person_a=?1 AND person_b=?2",
            params![pair.0, pair.1],
            |row| {
                Ok(serde_json::json!({
                    "samePerson": row.get::<_, i64>(0)?,
                    "confidence": row.get::<_, f64>(1)?,
                    "model": row.get::<_, String>(2)?,
                    "verifiedAt": row.get::<_, f64>(3)?,
                    "faceA": row.get::<_, Option<i64>>(4)?,
                    "faceB": row.get::<_, Option<i64>>(5)?,
                    "fileA": row.get::<_, Option<i64>>(6)?,
                    "bboxA": row.get::<_, Option<String>>(7)?,
                    "fileB": row.get::<_, Option<i64>>(8)?,
                    "bboxB": row.get::<_, Option<String>>(9)?,
                }))
            },
        )
        .optional()?
        .unwrap_or(serde_json::Value::Null))
}

fn restore_verification(conn: &Connection, pair: (i64, i64), same: i64, model: &str, previous: &serde_json::Value) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM face_verifications WHERE person_a=?1 AND person_b=?2 AND same_person=?3 AND vlm_model=?4",
        params![pair.0, pair.1, same, model],
    )?;
    if previous.is_object() {
        conn.execute(
            "INSERT OR REPLACE INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,face_a,face_b,file_a,bbox_a,file_b,bbox_b) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                pair.0,
                pair.1,
                previous["samePerson"].as_i64().unwrap_or(0),
                previous["confidence"].as_f64().unwrap_or(1.0),
                previous["model"].as_str().unwrap_or_default(),
                previous["verifiedAt"].as_f64().unwrap_or(0.0),
                previous["faceA"].as_i64(),
                previous["faceB"].as_i64(),
                previous["fileA"].as_i64(),
                previous["bboxA"].as_str(),
                previous["fileB"].as_i64(),
                previous["bboxB"].as_str()
            ],
        )?;
    }
    Ok(())
}

/// Writes a verdict between two people's anchor faces the way markPersonsDifferent
/// and mergeClusters store theirs: persons ordered, faces ordered by face ID.
fn write_verification(conn: &Connection, people: (i64, i64), anchors: (&Anchor, &Anchor), same: i64, model: &str, at: f64) -> rusqlite::Result<()> {
    let pair = ordered(people.0, people.1);
    let (first, second) = if same == 1 {
        if people.0 < people.1 { anchors } else { (anchors.1, anchors.0) }
    } else if anchors.0.face <= anchors.1.face {
        anchors
    } else {
        (anchors.1, anchors.0)
    };
    conn.execute(
        "INSERT OR REPLACE INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,face_a,face_b,file_a,bbox_a,file_b,bbox_b) \
         VALUES(?1,?2,?3,1.0,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![pair.0, pair.1, same, model, at, first.face, second.face, first.file, first.bbox, second.file, second.bbox],
    )?;
    Ok(())
}

fn journal(conn: &Connection, kind: &str, file: Option<i64>, before: &serde_json::Value, after: &serde_json::Value, at: f64) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let before = before.to_string();
    let plan = serde_json::json!({"type": kind, "resolution": after["resolution"], "personID": after["personID"]}).to_string();
    conn.execute(
        "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
        params![id, file, kind, before, after.to_string(), at],
    )?;
    conn.execute(
        "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?1,?2,?3,'completed',?4)",
        params![id, plan, before, at],
    )?;
    Ok(id)
}

const NAME_FIELDS: [&str; 6] = ["name", "title", "first_name", "middle_name", "last_name", "suffix"];

fn person_json(conn: &Connection, person: i64) -> rusqlite::Result<Option<serde_json::Value>> {
    conn.query_row(
        "SELECT name,title,first_name,middle_name,last_name,suffix,COALESCE(is_unknown,0),created_at,representative_face_id,anchor_radius,last_clustered_at \
         FROM persons WHERE id=?1",
        [person],
        |row| {
            let mut value = serde_json::Map::new();
            for (index, field) in NAME_FIELDS.iter().enumerate() {
                value.insert((*field).into(), serde_json::json!(row.get::<_, Option<String>>(index)?));
            }
            value.insert("is_unknown".into(), serde_json::json!(row.get::<_, i64>(6)?));
            value.insert("created_at".into(), serde_json::json!(row.get::<_, f64>(7)?));
            value.insert("representative_face_id".into(), serde_json::json!(row.get::<_, Option<i64>>(8)?));
            value.insert("anchor_radius".into(), serde_json::json!(row.get::<_, Option<f64>>(9)?));
            value.insert("last_clustered_at".into(), serde_json::json!(row.get::<_, Option<f64>>(10)?));
            Ok(serde_json::Value::Object(value))
        },
    )
    .optional()
}

fn names(value: &serde_json::Value) -> Vec<Option<String>> {
    NAME_FIELDS.iter().map(|field| value[*field].as_str().map(str::to_string)).collect()
}

fn blank(fields: &[Option<String>]) -> bool {
    fields.iter().all(|field| field.as_deref().is_none_or(|text| text.trim().is_empty()))
}

fn shares_photo(conn: &Connection, a: i64, b: i64) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM face_prints x JOIN face_prints y ON y.file_id=x.file_id WHERE x.person_id=?1 AND y.person_id=?2)",
        params![a, b],
        |row| row.get(0),
    )
}

pub(crate) fn resolve_in(conn: &Connection, payload: &ResolveFaceReviewPayload) -> rusqlite::Result<FaceReviewPage> {
    let request = payload.request_id.as_str();
    let resolution = payload.resolution.as_str();
    match (resolution, payload.other_person_id) {
        ("split", _) => split_in(conn, request, payload.person_id),
        ("dismiss", None) => dismiss_split_in(conn, request, payload.person_id),
        ("merge" | "notSamePerson" | "dismiss", Some(other)) => merge_in(conn, request, resolution, payload.person_id, other),
        _ => Ok(queue_refused(request, "resolve", "merges", OTHER_REQUIRED)),
    }
}

fn merge_in(conn: &Connection, request: &str, resolution: &str, keep: i64, other: i64) -> rusqlite::Result<FaceReviewPage> {
    let refuse = |message: &str| Ok(queue_refused(request, "resolve", "merges", message));
    let suggestion: Option<(f64, String, f64)> = conn
        .query_row(
            &format!("SELECT s.score, s.namespace, s.created_at {MERGE_FROM} AND s.person_id=?2 AND s.other_person_id=?3"),
            params![None::<i64>, keep, other],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((score, namespace, created)) = suggestion else { return refuse(STALE) };
    let (Some(keep_anchor), Some(other_anchor)) = (anchor(conn, keep)?, anchor(conn, other)?) else { return refuse(NO_ANCHOR) };
    let pair = ordered(keep, other);
    let at = now();
    let suggestion = serde_json::json!({"score": score, "namespace": namespace, "createdAt": created});
    let mut before = serde_json::json!({"resolution": resolution, "personID": keep, "otherPersonID": other, "suggestion": suggestion});
    let mut after = serde_json::json!({"resolution": resolution, "personID": keep, "otherPersonID": other});
    match resolution {
        "merge" => {
            if shares_photo(conn, keep, other)? {
                return refuse(SHARED_PHOTO);
            }
            if blocked_pairs(conn)?.contains(&pair) {
                return refuse(MARKED_DIFFERENT);
            }
            let Some(other_row) = person_json(conn, other)? else { return refuse(STALE) };
            let Some(keep_row) = person_json(conn, keep)? else { return refuse(STALE) };
            let faces: Vec<i64> = {
                let mut statement = conn.prepare("SELECT id FROM face_prints WHERE person_id=?1 ORDER BY id")?;
                statement.query_map([other], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            before["verification"] = verification(conn, pair)?;
            before["other"] = other_row;
            before["keepNames"] = serde_json::json!(names(&keep_row));
            write_verification(conn, (keep, other), (&keep_anchor, &other_anchor), 1, "user-merged", at)?;
            conn.execute("UPDATE face_prints SET person_id=?1 WHERE person_id=?2", params![keep, other])?;
            conn.execute(
                "UPDATE persons SET name=(SELECT name FROM persons WHERE id=?2), title=(SELECT title FROM persons WHERE id=?2), first_name=(SELECT first_name FROM persons WHERE id=?2), middle_name=(SELECT middle_name FROM persons WHERE id=?2), last_name=(SELECT last_name FROM persons WHERE id=?2), suffix=(SELECT suffix FROM persons WHERE id=?2), is_unknown=0 WHERE id=?1 AND length(trim(COALESCE(name,'') || COALESCE(title,'') || COALESCE(first_name,'') || COALESCE(middle_name,'') || COALESCE(last_name,'') || COALESCE(suffix,'')))=0 AND EXISTS(SELECT 1 FROM persons WHERE id=?2 AND length(trim(COALESCE(name,'') || COALESCE(title,'') || COALESCE(first_name,'') || COALESCE(middle_name,'') || COALESCE(last_name,'') || COALESCE(suffix,'')))>0)",
                params![keep, other],
            )?;
            conn.execute("DELETE FROM persons WHERE id=?1", [other])?;
            conn.execute(
                "UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?1), representative_face_id=COALESCE( \
                 (SELECT fp.id FROM face_prints fp WHERE fp.person_id=?1 AND fp.arcface_embedding IS NOT NULL ORDER BY COALESCE(fp.face_quality,0) DESC, fp.id LIMIT 1), \
                 (SELECT fp.id FROM face_prints fp WHERE fp.person_id=?1 ORDER BY COALESCE(fp.face_quality,0) DESC, fp.id LIMIT 1), representative_face_id) WHERE id=?1",
                [keep],
            )?;
            after["faceIDs"] = serde_json::json!(faces);
        }
        "notSamePerson" => {
            before["verification"] = verification(conn, pair)?;
            write_verification(conn, (keep, other), (&keep_anchor, &other_anchor), 0, "user-verified", at)?;
        }
        _ => {
            let created = conn.execute(
                "INSERT OR IGNORE INTO face_queue_dismissals(kind,file_id,bbox,other_file_id,other_bbox,created_at) VALUES('merge',?1,?2,?3,?4,?5)",
                params![keep_anchor.file, keep_anchor.bbox, other_anchor.file, other_anchor.bbox, at],
            )? > 0;
            let id: i64 = conn.query_row(
                "SELECT id FROM face_queue_dismissals WHERE kind='merge' AND file_id=?1 AND bbox=?2 AND other_file_id=?3 AND other_bbox=?4",
                params![keep_anchor.file, keep_anchor.bbox, other_anchor.file, other_anchor.bbox],
                |row| row.get(0),
            )?;
            after["dismissalID"] = serde_json::json!(id);
            after["dismissalCreated"] = serde_json::json!(created);
        }
    }
    conn.execute("DELETE FROM face_merge_suggestions WHERE person_id=?1 AND other_person_id=?2", params![keep, other])?;
    let id = journal(conn, "face_merge_review", Some(keep_anchor.file), &before, &after, at)?;
    Ok(FaceReviewPage {
        operation_id: Some(id),
        person_id: Some(keep),
        other_person_id: Some(other),
        total: merge_count(conn)?,
        ..queue_page(request, "resolve", "ok", "merges")
    })
}

fn split_row(conn: &Connection, person: i64) -> rusqlite::Result<Option<(f64, Vec<i64>, Vec<i64>, String, f64)>> {
    conn.query_row(
        &format!("SELECT s.score, s.keep_json, s.split_json, s.namespace, s.created_at {SPLIT_FROM}"),
        [Some(person)],
        |row| Ok((row.get(0)?, ids(&row.get::<_, String>(1)?), ids(&row.get::<_, String>(2)?), row.get(3)?, row.get(4)?)),
    )
    .optional()
}

fn owned_by(conn: &Connection, faces: &[i64], person: i64) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM face_prints WHERE person_id=?1 AND id IN (SELECT value FROM json_each(?2))",
        params![person, serde_json::to_string(faces).unwrap_or_else(|_| "[]".into())],
        |row| row.get(0),
    )?;
    Ok(count == faces.len() as i64)
}

fn face_anchor(conn: &Connection, face: i64) -> rusqlite::Result<Option<Anchor>> {
    conn.query_row("SELECT id, file_id, bbox FROM face_prints WHERE id=?1", [face], |row| {
        Ok(Anchor { face: row.get(0)?, file: row.get(1)?, bbox: row.get(2)? })
    })
    .optional()
}

fn split_in(conn: &Connection, request: &str, person: i64) -> rusqlite::Result<FaceReviewPage> {
    let refuse = |message: &str| Ok(queue_refused(request, "resolve", "splits", message));
    let Some((score, keep, split, namespace, created)) = split_row(conn, person)? else { return refuse(STALE) };
    if keep.is_empty() || split.is_empty() || !owned_by(conn, &keep, person)? || !owned_by(conn, &split, person)? {
        return refuse(STALE);
    }
    let representative: Option<i64> =
        conn.query_row("SELECT representative_face_id FROM persons WHERE id=?1", [person], |row| row.get(0))?;
    let keep_face = representative.filter(|rep| keep.contains(rep)).unwrap_or(keep[0]);
    let (Some(keep_anchor), Some(split_anchor)) = (face_anchor(conn, keep_face)?, face_anchor(conn, split[0])?) else {
        return refuse(STALE);
    };
    let at = now();
    conn.execute(
        "INSERT INTO persons(name,is_unknown,created_at,representative_face_id,file_count) VALUES(NULL,0,?1,?2,0)",
        params![at, split[0]],
    )?;
    let created_person = conn.last_insert_rowid();
    let pair = ordered(person, created_person);
    let previous = verification(conn, pair)?;
    conn.execute(
        "UPDATE face_prints SET person_id=?1 WHERE person_id=?2 AND id IN (SELECT value FROM json_each(?3))",
        params![created_person, person, serde_json::to_string(&split).unwrap_or_else(|_| "[]".into())],
    )?;
    recount(conn, person)?;
    recount(conn, created_person)?;
    write_verification(conn, (person, created_person), (&keep_anchor, &split_anchor), 0, "user-verified", at)?;
    conn.execute("DELETE FROM face_split_suggestions WHERE person_id=?1", [person])?;
    let before = serde_json::json!({
        "resolution": "split",
        "personID": person,
        "suggestion": {"score": score, "keep": keep, "split": split, "namespace": namespace, "createdAt": created},
        "verification": previous,
    });
    let after = serde_json::json!({"resolution": "split", "personID": person, "newPersonID": created_person, "faceIDs": split});
    let id = journal(conn, "face_split_review", Some(keep_anchor.file), &before, &after, at)?;
    Ok(FaceReviewPage {
        operation_id: Some(id),
        person_id: Some(person),
        other_person_id: Some(created_person),
        total: split_count(conn)?,
        ..queue_page(request, "resolve", "ok", "splits")
    })
}

fn dismiss_split_in(conn: &Connection, request: &str, person: i64) -> rusqlite::Result<FaceReviewPage> {
    let refuse = |message: &str| Ok(queue_refused(request, "resolve", "splits", message));
    let Some((score, keep, split, namespace, created)) = split_row(conn, person)? else { return refuse(STALE) };
    let Some(anchor) = anchor(conn, person)? else { return refuse(NO_ANCHOR) };
    let at = now();
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO face_queue_dismissals(kind,file_id,bbox,other_file_id,other_bbox,created_at) VALUES('split',?1,?2,NULL,NULL,?3)",
        params![anchor.file, anchor.bbox, at],
    )? > 0;
    let dismissal: i64 = conn.query_row(
        "SELECT id FROM face_queue_dismissals WHERE kind='split' AND file_id=?1 AND bbox=?2 AND other_file_id IS NULL",
        params![anchor.file, anchor.bbox],
        |row| row.get(0),
    )?;
    conn.execute("DELETE FROM face_split_suggestions WHERE person_id=?1", [person])?;
    let before = serde_json::json!({
        "resolution": "dismiss",
        "personID": person,
        "suggestion": {"score": score, "keep": keep, "split": split, "namespace": namespace, "createdAt": created},
    });
    let after = serde_json::json!({"resolution": "dismiss", "personID": person, "dismissalID": dismissal, "dismissalCreated": inserted});
    let id = journal(conn, "face_split_review", Some(anchor.file), &before, &after, at)?;
    Ok(FaceReviewPage {
        operation_id: Some(id),
        person_id: Some(person),
        total: split_count(conn)?,
        ..queue_page(request, "resolve", "ok", "splits")
    })
}

fn person_exists(conn: &Connection, person: i64) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM persons WHERE id=?1)", [person], |row| row.get(0))
}

fn restore_merge_row(conn: &Connection, keep: i64, other: i64, suggestion: &serde_json::Value) -> rusqlite::Result<()> {
    if person_exists(conn, keep)? && person_exists(conn, other)? {
        conn.execute(
            "INSERT OR REPLACE INTO face_merge_suggestions(person_id,other_person_id,score,namespace,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![
                keep,
                other,
                suggestion["score"].as_f64().unwrap_or(0.0),
                suggestion["namespace"].as_str().unwrap_or_default(),
                suggestion["createdAt"].as_f64().unwrap_or_else(now)
            ],
        )?;
    }
    Ok(())
}

fn restore_split_row(conn: &Connection, person: i64, suggestion: &serde_json::Value) -> rusqlite::Result<()> {
    if person_exists(conn, person)? {
        conn.execute(
            "INSERT OR REPLACE INTO face_split_suggestions(person_id,score,keep_json,split_json,namespace,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                person,
                suggestion["score"].as_f64().unwrap_or(0.0),
                suggestion["keep"].to_string(),
                suggestion["split"].to_string(),
                suggestion["namespace"].as_str().unwrap_or_default(),
                suggestion["createdAt"].as_f64().unwrap_or_else(now)
            ],
        )?;
    }
    Ok(())
}

fn id_list(value: &serde_json::Value) -> Vec<i64> {
    value.as_array().into_iter().flatten().filter_map(serde_json::Value::as_i64).collect()
}

pub(crate) fn undo_in(conn: &Connection, request: &str, before: &serde_json::Value, after: &serde_json::Value) -> rusqlite::Result<FaceReviewPage> {
    let refuse = |queue: &str, message: &str| Ok(queue_refused(request, "undo", queue, message));
    let Some(person) = after["personID"].as_i64() else { return refuse("merges", NOTHING_TO_UNDO) };
    let resolution = after["resolution"].as_str().unwrap_or_default();
    if let Some(other) = after["otherPersonID"].as_i64() {
        let pair = ordered(person, other);
        match resolution {
            "merge" => {
                let faces = id_list(&after["faceIDs"]);
                if person_exists(conn, other)? || !owned_by(conn, &faces, person)? {
                    return refuse("merges", super::face_review::MOVED_MANY);
                }
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
                        row["created_at"].as_f64().unwrap_or_else(now),
                        row["representative_face_id"].as_i64(),
                        row["anchor_radius"].as_f64(),
                        row["last_clustered_at"].as_f64()
                    ],
                )?;
                conn.execute(
                    "UPDATE face_prints SET person_id=?1 WHERE person_id=?2 AND id IN (SELECT value FROM json_each(?3))",
                    params![other, person, serde_json::to_string(&faces).unwrap_or_else(|_| "[]".into())],
                )?;
                let kept: Vec<Option<String>> =
                    before["keepNames"].as_array().into_iter().flatten().map(|v| v.as_str().map(str::to_string)).collect();
                if blank(&kept)
                    && let Some(current) = person_json(conn, person)?
                    && names(&current) == names(row)
                {
                    conn.execute(
                        "UPDATE persons SET name=NULL,title=NULL,first_name=NULL,middle_name=NULL,last_name=NULL,suffix=NULL WHERE id=?1",
                        [person],
                    )?;
                }
                restore_verification(conn, pair, 1, "user-merged", &before["verification"])?;
                recount(conn, person)?;
                recount(conn, other)?;
            }
            "notSamePerson" => restore_verification(conn, pair, 0, "user-verified", &before["verification"])?,
            "dismiss" => {
                if after["dismissalCreated"].as_bool() == Some(true)
                    && let Some(id) = after["dismissalID"].as_i64()
                {
                    conn.execute("DELETE FROM face_queue_dismissals WHERE id=?1", [id])?;
                }
            }
            _ => return refuse("merges", NOTHING_TO_UNDO),
        }
        restore_merge_row(conn, person, other, &before["suggestion"])?;
        return Ok(FaceReviewPage {
            person_id: Some(person),
            other_person_id: Some(other),
            total: merge_count(conn)?,
            ..queue_page(request, "undo", "ok", "merges")
        });
    }
    match resolution {
        "split" => {
            let Some(created) = after["newPersonID"].as_i64() else { return refuse("splits", NOTHING_TO_UNDO) };
            let faces = id_list(&after["faceIDs"]);
            if !person_exists(conn, person)? || !person_exists(conn, created)? || !owned_by(conn, &faces, created)? {
                return refuse("splits", super::face_review::MOVED_MANY);
            }
            let named = person_json(conn, created)?.is_some_and(|row| !blank(&names(&row)));
            if named {
                return refuse("splits", RENAMED);
            }
            conn.execute(
                "UPDATE face_prints SET person_id=?1 WHERE person_id=?2 AND id IN (SELECT value FROM json_each(?3))",
                params![person, created, serde_json::to_string(&faces).unwrap_or_else(|_| "[]".into())],
            )?;
            restore_verification(conn, ordered(person, created), 0, "user-verified", &before["verification"])?;
            conn.execute("DELETE FROM persons WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM face_prints WHERE person_id=?1)", [created])?;
            recount(conn, person)?;
        }
        "dismiss" => {
            if after["dismissalCreated"].as_bool() == Some(true)
                && let Some(id) = after["dismissalID"].as_i64()
            {
                conn.execute("DELETE FROM face_queue_dismissals WHERE id=?1", [id])?;
            }
        }
        _ => return refuse("splits", NOTHING_TO_UNDO),
    }
    restore_split_row(conn, person, &before["suggestion"])?;
    Ok(FaceReviewPage { person_id: Some(person), total: split_count(conn)?, ..queue_page(request, "undo", "ok", "splits") })
}

#[cfg(test)]
#[path = "face_queues_tests.rs"]
mod tests;
