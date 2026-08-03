//! DB read layer + a background loader that streams progress events.
//!
//! Every read goes through `fileid_engine::db::open_read`, the same read
//! surface the CLI uses (`platforms/cli/src/*.rs`), so there is no contract
//! drift: same schema, same column names, same FileKind / restructure logic.
//!
//! The loader runs on a worker thread and pushes [`LoadMsg`] values back to the
//! UI thread over an `mpsc` channel. The status/progress line is driven off
//! that stream — the architecture an engine-spawn-IPC event feed would slot
//! into unchanged (see README "Stubbed").

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender as Sender;

use anyhow::Result;
use rusqlite::params;

use fileid_engine::pipeline::discovery::FileKind;
use fileid_engine::pipeline::restructure::{self, FileForClassify};
use fileid_engine::util::content_hash::{group_exact_duplicates_until, ExactDuplicateCandidate};

/// Max rows pulled into any one list — keeps the snapshot bounded on huge
/// libraries while staying well past what fits on screen.
const ROW_CAP: usize = 5_000;
const DUP_GROUP_CAP: usize = 1_000;
const DUP_MEMBER_CAP: usize = 100;
const DUP_CANDIDATE_CAP: usize = 5_000;
const DUP_READ_BUDGET_BYTES: i64 = 64 * 1024 * 1024 * 1024;
pub(crate) const PLAN_CAP: usize = 3_000;
static ACTIVE_DUPE_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct FileRow {
    pub id: i64,
    pub path: String,
    pub kind: String,
    pub extension: String,
    pub size: i64,
    pub modified: Option<f64>,
    pub has_text: bool,
    pub has_faces: bool,
}

#[derive(Clone)]
pub struct PersonRow {
    pub id: i64,
    pub name: String,
    pub faces: i64,
    pub files: i64,
}

#[derive(Clone)]
pub struct AnalyzeRow {
    pub path: String,
    pub description: String,
    pub proposed_name: Option<String>,
    pub model: Option<String>,
    pub analyzed_at: Option<f64>,
}

#[derive(Clone)]
pub struct DupGroup {
    pub size: i64,
    pub copies: i64,
    pub paths: Vec<String>,
}

/// The result of the (potentially slow) live full-file duplicate verification,
/// delivered via [`LoadMsg::Dupes`] after the snapshot has already painted.
#[derive(Clone, Default)]
pub struct DupeReport {
    pub dupes: Vec<DupGroup>,
    pub dupes_truncated: bool,
    pub dupe_candidate_count: i64,
}

#[derive(Clone)]
pub struct PlanRow {
    pub source: String,
    pub destination: String,
    pub category: String,
    pub confidence: &'static str,
}

/// Everything the UI renders, loaded in one pass. `db_exists == false` means we
/// resolved a path but no library file is there yet (fresh install).
#[derive(Clone, Default)]
pub struct Snapshot {
    pub db_exists: bool,
    pub query: String,
    pub files: Vec<FileRow>,
    pub files_truncated: bool,
    pub people: Vec<PersonRow>,
    pub analyses: Vec<AnalyzeRow>,
    pub analyses_truncated: bool,
    pub dupes: Vec<DupGroup>,
    pub dupes_truncated: bool,
    pub dupe_candidate_count: i64,
    /// True between `Done` and the deferred `Dupes` message: the snapshot has
    /// painted but duplicate verification is still reading files.
    pub dupes_pending: bool,
    pub plan: Vec<PlanRow>,
    pub plan_truncated: bool,
    pub plan_candidate_count: i64,
    pub tags: HashMap<i64, Vec<String>>,
    pub snippets: HashMap<i64, String>,
    pub total_files: i64,
    pub total_tags: i64,
    pub total_analyses: i64,
}

/// Messages streamed from the loader thread to the UI thread.
pub enum LoadMsg {
    Versioned {
        generation: u64,
        message: Box<LoadMsg>,
    },
    Status(String),
    ScanPartial(String),
    /// Structured AI-model install progress, parsed from the CLI's porcelain
    /// `PROGRESS\t{percent}\t{label}` line (see [`crate::models`]): `percent` is
    /// the 0–100 overall figure, `label` a short human string like
    /// `arcface · 182/271 MB · 3.4 MB/s · model 2/9`. Drives the install gauge.
    DownloadProgress {
        percent: u16,
        label: String,
    },
    Done(Box<Snapshot>),
    /// The deferred duplicate verification result — arrives AFTER `Done` so
    /// the Library/People/Restructure tabs paint immediately instead of
    /// waiting out an up-to-64-GiB live full-file hash pass.
    Dupes(Box<DupeReport>),
    Error(String),
}

