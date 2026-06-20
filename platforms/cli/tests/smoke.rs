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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
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
