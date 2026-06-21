//! Model-free, isolated end-to-end smoke test.
//!
//! Builds a tiny text corpus in a tempdir, points the CLI at an ISOLATED
//! SQLite library (via `--db`, so it never touches the real
//! ~/.local/share/FileID or %LOCALAPPDATA%), then exercises
//! scan → search → info entirely with FTS (no ML models). Must run green on
//! any host with the engine's bundled SQLite (FTS5).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_fileid")
}

fn unique_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("fileid-cli-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn run(args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("spawn fileid binary")
}

fn run_env(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn fileid binary")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(out)).expect("parse json stdout")
}

#[test]
fn scan_then_search_then_info_model_free() {
    let corpus = unique_dir("corpus");
    let dbdir = unique_dir("db");
    let db = dbdir.join("lib.sqlite");
    let db_s = db.to_str().unwrap();

    std::fs::write(corpus.join("alpha.txt"), "the quick brown fox aardvark jumps over").unwrap();
    std::fs::write(corpus.join("notes.md"), "# Notes\nquarterly revenue report here\n").unwrap();
    std::fs::write(corpus.join("hello.txt"), "hello world greetings everyone").unwrap();

    // --- first scan (--json): indexes all three text files ---
    let out = run(&["--db", db_s, "--no-color", "--json", "scan", corpus.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(db.exists(), "scan did not create the library db");
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("scan json");
    assert_eq!(v["discovered"].as_u64(), Some(3), "expected 3 files discovered");
    assert_eq!(v["indexed"].as_u64(), Some(3), "expected 3 files indexed");
    assert_eq!(v["skipped"].as_u64(), Some(0), "first scan should skip nothing");
    assert!(v["textIndexed"].as_u64().unwrap_or(0) >= 2, "expected text-indexed files");

    // --- second scan: unchanged files are skipped ---
    let out = run(&["--db", db_s, "--json", "scan", corpus.to_str().unwrap()]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("rescan json");
    assert_eq!(v["discovered"].as_u64(), Some(3));
    assert_eq!(v["skipped"].as_u64(), Some(3), "expected 3 files skipped on re-scan");
    assert_eq!(v["indexed"].as_u64(), Some(0), "re-scan should index nothing");

    // --- search for a content word ---
    let out = run(&["--db", db_s, "--json", "search", "aardvark"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("search json");
    assert!(v["count"].as_u64().unwrap_or(0) >= 1, "search returned no results");
    let hit_alpha = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| ends_with(r["path"].as_str().unwrap_or(""), "alpha.txt"));
    assert!(hit_alpha, "search for 'aardvark' did not return alpha.txt");

    // --- search for a word in a second file ---
    let out = run(&["--db", db_s, "--json", "search", "revenue"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert!(
        v["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| ends_with(r["path"].as_str().unwrap_or(""), "notes.md")),
        "search for 'revenue' did not return notes.md"
    );

    // --- search miss returns cleanly with zero results ---
    let out = run(&["--db", db_s, "--json", "search", "zzzznotpresent"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["count"].as_u64(), Some(0), "miss should return 0 results");

    // --- info by path ---
    let alpha = corpus.join("alpha.txt");
    let out = run(&["--db", db_s, "--json", "info", alpha.to_str().unwrap()]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("info json");
    assert!(ends_with(v["path"].as_str().unwrap_or(""), "alpha.txt"));
    assert_eq!(v["hasText"].as_bool(), Some(true));
    assert!(v["sizeBytes"].as_i64().unwrap_or(0) > 0);
    let id = v["id"].as_i64().expect("info id");

    // --- info by id ---
    let out = run(&["--db", db_s, "--json", "info", &id.to_string()]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert!(ends_with(v["path"].as_str().unwrap_or(""), "alpha.txt"));

    // --- restructure --plan over the indexed (text) files ---
    let out = run(&["--db", db_s, "--json", "restructure", "--plan"]);
    assert!(out.status.success(), "restructure --plan failed");
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("plan json");
    assert_eq!(v["fileCount"].as_u64(), Some(3));
    assert!(v["moveCount"].as_u64().unwrap_or(0) >= 1);

    // cleanup (best-effort)
    let _ = std::fs::remove_dir_all(&corpus);
    let _ = std::fs::remove_dir_all(&dbdir);
}

fn ends_with(path: &str, name: &str) -> bool {
    Path::new(path)
        .file_name()
        .map(|n| n == name)
        .unwrap_or(false)
}

/// Covers the follow-on surfaces WITHOUT any ML models, fully isolated:
/// `search --similar <file>` with no embeddings; `dedupe --apply --dry-run`
/// (the no-signal message and a seeded group); `restructure --apply --dry-run`
/// plus a SAFE non-interactive abort; and `scan --models` messaging when models
/// aren't installed. Every assertion verifies nothing on disk or in the DB was
/// mutated.
#[test]
fn apply_dryrun_models_and_similar_model_free() {
    let corpus = unique_dir("corpus2");
    let dbdir = unique_dir("db2");
    let db = dbdir.join("lib.sqlite");
    let db_s = db.to_str().unwrap();

    // A byte-identical pair (exact-duplicate group) + a unique file.
    std::fs::write(corpus.join("dup1.txt"), "identical duplicate body for dedupe").unwrap();
    std::fs::write(corpus.join("dup2.txt"), "identical duplicate body for dedupe").unwrap();
    std::fs::write(corpus.join("solo.md"), "# Solo\nunique content here\n").unwrap();

    let out = run(&["--db", db_s, "--no-color", "--json", "scan", corpus.to_str().unwrap()]);
    assert!(out.status.success(), "scan failed: {}", String::from_utf8_lossy(&out.stderr));

    // ── search --similar with no CLIP embeddings → clear, non-fatal message ──
    let dup1 = corpus.join("dup1.txt");
    let out = run(&["--db", db_s, "--no-color", "--json", "search", "--similar", dup1.to_str().unwrap()]);
    assert!(out.status.success(), "search --similar failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        json(&out)["error"].as_str(),
        Some("no_embeddings"),
        "model-free library has no CLIP embeddings"
    );

    // ── dedupe --apply --dry-run BEFORE any content hashes → 'no signal' ──
    let out = run(&["--db", db_s, "--no-color", "--json", "dedupe", "--apply", "--dry-run"]);
    assert!(out.status.success());
    assert_eq!(
        json(&out)["available"].as_bool(),
        Some(false),
        "model-free scan computes no content hashes"
    );

    // Seed identical content hashes on the pair (simulating a full engine scan).
    seed_content_hash(&db, "dup1.txt", "dup2.txt");

    let out = run(&["--db", db_s, "--no-color", "--json", "dedupe", "--exact"]);
    assert!(out.status.success());
    assert_eq!(
        json(&out)["groups"]["exact"]["count"].as_u64(),
        Some(1),
        "expected one exact-duplicate group"
    );

    // ── dedupe --apply --dry-run lists exactly one victim, removes NOTHING ──
    let out = run(&["--db", db_s, "--no-color", "--json", "dedupe", "--apply", "--dry-run"]);
    assert!(out.status.success());
    let v = json(&out);
    assert_eq!(v["dryRun"].as_bool(), Some(true));
    assert_eq!(v["removeCount"].as_u64(), Some(1), "keep one, remove the other");
    assert!(
        corpus.join("dup1.txt").exists() && corpus.join("dup2.txt").exists(),
        "dry-run must not delete files"
    );
    assert_eq!(file_count(&db), 3, "dry-run must not drop DB rows");

    // ── restructure --apply --dry-run prints a plan, moves NOTHING ──
    let out = run(&["--db", db_s, "--no-color", "--json", "restructure", "--apply", "--dry-run"]);
    assert!(out.status.success(), "restructure --apply --dry-run failed: {}", String::from_utf8_lossy(&out.stderr));
    let v = json(&out);
    assert_eq!(v["mode"].as_str(), Some("apply"));
    assert_eq!(v["dryRun"].as_bool(), Some(true));
    assert!(v["moveCount"].as_u64().unwrap_or(0) >= 1, "expected at least one proposed move");
    assert!(corpus.join("solo.md").exists() && corpus.join("dup1.txt").exists(), "dry-run must not move files");

    // ── restructure --apply with no --yes on a non-interactive stdin → SAFE abort ──
    let out = run(&["--db", db_s, "--no-color", "restructure", "--apply"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("Aborted"), "non-interactive apply without --yes must abort");
    assert!(corpus.join("solo.md").exists(), "an aborted apply must not move files");

    // ── scan --models with no models installed → actionable message, no writes ──
    let state = unique_dir("state"); // empty FileID data root → no model sentinels
    let out = run_env(
        &["--db", db_s, "--no-color", "--json", "scan", "--models", corpus.to_str().unwrap()],
        &[
            ("XDG_DATA_HOME", state.to_str().unwrap()),
            ("LOCALAPPDATA", state.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "scan --models failed: {}", String::from_utf8_lossy(&out.stderr));
    let v = json(&out);
    assert_eq!(v["error"].as_str(), Some("models_not_installed"));
    let missing: Vec<String> = v["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["kind"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(missing.iter().any(|k| k == "mobileclip_s2"), "report mobileclip_s2 missing");
    assert!(missing.iter().any(|k| k == "arcface"), "report arcface missing");
    assert_eq!(file_count(&db), 3, "scan --models must not write when models are missing");

    let _ = std::fs::remove_dir_all(&corpus);
    let _ = std::fs::remove_dir_all(&dbdir);
    let _ = std::fs::remove_dir_all(&state);
}

/// Stamp identical `content_hash` blobs on two files so the exact-dedupe path
/// (which the model-free CLI scan never populates) has a group to act on.
fn seed_content_hash(db: &Path, a: &str, b: &str) {
    let conn = rusqlite::Connection::open(db).expect("open db for seeding");
    let blob: Vec<u8> = (0u8..32).collect();
    for name in [a, b] {
        conn.execute(
            "UPDATE files SET content_hash = ?1 WHERE path_text LIKE ?2",
            rusqlite::params![blob, format!("%/{name}")],
        )
        .expect("seed content_hash");
    }
}

fn file_count(db: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open db for count");
    conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .expect("count files")
}
