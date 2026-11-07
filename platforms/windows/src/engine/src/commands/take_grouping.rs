//! Library-wide related-take proposals: a durable `takeGrouping` catalog job
//! that groups tries by capture session, CLIP similarity and user-named
//! people, and stores them as suggestions. Mirror of the macOS
//! `CatalogTakeGrouping.swift`; keep thresholds, keys and evidence in step.

use super::catalog_take_auto::{self as auto, apply_decisions, rank};
use crate::ipc::sink::Sink;
use crate::ipc::{CatalogJob, CatalogRequest, CatalogResponse, CatalogTakeAutoGroup, EventPayload, IpcEvent, Wrap};
use crate::pipeline::near_duplicate_grouping::Item;
use crate::pipeline::take_scoring::round4;
use crate::util::smart_name_style as style;
use anyhow::{bail, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const JOB_ID: &str = "take-grouping";
pub const JOB_KIND: &str = "takeGrouping";
const VERSION: &str = "related-takes-v1";
const MODEL: &str = "mobileclip_s2";
const DIMENSION: usize = 512;
const SESSION_GAP: f64 = 1800.0;
const MAX_SESSION_FILES: usize = 2000;
const MAX_ACTIVE: usize = 32;
const MAX_MEMBERS: usize = 100;

struct Limits {
    max_gap: f64,
    max_span: f64,
    similarity: f32,
    subject_similarity: f32,
    simultaneous: f64,
}

const PHOTOS: Limits =
    Limits { max_gap: 120.0, max_span: 1200.0, similarity: 0.90, subject_similarity: 0.86, simultaneous: 2.0 };
const VIDEOS: Limits =
    Limits { max_gap: 1200.0, max_span: 7200.0, similarity: 0.88, subject_similarity: 0.84, simultaneous: 30.0 };

#[derive(Clone)]
struct File {
    id: i64,
    path: String,
    time: f64,
    camera: Option<String>,
    fingerprint: String,
    subjects: BTreeSet<i64>,
}

#[derive(Clone)]
struct Session {
    key: String,
    media: &'static str,
    files: Vec<File>,
}

struct Plan {
    dirty: Vec<Session>,
    keys: Vec<String>,
}

struct Proposal {
    key: String,
    relation: &'static str,
    file_ids: Vec<i64>,
    evidence: Vec<&'static str>,
    similarity: f64,
    captured_at: f64,
}

#[derive(Debug)]
struct Interrupted;

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Related-take grouping stopped")
    }
}

impl std::error::Error for Interrupted {}

fn now() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}

pub fn recover(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE catalog_jobs SET state='paused',error='Interrupted. Resume to continue; finished capture sessions are kept.',updated_at=?1 WHERE kind=?2 AND state IN ('running','queued')",
        params![now(), JOB_KIND],
    )?;
    Ok(())
}

pub(crate) fn is_job(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT kind FROM catalog_jobs WHERE id=?1", [id], |r| r.get::<_, String>(0))
        .optional()?
        .is_some_and(|kind| kind == JOB_KIND))
}

pub(crate) fn jobs(conn: &Connection) -> Result<Vec<CatalogJob>> {
    Ok(super::catalog::jobs(conn)?.into_iter().filter(|job| job.kind == JOB_KIND).collect())
}

pub(crate) fn enqueue(conn: &Connection, request: &CatalogRequest) -> Result<CatalogResponse> {
    let stamp = now();
    let recipe = serde_json::json!({"version": VERSION, "model": MODEL}).to_string();
    conn.execute(
        "INSERT OR IGNORE INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,priority,created_at,updated_at) VALUES(?1,?2,'[]',?3,'queued',0,?4,?4)",
        params![JOB_ID, JOB_KIND, recipe, stamp],
    )?;
    conn.execute(
        "UPDATE catalog_jobs SET state='queued',progress=0,error=NULL,checkpoint_json='{}',recipe_json=?1,updated_at=?2 WHERE id=?3 AND kind=?4 AND state IN ('completed','cancelled','failed','paused')",
        params![recipe, stamp, JOB_ID, JOB_KIND],
    )?;
    let mut response = super::catalog::response(
        &request.request_id,
        "ok",
        Some("Looking for related tries across the library. Suggestions never change, move or hide files.".into()),
    );
    response.jobs = jobs(conn)?;
    Ok(response)
}

