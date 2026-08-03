//! `fileid scan` — model-free library indexer.
//!
//! Walks a directory, writes a `files` row per regular file, and extracts
//! plain-text content (`.txt`/`.md`/source code/…) into `doc_text` so FTS
//! search works with no ML models loaded. The `doc_fts` index is maintained
//! automatically by the engine's v15 AFTER-INSERT triggers.
//!
//! What this DOESN'T do (documented follow-on, routes through the engine's
//! `startScan` once models are installed): image tags (RAM++), CLIP
//! embeddings, face detection/clustering, perceptual hashes, content hashes,
//! and binary-document text extraction (.docx/.pdf). The CLI is intentionally
//! non-destructive to those columns — it UPSERTs and preserves any ML data a
//! prior full engine scan wrote.

use std::io::{IsTerminal, Write as _};
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use fileid_engine::db::zero_byte::{
    apply_validated_zero_byte_files, finish_zero_byte_mutation, validate_zero_byte_files,
    ZeroByteObservation,
};
use fileid_engine::pipeline::discovery::FileKind;
use rusqlite::{params, OptionalExtension as _};
use walkdir::WalkDir;

use crate::context::{
    canonical_path_text, display_path, print_json, stable_path_hash, terminal_text, Ctx,
};

const TEXT_CAP_BYTES: u64 = 4 * 1024 * 1024;
const DB_BATCH_SIZE: usize = 500;

const UPSERT_SQL: &str = "\
INSERT INTO files (
    path_text, path_hash, size_bytes, created_at, modified_at, scanned_at,
    kind, extension, has_text, file_ref
)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
ON CONFLICT(path_text) DO UPDATE SET
    path_hash    = excluded.path_hash,
    size_bytes   = excluded.size_bytes,
    created_at   = COALESCE(excluded.created_at, files.created_at),
    modified_at  = excluded.modified_at,
    scanned_at   = excluded.scanned_at,
    kind         = excluded.kind,
    extension    = excluded.extension,
    file_ref     = excluded.file_ref,
    failed       = 0,
    error_message = NULL,
    has_text     = CASE
                     WHEN ?11 = 1 THEN excluded.has_text
                     WHEN ?9 = 1 THEN 1
                     ELSE files.has_text
                   END
RETURNING id";

enum ScanRecord {
    Zero(ZeroByteObservation),
    Seen {
        path_text: String,
        file_ref: Option<u64>,
    },
    Changed {
        path_text: String,
        path_hash: i64,
        size: u64,
        created: Option<f64>,
        modified: f64,
        kind: &'static str,
        extension: String,
        authoritative_text: bool,
        text: Option<String>,
        file_ref: Option<u64>,
        invalidate_derived: bool,
    },
}

fn prefix_upper_bound(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    for index in (0..bytes.len()).rev() {
        if bytes[index] != u8::MAX {
            bytes[index] += 1;
            bytes.truncate(index + 1);
            return String::from_utf8(bytes).ok();
        }
    }
    None
}

fn mark_existing_row_failed(
    conn: &rusqlite::Connection,
    path: &Path,
    message: &str,
) -> Result<usize> {
    let path_text = canonical_path_text(path);
    conn.execute(
        "UPDATE files SET failed = 1, error_message = ?2 WHERE path_text = ?1",
        params![path_text, message],
    )
    .context("mark unreadable existing file as failed")
}

fn soft_hide_missing_rows(
    conn: &rusqlite::Connection,
    root: &Path,
    scan_marker: f64,
) -> Result<u64> {
    let mut lower = canonical_path_text(root)
        .trim_end_matches(['/', '\\'])
        .to_string();
    lower.push(std::path::MAIN_SEPARATOR);
    let Some(upper) = prefix_upper_bound(&lower) else {
        return Ok(0);
    };
    let changed = conn
        .execute(
            "UPDATE files SET failed = 1, \
             error_message = 'File is no longer present under the completed scan root.' \
             WHERE path_text >= ?1 AND path_text < ?2 AND failed = 0 AND scanned_at != ?3",
            params![lower, upper, scan_marker],
        )
        .context("mark missing files after completed scan")?;
    Ok(changed as u64)
}

