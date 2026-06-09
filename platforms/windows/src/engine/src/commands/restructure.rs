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
    let files: Vec<FileForClassify> =
        match tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<FileForClassify>> {
            let conn = db.lock();
            // SQLite forbids GROUP_CONCAT(DISTINCT expr, separator) — the
            // DISTINCT form takes exactly ONE argument, so the old query failed
            // at prepare() time and the whole Restructure planner was dead.
            // Dedup (file_id, name) in a derived table, then aggregate with the
            // single-argument-plus-separator GROUP_CONCAT (which IS legal).
            let mut stmt = conn.prepare(
                "SELECT
                   f.id, f.path_text, f.kind, f.modified_at, f.created_at,
                   f.location_lat, f.location_lon, f.has_text,
                   GROUP_CONCAT(pn.name, char(31)) AS names
                 FROM files f
                 LEFT JOIN (
                   SELECT DISTINCT fp.file_id, p.name
                   FROM face_prints fp
                   JOIN persons p ON p.id = fp.person_id
                   WHERE p.name IS NOT NULL AND p.name != ''
                 ) pn ON pn.file_id = f.id
                 WHERE f.failed = 0
                 GROUP BY f.id"
            )?;
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
                    message: format!("planRestructure query failed: {err}"),
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
    let proposed = classify(&files, library_root_path);
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
