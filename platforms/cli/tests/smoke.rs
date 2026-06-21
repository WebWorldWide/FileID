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

/// Stamp an identical `phash` on the named files so the near-duplicate path
/// (which the model-free CLI scan never populates) has a group to act on.
fn seed_phash(db: &Path, names: &[&str]) {
    let conn = rusqlite::Connection::open(db).expect("open db for seeding");
    let phash: i64 = 0x0F0F_0F0F_0F0F_0F0F;
    for name in names {
        conn.execute(
            "UPDATE files SET phash = ?1 WHERE path_text LIKE ?2",
            rusqlite::params![phash, format!("%/{name}")],
        )
        .expect("seed phash");
    }
}

/// `dedupe --similar --apply` is gated: transitively-chained perceptual groups
/// can over-delete, so it demands an explicit `--yes`. With a non-interactive
/// stdin and no `--yes`, the command must print the over-delete WARNING and
/// refuse, removing nothing. (The byte-identical `--exact` path is unaffected.)
#[test]
fn similar_apply_requires_explicit_yes() {
    let corpus = unique_dir("corpus_sim");
    let dbdir = unique_dir("db_sim");
    let db = dbdir.join("lib.sqlite");
    let db_s = db.to_str().unwrap();

    std::fs::write(corpus.join("a.txt"), "near duplicate alpha body one").unwrap();
    std::fs::write(corpus.join("b.txt"), "near duplicate beta body two").unwrap();
    std::fs::write(corpus.join("c.md"), "# C\nunrelated content\n").unwrap();

    let out = run(&["--db", db_s, "--no-color", "--json", "scan", corpus.to_str().unwrap()]);
    assert!(out.status.success(), "scan failed: {}", String::from_utf8_lossy(&out.stderr));

    // Seed an identical perceptual hash on the pair → one near-duplicate group.
    seed_phash(&db, &["a.txt", "b.txt"]);

    // Read-only listing sees the group (the dry/read path is intentionally fine).
    let out = run(&["--db", db_s, "--no-color", "--json", "dedupe", "--similar"]);
    assert!(out.status.success());
    assert_eq!(
        json(&out)["groups"]["similar"]["count"].as_u64(),
        Some(1),
        "expected one near-duplicate group"
    );

    // `--similar --apply`, no --yes, non-interactive stdin → WARN + refuse.
    let out = run(&["--db", db_s, "--no-color", "dedupe", "--similar", "--apply"]);
    assert!(out.status.success());
    let so = stdout(&out);
    assert!(
        so.contains("WARNING: --similar --apply can over-delete"),
        "missing over-delete warning: {so}"
    );
    assert!(so.contains("Refusing without --yes"), "must refuse without --yes: {so}");
    assert!(
        corpus.join("a.txt").exists() && corpus.join("b.txt").exists(),
        "a refused similar-apply must not delete files"
    );
    assert_eq!(file_count(&db), 3, "a refused similar-apply must not drop DB rows");

    let _ = std::fs::remove_dir_all(&corpus);
    let _ = std::fs::remove_dir_all(&dbdir);
}

/// Like `run_env`, but first scrubs the library-locating env vars so default
/// resolution is deterministic regardless of the host shell. (macOS-only test
/// helper — gated to keep `-D warnings` happy on other platforms.)
#[cfg(target_os = "macos")]
fn run_env_clean(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    cmd.env_remove("FILEID_DB");
    cmd.env_remove("CFFIXED_USER_HOME");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn fileid binary")
}

/// On macOS, with no `--db`/env override, the CLI must resolve the macOS Swift
/// app's library (`~/Library/Application Support/FileID/fileid.sqlite`) when it
/// exists — so `fileid` "just works" against the desktop app. We point `HOME`
/// at a temp dir, seed a library at that exact sub-path with an explicit
/// `--db`, then confirm a no-`--db` `search` resolves to it.
#[cfg(target_os = "macos")]
#[test]
fn macos_default_resolves_swift_app_library() {
    let home = unique_dir("home_mac");
    let corpus = unique_dir("corpus_mac");
    let app_dir = home.join("Library/Application Support/FileID");
    std::fs::create_dir_all(&app_dir).unwrap();
    let swift_db = app_dir.join("fileid.sqlite");
    let swift_db_s = swift_db.to_str().unwrap();
    let home_s = home.to_str().unwrap();

    std::fs::write(corpus.join("kiwi.txt"), "macos default path probe token kiwi").unwrap();

    // Seed the Swift-location library explicitly (HOME pinned so we never touch
    // the real ~/Library).
    let out = run_env_clean(
        &["--db", swift_db_s, "--no-color", "--json", "scan", corpus.to_str().unwrap()],
        &[("HOME", home_s)],
    );
    assert!(out.status.success(), "seed scan failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(swift_db.exists(), "seed did not create the Swift-location library");

    // No --db: the macOS default must resolve to the Swift-app library.
    let out = run_env_clean(&["--no-color", "--json", "search", "kiwi"], &[("HOME", home_s)]);
    assert!(
        out.status.success(),
        "default-path search failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        json(&out)["count"].as_u64().unwrap_or(0) >= 1,
        "macOS default did not resolve to the Swift app library"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&corpus);
}

/// FIX 4 — a bare `fileid` (no subcommand) prints the friendly getting-started
/// intro to stdout and exits 0, instead of clap's terse usage error. It must
/// touch no library (it returns before resolving a DB), and `--help` /
/// `--version` must still work. Fully isolated: no `--db`, no env, no writes.
#[test]
fn no_subcommand_prints_friendly_intro() {
    let out = run(&[]);
    assert!(out.status.success(), "bare `fileid` should exit 0, got {:?}", out.status);
    let s = stdout(&out);
    assert!(
        s.contains("FileID — search, dedupe, and organize"),
        "intro headline missing: {s}"
    );
    assert!(s.contains("fileid people"), "intro should list the people example: {s}");
    assert!(s.contains("fileid search"), "intro should list the search example: {s}");
    assert!(s.contains("fileid dedupe --similar"), "intro should list the dedupe example: {s}");
    assert!(s.contains("fileid restructure --plan"), "intro should list the restructure example: {s}");
    assert!(s.contains("--help"), "intro should point at --help: {s}");

    // --version and --help still function with the subcommand now optional.
    let out = run(&["--version"]);
    assert!(out.status.success(), "--version should exit 0");
    assert!(stdout(&out).contains("fileid"), "version output missing program name");

    let out = run(&["--help"]);
    assert!(out.status.success(), "--help should exit 0");

    // An *unknown* subcommand must still be a hard usage error (exit != 0),
    // never the friendly intro.
    let out = run(&["definitely-not-a-command"]);
    assert!(!out.status.success(), "an unknown subcommand must still error");
}