pub(crate) fn proposals(conn: &Connection, request: &CatalogRequest) -> Result<CatalogResponse> {
    let limit = request.limit.unwrap_or(100);
    if !(1..=100).contains(&limit) {
        bail!("Invalid take group limit")
    }
    let grouped: HashSet<i64> = ids(conn, "SELECT file_id FROM catalog_event_files UNION SELECT derived_id FROM catalog_assets")?;
    let mut statement = conn.prepare(
        "SELECT group_key,media,relation,file_ids_json,evidence_json,similarity,captured_at FROM take_proposals ORDER BY group_key",
    )?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, f64>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut file = conn.prepare_cached("SELECT path_text,created_at FROM files WHERE id=?1 AND failed=0")?;
    let mut groups = Vec::new();
    for (key, media, relation, members, evidence, similarity, captured_at) in rows {
        let (Ok(members), Ok(evidence)) =
            (serde_json::from_str::<Vec<i64>>(&members), serde_json::from_str::<Vec<String>>(&evidence))
        else {
            continue;
        };
        if members.iter().any(|id| grouped.contains(id)) {
            continue;
        }
        let mut items = Vec::new();
        for id in members {
            if let Some((path, time)) =
                file.query_row([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?))).optional()?
            {
                items.push(Item { id, path, time, phash: None, embedding: None });
            }
        }
        let shots = auto::shots(&items.iter().collect::<Vec<_>>());
        if shots.len() < 2 {
            continue;
        }
        groups.push(CatalogTakeAutoGroup {
            group_key: key,
            kind: media,
            status: "open".into(),
            captured_at,
            similarity: similarity.clamp(0.0, 1.0),
            shots: shots.into_iter().take(100).collect(),
            relation: Some(relation),
            evidence: Some(evidence),
        });
    }
    let visible = apply_decisions(conn, groups)?;
    let newest = visible.iter().map(|g| g.captured_at).fold(0.0, f64::max);
    let mut ranked = rank(visible, newest);
    ranked.truncate(limit);
    let mut response = super::catalog::response(&request.request_id, "ok", None);
    response.take_groups = Some(ranked);
    response.jobs = jobs(conn)?;
    Ok(response)
}

fn ids(conn: &Connection, sql: &str) -> Result<HashSet<i64>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

pub(crate) fn schedule(sink: Sink, database: Arc<Mutex<Connection>>) {
    if ACTIVE.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async move {
        run(&sink, &database).await;
        ACTIVE.store(false, Ordering::Release);
        let queued = super::timeline::with_db(&database, |conn| {
            Ok(conn
                .query_row("SELECT state FROM catalog_jobs WHERE id=?1", [JOB_ID], |r| r.get::<_, String>(0))
                .optional()?)
        })
        .await;
        if matches!(queued, Ok(Some(state)) if state == "queued") {
            schedule(sink, database);
        }
    });
}

async fn publish(sink: &Sink, database: &Arc<Mutex<Connection>>) {
    if let Ok(jobs) = super::timeline::with_db(database, |conn| jobs(conn)).await {
        let mut response = super::catalog::response(JOB_ID, "ok", None);
        response.jobs = jobs;
        sink.send(IpcEvent::now(EventPayload::CatalogResponse(Wrap::new(response)))).await;
    }
}

