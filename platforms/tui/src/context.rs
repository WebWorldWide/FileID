//! Run context: resolve the library SQLite path the same way the CLI does, and
//! a tiny hand-rolled argv parser (`--db`, `--help`, `--version`) so the TUI
//! adds no CLI-parsing dependency.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

/// Resolved run context threaded into the app.
pub struct Ctx {
    /// Absolute path to the library SQLite file the TUI reads.
    pub db: PathBuf,
    /// SCRATCH mode (no `--db` / env override): the directory handed to the
    /// spawned engine as `XDG_DATA_HOME` / `%LOCALAPPDATA%` so a scan writes the
    /// SAME library the TUI reads (its `<dir>/FileID/fileid.sqlite`). `None` for
    /// an explicit `--db` / env library, where the engine uses its own
    /// canonical root (today's behavior).
    pub engine_data_home: Option<PathBuf>,
}

impl Ctx {
    /// Resolve the library the TUI opens. Precedence:
    ///   1. `--db <path>`
    ///   2. `$FILEID_DB`
    ///   3. `$CFFIXED_USER_HOME/fileid.sqlite` (parity with the macOS app's
    ///      sandbox-root env var; convenient for isolating a test library)
    ///   4. **default: a persistent SCRATCH library** so the TUI opens EMPTY —
    ///      `<scratch_base>/FileID/fileid.sqlite`, never the desktop app's
    ///      library at the engine's canonical `…/FileID/fileid.sqlite`.
    ///
    /// 1–3 set `engine_data_home = None` (explicit library; the spawned engine
    /// uses its own canonical root, as before). Only the default arm is scratch.
    pub fn resolve(db_flag: Option<PathBuf>) -> Result<Self> {
        let t = resolve_target(
            db_flag,
            std::env::var("FILEID_DB").ok(),
            std::env::var("CFFIXED_USER_HOME").ok(),
            default_scratch_home,
        )?;
        Ok(Self {
            db: t.db,
            engine_data_home: t.engine_data_home,
        })
    }

    /// True when running against the default scratch library (no explicit `--db`
    /// / env). Drives the friendly empty-screen + Settings copy.
    pub fn scratch(&self) -> bool {
        self.engine_data_home.is_some()
    }

    /// Collapse `$HOME` to `~` for compact display in the Settings/status line.
    pub fn db_label(&self) -> String {
        collapse_home(&self.db.to_string_lossy())
    }
}

/// Resolution outcome: the library path the TUI reads, plus (scratch mode only)
/// the data-dir base to hand the spawned engine so scans land in that same file.
struct Target {
    db: PathBuf,
    engine_data_home: Option<PathBuf>,
}

/// Pure precedence resolver (env + scratch base injected) so it can be
/// unit-tested without touching the process environment.
fn resolve_target(
    db_flag: Option<PathBuf>,
    fileid_db: Option<String>,
    cffixed_home: Option<String>,
    scratch_home: impl FnOnce() -> Result<PathBuf>,
) -> Result<Target> {
    if let Some(p) = db_flag {
        return Ok(Target {
            db: p,
            engine_data_home: None,
        });
    }
    if let Some(s) = fileid_db {
        return Ok(Target {
            db: PathBuf::from(s),
            engine_data_home: None,
        });
    }
    if let Some(home) = cffixed_home {
        return Ok(Target {
            db: PathBuf::from(home).join("fileid.sqlite"),
            engine_data_home: None,
        });
    }
    // No explicit library → SCRATCH. The engine appends `FileID/` to whatever
    // we hand it as its data home, so the scratch library is at
    // `<base>/FileID/fileid.sqlite`; pointing the engine at the same `<base>`
    // makes a scan write exactly the file the TUI reads.
    let base = scratch_home().context("resolving scratch library location")?;
    let db = base.join("FileID").join("fileid.sqlite");
    Ok(Target {
        db,
        engine_data_home: Some(base),
    })
}

/// The persistent scratch data-dir base used when no `--db`/env is given. Placed
/// as a *sibling* of the real FileID data dir (clearly named, easy to find or
/// delete) so the scratch library never collides with the desktop library at
/// `<data>/FileID/fileid.sqlite` — this base is `<data>/FileID-TUI-Scratch`.
/// Falls back to a temp dir when the platform data dir can't be resolved.
fn default_scratch_home() -> Result<PathBuf> {
    if let Ok(root) = fileid_engine::paths::root() {
        if let Some(parent) = root.parent() {
            return Ok(parent.join("FileID-TUI-Scratch"));
        }
        return Ok(root.join("tui-scratch"));
    }
    Ok(std::env::temp_dir().join("FileID-TUI-Scratch"))
}

/// Replace a leading `$HOME` with `~`.
pub fn collapse_home(p: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            if let Some(rest) = p.strip_prefix(&home) {
                return format!("~{rest}");
            }
        }
    }
    p.to_string()
}

