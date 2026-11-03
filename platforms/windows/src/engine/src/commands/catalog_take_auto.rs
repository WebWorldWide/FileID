//! Zero-setup Best Takes for the shared engine: suggested groups of tries from
//! capture time + CLIP + pHash (near_duplicate_grouping with TAKES/CLIPS
//! thresholds), cached model-free scores and Keep all / Skip choices. Mirror
//! of the macOS `CatalogTakeAuto.swift`; keep thresholds and reasons in step.

use crate::ipc::{
    CatalogTakeAutoGroup, CatalogTakeBox, CatalogTakeDecision, CatalogTakeGoalRanking, CatalogTakeQuality,
    CatalogTakeShot,
};
use crate::pipeline::near_duplicate_grouping::{self as ndg, Item, Thresholds, CLIPS, TAKES};
use crate::pipeline::take_scoring::{self, Box2, Face, Luma, Shot, Signals};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub const MAX_SCORED_FILES: usize = 800;

struct Row1 {
    id: i64,
    path: String,
    time: f64,
    phash: Option<u64>,
    kind: String,
}

pub fn discover(conn: &Connection, limit: usize) -> Result<Vec<CatalogTakeAutoGroup>> {
    ensure!((1..=100).contains(&limit), "Invalid take group limit");
    let mut statement = conn.prepare(
        "SELECT f.id, f.path_text, f.created_at, f.phash, f.kind FROM files f
         WHERE f.kind IN ('image','video') AND f.failed=0 AND f.created_at IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM catalog_assets a WHERE a.derived_id=f.id)
           AND NOT EXISTS (SELECT 1 FROM catalog_event_files ef WHERE ef.file_id=f.id)
         ORDER BY f.created_at, f.id",
    )?;
    let rows: Vec<Row1> = statement
        .query_map([], |r| {
            Ok(Row1 {
                id: r.get(0)?,
                path: r.get(1)?,
                time: r.get(2)?,
                phash: r.get::<_, Option<i64>>(3)?.map(|p| p as u64),
                kind: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut groups = Vec::new();
    for (kind, limits) in [("image", &TAKES), ("video", &CLIPS)] {
        let subset: Vec<&Row1> = rows.iter().filter(|r| r.kind == kind && r.time.is_finite()).collect();
        let embeddings = load_embeddings(conn, &candidate_ids(&subset, limits))?;
        let items: Vec<Item> = subset
            .iter()
            .map(|r| Item {
                id: r.id,
                path: r.path.clone(),
                time: Some(r.time),
                phash: r.phash,
                embedding: embeddings.get(&r.id).cloned(),
            })
            .collect();
        let aligned: HashMap<i64, Item> = ndg::align_sidecar_times(&items).into_iter().map(|i| (i.id, i)).collect();
        for group in ndg::groups_with(&items, limits) {
            let members: Vec<&Item> = group.members.iter().filter_map(|id| aligned.get(id)).collect();
            let shots = shots(&members);
            if shots.len() < 2 {
                continue;
            }
            let primaries: Vec<&Vec<f32>> =
                shots.iter().filter_map(|s| aligned.get(&s.primary_file_id).and_then(|i| i.embedding.as_ref())).collect();
            let mut similarity = 1.0f32;
            for pair in primaries.windows(2) {
                similarity = similarity.min(ndg::cosine(pair[0], pair[1]));
            }
            let mut ids: Vec<i64> = members.iter().map(|m| m.id).collect();
            ids.sort_unstable();
            groups.push(CatalogTakeAutoGroup {
                group_key: key(&ids),
                kind: if kind == "image" { "photos" } else { "videos" }.into(),
                status: "open".into(),
                captured_at: members.iter().filter_map(|m| m.time).fold(0.0, f64::max),
                similarity: take_scoring::round4(similarity.clamp(0.0, 1.0) as f64),
                shots: shots.into_iter().take(100).collect(),
            });
        }
    }
    let mut decisions: Vec<(String, HashSet<i64>)> = Vec::new();
    let mut statement = conn.prepare("SELECT decision,file_ids_json FROM take_group_decisions")?;
    for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (decision, json) = row?;
        if let Ok(ids) = serde_json::from_str::<Vec<i64>>(&json) {
            decisions.push((decision, ids.into_iter().collect()));
        }
    }
    let mut visible = Vec::new();
    for mut group in groups {
        let ids: HashSet<i64> = group.file_ids().into_iter().collect();
        if decisions.iter().any(|(d, set)| d == "keepAll" && ids.is_subset(set)) {
            continue;
        }
        if decisions.iter().any(|(d, set)| d == "skip" && ids.is_subset(set)) {
            group.status = "skipped".into();
        }
        visible.push(group);
    }
    let newest = visible.iter().map(|g| g.captured_at).fold(0.0, f64::max);
    let mut ranked = rank(visible, newest);
    ranked.truncate(limit);
    Ok(ranked)
}

/// Bigger and more recent groups first: 0.6 × size (saturating at 12 tries)
/// + 0.4 × recency (half-life 14 days before the newest group). Skipped last.
pub fn rank(mut groups: Vec<CatalogTakeAutoGroup>, newest: f64) -> Vec<CatalogTakeAutoGroup> {
    let value = |g: &CatalogTakeAutoGroup| {
        let size = g.shots.len().min(12) as f64 / 12.0;
        let age_days = (newest - g.captured_at).max(0.0) / 86_400.0;
        0.6 * size + 0.4 * 0.5f64.powf(age_days / 14.0)
    };
    groups.sort_by(|a, b| {
        (a.status == "skipped")
            .cmp(&(b.status == "skipped"))
            .then(value(b).total_cmp(&value(a)))
            .then(b.captured_at.total_cmp(&a.captured_at))
            .then(a.group_key.cmp(&b.group_key))
    });
    groups
}

fn candidate_ids(rows: &[&Row1], limits: &Thresholds) -> Vec<i64> {
    let mut ids = HashSet::new();
    for pair in rows.windows(2) {
        if pair[1].time - pair[0].time <= limits.max_gap_seconds {
            ids.insert(pair[0].id);
            ids.insert(pair[1].id);
        }
    }
    let mut stems: HashMap<String, Vec<i64>> = HashMap::new();
    for row in rows {
        stems.entry(ndg::stem(&row.path)).or_default().push(row.id);
    }
    for group in stems.values().filter(|g| g.len() > 1) {
        ids.extend(group.iter().copied());
    }
    let mut ids: Vec<i64> = ids.into_iter().collect();
    ids.sort_unstable();
    ids
}

fn load_embeddings(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, Vec<f32>>> {
    let mut out = HashMap::new();
    let mut statement = conn.prepare_cached("SELECT embedding FROM clip_embeddings WHERE file_id=?1")?;
    for id in ids {
        if let Some(blob) = statement.query_row([id], |r| r.get::<_, Vec<u8>>(0)).optional()? {
            if let Some(vector) = ndg::decode_embedding(&blob) {
                out.insert(*id, vector);
            }
        }
    }
    Ok(out)
}

/// One shot per file name: a RAW and its JPEG are one try. The viewable
/// (non-RAW) file is primary. Shots in capture order.
pub fn shots(items: &[&Item]) -> Vec<CatalogTakeShot> {
    let mut order: Vec<String> = Vec::new();
    let mut by_stem: HashMap<String, Vec<&Item>> = HashMap::new();
    for item in items {
        let stem = ndg::stem(&item.path);
        if !by_stem.contains_key(&stem) {
            order.push(stem.clone());
        }
        by_stem.entry(stem).or_default().push(item);
    }
    let mut shots: Vec<CatalogTakeShot> = order
        .iter()
        .filter_map(|stem| {
            let mut files = by_stem.get(stem)?.clone();
            files.sort_by(|a, b| ndg::is_raw(&a.path).cmp(&ndg::is_raw(&b.path)).then(a.id.cmp(&b.id)));
            Some(CatalogTakeShot {
                primary_file_id: files[0].id,
                file_ids: files.iter().map(|f| f.id).take(8).collect(),
                path: files[0].path.clone(),
                captured_at: files.iter().filter_map(|f| f.time).reduce(f64::min),
            })
        })
        .collect();
    shots.sort_by(|a, b| {
        a.captured_at
            .unwrap_or(0.0)
            .total_cmp(&b.captured_at.unwrap_or(0.0))
            .then(a.primary_file_id.cmp(&b.primary_file_id))
    });
    shots
}

pub fn key(sorted_ids: &[i64]) -> String {
    let text = sorted_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
    let digest = Sha256::digest(text.as_bytes());
    format!("takes-{}", hex::encode(&digest[..8]))
}

pub fn decide(conn: &Connection, decision: &CatalogTakeDecision) -> Result<()> {
    ensure!(
        !decision.group_key.is_empty()
            && decision.group_key.chars().count() <= 200
            && (1..=MAX_SCORED_FILES).contains(&decision.file_ids.len())
            && decision.file_ids.iter().all(|id| *id > 0)
            && ["keepAll", "skip", "reopen"].contains(&decision.decision.as_str()),
        "Invalid take decision"
    );
    if decision.decision == "reopen" {
        conn.execute("DELETE FROM take_group_decisions WHERE group_key=?1", [&decision.group_key])?;
        let ids: HashSet<i64> = decision.file_ids.iter().copied().collect();
        let mut statement = conn.prepare("SELECT group_key,file_ids_json FROM take_group_decisions")?;
        let stored: Vec<(String, String)> =
            statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        for (key, json) in stored {
            if serde_json::from_str::<Vec<i64>>(&json).is_ok_and(|s| ids.is_subset(&s.into_iter().collect())) {
                conn.execute("DELETE FROM take_group_decisions WHERE group_key=?1", [key])?;
            }
        }
        return Ok(());
    }
    let mut ids: Vec<i64> = decision.file_ids.iter().copied().collect::<HashSet<_>>().into_iter().collect();
    ids.sort_unstable();
    conn.execute(
        "INSERT INTO take_group_decisions(group_key,decision,file_ids_json,decided_at) VALUES(?1,?2,?3,?4)
         ON CONFLICT(group_key) DO UPDATE SET decision=excluded.decision,file_ids_json=excluded.file_ids_json,decided_at=excluded.decided_at",
        params![decision.group_key, decision.decision, serde_json::to_string(&ids)?, now()],
    )?;
    Ok(())
}

pub fn goal_unavailable(goal: &str, order: &[i64]) -> CatalogTakeGoalRanking {
    CatalogTakeGoalRanking {
        goal: goal.into(),
        status: "unavailable".into(),
        order: order.to_vec(),
        met_file_ids: vec![],
        summary: "Ranking by a goal isn't available on this computer yet. The order stays on photo quality.".into(),
        model: None,
    }
}

fn now() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}

// MARK: scoring

pub struct ScoreJob {
    shots: Vec<CatalogTakeShot>,
    rows: HashMap<i64, FileRow>,
    cached: HashMap<i64, Signals>,
    version: String,
}

struct FileRow {
    path: String,
    kind: String,
    revision: String,
}

/// Faces for take scoring. Kept behind a trait so the shared face-quality gate
/// can replace this judgement without touching the scorer. `None` means the
/// detector could not run, so the result is used once but not cached.
pub trait TakeFaceAnalyzer {
    fn id(&self) -> &str;
    fn faces(&mut self, rgb: &[u8], width: u32, height: u32) -> Option<Vec<Face>>;
}

/// YuNet when its weights are installed: boxes and detector score as quality.
/// Eye openness is not judged until a landmark-quality model is available.
pub struct YuNetFaces {
    model: Option<crate::models::yunet::YuNet>,
}

impl YuNetFaces {
    pub fn load() -> Self {
        let model = crate::models::yunet::default_weights_path()
            .ok()
            .filter(|path| path.exists())
            .and_then(|path| crate::models::yunet::YuNet::load(path).ok());
        Self { model }
    }
}

impl TakeFaceAnalyzer for YuNetFaces {
    fn id(&self) -> &str {
        if self.model.is_some() {
            "yunet-v1"
        } else {
            "no-faces-v1"
        }
    }

    fn faces(&mut self, rgb: &[u8], width: u32, height: u32) -> Option<Vec<Face>> {
        let Some(model) = self.model.as_mut() else { return Some(vec![]) };
        let found = model.detect(rgb, width, height).ok()?;
        let (w, h) = (width as f64, height as f64);
        Some(
            found
                .into_iter()
                .take(16)
                .map(|d| {
                    let [x1, y1, x2, y2] = d.bbox;
                    let x = (x1 as f64 / w).clamp(0.0, 1.0);
                    let y = (y1 as f64 / h).clamp(0.0, 1.0);
                    Face {
                        x: take_scoring::round4(x),
                        y: take_scoring::round4(y),
                        width: take_scoring::round4(((x2 - x1) as f64 / w).clamp(0.0, 1.0 - x)),
                        height: take_scoring::round4(((y2 - y1) as f64 / h).clamp(0.0, 1.0 - y)),
                        quality: Some(take_scoring::round4(d.score as f64)),
                        eyes_open: None,
                    }
                })
                .collect(),
        )
    }
}

/// Reads what scoring needs under the database lock; the caller then scores
/// without holding it.
pub fn prepare(conn: &Connection, file_ids: &[i64], faces_id: &str) -> Result<ScoreJob> {
    ensure!(
        (1..=MAX_SCORED_FILES).contains(&file_ids.len()) && file_ids.iter().all(|id| *id > 0),
        "Invalid take selection"
    );
    let version = format!("{}/{}", take_scoring::VERSION, faces_id);
    let mut ids: Vec<i64> = file_ids.iter().copied().collect::<HashSet<_>>().into_iter().collect();
    ids.sort_unstable();
    let mut items = Vec::new();
    let mut rows = HashMap::new();
    let mut cached = HashMap::new();
    let mut statement = conn.prepare_cached(
        "SELECT f.path_text,f.kind,f.created_at,q.signals_json,q.source_revision,q.version
         FROM files f LEFT JOIN take_quality q ON q.file_id=f.id WHERE f.id=?1 AND f.kind IN ('image','video')",
    )?;
    for id in ids {
        let Some((path, kind, time, json, stored_revision, stored_version)) = statement
            .query_row([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<f64>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })
            .optional()?
        else {
            continue;
        };
        let revision = super::timeline::revision(conn, id)?;
        if stored_revision.as_deref() == Some(revision.as_str()) && stored_version.as_deref() == Some(version.as_str()) {
            if let Some(signals) = json.and_then(|j| serde_json::from_str::<Signals>(&j).ok()) {
                cached.insert(id, signals);
            }
        }
        items.push(Item { id, path: path.clone(), time, phash: None, embedding: None });
        rows.insert(id, FileRow { path, kind, revision });
    }
    ensure!(!items.is_empty(), "None of these files can be scored");
    let refs: Vec<&Item> = items.iter().collect();
    Ok(ScoreJob { shots: shots(&refs), rows, cached, version })
}

pub struct Scored {
    pub qualities: Vec<CatalogTakeQuality>,
    fresh: Vec<(i64, String, String)>,
    version: String,
}

/// Decodes and measures every shot without a current cache entry.
pub fn compute(job: ScoreJob, faces: &mut dyn TakeFaceAnalyzer) -> Scored {
    let mut signals: HashMap<i64, Signals> = job.cached;
    let mut fresh = Vec::new();
    for shot in &job.shots {
        if signals.contains_key(&shot.primary_file_id) {
            continue;
        }
        let Some(row) = job.rows.get(&shot.primary_file_id) else { continue };
        let measured = if row.kind == "video" {
            video_signals(Path::new(&row.path)).map(|s| (s, true))
        } else {
            image_signals(Path::new(&row.path), faces)
        };
        if let Some((value, complete)) = measured {
            if complete {
                if let Ok(json) = serde_json::to_string(&value) {
                    fresh.push((shot.primary_file_id, row.revision.clone(), json));
                }
            }
            signals.insert(shot.primary_file_id, value);
        }
    }
    let scored = take_scoring::score(
        &job.shots
            .iter()
            .map(|s| Shot {
                primary_file_id: s.primary_file_id,
                file_ids: s.file_ids.clone(),
                path: s.path.clone(),
                signals: signals.get(&s.primary_file_id).cloned(),
            })
            .collect::<Vec<_>>(),
    );
    Scored { qualities: scored.into_iter().map(quality).collect(), fresh, version: job.version }
}

pub fn store(conn: &Connection, scored: &Scored) -> Result<()> {
    let at = now();
    for (id, revision, json) in &scored.fresh {
        conn.execute(
            "INSERT INTO take_quality(file_id,source_revision,version,signals_json,computed_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(file_id) DO UPDATE SET source_revision=excluded.source_revision,version=excluded.version,
               signals_json=excluded.signals_json,computed_at=excluded.computed_at",
            params![id, revision, scored.version, json, at],
        )?;
    }
    Ok(())
}

fn image_signals(path: &Path, faces: &mut dyn TakeFaceAnalyzer) -> Option<(Signals, bool)> {
    let (rgb, w, h) = crate::pipeline::tagging::decode_image_sync(path, None).ok()?;
    let (rgb, w, h) = fit(rgb, w, h, take_scoring::ANALYSIS_LONG_SIDE);
    let found = faces.faces(&rgb, w, h);
    let luma = Luma::from_rgb(&rgb, w as usize, h as usize);
    let complete = found.is_some();
    Some((take_scoring::signals(&luma, found.unwrap_or_default(), None::<Vec<Box2>>), complete))
}

fn fit(rgb: Vec<u8>, w: u32, h: u32, long_side: u32) -> (Vec<u8>, u32, u32) {
    if w.max(h) <= long_side || w == 0 || h == 0 {
        return (rgb, w, h);
    }
    let scale = long_side as f64 / w.max(h) as f64;
    let (nw, nh) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
    let resized =
        crate::pipeline::tagging::resize_rgb_quality(&rgb, w as usize, h as usize, nw as usize, nh as usize);
    (resized, nw, nh)
}

fn video_signals(path: &Path) -> Option<Signals> {
    use std::sync::atomic::AtomicBool;
    let cancelled = AtomicBool::new(false);
    let probe = super::video_export::analysis_probe(path, &cancelled).ok()?;
    let (w, h) = super::video_export::fitted_size(probe.width, probe.height, take_scoring::VIDEO_LONG_SIDE);
    let frame = |seconds: f64| {
        super::video_export::frame_rgb(path, seconds, w, h, &cancelled)
            .ok()
            .map(|rgb| Luma::from_rgb(&rgb, w as usize, h as usize))
    };
    let pairs: Vec<(f64, Luma, Luma)> = take_scoring::anchor_times(probe.duration, 6, 0.2)
        .into_iter()
        .filter_map(|t| Some((t, frame(t)?, frame(t + 0.2)?)))
        .collect();
    take_scoring::video_signals(&pairs, probe.duration)
}

fn quality(q: take_scoring::Quality) -> CatalogTakeQuality {
    let read = q.sharpness.is_some();
    let to_box = |b: Box2| CatalogTakeBox { x: b.x, y: b.y, width: b.width, height: b.height };
    CatalogTakeQuality {
        primary_file_id: q.primary_file_id,
        file_ids: q.file_ids,
        path: q.path,
        score: q.score,
        rank: q.rank,
        recommended: q.recommended,
        reasons: q.reasons,
        sharpness: q.sharpness,
        exposure: q.exposure,
        motion_blur: read.then_some(q.motion_blur),
        face_count: read.then_some(q.face_count),
        eyes_closed: read.then_some(q.eyes_closed),
        face_cut_off: read.then_some(q.face_cut_off),
        stability: q.stability,
        action: q.action,
        peak_seconds: q.peak_seconds,
        subject: q.subject.map(to_box),
        faces: q
            .faces
            .iter()
            .take(16)
            .map(|f| CatalogTakeBox { x: f.x, y: f.y, width: f.width, height: f.height })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(first: f32, second: f32) -> Vec<u8> {
        let mut vector = vec![0.0_f32; 512];
        vector[0] = first;
        vector[1] = second;
        vector.into_iter().flat_map(f32::to_le_bytes).collect()
    }

    fn insert(conn: &Connection, id: i64, path: &str, time: f64, angle: f32) {
        conn.execute(
            "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension) VALUES(?1,?2,?1,100,?3,?3,0,'image','jpg')",
            params![id, path, time],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?1,?2,'mobileclip_s2')",
            params![id, blob(angle.cos(), angle.sin())],
        )
        .unwrap();
    }

    #[test]
    fn bursts_group_into_shots_with_raw_twins_and_choices_hide_them() {
        let conn = crate::db::test_support::memory_db();
        insert(&conn, 1, "/c/JPEGs/DSC1.JPG", 100.0, 0.0);
        insert(&conn, 2, "/c/RAWs/DSC1.ARW", 100.0, 0.0);
        insert(&conn, 3, "/c/JPEGs/DSC2.JPG", 102.0, 0.05);
        insert(&conn, 4, "/c/RAWs/DSC2.ARW", 102.0, 0.05);
        insert(&conn, 5, "/c/JPEGs/DSC3.JPG", 103.0, 1.2);
        insert(&conn, 6, "/c/JPEGs/DSC9.JPG", 5000.0, 0.0);
        insert(&conn, 7, "/c/JPEGs/DSC10.JPG", 5001.0, 0.02);
        let groups = discover(&conn, 100).unwrap();
        assert_eq!(groups.len(), 2);
        let burst = groups.iter().find(|g| g.shots.len() == 2 && g.shots[0].primary_file_id == 1).unwrap();
        assert_eq!(burst.shots[0].file_ids, vec![1, 2]);
        assert_eq!(burst.shots[1].file_ids, vec![3, 4]);
        assert_eq!(burst.kind, "photos");
        assert!(groups.iter().all(|g| g.group_key.starts_with("takes-")));
        assert_eq!(groups[0].shots[0].primary_file_id, 6, "the newer group of equal size ranks first");

        let decision = |d: &str, ids: Vec<i64>| CatalogTakeDecision { group_key: burst.group_key.clone(), decision: d.into(), file_ids: ids };
        decide(&conn, &decision("skip", burst.file_ids())).unwrap();
        let groups = discover(&conn, 100).unwrap();
        assert_eq!(groups.last().unwrap().status, "skipped");
        decide(&conn, &decision("keepAll", burst.file_ids())).unwrap();
        assert_eq!(discover(&conn, 100).unwrap().len(), 1);
        decide(&conn, &decision("reopen", burst.file_ids())).unwrap();
        assert_eq!(discover(&conn, 100).unwrap().len(), 2);
        assert!(decide(&conn, &decision("delete", burst.file_ids())).is_err());
    }

    #[test]
    fn scoring_reads_png_tries_and_caches_by_revision() {
        let dir = std::env::temp_dir().join(format!("fileid-takes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::db::test_support::memory_db();
        for (id, blur) in [(1i64, 0usize), (2, 3)] {
            let (w, h) = (96u32, 64u32);
            let mut image = image::GrayImage::new(w, h);
            for (x, y, p) in image.enumerate_pixels_mut() {
                p.0 = [if ((x / 4) + (y / 4)) % 2 == 0 { 40 } else { 200 }];
            }
            let image = if blur > 0 { image::imageops::blur(&image, blur as f32) } else { image };
            let path = dir.join(format!("try{id}.png"));
            image.save(&path).unwrap();
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension) VALUES(?1,?2,?1,100,10,10,0,'image','png')",
                params![id, path.to_string_lossy()],
            )
            .unwrap();
        }
        let mut faces = YuNetFaces { model: None };
        let job = prepare(&conn, &[1, 2], faces.id()).unwrap();
        let scored = compute(job, &mut faces);
        assert_eq!(scored.qualities.len(), 2);
        let best = scored.qualities.iter().find(|q| q.recommended).unwrap();
        assert_eq!(best.primary_file_id, 1);
        assert_eq!(best.reasons, vec!["Sharpest".to_string()]);
        store(&conn, &scored).unwrap();
        let job = prepare(&conn, &[1, 2], faces.id()).unwrap();
        assert_eq!(job.cached.len(), 2);
        conn.execute("UPDATE files SET size_bytes=101 WHERE id=1", []).unwrap();
        let job = prepare(&conn, &[1, 2], faces.id()).unwrap();
        assert_eq!(job.cached.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