async fn run(sink: &Sink, database: &Arc<Mutex<Connection>>) {
    let started = super::timeline::with_db(database, |conn| {
        let changed = conn.execute(
            "UPDATE catalog_jobs SET state='running',error=NULL,updated_at=?1 WHERE id=?2 AND kind=?3 AND state='queued'",
            params![now(), JOB_ID, JOB_KIND],
        )?;
        Ok(changed == 1)
    })
    .await;
    if !matches!(started, Ok(true)) {
        return;
    }
    publish(sink, database).await;
    let outcome = async {
        let plan = super::timeline::with_db(database, |conn| {
            require_running(conn)?;
            plan(conn)
        })
        .await?;
        let total = plan.dirty.len();
        let mut published = Instant::now();
        for (index, session) in plan.dirty.into_iter().enumerate() {
            super::timeline::with_db(database, move |conn| process(conn, &session, index + 1, total)).await?;
            if published.elapsed() >= Duration::from_millis(500) {
                publish(sink, database).await;
                published = Instant::now();
            }
        }
        let keys = plan.keys;
        super::timeline::with_db(database, move |conn| finish(conn, &keys)).await
    }
    .await;
    let failure = match outcome {
        Ok(()) => None,
        Err(error) if error.is::<Interrupted>() => return publish(sink, database).await,
        Err(error) => Some(format!("{error:#}").chars().take(2_000).collect::<String>()),
    };
    let _ = super::timeline::with_db(database, move |conn| {
        match failure {
            None => conn.execute(
                "UPDATE catalog_jobs SET state='completed',progress=1,error=NULL,updated_at=?1 WHERE id=?2 AND state='running'",
                params![now(), JOB_ID],
            )?,
            Some(error) => conn.execute(
                "UPDATE catalog_jobs SET state='failed',error=?1,updated_at=?2 WHERE id=?3 AND state='running'",
                params![error, now(), JOB_ID],
            )?,
        };
        Ok(())
    })
    .await;
    publish(sink, database).await;
}

fn require_running(conn: &Connection) -> Result<()> {
    let state: Option<String> =
        conn.query_row("SELECT state FROM catalog_jobs WHERE id=?1", [JOB_ID], |r| r.get(0)).optional()?;
    if state.as_deref() != Some("running") {
        return Err(Interrupted.into());
    }
    Ok(())
}

#[cfg(test)]
fn run_blocking(conn: &mut Connection) -> Result<()> {
    conn.execute("UPDATE catalog_jobs SET state='running' WHERE id=?1 AND state='queued'", [JOB_ID])?;
    let plan = plan(conn)?;
    let total = plan.dirty.len();
    for (index, session) in plan.dirty.iter().enumerate() {
        process(conn, session, index + 1, total)?;
    }
    finish(conn, &plan.keys)?;
    conn.execute("UPDATE catalog_jobs SET state='completed',progress=1 WHERE id=?1 AND state='running'", [JOB_ID])?;
    Ok(())
}

fn plan(conn: &Connection) -> Result<Plan> {
    let files = eligible(conn)?;
    let mut stored: HashMap<i64, (String, String)> = HashMap::new();
    let mut statement = conn.prepare("SELECT file_id,session,fingerprint FROM take_proposal_files")?;
    for row in statement.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
        let (id, session, fingerprint) = row?;
        stored.insert(id, (session, fingerprint));
    }
    let mut dirty = Vec::new();
    let mut keys = Vec::new();
    for session in sessions(files) {
        let clean = session.files.iter().all(|file| {
            stored.get(&file.id).is_some_and(|(key, fingerprint)| *key == session.key && *fingerprint == file.fingerprint)
        });
        keys.push(session.key.clone());
        if !clean {
            dirty.push(session);
        }
    }
    Ok(Plan { dirty, keys })
}