/// What `parse_args` decided the process should do.
pub enum Invocation {
    /// Launch the TUI against this optional `--db` override.
    Run { db: Option<PathBuf> },
    /// Print help/version text and exit 0.
    Print(String),
    /// Bad usage; print to stderr and exit 2.
    Error(String),
}

/// Parse `argv[1..]`. Supports `--db <path>` / `--db=<path>`, `-h`/`--help`,
/// `-V`/`--version`. Anything else is an error.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Invocation {
    let mut db: Option<PathBuf> = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Invocation::Print(help_text()),
            "-V" | "--version" => {
                return Invocation::Print(format!("fileid-tui {}", env!("CARGO_PKG_VERSION")))
            }
            "--db" => match it.next() {
                Some(v) => db = Some(PathBuf::from(v)),
                None => return Invocation::Error("--db requires a PATH argument".into()),
            },
            other if other.starts_with("--db=") => {
                db = Some(PathBuf::from(&other["--db=".len()..]));
            }
            other => {
                return Invocation::Error(format!("unexpected argument: {other}"));
            }
        }
    }
    Invocation::Run { db }
}

fn help_text() -> String {
    format!(
        "fileid-tui {ver} — FileID terminal UI (ratatui) over the shared Rust engine\n\
         \n\
         USAGE:\n    fileid-tui [--db <PATH>]\n\
         \n\
         By default the TUI opens an EMPTY scratch library; press s to scan a\n\
         folder and its files accumulate there. Pass --db to open an existing\n\
         library instead (e.g. your desktop app's).\n\
         \n\
         OPTIONS:\n    \
         --db <PATH>    Open a specific library (SQLite path) instead of the\n                   \
         empty scratch default. Also honors $FILEID_DB /\n                   \
         $CFFIXED_USER_HOME.\n    \
         -h, --help     Print this help.\n    \
         -V, --version  Print version.\n\
         \n\
         KEYS (in-app):\n    \
         Tab / Shift-Tab   switch tab        1-5   jump to tab\n    \
         Up/Down or j/k    move selection    s     scan a folder (engine)\n    \
         /                 search (Library)  r     reload from DB\n    \
         ?                 toggle help       q     quit\n",
        ver = env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_flag_wins() {
        let got = resolve_target(
            Some(PathBuf::from("/tmp/explicit.sqlite")),
            Some("/env/fileid.sqlite".into()),
            Some("/sandbox".into()),
            || Ok(PathBuf::from("/scratch")),
        )
        .unwrap();
        assert_eq!(got.db, PathBuf::from("/tmp/explicit.sqlite"));
        assert_eq!(got.engine_data_home, None, "explicit --db is not scratch");
    }

    #[test]
    fn fileid_db_env_beats_cffixed_and_scratch() {
        let got = resolve_target(
            None,
            Some("/env/fileid.sqlite".into()),
            Some("/sandbox".into()),
            || Ok(PathBuf::from("/scratch")),
        )
        .unwrap();
        assert_eq!(got.db, PathBuf::from("/env/fileid.sqlite"));
        assert_eq!(got.engine_data_home, None);
    }

    #[test]
    fn cffixed_home_joins_fileid_sqlite() {
        let got = resolve_target(None, None, Some("/sandbox/home".into()), || {
            Ok(PathBuf::from("/scratch"))
        })
        .unwrap();
        assert_eq!(got.db, PathBuf::from("/sandbox/home/fileid.sqlite"));
        assert_eq!(got.engine_data_home, None);
    }

    /// FIX 1 — with no `--db`/env, the default is a SCRATCH library (the TUI
    /// opens EMPTY), and the engine is pointed at the same base so a scan writes
    /// the exact file the TUI reads: `<base>/FileID/fileid.sqlite`.
    #[test]
    fn defaults_to_empty_scratch_not_the_app_library() {
        let got = resolve_target(None, None, None, || Ok(PathBuf::from("/scratch"))).unwrap();
        assert_eq!(got.db, PathBuf::from("/scratch/FileID/fileid.sqlite"));
        assert_eq!(got.engine_data_home, Some(PathBuf::from("/scratch")));
    }

    #[test]
    fn parse_db_long_and_eq_forms() {
        match parse_args(["--db".to_string(), "/a/b.sqlite".to_string()]) {
            Invocation::Run { db } => assert_eq!(db, Some(PathBuf::from("/a/b.sqlite"))),
            _ => panic!("expected Run"),
        }
        match parse_args(["--db=/c/d.sqlite".to_string()]) {
            Invocation::Run { db } => assert_eq!(db, Some(PathBuf::from("/c/d.sqlite"))),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parse_help_and_unknown() {
        assert!(matches!(
            parse_args(["--help".to_string()]),
            Invocation::Print(_)
        ));
        assert!(matches!(
            parse_args(["--bogus".to_string()]),
            Invocation::Error(_)
        ));
        assert!(matches!(
            parse_args(["--db".to_string()]),
            Invocation::Error(_)
        ));
    }
}
