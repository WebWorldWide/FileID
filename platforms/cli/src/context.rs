//! Run context: global flags + output helpers shared by every subcommand.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// Resolved global flags, threaded through every command handler.
pub struct Ctx {
    /// Emit machine-readable JSON instead of human tables.
    pub json: bool,
    /// Suppress progress + non-essential chrome.
    pub quiet: bool,
    /// ANSI color is allowed on stdout (color permitted AND stdout is a TTY).
    pub color: bool,
    /// Color is permitted at all: neither `--no-color` nor `$NO_COLOR` set.
    /// Independent of which stream is a TTY — the stderr progress bar consults
    /// this with its own `stderr().is_terminal()` check.
    pub color_allowed: bool,
    /// Absolute path to the library SQLite file.
    pub db: PathBuf,
    /// The caller pinned the library location (`--db`, `$FILEID_DB`, or
    /// `$CFFIXED_USER_HOME`) rather than falling back to the engine default.
    /// `scan --models` uses this to warn that the full pipeline writes the
    /// engine's own library (XDG/`%LOCALAPPDATA%`-located), not an arbitrary db.
    pub db_explicit: bool,
}

impl Ctx {
    /// Resolve the database path. Precedence:
    ///   1. `--db <path>`
    ///   2. `$FILEID_DB`
    ///   3. `$CFFIXED_USER_HOME/fileid.sqlite` (parity with the macOS app's
    ///      sandbox-root env var; convenient for isolating a test library)
    ///   4. (macOS only) `~/Library/Application Support/FileID/fileid.sqlite`
    ///      if it exists — the location the macOS Swift app writes its library
    ///      to, so `fileid` "just works" against the desktop app on a Mac.
    ///   5. `fileid_engine::paths::db_path()` — the engine's canonical
    ///      location (honors `$XDG_DATA_HOME` / `%LOCALAPPDATA%`). On Windows
    ///      and Linux this is the same file the desktop app reads/writes; on
    ///      macOS the Swift app uses (4) instead (the engine defaults to the
    ///      XDG `~/.local/share/FileID` path there), which is why (4) wins.
    pub fn resolve(
        db_flag: Option<PathBuf>,
        json: bool,
        quiet: bool,
        no_color: bool,
    ) -> Result<Self> {
        let db_explicit = db_flag.is_some()
            || std::env::var_os("FILEID_DB").is_some()
            || std::env::var_os("CFFIXED_USER_HOME").is_some();
        let db = if let Some(p) = db_flag {
            p
        } else if let Ok(s) = std::env::var("FILEID_DB") {
            PathBuf::from(s)
        } else if let Ok(home) = std::env::var("CFFIXED_USER_HOME") {
            PathBuf::from(home).join("fileid.sqlite")
        } else if let Some(p) = macos_app_db() {
            p
        } else {
            fileid_engine::paths::db_path().context("resolving default library location")?
        };
        // Honor the de-facto `NO_COLOR` standard (no-color.org): any value,
        // even empty, disables color — in addition to the explicit `--no-color`.
        let color_allowed = !no_color && std::env::var_os("NO_COLOR").is_none();
        let color = color_allowed && std::io::stdout().is_terminal();
        Ok(Self { json, quiet, color, color_allowed, db, db_explicit })
    }