fn eligible(conn: &Connection) -> Result<Vec<(&'static str, File)>> {
    let subjects = subjects(conn)?;
    let mut statement = conn.prepare(
        "SELECT f.id,f.path_text,f.created_at,f.kind,f.size_bytes,f.modified_at,f.camera_model,f.content_hash
         FROM files f JOIN clip_embeddings e ON e.file_id=f.id
         WHERE f.kind IN ('image','video') AND f.failed=0 AND f.created_at IS NOT NULL
           AND e.model=?1 AND length(e.embedding)=?2
           AND NOT EXISTS (SELECT 1 FROM catalog_assets a WHERE a.derived_id=f.id)
           AND NOT EXISTS (SELECT 1 FROM catalog_event_files ef WHERE ef.file_id=f.id)
         ORDER BY f.id",
    )?;
    let mut rows = statement.query(params![MODEL, (DIMENSION * 4) as i64])?;
    let mut hashes: HashSet<Vec<u8>> = HashSet::new();
    let mut files = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let time: f64 = row.get(2)?;
        let hash: Option<Vec<u8>> = row.get(7)?;
        if !time.is_finite() || hash.is_some_and(|hash| !hashes.insert(hash)) {
            continue;
        }
        let path: String = row.get(1)?;
        let kind: String = row.get(3)?;
        let size: i64 = row.get(4)?;
        let modified: Option<f64> = row.get(5)?;
        let camera = row.get::<_, Option<String>>(6)?.map(|c| c.trim().to_owned()).filter(|c| !c.is_empty());
        let revision = revision(size, modified);
        let people: BTreeSet<i64> = subjects
            .get(&id)
            .map(|found| found.iter().filter(|(_, source)| *source == revision).map(|(person, _)| *person).collect())
            .unwrap_or_default();
        let fingerprint = format!(
            "{}|{}|{}|{}|{}",
            revision,
            time.to_bits(),
            camera.as_deref().unwrap_or(""),
            people.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(","),
            path
        );
        let media = if kind == "image" { "photos" } else { "videos" };
        files.push((media, File { id, path, time, camera, fingerprint, subjects: people }));
    }
    Ok(files)
}

fn revision(size: i64, modified: Option<f64>) -> String {
    format!("{}:{}", size, modified.map(|m| m.to_bits().to_string()).unwrap_or_else(|| "unknown".into()))
}

/// User-named people per file with the revision their evidence was taken
/// from: compatible faces of a named person, or the person's user observations.
fn subjects(conn: &Connection) -> Result<HashMap<i64, Vec<(i64, String)>>> {
    let mut statement = conn.prepare(
        "SELECT fp.file_id,p.id,p.title,p.first_name,p.name,fp.source_revision
         FROM face_prints fp JOIN persons p ON p.id=fp.person_id JOIN face_prints rep ON rep.id=p.representative_face_id
         WHERE COALESCE(fp.excluded,0)=0 AND IFNULL(p.is_unknown,0)=0
           AND fp.embedding_model IS NOT NULL AND fp.embedding_model<>''
           AND rep.embedding_model=fp.embedding_model AND rep.processing_version IS fp.processing_version
         UNION ALL
         SELECT o.file_id,p.id,p.title,p.first_name,p.name,o.source_revision
         FROM catalog_observations o JOIN persons p ON p.id=o.person_id
         WHERE o.user_edited=1 AND o.stale=0 AND IFNULL(p.is_unknown,0)=0",
    )?;
    let mut rows = statement.query([])?;
    let mut found: HashMap<i64, Vec<(i64, String)>> = HashMap::new();
    while let Some(row) = rows.next()? {
        let title: Option<String> = row.get(2)?;
        let first: Option<String> = row.get(3)?;
        let legacy: Option<String> = row.get(4)?;
        let Some(source) = row.get::<_, Option<String>>(5)? else { continue };
        if style::is_user_named(&style::person_display(title.as_deref(), first.as_deref(), legacy.as_deref())) {
            found.entry(row.get(0)?).or_default().push((row.get(1)?, source));
        }
    }
    Ok(found)
}

fn sessions(files: Vec<(&'static str, File)>) -> Vec<Session> {
    let mut result = Vec::new();
    for media in ["photos", "videos"] {
        let mut ordered: Vec<File> = files.iter().filter(|(m, _)| *m == media).map(|(_, f)| f.clone()).collect();
        ordered.sort_by(|a, b| a.time.total_cmp(&b.time).then(a.id.cmp(&b.id)));
        let mut current: Vec<File> = Vec::new();
        for file in ordered {
            let split = current.last().is_some_and(|last| file.time - last.time > SESSION_GAP)
                || current.len() >= MAX_SESSION_FILES;
            if split {
                result.push(session(media, std::mem::take(&mut current)));
            }
            current.push(file);
        }
        if !current.is_empty() {
            result.push(session(media, current));
        }
    }
    result
}

fn session(media: &'static str, files: Vec<File>) -> Session {
    let mut ids: Vec<i64> = files.iter().map(|f| f.id).collect();
    ids.sort_unstable();
    let text = format!(
        "{VERSION}|{media}|{}",
        ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",")
    );
    let digest = Sha256::digest(text.as_bytes());
    Session { key: format!("{media}-{}", hex::encode(&digest[..8])), media, files }
}

