//! Run context: resolve the library SQLite path the same way the CLI does, and
//! a tiny hand-rolled argv parser (`--db`, `--help`, `--version`) so the TUI
//! adds no CLI-parsing dependency.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

/// Resolved run context threaded into the app.
pub struct Ctx {
    /// Absolute path to the library SQLite file.
    pub db: PathBuf,
}

impl Ctx {
    /// Resolve the database path. Precedence is byte-identical to the CLI
    /// (`platforms/cli/src/context.rs`) so both front-ends point at the same
    /// library by default:
    ///   1. `--db <path>`
    ///   2. `$FILEID_DB`
    ///   3. `$CFFIXED_USER_HOME/fileid.sqlite` (parity with the macOS app's
    ///      sandbox-root env var; convenient for isolating a test library)
    ///   4. `fileid_engine::paths::db_path()` — the engine's canonical location
    ///      (honors `$XDG_DATA_HOME` / `%LOCALAPPDATA%`), i.e. the same file the
    ///      desktop apps read/write.
    pub fn resolve(db_flag: Option<PathBuf>) -> Result<Self> {
        let default = fileid_engine::paths::db_path;
        let db = resolve_db(
            db_flag,
            std::env::var("FILEID_DB").ok(),
            std::env::var("CFFIXED_USER_HOME").ok(),
            default,
        )?;
        Ok(Self { db })
    }

    /// Collapse `$HOME` to `~` for compact display in the Settings/status line.
    pub fn db_label(&self) -> String {
        collapse_home(&self.db.to_string_lossy())
    }
}

/// Pure precedence resolver (env values injected) so it can be unit-tested
/// without touching the process environment.
fn resolve_db(
    db_flag: Option<PathBuf>,
    fileid_db: Option<String>,
    cffixed_home: Option<String>,
    default: impl FnOnce() -> Result<PathBuf>,
) -> Result<PathBuf> {
    if let Some(p) = db_flag {
        return Ok(p);
    }
    if let Some(s) = fileid_db {
        return Ok(PathBuf::from(s));
    }
    if let Some(home) = cffixed_home {
        return Ok(PathBuf::from(home).join("fileid.sqlite"));
    }
    default().context("resolving default library location")
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
         OPTIONS:\n    \
         --db <PATH>    Library SQLite path. Overrides $FILEID_DB /\n                   \
         $CFFIXED_USER_HOME / the engine default.\n    \
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
        let got = resolve_db(
            Some(PathBuf::from("/tmp/explicit.sqlite")),
            Some("/env/fileid.sqlite".into()),
            Some("/sandbox".into()),
            || Ok(PathBuf::from("/default/fileid.sqlite")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/tmp/explicit.sqlite"));
    }

    #[test]
    fn fileid_db_env_beats_cffixed_and_default() {
        let got = resolve_db(
            None,
            Some("/env/fileid.sqlite".into()),
            Some("/sandbox".into()),
            || Ok(PathBuf::from("/default/fileid.sqlite")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/env/fileid.sqlite"));
    }

    #[test]
    fn cffixed_home_joins_fileid_sqlite() {
        let got = resolve_db(None, None, Some("/sandbox/home".into()), || {
            Ok(PathBuf::from("/default/fileid.sqlite"))
        })
        .unwrap();
        assert_eq!(got, PathBuf::from("/sandbox/home/fileid.sqlite"));
    }

    #[test]
    fn falls_through_to_engine_default() {
        let got = resolve_db(None, None, None, || Ok(PathBuf::from("/default/fileid.sqlite")))
            .unwrap();
        assert_eq!(got, PathBuf::from("/default/fileid.sqlite"));
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
        assert!(matches!(parse_args(["--help".to_string()]), Invocation::Print(_)));
        assert!(matches!(parse_args(["--bogus".to_string()]), Invocation::Error(_)));
        assert!(matches!(parse_args(["--db".to_string()]), Invocation::Error(_)));
    }
}