fn commit_batch(
    conn: &mut rusqlite::Connection,
    batch: &mut Vec<ScanRecord>,
    scan_marker: f64,
) -> Result<(u64, u64, u64)> {
    if batch.is_empty() {
        return Ok((0, 0, 0));
    }
    let mut zero_byte_observations = Vec::new();
    let mut regular_records = Vec::with_capacity(batch.len());
    for record in batch.drain(..) {
        match record {
            ScanRecord::Zero(observation) => zero_byte_observations.push(observation),
            record => regular_records.push(record),
        }
    }
    let zero_byte_validation = validate_zero_byte_files(&zero_byte_observations);
    let rejected_zero_bytes = zero_byte_validation.changed_since_observation;
    let tx = conn.transaction().context("begin scan batch")?;
    let zero_byte_mutation =
        apply_validated_zero_byte_files(&tx, &zero_byte_validation.observations)
            .context("transition observed zero-byte files")?;
    let mut indexed = 0;
    let mut text_indexed = 0;
    {
        let mut mark_seen = tx
            .prepare(
                "UPDATE files SET scanned_at = ?1, file_ref = ?2, failed = 0, error_message = NULL \
                 WHERE path_text = ?3",
            )
            .context("prepare unchanged-row update")?;
        let mut upsert = tx.prepare(UPSERT_SQL).context("prepare files upsert")?;
        let mut del_doc = tx
            .prepare("DELETE FROM doc_text WHERE file_id = ?1")
            .context("prepare doc_text delete")?;
        let mut ins_doc = tx
            .prepare("INSERT INTO doc_text (file_id, text) VALUES (?1, ?2)")
            .context("prepare doc_text insert")?;
        let mut invalidate_file = tx
            .prepare(
                "UPDATE files SET phash = NULL, aesthetic = NULL, has_faces = 0, \
                 has_text = ?2, camera_model = NULL, location_lat = NULL, location_lon = NULL, \
                 content_hash = NULL, vlm_description = NULL, vlm_proposed_name = NULL, \
                 vlm_model = NULL, vlm_analyzed_at = NULL, text_stage_done = 0 WHERE id = ?1",
            )
            .context("prepare derived-metadata invalidation")?;
        let mut delete_auto_tags = tx
            .prepare("DELETE FROM tags WHERE file_id = ?1 AND source = 'auto'")
            .context("prepare auto-tag invalidation")?;
        let mut delete_faces = tx
            .prepare("DELETE FROM face_prints WHERE file_id = ?1")
            .context("prepare face invalidation")?;
        let mut delete_ocr = tx
            .prepare("DELETE FROM ocr_text WHERE file_id = ?1")
            .context("prepare OCR invalidation")?;
        let mut delete_clip = tx
            .prepare("DELETE FROM clip_embeddings WHERE file_id = ?1")
            .context("prepare CLIP invalidation")?;
        let mut delete_text_embedding = tx
            .prepare("DELETE FROM text_embeddings WHERE file_id = ?1")
            .context("prepare text-embedding invalidation")?;

        for record in regular_records {
            match record {
                ScanRecord::Zero(_) => {
                    unreachable!("zero-byte records were partitioned before the transaction")
                }
                ScanRecord::Seen {
                    path_text,
                    file_ref,
                } => {
                    mark_seen
                        .execute(params![
                            scan_marker,
                            file_ref.map(|value| value as i64),
                            path_text
                        ])
                        .context("mark unchanged file as seen")?;
                }
                ScanRecord::Changed {
                    path_text,
                    path_hash,
                    size,
                    created,
                    modified,
                    kind,
                    extension,
                    authoritative_text,
                    text,
                    file_ref,
                    invalidate_derived,
                } => {
                    let file_id: i64 = upsert
                        .query_row(
                            params![
                                path_text,
                                path_hash,
                                size as i64,
                                created,
                                modified,
                                scan_marker,
                                kind,
                                extension,
                                i64::from(text.is_some()),
                                file_ref.map(|value| value as i64),
                                i64::from(authoritative_text)
                            ],
                            |row| row.get(0),
                        )
                        .context("upsert scanned file")?;
                    if invalidate_derived {
                        invalidate_file
                            .execute(params![file_id, i64::from(text.is_some())])
                            .context("invalidate stale derived file metadata")?;
                        // Only automatic tags are cleared on re-index — never
                        // user-authored ones — even when the file's identity
                        // (inode) changed at the same path (a replacement). This
                        // mirrors the engine reference (dbwriter deletes only
                        // source='auto'); an atomic-save editor that rewrites a
                        // file to a new inode must not silently wipe the user's
                        // manual tags.
                        delete_auto_tags
                            .execute(params![file_id])
                            .context("clear stale automatic tags")?;
                        delete_faces.execute(params![file_id])?;
                        delete_ocr.execute(params![file_id])?;
                        delete_clip.execute(params![file_id])?;
                        delete_text_embedding.execute(params![file_id])?;
                    }
                    if authoritative_text || invalidate_derived {
                        del_doc
                            .execute(params![file_id])
                            .context("clear prior document text")?;
                    }
                    if let Some(text) = text {
                        ins_doc
                            .execute(params![file_id, text])
                            .context("insert document text")?;
                        text_indexed += 1;
                    }
                    indexed += 1;
                }
            }
        }
    }
    tx.commit().context("commit scan batch")?;
    finish_zero_byte_mutation(zero_byte_mutation);
    Ok((indexed, text_indexed, rejected_zero_bytes))
}