    /// Interactive yes/no gate for destructive actions. SAFE by construction:
    /// returns true only on an explicit `--yes` (`assume_yes`) or a typed
    /// `y`/`yes` at a TTY. A non-interactive stdin (pipe/CI) without `--yes`
    /// returns false — we never apply on a guess.
    pub fn confirm(&self, prompt: &str, assume_yes: bool) -> bool {
        use std::io::Write as _;
        if assume_yes {
            return true;
        }
        if !std::io::stdin().is_terminal() {
            return false;
        }
        eprint!("{prompt} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }

    /// Stream a progress line to stderr unless `--quiet`. Never goes to stdout
    /// so it can't corrupt `--json` output (which is the only thing on stdout).
    pub fn progress(&self, msg: &str) {
        if !self.quiet {
            eprintln!("{msg}");
        }
    }

    pub fn bold(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn dim(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Brand gold (`#FFCC00` ≈ xterm 220) — used to mark required models and
    /// other identity accents. Pad text to its column width BEFORE wrapping, so
    /// the invisible escape bytes never throw off `{:<width}` alignment.
    pub fn gold(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[38;5;220m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Green accent — used for the "installed" state.
    pub fn green(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[32m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// The DB file must exist for any read command. Gives a friendlier error
    /// than rusqlite's raw "unable to open database file".
    pub fn require_db_exists(&self) -> Result<()> {
        if self.db.exists() {
            Ok(())
        } else {
            anyhow::bail!(
                "no library database at {}\n  Run `fileid scan <path>` first, or pass --db <path>.",
                self.db.display()
            )
        }
    }
}

/// The macOS Swift app's library location
/// (`~/Library/Application Support/FileID/fileid.sqlite`), but only when it
/// already exists. The Swift front-end writes there — NOT to the engine's XDG
/// default (`~/.local/share/FileID`) — so on a Mac we prefer it when present,
/// letting read commands resolve the desktop app's real library without an
/// explicit `--db`. Returns `None` off macOS, leaving Win/Linux on the engine
/// default unchanged.
#[cfg(target_os = "macos")]
fn macos_app_db() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home).join("Library/Application Support/FileID/fileid.sqlite");
    p.exists().then_some(p)
}

#[cfg(not(target_os = "macos"))]
fn macos_app_db() -> Option<PathBuf> {
    None
}

/// Resolve a `<path-or-id>` argument to a `files.id`. Mirrors the lookup
/// order `info` uses (numeric id → exact canonical/raw path → basename suffix)
/// so the same argument resolves identically across subcommands.
pub fn resolve_file_id(conn: &rusqlite::Connection, target: &str) -> Option<i64> {
    use rusqlite::{params, OptionalExtension};
    if let Ok(id) = target.parse::<i64>() {
        if let Ok(Some(id)) = conn
            .query_row("SELECT id FROM files WHERE id = ?1", params![id], |r| {
                r.get::<_, i64>(0)
            })
            .optional()
        {
            return Some(id);
        }
    }
    let canon = std::fs::canonicalize(target)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| target.to_string());
    for candidate in [canon.as_str(), target] {
        if let Ok(Some(id)) = conn
            .query_row(
                "SELECT id FROM files WHERE path_text = ?1",
                params![candidate],
                |r| r.get::<_, i64>(0),
            )
            .optional()
        {
            return Some(id);
        }
    }
    let like = format!("%/{}", target.trim_start_matches('/'));
    conn.query_row(
        "SELECT id FROM files WHERE path_text LIKE ?1 LIMIT 1",
        params![like],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Print a JSON value to stdout (pretty when on a TTY-less pipe is fine too).
pub fn print_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

/// Compact a path for table display: keep it absolute but collapse `$HOME`.
pub fn display_path(p: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            if let Some(rest) = p.strip_prefix(&home) {
                return format!("~{rest}");
            }
        }
    }
    p.to_string()
}

/// Human-readable byte size.
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

/// Mirror of the engine's `util::path_safety::stable_path_hash` (which is
/// crate-private). Byte-faithful: lowercase the path, hash with the std
/// `DefaultHasher` (fixed-key SipHash → deterministic across runs/machines on
/// the same std), store the `i64`. Keeping this in lockstep means a row the
/// CLI writes is found by the engine's `index_files_on_path_hash` lookups and
/// vice-versa.
pub fn stable_path_hash(path: &str) -> i64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.to_ascii_lowercase().hash(&mut h);
    h.finish() as i64
}

/// Best-effort absolute, lexically-normalized path string for `path_text`.
pub fn canonical_path_text(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