fn process(conn: &mut Connection, session: &Session, processed: usize, total: usize) -> Result<()> {
    let tx = conn.transaction()?;
    require_running(&tx)?;
    let proposals = cluster(&tx, session)?;
    let members =
        serde_json::to_string(&session.files.iter().map(|f| f.id).collect::<Vec<_>>())?;
    tx.execute(
        "DELETE FROM take_proposals WHERE session=?1 OR EXISTS (SELECT 1 FROM json_each(take_proposals.file_ids_json) j WHERE j.value IN (SELECT value FROM json_each(?2)))",
        params![session.key, members],
    )?;
    let stamp = now();
    for proposal in &proposals {
        tx.execute(
            "INSERT OR REPLACE INTO take_proposals(group_key,session,media,relation,file_ids_json,evidence_json,similarity,captured_at,proposed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                proposal.key,
                session.key,
                session.media,
                proposal.relation,
                serde_json::to_string(&proposal.file_ids)?,
                serde_json::to_string(&proposal.evidence)?,
                proposal.similarity,
                proposal.captured_at,
                stamp
            ],
        )?;
    }
    for file in &session.files {
        tx.execute(
            "INSERT INTO take_proposal_files(file_id,session,fingerprint) VALUES(?1,?2,?3) ON CONFLICT(file_id) DO UPDATE SET session=excluded.session,fingerprint=excluded.fingerprint",
            params![file.id, session.key, file.fingerprint],
        )?;
    }
    let checkpoint = serde_json::json!({"processed": processed, "total": total}).to_string();
    tx.execute(
        "UPDATE catalog_jobs SET checkpoint_json=?1,progress=?2,updated_at=?3 WHERE id=?4 AND state='running'",
        params![checkpoint, (processed as f64 / total.max(1) as f64).min(0.99), stamp, JOB_ID],
    )?;
    tx.commit()?;
    Ok(())
}

fn finish(conn: &mut Connection, keys: &[String]) -> Result<()> {
    let tx = conn.transaction()?;
    require_running(&tx)?;
    let keys = serde_json::to_string(keys)?;
    tx.execute("DELETE FROM take_proposals WHERE session NOT IN (SELECT value FROM json_each(?1))", [&keys])?;
    tx.execute("DELETE FROM take_proposal_files WHERE session NOT IN (SELECT value FROM json_each(?1))", [&keys])?;
    tx.commit()?;
    Ok(())
}

struct Group {
    anchor: Vec<f32>,
    anchor_time: f64,
    latest_time: f64,
    members: Vec<usize>,
    common: BTreeSet<i64>,
    known: BTreeSet<i64>,
    lowest: f32,
}