pub fn run(ctx: &Ctx, root: &Path, rescan: bool) -> Result<()> {
    let root_abs = std::fs::canonicalize(root)
        .with_context(|| format!("resolving scan root {}", root.display()))?;
    if !root_abs.is_dir() {
        anyhow::bail!("scan root is not a directory: {}", root_abs.display());
    }

    let mut conn = fileid_engine::db::open_writer(&ctx.db)
        .with_context(|| format!("opening library db at {}", ctx.db.display()))?;
    let now = now_unix();
    let started = Instant::now();

    // Live carriage-return progress only when stderr is a TTY (and not
    // --quiet/--json); otherwise fall back to coarse, non-spammy lines.
    let live = !ctx.quiet && !ctx.json && std::io::stderr().is_terminal();
    if !ctx.quiet && !ctx.json {
        ctx.progress(&format!("{} {}", ctx.bold("Scanning"), root_abs.display()));
    }

    let mut discovered: u64 = 0;
    let mut indexed: u64 = 0;
    let mut skipped: u64 = 0;
    let mut text_indexed: u64 = 0;
    let mut failed: u64 = 0;
    let mut warnings = Vec::new();

    let mut batch = Vec::with_capacity(DB_BATCH_SIZE);
    let mut sel = conn
        .prepare(
            "SELECT scanned_at, size_bytes, modified_at, file_ref \
             FROM files WHERE path_text = ?1",
        )
        .context("prepare existing-row probe")?;

    for entry in WalkDir::new(&root_abs).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                failed += 1;
                if let Some(path) = error.path() {
                    mark_existing_row_failed(
                        &conn,
                        path,
                        "Model-free scan could not observe this file.",
                    )?;
                }
                if warnings.len() < 5 {
                    warnings.push(format!("walk: {error}"));
                }
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let meta = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => {
                failed += 1;
                mark_existing_row_failed(
                    &conn,
                    path,
                    "Model-free scan could not read current file metadata.",
                )?;
                if warnings.len() < 5 {
                    warnings.push(format!("metadata {}: {error}", path.display()));
                }
                continue;
            }
        };
        let size = meta.len();
        if size == 0 {
            // New empty files remain unindexed. An exact-path existing row is
            // transitioned to an inactive zero state by the shared engine helper,
            // preserving its ID and user tags while clearing stale content facts.
            batch.push(ScanRecord::Zero(ZeroByteObservation {
                // The engine keys the row by `observation.path.to_string_lossy()`
                // verbatim, so this must be the SAME stripped form we store as
                // `path_text` (canonical_path_text) — not the raw `\\?\` walk
                // path, which would miss the existing row and leave stale content.
                path: std::path::PathBuf::from(canonical_path_text(path)),
                file_ref: fileid_engine::platform::file_ref(path),
            }));
            // Respect the same batch-flush bound as the main path so a tree of
            // mostly-empty files can't grow the batch without limit.
            if batch.len() >= DB_BATCH_SIZE {
                drop(sel);
                let (batch_indexed, batch_text_indexed, batch_raced) =
                    commit_batch(&mut conn, &mut batch, now)?;
                indexed += batch_indexed;
                text_indexed += batch_text_indexed;
                failed += batch_raced;
                if batch_raced > 0 && warnings.len() < 5 {
                    warnings.push(format!(
                        "{batch_raced} file(s) changed after zero-byte discovery; run the scan again"
                    ));
                }
                sel = conn
                    .prepare(
                        "SELECT scanned_at, size_bytes, modified_at, file_ref \
                         FROM files WHERE path_text = ?1",
                    )
                    .context("re-prepare existing-row probe")?;
            }
            continue;
        }
        discovered += 1;

        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let kind = FileKind::from_extension(&extension);
        let path_text = canonical_path_text(path);
        let path_hash = stable_path_hash(&path_text);
        let modified = system_time_to_unix(meta.modified().ok());
        let created = system_time_to_unix(meta.created().ok());
        let file_ref = fileid_engine::platform::file_ref(path);

        let prior: Option<(f64, i64, Option<f64>, Option<i64>)> = sel
            .query_row(params![path_text], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .optional()
            .context("query existing file identity")?;
        let current_ref = file_ref.map(|value| value as i64);
        let metadata_matches = prior.as_ref().is_some_and(
            |(_, prior_size, prior_modified, prior_ref)| {
                let identity_matches = match (*prior_ref, current_ref) {
                    (Some(prior), Some(current)) => prior == current,
                    _ => true,
                };
                *prior_size == size as i64
                    && matches!((prior_modified, modified), (Some(prior), Some(current)) if (*prior - current).abs() < f64::EPSILON)
                    && identity_matches
            },
        );
        let unchanged = !rescan
            && prior.as_ref().is_some_and(|(scanned_at, _, _, prior_ref)| {
                metadata_matches
                    && modified.is_some_and(|current| *scanned_at >= current)
                    && *prior_ref == current_ref
            });
        let invalidate_derived = prior.is_some() && !metadata_matches;

        if unchanged {
            skipped += 1;
            batch.push(ScanRecord::Seen {
                path_text,
                file_ref,
            });
        } else {
            let authoritative_text = is_plaintext_ext(&extension);
            let text = match extract_plaintext(&extension, size, path) {
                Ok(text) => text,
                Err(error) => {
                    failed += 1;
                    if prior.is_some() {
                        conn.execute(
                            "UPDATE files SET failed = 1, error_message = ?2 WHERE path_text = ?1",
                            params![
                                path_text,
                                "Model-free scan could not read current file content."
                            ],
                        )
                        .context("mark unreadable existing file content as failed")?;
                    }
                    if warnings.len() < 5 {
                        warnings.push(error.to_string());
                    }
                    continue;
                }
            };
            batch.push(ScanRecord::Changed {
                path_text,
                path_hash,
                size,
                created,
                modified: modified.unwrap_or(now),
                kind: kind.as_str(),
                extension,
                authoritative_text,
                text,
                file_ref,
                invalidate_derived,
            });
        }

        if batch.len() >= DB_BATCH_SIZE {
            drop(sel);
            let (batch_indexed, batch_text_indexed, batch_raced) =
                commit_batch(&mut conn, &mut batch, now)?;
            indexed += batch_indexed;
            text_indexed += batch_text_indexed;
            failed += batch_raced;
            if batch_raced > 0 && warnings.len() < 5 {
                warnings.push(format!(
                    "{batch_raced} file(s) changed after zero-byte discovery; run the scan again"
                ));
            }
            sel = conn
                .prepare(
                    "SELECT scanned_at, size_bytes, modified_at, file_ref \
                     FROM files WHERE path_text = ?1",
                )
                .context("re-prepare existing-row probe")?;
        }

        let processed = indexed + skipped + batch.len() as u64;
        if live && processed.is_multiple_of(32) {
            let mut err = std::io::stderr();
            let _ = write!(
                err,
                "\r  {} {indexed} indexed · {text_indexed} text · {skipped} unchanged\x1b[K",
                ctx.dim("scanning…")
            );
            let _ = err.flush();
        } else if !live && !ctx.quiet && !ctx.json && processed.is_multiple_of(2000) {
            ctx.progress(&format!("  processed {processed} files…"));
        }
    }
    drop(sel);
    let (batch_indexed, batch_text_indexed, batch_raced) =
        commit_batch(&mut conn, &mut batch, now)?;
    indexed += batch_indexed;
    text_indexed += batch_text_indexed;
    failed += batch_raced;
    if batch_raced > 0 && warnings.len() < 5 {
        warnings.push(format!(
            "{batch_raced} file(s) changed after zero-byte discovery; run the scan again"
        ));
    }
    if failed == 0 {
        soft_hide_missing_rows(&conn, &root_abs, now)?;
    }

    if live {
        let _ = write!(std::io::stderr(), "\r\x1b[K");
        let _ = std::io::stderr().flush();
    }

    let elapsed = started.elapsed();
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "scan",
            "root": root_abs.to_string_lossy(),
            "discovered": discovered,
            "indexed": indexed,
            "skipped": skipped,
            "textIndexed": text_indexed,
            "failed": failed,
            "warnings": warnings,
            "durationMs": elapsed.as_millis() as u64,
        }));
    } else {
        let secs = elapsed.as_secs_f64();
        let rate = if secs > 0.0 {
            format!("  ·  {:.0} files/s", indexed as f64 / secs)
        } else {
            String::new()
        };
        if failed > 0 {
            println!(
                "{}",
                ctx.bold("Scan complete (partial — some files were unreadable).")
            );
        } else {
            println!("{}", ctx.bold("Scan complete."));
        }
        println!(
            "  Root:         {}",
            display_path(root_abs.to_string_lossy().as_ref())
        );
        println!(
            "  Indexed:      {indexed}  {}",
            ctx.dim(&format!("({discovered} found, {skipped} unchanged)"))
        );
        println!(
            "  Text-indexed: {text_indexed} {}",
            ctx.dim("(full-text search)")
        );
        println!("  Duration:     {secs:.2}s{rate}");
        if failed > 0 {
            println!("  Failed:       {failed}");
            for warning in &warnings {
                println!("    {}", terminal_text(warning));
            }
        }
        if indexed > 0 {
            println!("  Search it:    {}", ctx.bold("fileid search \"<words>\""));
        }
        println!(
            "  Add AI tags · faces · visual search: {}",
            ctx.bold("fileid scan --models")
        );
        if text_indexed == 0 {
            println!(
                "  {}",
                ctx.dim(
                    "note: no plain-text files here; image tags/faces need an AI scan (`--models`)"
                )
            );
        }
    }
    if failed > 0 {
        return Err(PartialScan { failed }.into());
    }
    Ok(())
}

