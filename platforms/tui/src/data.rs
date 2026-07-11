//! DB read layer + a background loader that streams progress events.
//!
//! Every read goes through `fileid_engine::db::open_read` — the SAME read
//! surface the CLI uses (`platforms/cli/src/*.rs`) — so there is no contract
//! drift: same schema, same column names, same FileKind / restructure logic.
//!
//! The loader runs on a worker thread and pushes [`LoadMsg`] values back to the
//! UI thread over an `mpsc` channel. The status/progress line is driven off
//! that stream — the architecture an engine-spawn-IPC event feed would slot
//! into unchanged (see README "Stubbed").

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;
use rusqlite::params;

use fileid_engine::pipeline::discovery::FileKind;
use fileid_engine::pipeline::restructure::{self, FileForClassify};

/// Max rows pulled into any one list — keeps the snapshot bounded on huge
/// libraries while staying well past what fits on screen.
const ROW_CAP: usize = 5_000;
const DUP_GROUP_CAP: usize = 1_000;
const DUP_MEMBER_CAP: usize = 100;

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
pub struct DupGroup {
    pub size: i64,
    pub copies: i64,
    pub paths: Vec<String>,
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
    pub dupes: Vec<DupGroup>,
    pub plan: Vec<PlanRow>,
    pub tags: HashMap<i64, Vec<String>>,
    pub snippets: HashMap<i64, String>,
    pub total_files: i64,
    pub total_tags: i64,
}

/// Messages streamed from the loader thread to the UI thread.
pub enum LoadMsg {
    Status(String),
    /// Structured AI-model install progress, parsed from the CLI's porcelain
    /// `PROGRESS\t{percent}\t{label}` line (see [`crate::models`]): `percent` is
    /// the 0–100 overall figure, `label` a short human string like
    /// `arcface · 182/271 MB · 3.4 MB/s · model 2/9`. Drives the install gauge.
    DownloadProgress {
        percent: u16,
        label: String,
    },
    Done(Box<Snapshot>),
    Error(String),
}

/// Spawn the loader on a worker thread. Non-blocking; the UI keeps drawing.
pub fn spawn_load(db: PathBuf, query: String, tx: Sender<LoadMsg>) {
    std::thread::spawn(move || {
        let _ = tx.send(LoadMsg::Status(format!(
            "Opening {}…",
            short(&db.to_string_lossy())
        )));
        match load(&db, &query, &tx) {
            Ok(snap) => {
                let file_count = if snap.files_truncated {
                    format!("{}+", snap.files.len())
                } else {
                    snap.files.len().to_string()
                };
                let _ = tx.send(LoadMsg::Status(format!(
                    "Loaded {} files · {} people · {} duplicate groups · {} planned moves",
                    file_count,
                    snap.people.len(),
                    snap.dupes.len(),
                    snap.plan.len()
                )));
                let _ = tx.send(LoadMsg::Done(Box::new(snap)));
            }
            Err(e) => {
                let _ = tx.send(LoadMsg::Error(format!("load failed: {e}")));
            }
        }
    });
}

pub(crate) fn load(db: &Path, query: &str, tx: &Sender<LoadMsg>) -> Result<Snapshot> {
    let query = query.trim().to_string();
    if !db.exists() {
        return Ok(Snapshot {
            db_exists: false,
            query,
            ..Snapshot::default()
        });
    }
    let conn = fileid_engine::db::open_read(db)?;

    let _ = tx.send(LoadMsg::Status("Reading files…".into()));
    let (files, files_truncated) = load_files(&conn, &query)?;
    let total_files: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap_or(files.len() as i64);

    let _ = tx.send(LoadMsg::Status("Reading tags…".into()));
    let file_ids: Vec<i64> = files.iter().map(|f| f.id).collect();
    let tags = load_tags(&conn, &file_ids);
    let snippets = load_snippets(&conn, &file_ids);
    let total_tags: i64 = conn
        .query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))
        .unwrap_or(0);

    let _ = tx.send(LoadMsg::Status("Reading people…".into()));
    let people = load_people(&conn).unwrap_or_default();

    let _ = tx.send(LoadMsg::Status("Grouping duplicates…".into()));
    let dupes = load_dupes(&conn).unwrap_or_default();

    let _ = tx.send(LoadMsg::Status("Computing restructure plan…".into()));
    let plan = compute_plan(&conn).unwrap_or_default();

    Ok(Snapshot {
        db_exists: true,
        query,
        files,
        files_truncated,
        people,
        dupes,
        plan,
        tags,
        snippets,
        total_files,
        total_tags,
    })
}