pub(crate) fn begin_generation(generation: u64) {
    ACTIVE_DUPE_GENERATION.fetch_max(generation, Ordering::AcqRel);
}

pub(crate) fn send_versioned(
    tx: &Sender<LoadMsg>,
    generation: u64,
    message: LoadMsg,
) -> Result<(), std::sync::mpsc::SendError<LoadMsg>> {
    tx.send(LoadMsg::Versioned {
        generation,
        message: Box::new(message),
    })
}

/// Spawn the loader on a worker thread. Non-blocking; the UI keeps drawing.
pub fn spawn_load(db: PathBuf, query: String, tx: Sender<LoadMsg>, generation: u64) {
    std::thread::spawn(move || {
        let _ = send_versioned(
            &tx,
            generation,
            LoadMsg::Status(format!("Opening {}…", short(&db.to_string_lossy()))),
        );
        match load(&db, &query, &tx, generation) {
            Ok(snap) => {
                let file_count = if snap.files_truncated {
                    format!("{}+", snap.files.len())
                } else {
                    snap.files.len().to_string()
                };
                let plan_count = if snap.plan_truncated {
                    format!("{} partial planned moves", snap.plan.len())
                } else {
                    format!("{} planned moves", snap.plan.len())
                };
                let _ = send_versioned(
                    &tx,
                    generation,
                    LoadMsg::Status(format!(
                        "Loaded {} files · {} people · {} analyzed · {}",
                        file_count,
                        snap.people.len(),
                        snap.total_analyses,
                        plan_count
                    )),
                );
                let _ = send_versioned(&tx, generation, LoadMsg::Done(Box::new(snap)));
                run_deferred_dupes(&db, &tx, generation);
            }
            Err(e) => {
                let _ =
                    send_versioned(&tx, generation, LoadMsg::Error(format!("load failed: {e}")));
            }
        }
    });
}

pub(crate) fn load(
    db: &Path,
    query: &str,
    tx: &Sender<LoadMsg>,
    generation: u64,
) -> Result<Snapshot> {
    let query = query.trim().to_string();
    if !db.exists() {
        return Ok(Snapshot {
            db_exists: false,
            query,
            ..Snapshot::default()
        });
    }
    let conn = fileid_engine::db::open_read(db)?;

    let _ = send_versioned(tx, generation, LoadMsg::Status("Reading files…".into()));
    let (files, files_truncated) = load_files(&conn, &query)?;
    let total_files: i64 =
        conn.query_row("SELECT COUNT(*) FROM files WHERE failed = 0", [], |r| {
            r.get(0)
        })?;

    let _ = send_versioned(tx, generation, LoadMsg::Status("Reading tags…".into()));
    let file_ids: Vec<i64> = files.iter().map(|f| f.id).collect();
    let tags = load_tags(&conn, &file_ids)?;
    let snippets = load_snippets(&conn, &file_ids)?;
    let total_tags: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tags t JOIN files f ON f.id = t.file_id WHERE f.failed = 0",
        [],
        |r| r.get(0),
    )?;

    let _ = send_versioned(tx, generation, LoadMsg::Status("Reading people…".into()));
    let people = load_people(&conn)?;

    let _ = send_versioned(
        tx,
        generation,
        LoadMsg::Status("Reading Deep Analyze results…".into()),
    );
    let (analyses, analyses_truncated, total_analyses) = load_analyses(&conn)?;

    let _ = send_versioned(
        tx,
        generation,
        LoadMsg::Status("Computing restructure plan…".into()),
    );
    let (plan, plan_truncated, plan_candidate_count) = compute_plan(&conn)?;

    // Duplicate verification is DEFERRED to run_deferred_dupes / LoadMsg::Dupes:
    // it live-reads up to 64 GiB off disk, and gating the whole snapshot's
    // first paint behind it blanked every tab for minutes on a real corpus.
    Ok(Snapshot {
        db_exists: true,
        query,
        files,
        files_truncated,
        people,
        analyses,
        analyses_truncated,
        dupes: Vec::new(),
        dupes_truncated: false,
        dupe_candidate_count: 0,
        dupes_pending: true,
        plan,
        plan_truncated,
        plan_candidate_count,
        tags,
        snippets,
        total_files,
        total_tags,
        total_analyses,
    })
}