/// A scan whose results committed successfully but which skipped unreadable
/// files or directories. Surfaced as its own error type so `main` can exit
/// with the dedicated partial code (3, rsync-style) instead of hard failure —
/// on a real corpus a locked file or ACL-restricted folder is routine, and a
/// wrapper keying on the exit code must be able to tell "index is usable,
/// some files were missed" apart from "the scan itself failed".
#[derive(Debug)]
pub struct PartialScan {
    pub failed: u64,
}

impl std::fmt::Display for PartialScan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "scan completed partially: {} file or traversal error(s); indexed results were committed",
            self.failed
        )
    }
}

impl std::error::Error for PartialScan {}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn system_time_to_unix(t: Option<SystemTime>) -> Option<f64> {
    t.and_then(|st| st.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
}

/// Read a small plain-text file as lossy UTF-8 for FTS indexing. Binary
/// documents (docx/pdf/…) return None — their extractors live in the engine.
fn extract_plaintext(ext: &str, size: u64, path: &Path) -> Result<Option<String>> {
    if size > TEXT_CAP_BYTES || !is_plaintext_ext(ext) {
        return Ok(None);
    }
    let bytes =
        std::fs::read(path).with_context(|| format!("read plaintext {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn is_plaintext_ext(ext: &str) -> bool {
    matches!(
        ext,
        // prose / markup
        "txt" | "md" | "markdown" | "rst" | "org" | "adoc" | "tex" | "bib"
            | "log" | "csv" | "tsv" | "json" | "yaml" | "yml" | "toml" | "ini"
            | "cfg" | "conf" | "xml" | "html" | "htm" | "css" | "svg"
            // source code (mirrors the engine's FileKind::Doc code set)
            | "swift" | "py" | "rb" | "js" | "jsx" | "ts" | "tsx" | "java" | "kt"
            | "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "cs" | "go" | "rs"
            | "php" | "sh" | "bash" | "zsh" | "sql" | "scala" | "m" | "mm" | "r"
            | "jl" | "lua" | "dart" | "vue" | "pl" | "pm" | "ps1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    type DerivedState = (
        i64,
        Option<i64>,
        Option<Vec<u8>>,
        Option<String>,
        i64,
        i64,
        i64,
        i64,
        i64,
    );
    type ZeroDormantState = (
        i64,
        i64,
        i64,
        Option<i64>,
        Option<Vec<u8>>,
        Option<String>,
        i64,
    );

    fn test_layout(name: &str) -> (PathBuf, PathBuf, Ctx) {
        let temp = std::env::temp_dir().join(format!(
            "fileid-cli-scan-{name}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let root = temp.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let db = temp.join("fileid.sqlite");
        let ctx = Ctx {
            json: true,
            quiet: true,
            color: false,
            color_allowed: false,
            db: db.clone(),
            db_explicit: true,
        };
        (temp, root, ctx)
    }

    #[test]
    fn replacement_with_same_metadata_is_not_skipped_when_identity_changes() {
        let (temp, root, ctx) = test_layout("identity");
        let path = root.join("same.txt");
        std::fs::write(&path, "old!").unwrap();
        run(&ctx, &root, false).unwrap();

        let old_ref = fileid_engine::platform::file_ref(&path).unwrap();
        std::fs::rename(&path, temp.join("held.txt")).unwrap();
        std::fs::write(&path, "new!").unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let modified = system_time_to_unix(meta.modified().ok()).unwrap();
        {
            let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
            conn.execute(
                "UPDATE files SET size_bytes = ?1, modified_at = ?2, scanned_at = ?3, file_ref = ?4, \
                 failed = 1, error_message = 'missing', phash = 123, has_faces = 1, \
                 content_hash = x'0102', vlm_description = 'old caption' WHERE path_text = ?5",
                params![
                    meta.len() as i64,
                    modified,
                    modified + 100.0,
                    old_ref as i64,
                    canonical_path_text(&path)
                ],
            )
            .unwrap();
            let file_id: i64 = conn
                .query_row(
                    "SELECT id FROM files WHERE path_text = ?1",
                    params![canonical_path_text(&path)],
                    |row| row.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO tags(file_id, tag, source) VALUES (?1, 'old-auto', 'auto'), (?1, 'old-user', 'user')",
                params![file_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO persons(name, file_count, created_at) VALUES ('Old person', 1, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO face_prints(file_id, person_id, print_data, bbox) VALUES (?1, 1, x'00', '0,0,1,1')",
                params![file_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ocr_text(file_id, text) VALUES (?1, 'old OCR')",
                params![file_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO clip_embeddings(file_id, embedding, model) VALUES (?1, x'00', 'old')",
                params![file_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO text_embeddings(file_id, embedding, model) VALUES (?1, x'00', 'old')",
                params![file_id],
            )
            .unwrap();
        }

        run(&ctx, &root, false).unwrap();
        let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
        let (new_ref, text): (i64, String) = conn
            .query_row(
                "SELECT f.file_ref, d.text FROM files f \
                 JOIN doc_text d ON d.file_id = f.id WHERE f.path_text = ?1",
                params![canonical_path_text(&path)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_ne!(new_ref, old_ref as i64);
        assert_eq!(text, "new!");
        let state: DerivedState = conn
            .query_row(
                "SELECT failed, phash, content_hash, vlm_description, \
                 (SELECT COUNT(*) FROM tags WHERE file_id = files.id), \
                 (SELECT COUNT(*) FROM face_prints WHERE file_id = files.id), \
                 (SELECT COUNT(*) FROM ocr_text WHERE file_id = files.id), \
                 (SELECT COUNT(*) FROM clip_embeddings WHERE file_id = files.id), \
                 (SELECT COUNT(*) FROM text_embeddings WHERE file_id = files.id) \
                 FROM files WHERE path_text = ?1",
                params![canonical_path_text(&path)],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .unwrap();
        // One tag survives (the user tag); only the auto tag is cleared. A
        // replacement (inode change) must never wipe user-authored tags —
        // parity with the engine, which deletes only source='auto' on re-index.
        assert_eq!(state, (0, None, None, None, 1, 0, 0, 0, 0));
        let surviving: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT t.tag, t.source FROM tags t \
                     JOIN files f ON f.id = t.file_id WHERE f.path_text = ?1 ORDER BY t.tag",
                )
                .unwrap();
            stmt.query_map(params![canonical_path_text(&path)], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        };
        assert_eq!(
            surviving,
            vec![("old-user".to_string(), "user".to_string())]
        );
        drop(conn);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn zero_byte_rescan_dormants_then_restores_same_row_without_stale_content() {
        let (temp, root, ctx) = test_layout("zero-byte-lifecycle");
        let path = root.join("report.txt");
        std::fs::write(&path, "original report").unwrap();
        let original_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        run(&ctx, &root, false).unwrap();

        let original_id;
        {
            let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
            original_id = conn
                .query_row(
                    "SELECT id FROM files WHERE path_text = ?1",
                    params![canonical_path_text(&path)],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            conn.execute(
                "UPDATE files SET phash = 9, aesthetic = 0.5, has_faces = 1, has_text = 1, \
                 camera_model = 'camera', location_lat = 1, location_lon = 2, content_hash = x'01', \
                 vlm_description = 'stale caption', vlm_proposed_name = 'stale name', \
                 vlm_model = 'stale model', vlm_analyzed_at = 4, text_stage_done = 1 WHERE id = ?1",
                params![original_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tags(file_id, tag, source) VALUES \
                 (?1, 'auto-tag', 'auto'), (?1, 'vlm-tag', 'vlm'), (?1, 'user-tag', 'user')",
                params![original_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO persons(id, name, file_count, created_at) VALUES (8, 'Person', 1, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO face_prints(id, file_id, person_id, print_data, bbox, face_quality) \
                 VALUES (12, ?1, 8, x'00', '0,0,1,1', 0.8)",
                params![original_id],
            )
            .unwrap();
            conn.execute(
                "UPDATE persons SET representative_face_id = 12 WHERE id = 8",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ocr_text(file_id, text) VALUES (?1, 'stale OCR')",
                params![original_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO clip_embeddings(file_id, embedding, model) VALUES (?1, x'00', 'clip')",
                params![original_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO text_embeddings(file_id, embedding, model) VALUES (?1, x'00', 'text')",
                params![original_id],
            )
            .unwrap();
        }

        std::fs::write(&path, b"").unwrap();
        run(&ctx, &root, false).unwrap();
        {
            let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
            let state: ZeroDormantState = conn
                .query_row(
                    "SELECT id, size_bytes, failed, phash, content_hash, vlm_description, text_stage_done \
                     FROM files WHERE path_text = ?1",
                    params![canonical_path_text(&path)],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
                )
                .unwrap();
            assert_eq!(state, (original_id, 0, 1, None, None, None, 0));
            let tags: Vec<(String, String)> = conn
                .prepare("SELECT tag, source FROM tags WHERE file_id = ?1 ORDER BY tag")
                .unwrap()
                .query_map(params![original_id], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(tags, vec![("user-tag".to_string(), "user".to_string())]);
            for table in [
                "face_prints",
                "ocr_text",
                "doc_text",
                "clip_embeddings",
                "text_embeddings",
            ] {
                let count: i64 = conn
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE file_id = ?1"),
                        params![original_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 0, "{table}");
            }
            for (fts, term) in [("ocr_fts", "stale"), ("doc_fts", "original")] {
                let hits: i64 = conn
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {fts} WHERE {fts} MATCH ?1"),
                        [term],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(hits, 0, "{fts}");
            }
            let person: (i64, Option<i64>) = conn
                .query_row(
                    "SELECT file_count, representative_face_id FROM persons WHERE id = 8",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(person, (0, None));
        }

        std::fs::write(&path, "restored report").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
        run(&ctx, &root, false).unwrap();
        let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
        let restored: (i64, i64, i64, String) = conn
            .query_row(
                "SELECT f.id, f.size_bytes, f.failed, d.text FROM files f \
                 JOIN doc_text d ON d.file_id = f.id WHERE f.path_text = ?1",
                params![canonical_path_text(&path)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            restored,
            (original_id, 15, 0, "restored report".to_string())
        );
        drop(conn);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn zero_byte_growth_race_is_partial_and_preserves_the_active_row() {
        let (temp, root, ctx) = test_layout("zero-byte-race");
        let path = root.join("race.txt");
        std::fs::write(&path, b"").unwrap();
        let observed_ref = fileid_engine::platform::file_ref(&path);
        let mut conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
        conn.execute(
            "INSERT INTO files(path_text, path_hash, size_bytes, scanned_at, kind, extension) \
             VALUES (?1, 1, 20, 1, 'doc', 'txt')",
            params![canonical_path_text(&path)],
        )
        .unwrap();
        std::fs::write(&path, "grew after discovery").unwrap();
        let mut batch = vec![ScanRecord::Zero(ZeroByteObservation {
            path: path.clone(),
            file_ref: observed_ref,
        })];
        let result = commit_batch(&mut conn, &mut batch, 2.0).unwrap();
        assert_eq!(result, (0, 0, 1));
        let state: (i64, i64, f64) = conn
            .query_row(
                "SELECT size_bytes, failed, scanned_at FROM files",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, (20, 0, 1.0));
        drop(conn);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn zero_and_regular_records_share_one_transaction() {
        let (temp, root, ctx) = test_layout("zero-byte-atomic");
        let zero = root.join("zero.txt");
        let regular = root.join("regular.txt");
        std::fs::write(&zero, b"").unwrap();
        std::fs::write(&regular, "regular").unwrap();
        let mut conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
        conn.execute(
            "INSERT INTO files(id, path_text, path_hash, size_bytes, scanned_at, kind, extension) \
             VALUES (1, ?1, 1, 20, 1, 'doc', 'txt'), (2, ?2, 2, 7, 1, 'doc', 'txt')",
            params![canonical_path_text(&zero), canonical_path_text(&regular)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tags(file_id, tag, source) VALUES (1, 'old-auto', 'auto')",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_regular_update BEFORE UPDATE ON files \
             WHEN old.id = 2 BEGIN SELECT RAISE(ABORT, 'injected regular failure'); END;",
        )
        .unwrap();
        let mut batch = vec![
            ScanRecord::Zero(ZeroByteObservation {
                path: zero.clone(),
                file_ref: fileid_engine::platform::file_ref(&zero),
            }),
            ScanRecord::Seen {
                path_text: canonical_path_text(&regular),
                file_ref: fileid_engine::platform::file_ref(&regular),
            },
        ];
        assert!(commit_batch(&mut conn, &mut batch, 2.0).is_err());
        let zero_state: (i64, i64, i64) = conn
            .query_row(
                "SELECT size_bytes, failed, (SELECT COUNT(*) FROM tags WHERE file_id = 1) \
                 FROM files WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(zero_state, (20, 0, 1));
        drop(conn);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn zero_byte_observations_remain_bounded_across_batch_boundary() {
        let (temp, root, ctx) = test_layout("zero-byte-batches");
        for index in 0..=DB_BATCH_SIZE {
            std::fs::write(root.join(format!("{index}.txt")), "content").unwrap();
        }
        run(&ctx, &root, false).unwrap();
        for index in 0..=DB_BATCH_SIZE {
            std::fs::write(root.join(format!("{index}.txt")), b"").unwrap();
        }
        run(&ctx, &root, false).unwrap();
        let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
        let state: (i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), SUM(CASE WHEN failed = 1 AND size_bytes = 0 THEN 1 ELSE 0 END), \
                 SUM(CASE WHEN failed = 0 THEN 1 ELSE 0 END) FROM files",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, (501, 501, 0));
        drop(conn);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn completed_rescan_soft_hides_disappeared_rows() {
        let (temp, root, ctx) = test_layout("missing");
        let kept = root.join("kept.txt");
        let removed = root.join("removed.txt");
        std::fs::write(&kept, "kept").unwrap();
        std::fs::write(&removed, "removed").unwrap();
        let removed_key = canonical_path_text(&removed);
        run(&ctx, &root, false).unwrap();
        std::fs::remove_file(&removed).unwrap();
        run(&ctx, &root, false).unwrap();

        let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
        let missing: i64 = conn
            .query_row(
                "SELECT failed FROM files WHERE path_text = ?1",
                params![removed_key],
                |row| row.get(0),
            )
            .unwrap();
        let active: i64 = conn
            .query_row(
                "SELECT failed FROM files WHERE path_text = ?1",
                params![canonical_path_text(&kept)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((missing, active), (1, 0));
        drop(conn);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn unchanged_successful_observation_rehabilitates_failed_row() {
        let (temp, root, ctx) = test_layout("rehabilitate");
        let path = root.join("file.txt");
        std::fs::write(&path, "text").unwrap();
        run(&ctx, &root, false).unwrap();
        {
            let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
            conn.execute(
                "UPDATE files SET failed = 1, error_message = 'missing' WHERE path_text = ?1",
                params![canonical_path_text(&path)],
            )
            .unwrap();
        }
        run(&ctx, &root, false).unwrap();
        let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
        let state: (i64, Option<String>) = conn
            .query_row(
                "SELECT failed, error_message FROM files WHERE path_text = ?1",
                params![canonical_path_text(&path)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (0, None));
        drop(conn);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn scan_commits_every_record_across_batch_boundaries() {
        let (temp, root, ctx) = test_layout("batches");
        for index in 0..=DB_BATCH_SIZE {
            std::fs::write(root.join(format!("{index}.txt")), "text").unwrap();
        }
        run(&ctx, &root, false).unwrap();
        let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, (DB_BATCH_SIZE + 1) as i64);
        drop(conn);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_existing_file_is_soft_hidden_without_erasing_prior_text() {
        use std::os::unix::fs::PermissionsExt;

        let (temp, root, ctx) = test_layout("unreadable-existing");
        let path = root.join("report.txt");
        std::fs::write(&path, "secret-old").unwrap();
        run(&ctx, &root, false).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&path, permissions).unwrap();
        if std::fs::read(&path).is_ok() {
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(&path, permissions).unwrap();
            std::fs::remove_dir_all(temp).unwrap();
            return;
        }

        let scan = run(&ctx, &root, true);
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert!(scan.is_err());

        let conn = fileid_engine::db::open_writer(&ctx.db).unwrap();
        let (failed, text): (i64, String) = conn
            .query_row(
                "SELECT files.failed, doc_text.text FROM files \
                 JOIN doc_text ON doc_text.file_id = files.id WHERE files.path_text = ?1",
                params![canonical_path_text(&path)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(failed, 1);
        assert_eq!(text, "secret-old");
        drop(conn);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn plaintext_read_failure_is_not_silently_treated_as_empty_text() {
        let missing = std::env::temp_dir().join(format!(
            "fileid-missing-text-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(extract_plaintext("txt", 10, &missing).is_err());
    }
}