fn cluster(conn: &Connection, session: &Session) -> Result<Vec<Proposal>> {
    let limits = if session.media == "photos" { &PHOTOS } else { &VIDEOS };
    let mut statement = conn.prepare_cached("SELECT embedding FROM clip_embeddings WHERE file_id=?1 AND model=?2")?;
    let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(session.files.len());
    for file in &session.files {
        let blob: Option<Vec<u8>> =
            statement.query_row(params![file.id, MODEL], |r| r.get(0)).optional()?;
        vectors.push(blob.as_deref().and_then(unit_vector));
    }
    let mut active: Vec<Group> = Vec::new();
    let mut finished: Vec<Group> = Vec::new();
    for (index, file) in session.files.iter().enumerate() {
        let Some(vector) = vectors[index].as_ref() else { continue };
        let mut still = Vec::with_capacity(active.len());
        for group in active.drain(..) {
            if file.time - group.latest_time > limits.max_gap || file.time - group.anchor_time > limits.max_span {
                finished.push(group);
            } else {
                still.push(group);
            }
        }
        active = still;
        let mut best: Option<(usize, f32)> = None;
        for (position, group) in active.iter().enumerate() {
            if group.members.len() >= MAX_MEMBERS {
                continue;
            }
            if !file.subjects.is_empty() && !group.known.is_empty() && file.subjects.is_disjoint(&group.known) {
                continue;
            }
            let similarity = dot(vector, &group.anchor);
            let threshold = if file.subjects.is_disjoint(&group.common) {
                limits.similarity
            } else {
                limits.subject_similarity
            };
            if similarity >= threshold && best.is_none_or(|(_, value)| similarity > value) {
                best = Some((position, similarity));
            }
        }
        if let Some((position, similarity)) = best {
            let group = &mut active[position];
            group.members.push(index);
            group.latest_time = file.time;
            group.lowest = group.lowest.min(similarity);
            group.common = group.common.intersection(&file.subjects).copied().collect();
            group.known.extend(file.subjects.iter().copied());
        } else {
            if active.len() >= MAX_ACTIVE {
                finished.push(active.remove(0));
            }
            active.push(Group {
                anchor: vector.clone(),
                anchor_time: file.time,
                latest_time: file.time,
                members: vec![index],
                common: file.subjects.clone(),
                known: file.subjects.clone(),
                lowest: 1.0,
            });
        }
    }
    finished.extend(active);
    let mut proposals = Vec::new();
    for group in finished.into_iter().filter(|g| g.members.len() >= 2) {
        let files: Vec<&File> = group.members.iter().map(|&i| &session.files[i]).collect();
        let items: Vec<Item> = files
            .iter()
            .map(|f| Item { id: f.id, path: f.path.clone(), time: Some(f.time), phash: None, embedding: None })
            .collect();
        let shots = auto::shots(&items.iter().collect::<Vec<_>>());
        if shots.len() < 2 {
            continue;
        }
        let by_id: HashMap<i64, &File> = files.iter().map(|f| (f.id, *f)).collect();
        let primaries: Vec<&File> = shots.iter().filter_map(|s| by_id.get(&s.primary_file_id).copied()).collect();
        let mut evidence = vec!["captureSession", "visualSimilarity"];
        if !group.common.is_empty() {
            evidence.push("sharedSubject");
        }
        let (relation, camera) = relation(&primaries, limits.simultaneous);
        evidence.push(camera);
        let mut ids: Vec<i64> = files.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        proposals.push(Proposal {
            key: auto::key(&ids),
            relation,
            file_ids: ids,
            evidence,
            similarity: round4(f64::from(group.lowest).clamp(0.0, 1.0)),
            captured_at: files.iter().map(|f| f.time).fold(f64::MIN, f64::max),
        });
    }
    Ok(proposals)
}

fn relation(primaries: &[&File], simultaneous: f64) -> (&'static str, &'static str) {
    for (index, left) in primaries.iter().enumerate() {
        for right in &primaries[index + 1..] {
            if let (Some(a), Some(b)) = (&left.camera, &right.camera)
                && a != b
                && (left.time - right.time).abs() <= simultaneous
            {
                return ("alternateViews", "simultaneousCameras");
            }
        }
    }
    if primaries.iter().any(|f| f.camera.is_none()) {
        return ("uncertain", "cameraDetailsMissing");
    }
    if primaries.windows(2).all(|pair| pair[0].camera == pair[1].camera) {
        return ("repeatedAttempts", "sameCamera");
    }
    ("uncertain", "mixedCameras")
}

fn unit_vector(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() != DIMENSION * 4 {
        return None;
    }
    let mut vector: Vec<f32> = blob.as_chunks::<4>().0.iter().map(|bytes| f32::from_le_bytes(*bytes)).collect();
    if !vector.iter().all(|value| value.is_finite()) {
        return None;
    }
    let norm: f64 = vector.iter().map(|value| f64::from(*value) * f64::from(*value)).sum();
    if (norm - 1.0).abs() > 0.02 {
        return None;
    }
    let scale = norm.sqrt() as f32;
    for value in &mut vector {
        *value /= scale;
    }
    Some(vector)
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

#[cfg(test)]
#[path = "take_grouping_tests.rs"]
mod tests;