/// Run the bounded live duplicate verification AFTER the snapshot has painted
/// and deliver it as [`LoadMsg::Dupes`]. Every caller that sends `Done` from a
/// [`load`] result must follow with this, or the Cleanup tab stays pending.
pub(crate) fn run_deferred_dupes(db: &Path, tx: &Sender<LoadMsg>, generation: u64) {
    begin_generation(generation);
    if ACTIVE_DUPE_GENERATION.load(Ordering::Acquire) != generation {
        return;
    }
    if !db.exists() {
        let _ = send_versioned(tx, generation, LoadMsg::Dupes(Box::default()));
        return;
    }
    let _ = send_versioned(
        tx,
        generation,
        LoadMsg::Status("Verifying duplicates…".into()),
    );
    let cancelled = || ACTIVE_DUPE_GENERATION.load(Ordering::Acquire) != generation;
    let report = fileid_engine::db::open_read(db).and_then(|conn| {
        let (dupes, dupes_truncated, dupe_candidate_count) = load_dupes_until(&conn, cancelled)?;
        Ok(DupeReport {
            dupes,
            dupes_truncated,
            dupe_candidate_count,
        })
    });
    if cancelled() {
        return;
    }
    match report {
        Ok(report) => {
            let dupe_count = if report.dupes_truncated {
                format!("{} partial duplicate groups", report.dupes.len())
            } else {
                format!("{} duplicate groups", report.dupes.len())
            };
            let _ = send_versioned(
                tx,
                generation,
                LoadMsg::Status(format!("Duplicates verified · {dupe_count}")),
            );
            let _ = send_versioned(tx, generation, LoadMsg::Dupes(Box::new(report)));
        }
        Err(e) => {
            // Deliver an empty report so the Cleanup tab leaves its pending
            // state, then surface the failure on the status row.
            let _ = send_versioned(tx, generation, LoadMsg::Dupes(Box::default()));
            let _ = send_versioned(
                tx,
                generation,
                LoadMsg::Error(format!("duplicate verification failed: {e}")),
            );
        }
    }
}

fn load_files(conn: &rusqlite::Connection, query: &str) -> Result<(Vec<FileRow>, bool)> {
    if !query.is_empty() {
        let mut ids = search_file_ids(conn, query, ROW_CAP + 1)?;
        let truncated = ids.len() > ROW_CAP;
        ids.truncate(ROW_CAP);
        return Ok((load_file_rows(conn, &ids)?, truncated));
    }

    let mut stmt = conn.prepare(
        "SELECT id, path_text, kind, extension, size_bytes, modified_at, has_text, has_faces \
         FROM files WHERE failed = 0 ORDER BY scanned_at DESC, id DESC LIMIT ?1",
    )?;
    let mut rows: Vec<FileRow> = stmt
        .query_map(params![(ROW_CAP + 1) as i64], file_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = rows.len() > ROW_CAP;
    rows.truncate(ROW_CAP);
    Ok((rows, truncated))
}

fn file_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: r.get(0)?,
        path: r.get(1)?,
        kind: r.get(2)?,
        extension: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        size: r.get(4)?,
        modified: r.get(5)?,
        has_text: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
        has_faces: r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
    })
}

fn search_file_ids(conn: &rusqlite::Connection, raw: &str, cap: usize) -> Result<Vec<i64>> {
    let mut ids = Vec::with_capacity(cap);
    let mut seen = HashSet::with_capacity(cap);
    let fts = raw
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ");

    for table in ["doc_fts", "ocr_fts"] {
        if ids.len() >= cap {
            break;
        }
        let sql = format!(
            "SELECT {table}.rowid FROM {table} JOIN files f ON f.id = {table}.rowid \
             WHERE {table} MATCH ?1 AND f.failed = 0 ORDER BY rank LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![fts, cap as i64], |r| r.get::<_, i64>(0))?;
        for row in rows {
            let id = row?;
            if seen.insert(id) {
                ids.push(id);
                if ids.len() >= cap {
                    break;
                }
            }
        }
    }

    if ids.len() < cap {
        let escaped = raw
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let like = format!("%{escaped}%");
        let mut stmt = conn.prepare(
            "SELECT id FROM files \
             WHERE failed = 0 AND lower(COALESCE(path_search, path_text)) LIKE ?1 ESCAPE '\\' \
             ORDER BY scanned_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![like, cap as i64], |r| r.get::<_, i64>(0))?;
        for row in rows {
            let id = row?;
            if seen.insert(id) {
                ids.push(id);
                if ids.len() >= cap {
                    break;
                }
            }
        }
    }
    Ok(ids)
}

fn load_file_rows(conn: &rusqlite::Connection, ids: &[i64]) -> Result<Vec<FileRow>> {
    let mut by_id = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, path_text, kind, extension, size_bytes, modified_at, has_text, has_faces \
             FROM files WHERE failed = 0 AND id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), file_row)?;
        for row in rows {
            let row = row?;
            by_id.insert(row.id, row);
        }
    }
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

