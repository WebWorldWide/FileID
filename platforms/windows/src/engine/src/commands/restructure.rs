//! Restructure tab handlers: plan (classify + propose moves) and apply
//! (execute on disk + update DB). The actual file-move machinery lives in
//! `pipeline::restructure_apply`; classification logic lives in
//! `pipeline::restructure`. These handlers wire app payloads through to
//! those modules.

use std::path::PathBuf;

use crate::ipc::{
    self, sink::Sink, EngineError, EventPayload, FolderClassificationCounts, IpcEvent,
    RestructureCategoryCount, RestructureMove as IpcMove, RestructurePlan, Wrap,
};
use crate::pipeline::discovery::FileKind;
use crate::pipeline::restructure::{self, classify, FileForClassify, FolderClassification};
use crate::pipeline::restructure_apply::RestructureApply;
use crate::pipeline::restructure_semantic;

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
 WHERE f.failed = 0";

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
    let db_for_semantic = std::sync::Arc::clone(&db);
    let files: Vec<FileForClassify> =
        match tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<FileForClassify>> {
            let conn = db.lock();
            let mut stmt = conn.prepare(PLAN_FILES_SQL)?;
            let rows = stmt.query_map([], |row| {
                let kind_str: String = row.get(2)?;
                let kind = match kind_str.as_str() {
                    "image" => FileKind::Image,
                    "video" => FileKind::Video,
                    "pdf" => FileKind::Pdf,
                    "doc" => FileKind::Doc,
                    "audio" => FileKind::Audio,
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
                    message: format!("Couldn't read files table: {err}"),
                    path: None,
                    model_kind: None,
                }))))
                .await;
                return;
            }
            Err(err) => {
                tracing::warn!(?err, "planRestructure spawn_blocking failed");
                return;
            }
        };

    let library_root_path = std::path::Path::new(&library_root);

    // Butler P1: semantic + learn-your-style classification for image files that
    // have a CLIP embedding; everything else (and density-clustering noise)
    // falls back to the rule cascade. See pipeline/restructure_semantic.rs.
    let signals = tokio::task::spawn_blocking(
        move || -> rusqlite::Result<(
            std::collections::HashMap<i64, Vec<f32>>,
            std::collections::HashMap<i64, Vec<String>>,
        )> {
            let conn = db_for_semantic.lock();
            let mut embeddings = std::collections::HashMap::new();
            let mut stmt = conn.prepare(
                "SELECT ce.file_id, ce.embedding FROM clip_embeddings ce
                 JOIN files f ON f.id = ce.file_id
                 WHERE f.failed = 0 AND f.kind = 'image'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            for r in rows {
                let (id, blob) = r?;
                if !blob.is_empty() && blob.len() % 4 == 0 {
                    let v = blob
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect();
                    embeddings.insert(id, v);
                }
            }
            let mut tags: std::collections::HashMap<i64, Vec<String>> =
                std::collections::HashMap::new();
            // DISTINCT so a tag carried under multiple sources for the same
            // file counts ONCE — otherwise c-TF-IDF tf/df double-counts it and
            // skews distinctive_terms group naming (#18).
            let mut tstmt =
                conn.prepare("SELECT DISTINCT file_id, tag FROM tags WHERE source IN ('auto','vlm','user')")?;
            let trows =
                tstmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;
            for r in trows {
                let (id, tag) = r?;
                tags.entry(id).or_default().push(tag);
            }
            Ok((embeddings, tags))
        },
    )
    .await;
    let (embeddings, tags_map) = match signals {
        Ok(Ok(v)) => v,
        _ => (std::collections::HashMap::new(), std::collections::HashMap::new()),
    };

    let semantic_files: Vec<restructure_semantic::SemanticFile> = files
        .iter()
        .filter(|f| matches!(f.kind, FileKind::Image))
        .filter_map(|f| {
            embeddings.get(&f.file_id).map(|clip| restructure_semantic::SemanticFile {
                file_id: f.file_id,
                source: f.source.clone(),
                clip: clip.clone(),
                tags: tags_map.get(&f.file_id).cloned().unwrap_or_default(),
                time_unix: f.created_unix.unwrap_or(f.modified_unix),
            })
        })
        .collect();

    let proposed = if semantic_files.len() >= 2 {
        let protos = restructure_semantic::folder_prototypes(&semantic_files, 4);
        let moves = restructure_semantic::semantic_classify(&semantic_files, &protos, library_root_path);
        let moved: std::collections::HashSet<i64> = moves.iter().map(|m| m.file_id).collect();
        let rule_files: Vec<FileForClassify> =
            files.iter().filter(|f| !moved.contains(&f.file_id)).cloned().collect();
        let mut out = moves;
        out.extend(classify(&rule_files, library_root_path));
        out
    } else {
        classify(&files, library_root_path)
    };
    let category_summary = restructure::category_counts(&proposed);

    // Engine-authoritative folder classification.
    let folder_class = restructure::classify_folders(&proposed);
    let mut anchor = 0u32;
    let mut mixed = 0u32;
    let mut junk = 0u32;
    // Index classification by source folder so per-move tiers can be
    // stamped without re-classifying.
    let mut tier_by_folder: std::collections::HashMap<PathBuf, &'static str> =
        std::collections::HashMap::with_capacity(folder_class.len());
    for f in &folder_class {
        let tier_label = match f.classification {
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

    let plan = RestructurePlan {
        library_root,
        moves: proposed
            .into_iter()
            .map(|m| {
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
            })
            .collect(),
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
pub(crate) async fn handle_apply_restructure(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::ApplyRestructurePayload,
) {
    let result = tokio::task::spawn_blocking(
        move || -> anyhow::Result<ipc::RestructureApplyResult> {
            let apply = RestructureApply::new(
                db,
                PathBuf::from(payload.library_root),
                payload.use_symlinks,
            );
            apply.apply(&payload.moves)
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
            tracing::warn!(?err, "applyRestructure spawn_blocking failed");
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
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(8)?)))
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
}
