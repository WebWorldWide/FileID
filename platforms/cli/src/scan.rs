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
use fileid_engine::pipeline::discovery::FileKind;
use rusqlite::params;
use walkdir::WalkDir;

use crate::context::{canonical_path_text, print_json, stable_path_hash, Ctx};

const TEXT_CAP_BYTES: u64 = 4 * 1024 * 1024;

const UPSERT_SQL: &str = "\
INSERT INTO files (path_text, path_hash, size_bytes, created_at, modified_at, scanned_at, kind, extension, has_text)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
ON CONFLICT(path_text) DO UPDATE SET
    path_hash   = excluded.path_hash,
    size_bytes  = excluded.size_bytes,
    created_at  = COALESCE(excluded.created_at, files.created_at),
    modified_at = excluded.modified_at,
    scanned_at  = excluded.scanned_at,
    kind        = excluded.kind,
    extension   = excluded.extension,
    has_text    = CASE WHEN ?9 = 1 THEN 1 ELSE files.has_text END
RETURNING id";

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

    let tx = conn.transaction().context("begin scan transaction")?;
    {
        let mut sel = tx
            .prepare("SELECT scanned_at FROM files WHERE path_text = ?1")
            .context("prepare existing-row probe")?;
        let mut upsert = tx.prepare(UPSERT_SQL).context("prepare files upsert")?;
        let mut del_doc = tx
            .prepare("DELETE FROM doc_text WHERE file_id = ?1")
            .context("prepare doc_text delete")?;
        let mut ins_doc = tx
            .prepare("INSERT INTO doc_text (file_id, text) VALUES (?1, ?2)")
            .context("prepare doc_text insert")?;

        for entry in WalkDir::new(&root_abs).follow_links(false) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let meta = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let size = meta.len();
            if size == 0 {
                continue; // engine parity: zero-byte files carry no content
            }
            discovered += 1;

            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let kind = FileKind::from_extension(&ext);
            let path_text = canonical_path_text(path);
            let path_hash = stable_path_hash(&path_text);
            let modified = system_time_to_unix(meta.modified().ok());
            let created = system_time_to_unix(meta.created().ok());

            if !rescan {
                let prior: Option<f64> = sel
                    .query_row(params![path_text], |r| r.get(0))
                    .ok();
                if let (Some(prev), Some(modi)) = (prior, modified) {
                    if prev >= modi {
                        skipped += 1;
                        continue;
                    }
                }
            }

            let text = extract_plaintext(&ext, size, path);
            let has_text = i64::from(text.is_some());

            let file_id: i64 = upsert
                .query_row(
                    params![
                        path_text,
                        path_hash,
                        size as i64,
                        created,
                        modified.unwrap_or(now),
                        now,
                        kind.as_str(),
                        ext,
                        has_text
                    ],
                    |r| r.get(0),
                )
                .with_context(|| format!("upsert file row for {}", path.display()))?;

            del_doc.execute(params![file_id]).ok();
            if let Some(t) = text {
                ins_doc
                    .execute(params![file_id, t])
                    .with_context(|| format!("doc_text insert for {}", path.display()))?;
                text_indexed += 1;
            }
            indexed += 1;

            if live && indexed.is_multiple_of(32) {
                let mut err = std::io::stderr();
                let _ = write!(
                    err,
                    "\r  {} {indexed} indexed · {text_indexed} text · {skipped} unchanged\x1b[K",
                    ctx.dim("scanning…")
                );
                let _ = err.flush();
            } else if !live && !ctx.quiet && !ctx.json && indexed.is_multiple_of(2000) {
                ctx.progress(&format!("  indexed {indexed} files…"));
            }
        }
    }
    if live {
        let _ = write!(std::io::stderr(), "\r\x1b[K");
        let _ = std::io::stderr().flush();
    }
    tx.commit().context("commit scan transaction")?;

    let elapsed = started.elapsed();
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "scan",
            "root": root_abs.to_string_lossy(),
            "discovered": discovered,
            "indexed": indexed,
            "skipped": skipped,
            "textIndexed": text_indexed,
            "durationMs": elapsed.as_millis() as u64,
        }));
    } else {
        let secs = elapsed.as_secs_f64();
        let rate = if secs > 0.0 {
            format!("  ·  {:.0} files/s", indexed as f64 / secs)
        } else {
            String::new()
        };
        println!("{}", ctx.bold("Scan complete."));
        println!("  Root:         {}", root_abs.display());
        println!(
            "  Indexed:      {indexed}  {}",
            ctx.dim(&format!("({discovered} found, {skipped} unchanged)"))
        );
        println!("  Text-indexed: {text_indexed} {}", ctx.dim("(full-text search)"));
        println!("  Duration:     {secs:.2}s{rate}");
        if indexed > 0 {
            println!(
                "  Search it:    {}",
                ctx.bold("fileid search \"<words>\"")
            );
        }
        println!(
            "  Add AI tags · faces · visual search: {}",
            ctx.bold("fileid scan --models")
        );
        if text_indexed == 0 {
            println!(
                "  {}",
                ctx.dim("note: no plain-text files here; image tags/faces need an AI scan (`--models`)")
            );
        }
    }
    Ok(())
}

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
fn extract_plaintext(ext: &str, size: u64, path: &Path) -> Option<String> {
    if size > TEXT_CAP_BYTES || !is_plaintext_ext(ext) {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
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