fn load_tags(conn: &rusqlite::Connection, file_ids: &[i64]) -> Result<HashMap<i64, Vec<String>>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    for chunk in file_ids.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT file_id, tag FROM tags WHERE file_id IN ({placeholders}) \
             ORDER BY file_id, source, score DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, tag) = row?;
            let tags = out.entry(id).or_default();
            if tags.len() < 8 && !tags.iter().any(|existing| existing == &tag) {
                tags.push(tag);
            }
        }
    }
    Ok(out)
}

fn load_snippets(conn: &rusqlite::Connection, file_ids: &[i64]) -> Result<HashMap<i64, String>> {
    let mut out = HashMap::new();
    for table in ["doc_text", "ocr_text"] {
        for chunk in file_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT file_id, substr(text, 1, 200) FROM {table} \
                 WHERE file_id IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, text) = row?;
                let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !one_line.is_empty() {
                    out.entry(id).or_insert(one_line);
                }
            }
        }
    }
    Ok(out)
}

fn load_people(conn: &rusqlite::Connection) -> Result<Vec<PersonRow>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.first_name, p.last_name, p.is_unknown, \
            (SELECT COUNT(DISTINCT fp.file_id) FROM face_prints fp JOIN files f ON f.id = fp.file_id \
             WHERE fp.person_id = p.id AND f.failed = 0) AS files, \
            (SELECT COUNT(*) FROM face_prints fp JOIN files f ON f.id = fp.file_id \
             WHERE fp.person_id = p.id AND f.failed = 0) AS faces \
         FROM persons p WHERE EXISTS ( \
             SELECT 1 FROM face_prints fp JOIN files f ON f.id = fp.file_id \
             WHERE fp.person_id = p.id AND f.failed = 0 \
         ) ORDER BY faces DESC, p.id ASC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![ROW_CAP as i64], |r| {
            let name: Option<String> = r.get(1)?;
            let first: Option<String> = r.get(2)?;
            let last: Option<String> = r.get(3)?;
            let is_unknown = r.get::<_, Option<i64>>(4)?.unwrap_or(0) != 0;
            Ok(PersonRow {
                id: r.get(0)?,
                name: display_name(name, first, last, is_unknown),
                files: r.get(5)?,
                faces: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn load_analyses(conn: &rusqlite::Connection) -> Result<(Vec<AnalyzeRow>, bool, i64)> {
    let filter = "failed = 0 AND (NULLIF(TRIM(vlm_description), '') IS NOT NULL OR NULLIF(TRIM(vlm_proposed_name), '') IS NOT NULL)";
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM files WHERE {filter}"),
        [],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT id, path_text, COALESCE(vlm_description, ''), \
                NULLIF(TRIM(vlm_proposed_name), ''), NULLIF(TRIM(vlm_model), ''), \
                vlm_analyzed_at \
         FROM files WHERE {filter} \
         ORDER BY vlm_analyzed_at DESC, id DESC LIMIT ?1"
    ))?;
    let mut rows = stmt
        .query_map(params![(ROW_CAP + 1) as i64], |row| {
            Ok(AnalyzeRow {
                path: row.get(1)?,
                description: row.get::<_, String>(2)?.trim().to_string(),
                proposed_name: row.get(3)?,
                model: row.get(4)?,
                analyzed_at: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = rows.len() > ROW_CAP;
    rows.truncate(ROW_CAP);
    Ok((rows, truncated, total))
}

fn display_name(
    name: Option<String>,
    first: Option<String>,
    last: Option<String>,
    is_unknown: bool,
) -> String {
    if let Some(n) = name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return n.to_string();
    }
    let composed = [first.as_deref(), last.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !composed.is_empty() {
        composed
    } else if is_unknown {
        "Unknown".to_string()
    } else {
        "Unnamed".to_string()
    }
}

#[cfg(test)]
fn load_dupes(conn: &rusqlite::Connection) -> Result<(Vec<DupGroup>, bool, i64)> {
    load_dupes_until(conn, || false)
}

fn load_dupes_until(
    conn: &rusqlite::Connection,
    should_cancel: impl Fn() -> bool,
) -> Result<(Vec<DupGroup>, bool, i64)> {
    let candidate_stats: (i64, i64) = conn.query_row(
        "WITH candidate_sizes AS ( \
             SELECT size_bytes FROM files WHERE failed = 0 AND content_hash IS NOT NULL \
             GROUP BY size_bytes HAVING COUNT(*) > 1 \
         ) \
         SELECT COUNT(*), COALESCE(SUM(MAX(f.size_bytes, 0)), 0) FROM files f \
         JOIN candidate_sizes s ON s.size_bytes = f.size_bytes \
         WHERE f.failed = 0 AND f.content_hash IS NOT NULL",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let candidate_count = candidate_stats.0;
    let mut stmt = conn.prepare(
        "WITH candidate_sizes AS ( \
             SELECT size_bytes FROM files WHERE failed = 0 AND content_hash IS NOT NULL \
             GROUP BY size_bytes HAVING COUNT(*) > 1 \
         ) \
         SELECT f.id, f.path_text, f.size_bytes \
         FROM files f JOIN candidate_sizes s ON s.size_bytes = f.size_bytes \
         WHERE f.failed = 0 AND f.content_hash IS NOT NULL \
         ORDER BY f.size_bytes, f.path_text, f.id LIMIT ?1",
    )?;
    let queried = stmt
        .query_map(params![DUP_CANDIDATE_CAP as i64], |row| {
            Ok(ExactDuplicateCandidate {
                id: row.get(0)?,
                path: PathBuf::from(row.get::<_, String>(1)?),
                indexed_size: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut selected_bytes = 0i64;
    let mut candidates = Vec::with_capacity(queried.len());
    for candidate in queried {
        let bytes = candidate.indexed_size.max(0);
        if bytes > DUP_READ_BUDGET_BYTES - selected_bytes {
            break;
        }
        selected_bytes += bytes;
        candidates.push(candidate);
    }
    let grouping = group_exact_duplicates_until(candidates, should_cancel);
    let skipped = grouping.skipped;
    let mut groups: Vec<DupGroup> = grouping
        .groups
        .into_iter()
        .map(|group| DupGroup {
            size: i64::try_from(group.size).unwrap_or(i64::MAX),
            copies: group.files.len() as i64,
            paths: group
                .files
                .into_iter()
                .take(DUP_MEMBER_CAP)
                .map(|file| file.path.to_string_lossy().into_owned())
                .collect(),
        })
        .collect();
    groups.sort_by(|a, b| b.copies.cmp(&a.copies).then_with(|| a.paths.cmp(&b.paths)));
    let candidate_truncated = candidate_count > DUP_CANDIDATE_CAP as i64;
    let byte_truncated = candidate_stats.1 > DUP_READ_BUDGET_BYTES;
    let group_truncated = groups.len() > DUP_GROUP_CAP;
    groups.truncate(DUP_GROUP_CAP);
    Ok((
        groups,
        candidate_truncated || byte_truncated || group_truncated || skipped > 0,
        candidate_count,
    ))
}

/// Build a read-only restructure preview by feeding indexed file metadata into
/// the engine's pure `restructure::classify` — the identical rule cascade the
/// desktop apps and CLI use. Read-only: nothing is moved.
fn compute_plan(conn: &rusqlite::Connection) -> Result<(Vec<PlanRow>, bool, i64)> {
    let (root, candidate_count) = plan_root_and_count(conn)?;
    if candidate_count == 0 {
        return Ok((Vec::new(), false, 0));
    }

    let mut stmt = conn.prepare(
        "SELECT f.id, f.path_text, f.extension, f.modified_at, f.created_at, \
            f.location_lat, f.location_lon, f.has_text, \
            (SELECT p.name FROM face_prints fp JOIN persons p ON p.id = fp.person_id \
             WHERE fp.file_id = f.id LIMIT 1) AS person_name \
         FROM files f WHERE f.failed = 0 ORDER BY f.scanned_at DESC, f.id DESC LIMIT ?1",
    )?;
    let files: Vec<FileForClassify> = stmt
        .query_map(params![PLAN_CAP as i64], |r| {
            let path: String = r.get(1)?;
            let ext: Option<String> = r.get(2)?;
            Ok(FileForClassify {
                file_id: r.get(0)?,
                source: PathBuf::from(path),
                kind: FileKind::from_extension(ext.as_deref().unwrap_or("")),
                modified_unix: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                created_unix: r.get(4)?,
                person_name: r.get(8)?,
                location_lat: r.get(5)?,
                location_lon: r.get(6)?,
                has_text: r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let plan = restructure::classify(&files, &root)
        .into_iter()
        .map(|m| PlanRow {
            source: m.source.to_string_lossy().into_owned(),
            destination: m.destination.to_string_lossy().into_owned(),
            category: m.category,
            confidence: m.confidence.as_str(),
        })
        .collect();
    Ok((plan, candidate_count > PLAN_CAP as i64, candidate_count))
}

fn plan_root_and_count(conn: &rusqlite::Connection) -> Result<(PathBuf, i64)> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM files WHERE failed = 0", [], |row| {
        row.get(0)
    })?;
    if count == 0 {
        return Ok((
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            0,
        ));
    }
    // The longest common string prefix of a sorted set equals that of its first
    // and last members; comparing path components of those indexed extremes is
    // therefore exact without transferring every catalog path into the TUI.
    let first: String = conn.query_row(
        "SELECT path_text FROM files WHERE failed = 0 ORDER BY path_text ASC LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let last: String = conn.query_row(
        "SELECT path_text FROM files WHERE failed = 0 ORDER BY path_text DESC LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let first = PathBuf::from(first);
    let last = PathBuf::from(last);
    Ok((
        common_ancestor([first.as_path(), last.as_path()].into_iter()),
        count,
    ))
}

/// Longest shared directory prefix of all sources; falls back to the current
/// working directory when there's nothing in common.
fn common_ancestor<'a>(paths: impl Iterator<Item = &'a Path>) -> PathBuf {
    let mut prefix: Option<Vec<Component<'a>>> = None;
    for p in paths {
        let parent = p.parent().unwrap_or(p);
        let comps: Vec<Component> = parent.components().collect();
        prefix = Some(match prefix {
            None => comps,
            Some(prev) => {
                let n = prev.iter().zip(&comps).take_while(|(a, b)| a == b).count();
                prev.into_iter().take(n).collect()
            }
        });
    }
    match prefix {
        Some(comps) if !comps.is_empty() => comps.into_iter().map(Component::as_os_str).collect(),
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// Trim a long absolute path for one-line display: keep the last few segments.
pub fn short(path: &str) -> String {
    let collapsed: String = crate::context::collapse_home(path)
        .chars()
        .map(|ch| {
            if matches!(ch, '\n' | '\r' | '\t') {
                ' '
            } else if ch.is_control() {
                '\u{fffd}'
            } else {
                ch
            }
        })
        .collect();
    const MAX: usize = 64;
    if collapsed.chars().count() <= MAX {
        return collapsed;
    }
    let tail: String = collapsed
        .chars()
        .rev()
        .take(MAX - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("…{tail}")
}

/// Human-readable byte size (mirrors the CLI's `human_size`).
pub fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fileid-tui-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn insert_file(conn: &rusqlite::Connection, path: &str, scanned_at: f64) -> i64 {
        conn.execute(
            "INSERT INTO files \
             (path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension, path_search) \
             VALUES (?1, ?2, 1, ?3, ?3, 'doc', 'txt', ?1)",
            params![path, scanned_at as i64, scanned_at],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn human_size_scales() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn common_ancestor_of_siblings() {
        let a = PathBuf::from("/home/u/Pictures/2021/a.jpg");
        let b = PathBuf::from("/home/u/Pictures/2022/b.jpg");
        let root = common_ancestor([a.as_path(), b.as_path()].into_iter());
        assert_eq!(root, PathBuf::from("/home/u/Pictures"));
    }

    #[test]
    fn short_keeps_tail() {
        let s = short(&"/very/long/path/that/keeps/going/and/going/file.txt".repeat(3));
        assert!(s.starts_with('…'));
        assert!(s.chars().count() <= 64);
    }

    #[test]
    fn search_reaches_content_outside_the_recent_snapshot() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        let target = insert_file(&conn, "/archive/old-report.txt", 0.0);
        conn.execute(
            "INSERT INTO doc_text (file_id, text) VALUES (?1, 'needle quarterly report')",
            params![target],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        for i in 0..=ROW_CAP {
            insert_file(&tx, &format!("/recent/filler-{i}.txt"), (i + 1) as f64);
        }
        tx.commit().unwrap();

        let (recent, truncated) = load_files(&conn, "").unwrap();
        assert!(truncated);
        assert!(!recent.iter().any(|f| f.id == target));

        let (matches, truncated) = load_files(&conn, "needle").unwrap();
        assert!(!truncated);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, target);
    }

    #[test]
    fn deep_analyze_results_are_loaded_and_failed_rows_are_hidden() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        let active = insert_file(&conn, "/library/photo.jpg", 2.0);
        let failed = insert_file(&conn, "/library/failed.jpg", 1.0);
        conn.execute(
            "UPDATE files SET vlm_description = 'A quiet lake', \
                              vlm_proposed_name = 'quiet-lake.jpg', \
                              vlm_model = 'qwen', vlm_analyzed_at = 123 \
             WHERE id = ?1",
            params![active],
        )
        .unwrap();
        conn.execute(
            "UPDATE files SET failed = 1, vlm_description = 'must stay hidden' WHERE id = ?1",
            params![failed],
        )
        .unwrap();

        let (rows, truncated, total) = load_analyses(&conn).unwrap();
        assert!(!truncated);
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "/library/photo.jpg");
        assert_eq!(rows[0].description, "A quiet lake");
        assert_eq!(rows[0].proposed_name.as_deref(), Some("quiet-lake.jpg"));
    }

    #[test]
    fn failed_rows_are_hidden_from_library_search_and_planning() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        let active = insert_file(&conn, "/library/active.txt", 2.0);
        let missing = insert_file(&conn, "/library/missing.txt", 1.0);
        conn.execute(
            "UPDATE files SET failed = 1, error_message = 'missing' WHERE id = ?1",
            params![missing],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO doc_text(file_id, text) VALUES (?1, 'hidden needle')",
            params![missing],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO persons (id, name, file_count, created_at) VALUES \
             (10, 'Visible', 99, 0), (20, 'Hidden', 99, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO face_prints (file_id, person_id, print_data, bbox) VALUES \
             (?1, 10, X'00', '0,0,1,1'), (?1, 10, X'00', '0,0,1,1'), \
             (?2, 10, X'00', '0,0,1,1'), (?2, 20, X'00', '0,0,1,1')",
            params![active, missing],
        )
        .unwrap();

        let (files, _) = load_files(&conn, "").unwrap();
        assert_eq!(
            files.iter().map(|file| file.id).collect::<Vec<_>>(),
            vec![active]
        );
        let (matches, _) = load_files(&conn, "needle").unwrap();
        assert!(matches.is_empty());
        let (_, count) = plan_root_and_count(&conn).unwrap();
        assert_eq!(count, 1);
        let people = load_people(&conn).unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].id, 10);
        assert_eq!(people[0].files, 1);
        assert_eq!(people[0].faces, 2);
    }

    #[test]
    fn required_query_failure_propagates_from_load() {
        let path = std::env::temp_dir().join(format!(
            "fileid-tui-load-error-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let conn = rusqlite::Connection::open(&path).unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        conn.execute_batch("DROP TABLE persons;").unwrap();
        drop(conn);

        let (tx, _rx) = std::sync::mpsc::sync_channel(1_024);
        let error = load(&path, "", &tx, 1)
            .err()
            .expect("missing table must fail load");
        assert!(error.to_string().contains("no such table"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn restructure_preview_marks_cap_and_uses_full_library_root() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        let tx = conn.transaction().unwrap();
        for i in 0..5 {
            insert_file(&tx, &format!("/library/a/old-{i}.txt"), (i + 1) as f64);
        }
        for i in 0..PLAN_CAP {
            insert_file(&tx, &format!("/library/b/recent-{i}.txt"), (i + 100) as f64);
        }
        tx.commit().unwrap();

        let (root, count) = plan_root_and_count(&conn).unwrap();
        assert_eq!(root, PathBuf::from("/library"));
        assert_eq!(count, (PLAN_CAP + 5) as i64);
        let (plan, truncated, candidates) = compute_plan(&conn).unwrap();
        assert!(truncated);
        assert_eq!(candidates, count);
        assert!(!plan.is_empty());
    }

    #[test]
    fn duplicate_snapshot_caps_paths_but_keeps_the_real_copy_count() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        let dir = temp_dir("duplicate-cap");
        for i in 0..(DUP_MEMBER_CAP + 25) {
            let path = dir.join(format!("file-{i}.bin"));
            std::fs::write(&path, b"x").unwrap();
            let id = insert_file(&conn, &path.to_string_lossy(), i as f64);
            conn.execute(
                "UPDATE files SET content_hash = X'01020304' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        let (groups, truncated, candidates) = load_dupes(&conn).unwrap();
        assert!(!truncated);
        assert_eq!(candidates, (DUP_MEMBER_CAP + 25) as i64);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].copies, (DUP_MEMBER_CAP + 25) as i64);
        assert_eq!(groups[0].paths.len(), DUP_MEMBER_CAP);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn duplicate_snapshot_merges_different_stored_hash_recipes() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        let dir = temp_dir("mixed-recipes");
        for (i, stored) in [vec![1u8; 32], vec![2u8; 32]].into_iter().enumerate() {
            let path = dir.join(format!("copy-{i}.bin"));
            std::fs::write(&path, b"same").unwrap();
            let id = insert_file(&conn, &path.to_string_lossy(), i as f64);
            conn.execute(
                "UPDATE files SET size_bytes = 4, content_hash = ?2 WHERE id = ?1",
                params![id, stored],
            )
            .unwrap();
        }
        let (groups, truncated, candidates) = load_dupes(&conn).unwrap();
        assert!(!truncated);
        assert_eq!(candidates, 2);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].copies, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn duplicate_snapshot_bounds_full_file_hash_candidates() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        let tx = conn.transaction().unwrap();
        for i in 0..=DUP_CANDIDATE_CAP {
            let id = insert_file(&tx, &format!("/missing/copy-{i}.bin"), i as f64);
            tx.execute(
                "UPDATE files SET content_hash = X'01' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let (groups, truncated, candidates) = load_dupes(&conn).unwrap();
        assert!(groups.is_empty());
        assert!(truncated);
        assert_eq!(candidates, (DUP_CANDIDATE_CAP + 1) as i64);
    }

    #[test]
    fn duplicate_snapshot_bounds_full_file_hash_bytes() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        for id in [1i64, 2] {
            conn.execute(
                "INSERT INTO files \
                 (id, path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension, content_hash) \
                 VALUES (?1, printf('/huge-%d.bin', ?1), ?1, ?2, 0, 0, 'other', '', X'01')",
                params![id, DUP_READ_BUDGET_BYTES + 1],
            )
            .unwrap();
        }
        let (groups, truncated, candidates) = load_dupes(&conn).unwrap();
        assert!(groups.is_empty());
        assert!(truncated);
        assert_eq!(candidates, 2);
    }

    #[test]
    #[ignore = "million-row scale regression; run explicitly"]
    fn million_file_snapshot_is_bounded() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        conn.execute_batch(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x < 1000000) \
             INSERT INTO files \
             (path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension, path_search) \
             SELECT printf('/million/file-%07d.jpg', x), x, 1, x, x, 'image', 'jpg', \
                    printf('/million/file-%07d.jpg', x) FROM n;",
        )
        .unwrap();
        let started = std::time::Instant::now();
        let (files, truncated) = load_files(&conn, "").unwrap();
        assert_eq!(files.len(), ROW_CAP);
        assert!(truncated);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
    /// The first paint must not wait on duplicate verification: load() returns
    /// a pending snapshot with no dupes, and the deferred pass delivers them
    /// via LoadMsg::Dupes afterward.
    #[test]
    fn snapshot_paints_before_deferred_duplicate_verification() {
        let dir = temp_dir("deferred-dupes");
        let db_path = dir.join("lib.sqlite");
        {
            let conn = fileid_engine::db::open_writer(&db_path).unwrap();
            let a = dir.join("a.bin");
            let b = dir.join("b.bin");
            std::fs::write(&a, b"same-bytes").unwrap();
            std::fs::write(&b, b"same-bytes").unwrap();
            for p in [&a, &b] {
                conn.execute(
                    "INSERT INTO files \
                     (path_text, path_hash, size_bytes, modified_at, scanned_at, kind, \
                      extension, path_search, content_hash) \
                     VALUES (?1, 1, 10, 1.0, 1.0, 'doc', 'bin', ?1, x'01')",
                    params![p.to_string_lossy()],
                )
                .unwrap();
            }
        }

        let (tx, rx) = std::sync::mpsc::sync_channel(1_024);
        let snap = load(&db_path, "", &tx, 1).unwrap();
        assert!(
            snap.dupes_pending,
            "snapshot must paint in the pending state"
        );
        assert!(snap.dupes.is_empty(), "no dupes before the deferred pass");

        run_deferred_dupes(&db_path, &tx, 1);
        let report = loop {
            match rx.try_recv().expect("deferred pass must send messages") {
                LoadMsg::Versioned {
                    generation: 1,
                    message,
                } => match *message {
                    LoadMsg::Dupes(report) => break report,
                    _ => continue,
                },
                _ => continue,
            }
        };
        assert_eq!(report.dupes.len(), 1, "the byte-identical pair groups");
        assert_eq!(report.dupes[0].copies, 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