fn load_files(conn: &rusqlite::Connection, query: &str) -> Result<(Vec<FileRow>, bool)> {
    if !query.is_empty() {
        let mut ids = search_file_ids(conn, query, ROW_CAP + 1);
        let truncated = ids.len() > ROW_CAP;
        ids.truncate(ROW_CAP);
        return Ok((load_file_rows(conn, &ids)?, truncated));
    }

    let mut stmt = conn.prepare(
        "SELECT id, path_text, kind, extension, size_bytes, modified_at, has_text, has_faces \
         FROM files ORDER BY scanned_at DESC, id DESC LIMIT ?1",
    )?;
    let mut rows: Vec<FileRow> = stmt
        .query_map(params![(ROW_CAP + 1) as i64], file_row)?
        .filter_map(Result::ok)
        .collect();
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

fn search_file_ids(conn: &rusqlite::Connection, raw: &str, cap: usize) -> Vec<i64> {
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
        let sql =
            format!("SELECT rowid FROM {table} WHERE {table} MATCH ?1 ORDER BY rank LIMIT ?2");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map(params![fts, cap as i64], |r| r.get::<_, i64>(0)) {
                for id in rows.flatten() {
                    if seen.insert(id) {
                        ids.push(id);
                        if ids.len() >= cap {
                            break;
                        }
                    }
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
        if let Ok(mut stmt) = conn.prepare(
            "SELECT id FROM files \
             WHERE lower(COALESCE(path_search, path_text)) LIKE ?1 ESCAPE '\\' \
             ORDER BY scanned_at DESC, id DESC LIMIT ?2",
        ) {
            if let Ok(rows) = stmt.query_map(params![like, cap as i64], |r| r.get::<_, i64>(0)) {
                for id in rows.flatten() {
                    if seen.insert(id) {
                        ids.push(id);
                        if ids.len() >= cap {
                            break;
                        }
                    }
                }
            }
        }
    }
    ids
}

fn load_file_rows(conn: &rusqlite::Connection, ids: &[i64]) -> Result<Vec<FileRow>> {
    let mut by_id = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, path_text, kind, extension, size_bytes, modified_at, has_text, has_faces \
             FROM files WHERE id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        for row in stmt
            .query_map(rusqlite::params_from_iter(chunk.iter()), file_row)?
            .flatten()
        {
            by_id.insert(row.id, row);
        }
    }
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

fn load_tags(conn: &rusqlite::Connection, file_ids: &[i64]) -> HashMap<i64, Vec<String>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    for chunk in file_ids.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT file_id, tag FROM tags WHERE file_id IN ({placeholders}) \
             ORDER BY file_id, source, score DESC"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            continue;
        };
        if let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        }) {
            for (id, tag) in rows.flatten() {
                let v = out.entry(id).or_default();
                if v.len() < 8 && !v.iter().any(|t| t == &tag) {
                    v.push(tag);
                }
            }
        };
    }
    out
}

fn load_snippets(conn: &rusqlite::Connection, file_ids: &[i64]) -> HashMap<i64, String> {
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
            let Ok(mut stmt) = conn.prepare(&sql) else {
                continue;
            };
            if let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            }) {
                for (id, text) in rows.flatten() {
                    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !one_line.is_empty() {
                        out.entry(id).or_insert(one_line);
                    }
                }
            };
        }
    }
    out
}

fn load_people(conn: &rusqlite::Connection) -> Result<Vec<PersonRow>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.first_name, p.last_name, p.is_unknown, p.file_count, \
            (SELECT COUNT(*) FROM face_prints fp WHERE fp.person_id = p.id) AS faces \
         FROM persons p ORDER BY faces DESC, p.id ASC LIMIT ?1",
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
        .filter_map(Result::ok)
        .collect();
    Ok(rows)
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

fn load_dupes(conn: &rusqlite::Connection) -> Result<Vec<DupGroup>> {
    let mut stmt = conn.prepare(
        "SELECT content_hash, MAX(size_bytes), COUNT(*) AS copies \
         FROM files WHERE content_hash IS NOT NULL GROUP BY content_hash \
         HAVING COUNT(*) > 1 ORDER BY copies DESC, content_hash LIMIT ?1",
    )?;
    let meta: Vec<(Vec<u8>, i64, i64)> = stmt
        .query_map(params![DUP_GROUP_CAP as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .flatten()
        .collect();
    let mut path_stmt = conn.prepare(
        "SELECT path_text FROM files WHERE content_hash = ?1 ORDER BY path_text LIMIT ?2",
    )?;
    let mut groups = Vec::with_capacity(meta.len());
    for (blob, size, copies) in meta {
        let paths = path_stmt
            .query_map(params![blob, DUP_MEMBER_CAP as i64], |r| {
                r.get::<_, String>(0)
            })?
            .flatten()
            .collect();
        groups.push(DupGroup {
            size,
            copies,
            paths,
        });
    }
    Ok(groups)
}

/// Build a read-only restructure preview by feeding indexed file metadata into
/// the engine's pure `restructure::classify` — the identical rule cascade the
/// desktop apps and CLI use. Read-only: nothing is moved.
fn compute_plan(conn: &rusqlite::Connection) -> Result<Vec<PlanRow>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path_text, f.extension, f.modified_at, f.created_at, \
            f.location_lat, f.location_lon, f.has_text, \
            (SELECT p.name FROM face_prints fp JOIN persons p ON p.id = fp.person_id \
             WHERE fp.file_id = f.id LIMIT 1) AS person_name \
         FROM files f ORDER BY f.scanned_at DESC, f.id DESC LIMIT 3000",
    )?;
    let files: Vec<FileForClassify> = stmt
        .query_map([], |r| {
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
        .filter_map(Result::ok)
        .collect();

    if files.is_empty() {
        return Ok(Vec::new());
    }
    let root = common_ancestor(files.iter().map(|f| f.source.as_path()));
    let moves = restructure::classify(&files, &root);
    let plan = moves
        .into_iter()
        .map(|m| PlanRow {
            source: m.source.to_string_lossy().into_owned(),
            destination: m.destination.to_string_lossy().into_owned(),
            category: m.category,
            confidence: m.confidence.as_str(),
        })
        .collect();
    Ok(plan)
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
    let collapsed = crate::context::collapse_home(path);
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
    fn duplicate_snapshot_caps_paths_but_keeps_the_real_copy_count() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        for i in 0..(DUP_MEMBER_CAP + 25) {
            let id = insert_file(&conn, &format!("/copies/file-{i}.bin"), i as f64);
            conn.execute(
                "UPDATE files SET content_hash = X'01020304' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        let groups = load_dupes(&conn).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].copies, (DUP_MEMBER_CAP + 25) as i64);
        assert_eq!(groups[0].paths.len(), DUP_MEMBER_CAP);
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
}
