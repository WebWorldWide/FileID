//! Restructure tab handlers: plan (classify + propose moves) and apply
//! (execute on disk + update DB). The actual file-move machinery lives in
//! `pipeline::restructure_apply`; classification logic lives in
//! `pipeline::restructure`. These handlers wire app payloads through to
//! those modules.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::ipc::{
    self, sink::Sink, EngineError, EventPayload, FolderClassificationCounts, IpcEvent,
    RestructureCategoryCount, RestructureMove as IpcMove, RestructurePlan, Wrap,
};
use crate::pipeline::discovery::FileKind;
use crate::pipeline::restructure::{self, classify, FileForClassify, FolderClassification};
use crate::pipeline::restructure_apply::RestructureApply;
use crate::pipeline::restructure_feedback;
use crate::pipeline::restructure_semantic;

const RESTRUCTURE_PREVIEW_CAP: usize = 5_000;
const LARGE_PLAN_STREAM_THRESHOLD: i64 = 50_000;
const LARGE_PLAN_CHUNK: usize = 4_096;
const STORED_PLAN_VERSION: u8 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPlanHeader {
    version: u8,
    library_root: String,
    total_moves: usize,
}

struct StoredPlanMoveIter {
    lines: std::io::Lines<BufReader<File>>,
}

impl Iterator for StoredPlanMoveIter {
    type Item = anyhow::Result<IpcMove>;

    fn next(&mut self) -> Option<Self::Item> {
        self.lines.next().map(|line| {
            let line = line.context("reading persisted restructure plan")?;
            serde_json::from_str(&line).context("decoding persisted restructure move")
        })
    }
}

fn plan_path_in(dir: &std::path::Path, plan_id: &str) -> anyhow::Result<PathBuf> {
    let parsed = uuid::Uuid::parse_str(plan_id).context("invalid restructure plan ID")?;
    Ok(dir.join(format!("{parsed}.ndjson")))
}

fn write_stored_plan(
    library_root: &str,
    moves: impl IntoIterator<Item = anyhow::Result<IpcMove>>,
    total_moves: usize,
) -> anyhow::Result<(String, Vec<IpcMove>)> {
    let dir = crate::paths::restructure_plans_dir()?;
    write_stored_plan_in(&dir, library_root, moves, total_moves)
}

fn write_stored_plan_in(
    dir: &std::path::Path,
    library_root: &str,
    moves: impl IntoIterator<Item = anyhow::Result<IpcMove>>,
    total_moves: usize,
) -> anyhow::Result<(String, Vec<IpcMove>)> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating restructure plan directory {}", dir.display()))?;
    // Only one plan is actionable in the UI at a time. Remove stale spools so
    // repeated planning cannot grow engine state without bound.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if matches!(path.extension().and_then(|e| e.to_str()), Some("ndjson" | "tmp")) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let plan_id = uuid::Uuid::new_v4().to_string();
    let final_path = plan_path_in(dir, &plan_id)?;
    let tmp_path = dir.join(format!("{plan_id}.tmp"));
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp_path)
        .with_context(|| format!("creating restructure plan {}", tmp_path.display()))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(
        &mut writer,
        &StoredPlanHeader {
            version: STORED_PLAN_VERSION,
            library_root: library_root.to_string(),
            total_moves,
        },
    )?;
    writer.write_all(b"\n")?;
    let mut preview = Vec::with_capacity(RESTRUCTURE_PREVIEW_CAP);
    let write_result = (|| -> anyhow::Result<()> {
        for move_ in moves {
            let move_ = move_?;
            if preview.len() < RESTRUCTURE_PREVIEW_CAP {
                preview.push(move_.clone());
            }
            serde_json::to_writer(&mut writer, &move_)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        drop(writer);
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error.context("persisting restructure plan"));
    }
    drop(writer);
    std::fs::rename(&tmp_path, &final_path).with_context(|| {
        format!(
            "publishing restructure plan {} -> {}",
            tmp_path.display(),
            final_path.display()
        )
    })?;
    Ok((plan_id, preview))
}

fn open_stored_plan(
    plan_id: &str,
    expected_root: &str,
) -> anyhow::Result<(StoredPlanMoveIter, usize)> {
    let dir = crate::paths::restructure_plans_dir()?;
    open_stored_plan_in(&dir, plan_id, expected_root)
}

fn open_stored_plan_in(
    dir: &std::path::Path,
    plan_id: &str,
    expected_root: &str,
) -> anyhow::Result<(StoredPlanMoveIter, usize)> {
    let path = plan_path_in(dir, plan_id)?;
    let file = File::open(&path)
        .with_context(|| format!("opening restructure plan {}", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    let header_line = lines
        .next()
        .context("persisted restructure plan is empty")??;
    let header: StoredPlanHeader =
        serde_json::from_str(&header_line).context("decoding restructure plan header")?;
    anyhow::ensure!(
        header.version == STORED_PLAN_VERSION,
        "unsupported restructure plan version {}",
        header.version
    );
    anyhow::ensure!(
        roots_equal(&header.library_root, expected_root),
        "restructure plan belongs to a different library root"
    );
    Ok((StoredPlanMoveIter { lines }, header.total_moves))
}

fn roots_equal(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn plan_row_to_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileForClassify> {
    let kind_str: String = row.get(2)?;
    let kind = match kind_str.as_str() {
        "image" => FileKind::Image,
        "video" => FileKind::Video,
        "pdf" => FileKind::Pdf,
        "doc" => FileKind::Doc,
        "audio" => FileKind::Audio,
        "model" => FileKind::Model,
        _ => FileKind::Other,
    };
    let names: Option<String> = row.get(8)?;
    let person_name = names
        .as_deref()
        .and_then(|s| s.split('\x1F').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(FileForClassify {
        file_id: row.get(0)?,
        source: PathBuf::from(row.get::<_, String>(1)?),
        kind,
        modified_unix: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
        created_unix: row.get(4)?,
        person_name,
        location_lat: row.get(5)?,
        location_lon: row.get(6)?,
        has_text: row.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
    })
}

fn folder_tier(folder: &str, total: i64, top: i64) -> &'static str {
    let name = Path::new(folder)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let generic = matches!(
        name.as_str(),
        "downloads"
            | "downloaded"
            | "new folder"
            | "untitled"
            | "temp"
            | "tmp"
            | "misc"
            | "other"
            | "stuff"
            | "things"
            | "files"
    );
    if generic || total <= 2 {
        "Junk"
    } else if top.saturating_mul(100) >= total.saturating_mul(80) {
        "Anchor"
    } else {
        "Mixed"
    }
}

fn persist_rule_chunk(
    tx: &rusqlite::Transaction<'_>,
    chunk: &mut Vec<FileForClassify>,
    library_root: &Path,
    sequence: &mut i64,
) -> anyhow::Result<()> {
    if chunk.is_empty() {
        return Ok(());
    }
    let mut insert_move = tx.prepare_cached(
        "INSERT INTO raw_moves
         (seq,file_id,source,source_folder,destination,category,confidence,reason)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
    )?;
    let mut insert_stat = tx.prepare_cached(
        "INSERT INTO folder_stats(folder,category,count) VALUES (?1,?2,1)
         ON CONFLICT(folder,category) DO UPDATE SET count=count+1",
    )?;
    for move_ in classify(chunk, library_root) {
        let source_folder = move_
            .source
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_string_lossy()
            .into_owned();
        insert_move.execute(rusqlite::params![
            *sequence,
            move_.file_id,
            move_.source.to_string_lossy(),
            source_folder,
            move_.destination.to_string_lossy(),
            move_.category,
            move_.confidence.as_str(),
            move_.reason,
        ])?;
        insert_stat.execute(rusqlite::params![source_folder, move_.category])?;
        *sequence += 1;
    }
    chunk.clear();
    Ok(())
}

// An engine kill mid-plan orphans the `{uuid}.planning.sqlite` scratch DB (the
// `remove_file` at the end of `plan_large_library_in` never runs). The
// ndjson/tmp sweep in `write_stored_plan_in` executes while the live scratch is
// still open, so stale scratch files are swept here — before a new one exists.
fn sweep_stale_planning_scratch(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".planning.sqlite"))
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn plan_large_library(
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    library_root: &str,
) -> anyhow::Result<RestructurePlan> {
    let dir = crate::paths::restructure_plans_dir()?;
    plan_large_library_in(db, library_root, &dir)
}

fn plan_large_library_in(
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    library_root: &str,
    dir: &Path,
) -> anyhow::Result<RestructurePlan> {
    std::fs::create_dir_all(dir)?;
    sweep_stale_planning_scratch(dir);
    let planning_path = dir.join(format!("{}.planning.sqlite", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<RestructurePlan> {
        let mut plan_db = rusqlite::Connection::open(&planning_path)?;
        plan_db.execute_batch(
            "PRAGMA journal_mode=OFF;
             PRAGMA synchronous=OFF;
             PRAGMA temp_store=FILE;
             CREATE TABLE raw_moves(
                 seq INTEGER PRIMARY KEY,
                 file_id INTEGER NOT NULL,
                 source TEXT NOT NULL,
                 source_folder TEXT NOT NULL,
                 destination TEXT NOT NULL,
                 category TEXT NOT NULL,
                 confidence TEXT NOT NULL,
                 reason TEXT);
             CREATE TABLE folder_stats(
                 folder TEXT NOT NULL,
                 category TEXT NOT NULL,
                 count INTEGER NOT NULL,
                 PRIMARY KEY(folder,category));
             CREATE TABLE folder_tiers(
                 folder TEXT PRIMARY KEY,
                 tier TEXT NOT NULL);",
        )?;

        let tx = plan_db.transaction()?;
        let bounds = plan_root_bounds(library_root);
        let mut sequence = 0_i64;
        let mut last_id = i64::MIN;
        loop {
            // Keyset-page the shared connection so a million-file plan does not
            // monopolize the engine's single SQLite mutex for the whole scan.
            // Each lock holds only while 4,096 compact metadata rows are copied.
            let mut chunk = {
                let source = db.lock();
                let mut stmt = source.prepare(PLAN_FILES_PAGE_SQL)?;
                let rows = stmt.query_map(
                    rusqlite::params![
                        &bounds.0,
                        &bounds.1,
                        &bounds.2,
                        last_id,
                        LARGE_PLAN_CHUNK as i64
                    ],
                    plan_row_to_file,
                )?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if chunk.is_empty() {
                break;
            }
            last_id = chunk.last().map_or(last_id, |file| file.file_id);
            let page_was_full = chunk.len() == LARGE_PLAN_CHUNK;
            persist_rule_chunk(
                &tx,
                &mut chunk,
                Path::new(library_root),
                &mut sequence,
            )?;
            if !page_was_full {
                break;
            }
        }
        tx.commit()?;

        let mut anchor = 0_u32;
        let mut mixed = 0_u32;
        let mut junk = 0_u32;
        {
            let mut stats = plan_db.prepare(
                "SELECT folder, SUM(count), MAX(count)
                 FROM folder_stats GROUP BY folder ORDER BY folder",
            )?;
            let mut rows = stats.query([])?;
            let mut insert = plan_db.prepare_cached(
                "INSERT INTO folder_tiers(folder,tier) VALUES (?1,?2)",
            )?;
            while let Some(row) = rows.next()? {
                let folder: String = row.get(0)?;
                let total: i64 = row.get(1)?;
                let top: i64 = row.get(2)?;
                let tier = folder_tier(&folder, total, top);
                match tier {
                    "Anchor" => anchor = anchor.saturating_add(1),
                    "Mixed" => mixed = mixed.saturating_add(1),
                    _ => junk = junk.saturating_add(1),
                }
                insert.execute(rusqlite::params![folder, tier])?;
            }
        }

        let category_counts = {
            let mut stmt = plan_db.prepare(
                "SELECT r.category, COUNT(*) AS n
                 FROM raw_moves r JOIN folder_tiers t ON t.folder=r.source_folder
                 WHERE t.tier <> 'Anchor'
                 GROUP BY r.category ORDER BY n DESC, r.category ASC",
            )?;
            let counts = stmt.query_map([], |row| {
                let count: i64 = row.get(1)?;
                Ok(RestructureCategoryCount {
                    category: row.get(0)?,
                    count: count.clamp(0, u32::MAX as i64) as u32,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
            counts
        };
        let total_moves: i64 = plan_db.query_row(
            "SELECT COUNT(*) FROM raw_moves r
             JOIN folder_tiers t ON t.folder=r.source_folder
             WHERE t.tier <> 'Anchor'",
            [],
            |row| row.get(0),
        )?;

        let (plan_id, preview) = {
            let mut stmt = plan_db.prepare(
                "SELECT r.file_id,r.source,r.destination,r.category,t.tier,
                        r.confidence,r.reason
                 FROM raw_moves r JOIN folder_tiers t ON t.folder=r.source_folder
                 WHERE t.tier <> 'Anchor' ORDER BY r.seq",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(IpcMove {
                    file_id: row.get(0)?,
                    source: row.get(1)?,
                    destination: row.get(2)?,
                    category: row.get(3)?,
                    tier: Some(row.get(4)?),
                    confidence: row.get(5)?,
                    reason: row.get(6)?,
                })
            })?;
            write_stored_plan_in(
                dir,
                library_root,
                rows.map(|row| row.map_err(anyhow::Error::from)),
                total_moves.max(0) as usize,
            )?
        };

        let truncated = total_moves as usize > RESTRUCTURE_PREVIEW_CAP;
        if !truncated {
            if let Ok(path) = plan_path_in(dir, &plan_id) {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(RestructurePlan {
            library_root: library_root.to_string(),
            plan_id: truncated.then_some(plan_id),
            total_moves: truncated.then_some(total_moves.max(0) as u64),
            truncated,
            moves: preview,
            category_counts,
            folder_classifications: Some(FolderClassificationCounts {
                anchor_folders: anchor,
                mixed_folders: mixed,
                junk_folders: junk,
            }),
        })
    })();
    let _ = std::fs::remove_file(&planning_path);
    result
}

/// Files + per-file person names for restructure planning. Person names come
/// from a deduped, ordered correlated subquery — NOT
/// `GROUP_CONCAT(DISTINCT p.name, char(31))`, which SQLite rejects at run with
/// "DISTINCT aggregates must have exactly one argument". `names` (column 8) is a
/// char(31)-separated list; the row reader takes the first.
const PLAN_FILES_SQL: &str = "SELECT
   f.id, f.path_text, f.kind, f.modified_at, f.created_at,
   f.location_lat, f.location_lon, f.has_text,
   (SELECT GROUP_CONCAT(name, char(31))
      FROM (SELECT DISTINCT p.name
              FROM persons p
              JOIN face_prints fp ON fp.person_id = p.id
             WHERE fp.file_id = f.id
               AND p.name IS NOT NULL AND p.name <> ''
             ORDER BY p.name)) AS names
 FROM files f
 WHERE f.failed = 0
   AND (?1 = '' OR f.path_text = ?1 OR (f.path_text >= ?2 AND f.path_text < ?3))
 ORDER BY f.id";

const PLAN_FILES_PAGE_SQL: &str = "SELECT
   f.id, f.path_text, f.kind, f.modified_at, f.created_at,
   f.location_lat, f.location_lon, f.has_text,
   (SELECT GROUP_CONCAT(name, char(31))
      FROM (SELECT DISTINCT p.name
              FROM persons p
              JOIN face_prints fp ON fp.person_id = p.id
             WHERE fp.file_id = f.id
               AND p.name IS NOT NULL AND p.name <> ''
             ORDER BY p.name)) AS names
 FROM files f
 WHERE f.failed = 0
   AND (?1 = '' OR f.path_text = ?1 OR (f.path_text >= ?2 AND f.path_text < ?3))
   AND f.id > ?4
 ORDER BY f.id
 LIMIT ?5";

fn plan_root_bounds(root: &str) -> (String, String, String) {
    let root = root.trim_end_matches(['/', '\\']).to_string();
    let separator = if root.contains('\\') { '\\' } else { '/' };
    let prefix = format!("{root}{separator}");
    let mut bytes = prefix.as_bytes().to_vec();
    for index in (0..bytes.len()).rev() {
        if bytes[index] != u8::MAX {
            bytes[index] += 1;
            bytes.truncate(index + 1);
            return (root, prefix, String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    (root, prefix, "\u{10ffff}".into())
}

/// Max embeddings of each modality retained for one restructure plan. Every
/// tier is bounded: "high memory" cannot mean unbounded when the library can
/// contain millions of 2 KiB vectors.
fn embedding_load_cap(tier: crate::platform::MemoryTier) -> usize {
    match tier {
        crate::platform::MemoryTier::Low => 20_000,
        crate::platform::MemoryTier::Balanced => 50_000,
        crate::platform::MemoryTier::High => 100_000,
    }
}

/// Stream the image CLIP embeddings into a map, stopping at `cap` so the heavy
/// blob transient never exceeds the memory-tier budget. The query streams
/// row-by-row, so breaking early holds at most `cap` decoded vectors resident.
/// (audit F-C6-016)
fn load_capped_embeddings(
    conn: &rusqlite::Connection,
    cap: usize,
    bounds: &(String, String, String),
) -> rusqlite::Result<std::collections::HashMap<i64, Vec<f32>>> {
    let mut embeddings = std::collections::HashMap::new();
    if cap == 0 {
        return Ok(embeddings);
    }
    let mut stmt = conn.prepare(
        "SELECT ce.file_id, ce.embedding FROM clip_embeddings ce
         JOIN files f ON f.id = ce.file_id
         WHERE f.failed = 0 AND f.kind IN ('image', 'video', 'model')
           AND (?1 = '' OR f.path_text = ?1 OR (f.path_text >= ?2 AND f.path_text < ?3))",
    )?;
    let rows = stmt.query_map(rusqlite::params![bounds.0, bounds.1, bounds.2], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    for r in rows {
        if embeddings.len() >= cap {
            break;
        }
        let (id, blob) = r?;
        if !blob.is_empty() && blob.len() % 4 == 0 {
            let v = blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            embeddings.insert(id, v);
        }
    }
    Ok(embeddings)
}

/// Load BGE document text embeddings (file_id → 384-d vector) for the doc-content pass.
/// Not capped: documents are a small fraction of a library and the vectors are 384-d.
fn load_text_embeddings(
    conn: &rusqlite::Connection,
    cap: usize,
    bounds: &(String, String, String),
) -> rusqlite::Result<std::collections::HashMap<i64, Vec<f32>>> {
    let mut out = std::collections::HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT te.file_id, te.embedding FROM text_embeddings te
         JOIN files f ON f.id = te.file_id
         WHERE f.failed = 0 AND f.kind IN ('doc', 'pdf')
           AND (?1 = '' OR f.path_text = ?1 OR (f.path_text >= ?2 AND f.path_text < ?3))",
    )?;
    let rows = stmt.query_map(rusqlite::params![bounds.0, bounds.1, bounds.2], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    for r in rows {
        if out.len() >= cap {
            break;
        }
        let (id, blob) = r?;
        if !blob.is_empty() && blob.len() % 4 == 0 {
            let v = blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            out.insert(id, v);
        }
    }
    Ok(out)
}

fn absorb_semantic_moves(
    moves: Vec<restructure::ProposedMove>,
    moved: &mut std::collections::HashSet<i64>,
    semantic_source_folders: &mut std::collections::HashSet<PathBuf>,
    proposed: &mut Vec<restructure::ProposedMove>,
) {
    for m in &moves {
        moved.insert(m.file_id);
        if let Some(parent) = m.source.parent() {
            semantic_source_folders.insert(parent.to_path_buf());
        }
    }
    proposed.extend(moves);
}

/// Walk the `files` table for the picked library root, classify each file,
/// and emit a `restructurePlan` event with the proposed moves + per-category
/// counts. The app's Restructure tab consumes this to render the Sankey +
/// tree-diff.
pub(crate) async fn handle_plan_restructure(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::PlanRestructurePayload,
) {
    let library_root = payload.library_root.clone();
    let supports_paged_plans = payload.supports_paged_plans;
    let query_root = library_root.clone();
    let db_for_semantic = std::sync::Arc::clone(&db);
    // Kept alive past the query/signals spawn_blocking closures (which move `db` and
    // `db_for_semantic`) so the learn-from-corrections boost can read the feedback
    // table after the proposal set is built.
    let db_for_boost = std::sync::Arc::clone(&db);
    if supports_paged_plans {
        let count_db = std::sync::Arc::clone(&db);
        let count_root = library_root.clone();
        let scoped_count = tokio::task::spawn_blocking(move || -> rusqlite::Result<i64> {
            let conn = count_db.lock();
            let bounds = plan_root_bounds(&count_root);
            conn.query_row(
                "SELECT COUNT(*) FROM files f WHERE f.failed=0
                 AND (?1='' OR f.path_text=?1 OR (f.path_text>=?2 AND f.path_text<?3))",
                rusqlite::params![bounds.0, bounds.1, bounds.2],
                |row| row.get(0),
            )
        })
        .await;
        match scoped_count {
            Ok(Ok(count)) if count > LARGE_PLAN_STREAM_THRESHOLD => {
                let plan_db = std::sync::Arc::clone(&db);
                let plan_root = library_root.clone();
                let planned = tokio::task::spawn_blocking(move || {
                    plan_large_library(&plan_db, &plan_root)
                })
                .await;
                match planned {
                    Ok(Ok(plan)) => {
                        sink.send(IpcEvent::now(EventPayload::RestructurePlan(Wrap::new(plan))))
                            .await;
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(?err, "large restructure planning failed");
                        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                            kind: "plan_restructure_failed".into(),
                            message: format!("Restructure planning did not complete: {err}"),
                            path: None,
                            model_kind: None,
                        }))))
                        .await;
                    }
                    Err(err) => {
                        tracing::warn!(?err, "large restructure planning task failed");
                        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                            kind: "plan_restructure_failed".into(),
                            message: format!("Restructure planning did not complete: {err}"),
                            path: None,
                            model_kind: None,
                        }))))
                        .await;
                    }
                }
                return;
            }
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                tracing::warn!(?err, "counting restructure scope failed");
            }
            Err(err) => {
                tracing::warn!(?err, "counting restructure scope task failed");
            }
        }
    }
    let files: Vec<FileForClassify> =
        match tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<FileForClassify>> {
            let conn = db.lock();
            let mut stmt = conn.prepare(PLAN_FILES_SQL)?;
            let (root, prefix, upper) = plan_root_bounds(&query_root);
            let rows = stmt.query_map(rusqlite::params![root, prefix, upper], |row| {
                let kind_str: String = row.get(2)?;
                let kind = match kind_str.as_str() {
                    "image" => FileKind::Image,
                    "video" => FileKind::Video,
                    "pdf" => FileKind::Pdf,
                    "doc" => FileKind::Doc,
                    "audio" => FileKind::Audio,
                    "model" => FileKind::Model,
                    _ => FileKind::Other,
                };
                let modified: Option<f64> = row.get(3)?;
                let created: Option<f64> = row.get(4)?;
                let names: Option<String> = row.get(8)?;
                let person_name = names
                    .as_deref()
                    .and_then(|s| s.split('\x1F').next())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                Ok(FileForClassify {
                    file_id: row.get(0)?,
                    source: PathBuf::from(row.get::<_, String>(1)?),
                    kind,
                    modified_unix: modified.unwrap_or(0.0),
                    created_unix: created,
                    person_name,
                    location_lat: row.get(5)?,
                    location_lon: row.get(6)?,
                    has_text: row.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
                })
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(err)) => {
                tracing::warn!(?err, "planRestructure query failed");
                sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                    kind: "plan_restructure_db".into(),
                    message: format!("planRestructure query failed: {err}"),
                    path: None,
                    model_kind: None,
                }))))
                .await;
                return;
            }
            Err(err) => {
                // JoinError = the blocking query task panicked / was aborted.
                // Emit a terminal error so the Restructure tab's "Computing
                // plan…" status recovers instead of awaiting forever (mirrors
                // the face_clustering PAR-111 JoinError handling).
                tracing::warn!(?err, "planRestructure spawn_blocking failed");
                sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                    kind: "plan_restructure_failed".into(),
                    message: format!("Restructure planning did not complete: {err}"),
                    path: None,
                    model_kind: None,
                }))))
                .await;
                return;
            }
        };

    let library_root_path = std::path::Path::new(&library_root);

    // Butler P1: semantic + learn-your-style classification for image files that
    // have a CLIP embedding; everything else (and density-clustering noise)
    // falls back to the rule cascade. See pipeline/restructure_semantic.rs.
    //
    // The CLIP embeddings are the heavy transient here (each ~2 KiB), so cap how
    // many we load by memory tier: on a low-RAM box an unbounded full-table load
    // could materialize hundreds of MiB at once. Above Low we keep the prior
    // behavior (effectively uncapped). Files past the cap simply fall through to
    // the rule cascade — the same graceful degradation as a file with no
    // embedding. Tags are then loaded only for the ids we kept, so neither map
    // grows unbounded under pressure. (audit F-C6-016)
    let embedding_cap = embedding_load_cap(crate::platform::memory_tier());
    let signal_bounds = plan_root_bounds(&library_root);
    let signals = tokio::task::spawn_blocking(
        move || -> rusqlite::Result<(
            std::collections::HashMap<i64, Vec<f32>>,
            std::collections::HashMap<i64, Vec<String>>,
            std::collections::HashMap<i64, Vec<f32>>,
        )> {
            let conn = db_for_semantic.lock();
            let embeddings = load_capped_embeddings(&conn, embedding_cap, &signal_bounds)?;
            let text_embeddings =
                load_text_embeddings(&conn, embedding_cap, &signal_bounds)?;
            let mut tags: std::collections::HashMap<i64, Vec<String>> =
                std::collections::HashMap::new();
            // DISTINCT so a tag carried under multiple sources for the same
            // file counts ONCE — otherwise c-TF-IDF tf/df double-counts it and
            // skews distinctive_terms group naming (#18).
            let mut tstmt = conn.prepare(
                "SELECT DISTINCT t.file_id, t.tag FROM tags t
                 JOIN files f ON f.id = t.file_id
                 WHERE t.source IN ('auto','vlm','user') AND f.failed = 0
                   AND (?1 = '' OR f.path_text = ?1 OR (f.path_text >= ?2 AND f.path_text < ?3))
                 LIMIT ?4",
            )?;
            let tag_cap = embedding_cap.saturating_mul(8).min(i64::MAX as usize) as i64;
            let trows = tstmt.query_map(
                rusqlite::params![
                    signal_bounds.0,
                    signal_bounds.1,
                    signal_bounds.2,
                    tag_cap
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )?;
            for r in trows {
                let (id, tag) = r?;
                // Load tags for ALL files, not just embedded images: the R1
                // non-image semantic pass consults them for documents/video/audio
                // too (matching the macOS engine, which loads tags unfiltered).
                // Tags are short strings, so this stays cheap even when the
                // embedding cap bounds the heavier image set. (RESTRUCTURE.md R1)
                tags.entry(id).or_default().push(tag);
            }
            Ok((embeddings, tags, text_embeddings))
        },
    )
    .await;
    let (mut embeddings, mut tags_map, mut text_embeddings) = match signals {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => {
            tracing::warn!(?err, "signals load failed; proceeding with empty maps");
            (
                std::collections::HashMap::new(),
                std::collections::HashMap::new(),
                std::collections::HashMap::new(),
            )
        }
        Err(err) => {
            tracing::warn!(?err, "signals task panicked; proceeding with empty maps");
            (
                std::collections::HashMap::new(),
                std::collections::HashMap::new(),
                std::collections::HashMap::new(),
            )
        }
    };

    // Drain (remove) instead of clone: each file_id is a PK consumed once here,
    // and both maps are dead afterward — moving avoids doubling the ~100 MB of
    // CLIP blobs transiently on a low-RAM box.
    let semantic_files: Vec<restructure_semantic::SemanticFile> = files
        .iter()
        .filter(|f| matches!(f.kind, FileKind::Image | FileKind::Video | FileKind::Model))
        .filter_map(|f| {
            embeddings.remove(&f.file_id).map(|clip| restructure_semantic::SemanticFile {
                file_id: f.file_id,
                source: f.source.clone(),
                clip,
                // Read tags non-destructively (NOT remove): an image the CLIP pass
                // examined but didn't cluster (a density-noise singleton) falls
                // through to the non-image pass below, which must still see its
                // content tags. Tags are short strings, so keeping them in the map
                // is cheap — only the heavy CLIP blob is drained (above). Matches
                // macOS, which reads tags via a non-consuming lookup. (audit parity)
                tags: tags_map.get(&f.file_id).cloned().unwrap_or_default(),
                time_unix: f.created_unix.unwrap_or(f.modified_unix),
            })
        })
        .collect();

    // Source folders the semantic butler actively claimed (every file relocated
    // into a content group). These classify Anchor on destination-category
    // homogeneity but are real relocations, not in-place anchors — exempt them
    // from the anchor strip so their highest-confidence moves survive. (F-C1-004)
    let mut semantic_source_folders: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::new();
    let mut proposed = Vec::new();
    let mut moved: std::collections::HashSet<i64> = std::collections::HashSet::new();
    // One new-folder-name registry shared across all three semantic passes
    // (image, document, non-image) — they target the same library_root, so a
    // shared registry stops two passes minting the same new folder and silently
    // merging unrelated content into it.
    let mut used_group_names: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // Butler P1: image semantic pass (CLIP-embedding content clusters).
    if semantic_files.len() >= 2 {
        let protos = restructure_semantic::folder_prototypes(&semantic_files, 4);
        let moves = restructure_semantic::semantic_classify(&semantic_files, &protos, library_root_path, &mut used_group_names);
        absorb_semantic_moves(moves, &mut moved, &mut semantic_source_folders, &mut proposed);
    }

    // Butler R3: document-content pass. Cluster documents by their BGE text embedding
    // (the content), which the scan stored in `text_embeddings`. Far stronger than the
    // filename-token fallback (owner A/B: nearest-neighbour-same-folder 49%→57%). Docs
    // WITH an embedding cluster here; docs without (no extractable text) fall through to
    // the bag-of-words pass below. Runs before it so it claims the text-bearing docs.
    if restructure_semantic::non_image_enabled() {
        let doc_files: Vec<restructure_semantic::SemanticFile> = files
            .iter()
            .filter(|f| !moved.contains(&f.file_id))
            .filter(|f| matches!(f.kind, FileKind::Doc | FileKind::Pdf))
            .filter_map(|f| {
                text_embeddings.remove(&f.file_id).map(|emb| restructure_semantic::SemanticFile {
                    file_id: f.file_id,
                    source: f.source.clone(),
                    clip: emb,
                    tags: tags_map.get(&f.file_id).cloned().unwrap_or_default(),
                    time_unix: f.created_unix.unwrap_or(f.modified_unix),
                })
            })
            .collect();
        let doc_moves =
            restructure_semantic::classify_documents(&doc_files, library_root_path, &mut used_group_names);
        absorb_semantic_moves(doc_moves, &mut moved, &mut semantic_source_folders, &mut proposed);
    }

    // Butler R1: non-image semantic pass. Cluster everything the doc + image passes didn't
    // claim (video, audio, docs without an extractable-text embedding, and any
    // embedding-less file) by a filename+tag bag-of-words signature, so a mixed library
    // groups by content instead of dumping every file into <Year>. Additive + separately
    // tuned (non_image_profile); the rule cascade below still catches the
    // remainder. Owner kill-switch: FILEID_RESTRUCTURE_NONIMAGE=0.
    if restructure_semantic::non_image_enabled() {
        let non_image_files: Vec<restructure_semantic::SemanticFile> = files
            .iter()
            .filter(|f| !moved.contains(&f.file_id))
            .map(|f| restructure_semantic::SemanticFile {
                file_id: f.file_id,
                source: f.source.clone(),
                clip: Vec::new(),
                tags: tags_map.remove(&f.file_id).unwrap_or_default(),
                time_unix: f.created_unix.unwrap_or(f.modified_unix),
            })
            .collect();
        let ni_moves =
            restructure_semantic::classify_non_image(&non_image_files, library_root_path, &mut used_group_names);
        absorb_semantic_moves(ni_moves, &mut moved, &mut semantic_source_folders, &mut proposed);
    }

    // Rule cascade for everything neither semantic pass claimed.
    let rule_files: Vec<FileForClassify> =
        files.iter().filter(|f| !moved.contains(&f.file_id)).cloned().collect();
    proposed.extend(classify(&rule_files, library_root_path));

    // Learn-from-corrections: upgrade any planned move toward a folder the user has
    // previously filed similar files into (the v18 restructure_feedback memory,
    // written on each apply). Additive — only raises confidence on moves the planner
    // already produced, never re-routes — so it can't regress the calibrated passes.
    // Runs on the full proposal set, before the anchor strip preserves the upgraded
    // confidence into the emitted plan. (R3 → learn-your-style)
    restructure_feedback::boost(&db_for_boost, &mut proposed);

    // Engine-authoritative folder classification, computed on the FULL proposal
    // set so the Keep/Tidy/Reorganize tile counts stay accurate.
    let folder_class = restructure::classify_folders(&proposed);
    let mut anchor = 0u32;
    let mut mixed = 0u32;
    let mut junk = 0u32;
    // Index classification by source folder so per-move tiers can be
    // stamped without re-classifying.
    let mut tier_by_folder: std::collections::HashMap<PathBuf, &'static str> =
        std::collections::HashMap::with_capacity(folder_class.len());
    for f in &folder_class {
        // An exempted Anchor folder is the butler actively relocating its files
        // into a content group — it is NOT kept in place, so it must not inflate
        // the "Keep" tile or label its moves Anchor. Count + tier it as Mixed so
        // the tile and the surviving moves agree. (F-C1-004)
        let classification = if matches!(f.classification, FolderClassification::Anchor)
            && semantic_source_folders.contains(&f.source_folder)
        {
            &FolderClassification::Mixed
        } else {
            &f.classification
        };
        let tier_label = match classification {
            FolderClassification::Anchor => {
                anchor += 1;
                "Anchor"
            }
            FolderClassification::Mixed => {
                mixed += 1;
                "Mixed"
            }
            FolderClassification::Junk => {
                junk += 1;
                "Junk"
            }
        };
        tier_by_folder.insert(f.source_folder.clone(), tier_label);
    }

    // Anchor folders are well-organized with clear names; the macOS reference
    // emits NO proposals for them ("Files inside Anchor folders stay put"), and
    // the Keep tile (driven by folder_classifications.anchor_folders, counted
    // just above) tells the user they're left untouched. Drop their moves so the
    // plan the app applies can never silently relocate a file the UI promised
    // would stay put — without this, default-selected Anchor rows were applied.
    // Folders the semantic butler actively claimed are exempt: their homogeneity
    // is a real relocation, not an in-place anchor, so stripping would eat the
    // best proposals. (audit A1/A3, F-C1-004)
    let proposed = restructure::strip_anchor_folder_moves_except(
        proposed,
        &folder_class,
        &semantic_source_folders,
    );
    let category_summary = restructure::category_counts(&proposed);

    let total_moves = proposed.len();
    let library_root_for_spool = library_root.clone();
    let encoded = tokio::task::spawn_blocking(move || {
        let moves = proposed.into_iter().map(|m| {
                let tier = m
                    .source
                    .parent()
                    .and_then(|p| tier_by_folder.get(p))
                    .map(|s| (*s).to_string());
                IpcMove {
                    file_id: m.file_id,
                    source: m.source.to_string_lossy().to_string(),
                    destination: m.destination.to_string_lossy().to_string(),
                    category: m.category,
                    tier,
                    confidence: m.confidence.as_str().to_string(),
                    reason: m.reason,
                }
            });
        if supports_paged_plans && total_moves > RESTRUCTURE_PREVIEW_CAP {
            let (plan_id, preview) =
                write_stored_plan(
                    &library_root_for_spool,
                    moves.map(Ok),
                    total_moves,
                )?;
            Ok::<_, anyhow::Error>((Some(plan_id), preview, Some(total_moves as u64), true))
        } else {
            Ok((None, moves.collect(), None, false))
        }
    })
    .await;
    let (plan_id, moves, total_moves, truncated) = match encoded {
        Ok(Ok(encoded)) => encoded,
        Ok(Err(err)) => {
            tracing::warn!(?err, "persisting paged restructure plan failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "plan_restructure_store".into(),
                message: format!("Could not store the restructure plan: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
            return;
        }
        Err(err) => {
            tracing::warn!(?err, "encoding restructure plan task failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "plan_restructure_failed".into(),
                message: format!("Restructure planning did not complete: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
            return;
        }
    };

    let plan = RestructurePlan {
        library_root,
        plan_id,
        total_moves,
        truncated,
        moves,
        category_counts: category_summary
            .into_iter()
            .map(|c| RestructureCategoryCount {
                category: c.category,
                count: c.count,
            })
            .collect(),
        folder_classifications: Some(FolderClassificationCounts {
            anchor_folders: anchor,
            mixed_folders: mixed,
            junk_folders: junk,
        }),
    };

    sink.send(IpcEvent::now(EventPayload::RestructurePlan(Wrap::new(plan))))
        .await;
}

/// Apply a previously-planned set of moves on disk + update DB rows.
/// Path-traversal safe (every destination must canonicalize to inside the
/// library root); supports symlink mode for non-destructive preview.
pub(crate) async fn handle_undo_restructure(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::UndoRestructurePayload,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let result = tokio::task::spawn_blocking(
        move || -> anyhow::Result<ipc::RestructureApplyResult> {
            let apply = RestructureApply::new(db, PathBuf::from(payload.library_root), false)
                .with_cancel(cancel);
            apply.undo_last()
        },
    )
    .await;

    match result {
        Ok(Ok(r)) => {
            sink.send(IpcEvent::now(EventPayload::RestructureApplyResult(Wrap::new(r))))
                .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "undoRestructure failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "undo_restructure".into(),
                message: format!("Undo failed: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "undoRestructure spawn_blocking failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "undo_restructure".into(),
                message: format!("Undo did not complete: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
    }
}

pub(crate) async fn handle_apply_restructure(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::ApplyRestructurePayload,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let result = tokio::task::spawn_blocking(
        move || -> anyhow::Result<ipc::RestructureApplyResult> {
            // F-C6-013 wiring: inject the shared cancel flag the CancelScan
            // dispatch arm sets, so a long apply is actually stoppable. Before
            // this, the apply built a fresh never-set flag and the cooperative
            // cancel poll was dead in production. (The flag is reset to false in
            // the ApplyRestructure dispatch arm so a stale cancel can't pre-stop
            // a fresh apply.)
            let ipc::ApplyRestructurePayload {
                library_root,
                plan_id,
                moves,
                use_symlinks,
            } = payload;
            let apply = RestructureApply::new(
                db,
                PathBuf::from(&library_root),
                use_symlinks,
            )
            .with_cancel(cancel);
            if let Some(plan_id) = plan_id {
                anyhow::ensure!(moves.is_empty(), "paged plan apply must not also include moves");
                let (moves, total) = open_stored_plan(&plan_id, &library_root)?;
                apply.apply_iter(moves, Some(total))
            } else {
                let total = moves.len();
                apply.apply_iter(moves.into_iter().map(Ok), Some(total))
            }
        },
    )
    .await;

    match result {
        Ok(Ok(r)) => {
            sink.send(IpcEvent::now(EventPayload::RestructureApplyResult(
                Wrap::new(r),
            )))
            .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "applyRestructure failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "apply_restructure".into(),
                message: format!("Apply failed: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
        Err(err) => {
            // JoinError = the apply task panicked / was aborted. Emit a terminal
            // error so the Restructure tab's "Moving N files…" status recovers
            // instead of hanging forever (ApplyRestructureAsync has no app-side
            // timeout; mirrors the face_clustering PAR-111 JoinError handling).
            tracing::warn!(?err, "applyRestructure spawn_blocking failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "apply_restructure".into(),
                message: format!("Apply did not complete: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// The planner SQL must prepare AND run — the old
    /// `GROUP_CONCAT(DISTINCT name, char(31))` form prepared but failed at run
    /// with "DISTINCT aggregates must have exactly one argument". This also pins
    /// the dedup, char(31) separator, failed-row exclusion, and NULL-when-no-faces
    /// behavior the row reader depends on.
    #[test]
    fn plan_files_sql_runs_and_dedupes_person_names() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files(
                 id INTEGER PRIMARY KEY, path_text TEXT, kind TEXT,
                 modified_at REAL, created_at REAL,
                 location_lat REAL, location_lon REAL,
                 has_text INTEGER, failed INTEGER DEFAULT 0);
             CREATE TABLE persons(id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE face_prints(file_id INTEGER, person_id INTEGER);
             INSERT INTO files(id,path_text,kind,failed) VALUES
                 (1,'/a.jpg','image',0),(2,'/b.jpg','image',0),(3,'/c.jpg','image',1);
             INSERT INTO persons(id,name) VALUES (1,'Bob'),(2,'Alice');
             INSERT INTO face_prints(file_id,person_id) VALUES (1,1),(1,1),(1,2);",
        )
        .unwrap();

        let mut stmt = conn.prepare(PLAN_FILES_SQL).expect("planner SQL prepares");
        let mut rows: Vec<(i64, Option<String>)> = stmt
            .query_map(rusqlite::params!["", "", ""], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(8)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.sort_by_key(|(id, _)| *id);

        // failed=1 (file 3) is excluded; file 1 dedupes Bob+Bob+Alice into two
        // names joined by char(31); file 2 has no faces → NULL. Compare as a set
        // (SQLite doesn't guarantee aggregate order across versions).
        assert_eq!(rows.len(), 2);
        let mut names: Vec<&str> = rows[0].1.as_deref().unwrap().split('\u{1f}').collect();
        names.sort_unstable();
        assert_eq!(names, ["Alice", "Bob"]);
        assert_eq!(rows[1].1, None);
    }

    #[test]
    fn plan_files_sql_is_scoped_to_the_selected_library_root() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files(
                 id INTEGER PRIMARY KEY, path_text TEXT, kind TEXT,
                 modified_at REAL, created_at REAL,
                 location_lat REAL, location_lon REAL,
                 has_text INTEGER, failed INTEGER DEFAULT 0);
             CREATE TABLE persons(id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE face_prints(file_id INTEGER, person_id INTEGER);
             INSERT INTO files(id,path_text,kind,failed) VALUES
                 (1,'/library/a.jpg','image',0),
                 (2,'/library/nested/b.jpg','image',0),
                 (3,'/library-old/not-ours.jpg','image',0),
                 (4,'/other/c.jpg','image',0);",
        )
        .unwrap();
        let (root, prefix, upper) = plan_root_bounds("/library/");
        let ids: Vec<i64> = conn
            .prepare(PLAN_FILES_SQL)
            .unwrap()
            .query_map(rusqlite::params![root, prefix, upper], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(ids, [1, 2]);
    }

    /// Every tier must cap the resident map; larger machines get a larger
    /// quality sample, never an unbounded full-table allocation.
    #[test]
    fn embedding_load_cap_is_bounded_on_every_tier() {
        use crate::platform::MemoryTier;
        let low = embedding_load_cap(MemoryTier::Low);
        let balanced = embedding_load_cap(MemoryTier::Balanced);
        let high = embedding_load_cap(MemoryTier::High);
        assert!(low < balanced && balanced < high);
        assert!(high < usize::MAX);
    }

    /// The load streams and stops at the cap, so a large corpus never
    /// materializes a full-table HashMap under memory pressure. Before the fix
    /// the load was unconditional (one HashMap holding every row); this asserts
    /// the cap actually bounds the resident map. (audit F-C6-016)
    #[test]
    fn capped_embedding_load_bounds_the_resident_map() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files(id INTEGER PRIMARY KEY, path_text TEXT, kind TEXT, failed INTEGER DEFAULT 0);
             CREATE TABLE clip_embeddings(file_id INTEGER PRIMARY KEY, embedding BLOB);",
        )
        .unwrap();
        // 100 image rows, each a valid 4-float embedding blob.
        let blob: Vec<u8> = (0..4)
            .flat_map(|i| (i as f32).to_le_bytes())
            .collect();
        for id in 1..=100i64 {
            conn.execute(
                "INSERT INTO files(id,path_text,kind,failed) VALUES (?1,?2,'image',0)",
                rusqlite::params![id, format!("/library/{id}.jpg")],
            )
                .unwrap();
            conn.execute(
                "INSERT INTO clip_embeddings(file_id,embedding) VALUES (?1,?2)",
                rusqlite::params![id, blob],
            )
            .unwrap();
        }

        // A small cap bounds the resident map even though 100 rows qualify.
        let bounds = plan_root_bounds("");
        let capped = load_capped_embeddings(&conn, 10, &bounds).unwrap();
        assert_eq!(capped.len(), 10, "cap must bound the resident map");

        // An effectively-uncapped load (Balanced/High) returns the full table.
        let full = load_capped_embeddings(&conn, usize::MAX, &bounds).unwrap();
        assert_eq!(full.len(), 100, "uncapped load returns every qualifying row");
    }

    #[test]
    fn stored_plan_preview_is_bounded_and_root_bound() {
        let dir = std::env::temp_dir().join(format!(
            "fileid-plan-spool-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let root = "/library";
        let total = RESTRUCTURE_PREVIEW_CAP + 37;
        let moves = (0..total).map(|index| IpcMove {
            file_id: index as i64,
            source: format!("{root}/incoming/{index}.jpg"),
            destination: format!("{root}/Photos/2024/{index}.jpg"),
            category: "photo".into(),
            tier: Some("Mixed".into()),
            confidence: "review".into(),
            reason: Some("Photo from 2024".into()),
        });

        let (plan_id, preview) =
            write_stored_plan_in(&dir, root, moves.map(Ok), total).unwrap();
        assert_eq!(preview.len(), RESTRUCTURE_PREVIEW_CAP);
        assert!(plan_path_in(&dir, &plan_id).unwrap().is_file());

        let (stream, stored_total) = open_stored_plan_in(&dir, &plan_id, root).unwrap();
        assert_eq!(stored_total, total);
        assert_eq!(stream.collect::<anyhow::Result<Vec<_>>>().unwrap().len(), total);
        assert!(
            open_stored_plan_in(&dir, &plan_id, "/different-library").is_err(),
            "an opaque plan cannot be replayed under a broader or different root"
        );
        assert!(plan_path_in(&dir, "../escape").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Full paged-plan loop: spool a plan over real files, reopen it by
    /// planID, and stream it through `apply_iter` — the exact path a
    /// truncated GUI plan takes when the app applies with planID + empty
    /// moves. Guards the spool/apply seam the piecewise tests can't.
    #[test]
    fn stored_plan_applies_end_to_end_by_plan_id() {
        let root = std::env::temp_dir().join(format!(
            "fileid-spool-apply-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let incoming = root.join("incoming");
        std::fs::create_dir_all(&incoming).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let total = 10usize;
        let mut moves = Vec::with_capacity(total);
        for index in 0..total {
            let source = incoming.join(format!("{index}.jpg"));
            std::fs::write(&source, format!("payload-{index}")).unwrap();
            let source_text = source.to_string_lossy().into_owned();
            conn.execute(
                "INSERT INTO files
                    (id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed)
                 VALUES (?1,?2,?3,10,1.0,'image','jpg',0)",
                rusqlite::params![index as i64 + 1, source_text, index as i64 + 1],
            )
            .unwrap();
            moves.push(IpcMove {
                file_id: index as i64 + 1,
                source: source_text,
                destination: root
                    .join("Photos")
                    .join("2024")
                    .join(format!("{index}.jpg"))
                    .to_string_lossy()
                    .into_owned(),
                category: "photo".into(),
                tier: Some("Mixed".into()),
                confidence: "review".into(),
                reason: None,
            });
        }

        let spool_dir = root.join("plans");
        let root_text = root.to_string_lossy().into_owned();
        let (plan_id, _preview) =
            write_stored_plan_in(&spool_dir, &root_text, moves.into_iter().map(Ok), total)
                .unwrap();

        let (stream, stored_total) =
            open_stored_plan_in(&spool_dir, &plan_id, &root_text).unwrap();
        let db = std::sync::Arc::new(parking_lot::Mutex::new(conn));
        let apply = crate::pipeline::restructure_apply::RestructureApply::new(
            db,
            root.clone(),
            false,
        );
        let result = apply.apply_iter(stream, Some(stored_total)).unwrap();

        assert_eq!(result.applied, total as u32, "every spooled move applied");
        assert_eq!(result.failed, 0);
        for index in 0..total {
            let dest = root.join("Photos").join("2024").join(format!("{index}.jpg"));
            assert_eq!(
                std::fs::read_to_string(&dest).unwrap(),
                format!("payload-{index}"),
                "payload moved intact"
            );
            assert!(
                !incoming.join(format!("{index}.jpg")).exists(),
                "source removed after move"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "explicit million-file disk-backed planner regression"]
    fn million_file_large_plan_is_disk_backed_and_preview_bounded() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        for start in (1..=1_000_000_i64).step_by(10_000) {
            let end = (start + 9_999).min(1_000_000);
            conn.execute(
                "WITH RECURSIVE ids(x) AS (
                     SELECT ?1 UNION ALL SELECT x+1 FROM ids WHERE x < ?2
                 )
                 INSERT INTO files
                    (id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed)
                 SELECT x, printf('/library/downloads/%d.jpg',x), x, 1, 1.0,
                        'image','jpg',0 FROM ids",
                rusqlite::params![start, end],
            )
            .unwrap();
        }
        let db = std::sync::Arc::new(parking_lot::Mutex::new(conn));
        let dir = std::env::temp_dir().join(format!(
            "fileid-million-plan-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let started = std::time::Instant::now();
        let plan = plan_large_library_in(&db, "/library", &dir).unwrap();
        assert!(plan.truncated);
        assert_eq!(plan.total_moves, Some(1_000_000));
        assert_eq!(plan.moves.len(), RESTRUCTURE_PREVIEW_CAP);
        assert!(started.elapsed() < std::time::Duration::from_secs(60));
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .all(|entry| !entry.path().to_string_lossy().contains("planning.sqlite")),
            "temporary planning database must be removed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An engine kill mid-plan leaves the `{uuid}.planning.sqlite` scratch DB
    /// behind (the end-of-plan `remove_file` never runs). The next large plan
    /// must sweep those orphans, or they accumulate without bound.
    #[test]
    fn large_plan_sweeps_stale_planning_scratch() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let db = std::sync::Arc::new(parking_lot::Mutex::new(conn));
        let dir = std::env::temp_dir().join(format!(
            "fileid-stale-scratch-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let stale = dir.join(format!("{}.planning.sqlite", uuid::Uuid::new_v4()));
        std::fs::write(&stale, b"orphaned by an engine kill mid-plan").unwrap();

        plan_large_library_in(&db, "/library", &dir).unwrap();

        assert!(
            !stale.exists(),
            "stale planning scratch swept before a new plan"
        );
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .all(|entry| !entry.path().to_string_lossy().contains("planning.sqlite")),
            "no planning scratch left behind after planning"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F-C6-013 dispatch wiring: `handle_apply_restructure` must honor the
    /// cancel flag the CancelScan arm sets. Before the wiring it built a fresh
    /// never-set flag and ignored cancellation entirely — this calls the
    /// dispatch entry with a pre-set flag and asserts NOTHING moves.
    #[tokio::test]
    async fn apply_dispatch_honors_preset_cancel() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let root =
            std::env::temp_dir().join(format!("fileid-dispatch-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("a.jpg");
        std::fs::write(&src, b"data").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, failed) \
             VALUES (1, ?1, 0, 4, 0.0, 'image', 'jpg', 0)",
            rusqlite::params![src.to_string_lossy()],
        )
        .unwrap();
        let db = Arc::new(parking_lot::Mutex::new(conn));

        let dest = root.join("Sorted").join("a.jpg").to_string_lossy().into_owned();
        let payload = ipc::ApplyRestructurePayload {
            library_root: root.to_string_lossy().into_owned(),
            plan_id: None,
            moves: vec![ipc::RestructureMove {
                file_id: 1,
                source: src.to_string_lossy().into_owned(),
                destination: dest,
                category: "Sorted".into(),
                tier: None,
                confidence: String::new(),
                reason: None,
            }],
            use_symlinks: false,
        };

        // Pre-set the cancel flag: a wired dispatch refuses to move anything.
        let cancel = Arc::new(AtomicBool::new(true));
        let (sink, _rx) = Sink::channel_for_test(4);
        handle_apply_restructure(sink, db, payload, cancel).await;

        assert!(src.exists(), "source untouched when apply is cancelled at the dispatch");
        assert!(
            !root.join("Sorted").join("a.jpg").exists(),
            "no move performed under a pre-set cancel"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
