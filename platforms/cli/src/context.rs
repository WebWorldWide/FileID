use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
pub use fileid_engine::read::text::escape_like;
pub use fileid_engine::util::display::{file_size_i64, truncate};
use fileid_engine::util::path_safety::strip_extended_length;

pub struct Ctx {
    pub json: bool,
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
    /// `scan --models` uses this to forward the exact path to the spawned
    /// engine through `FILEID_DB`.
    pub db_explicit: bool,
}

impl Ctx {
    /// Resolve the database path: `--db`, `$FILEID_DB`, `$CFFIXED_USER_HOME/fileid.sqlite`,
    /// then on macOS the desktop app's library if it exists, else the engine default.
    /// The macOS app writes to its own library path, not the engine's XDG path, so it wins over the default.
    pub fn resolve(
        db_flag: Option<PathBuf>,
        json: bool,
        quiet: bool,
        no_color: bool,
    ) -> Result<Self> {
        let fileid_db = nonempty_env_path("FILEID_DB");
        let cffixed_home = nonempty_env_path("CFFIXED_USER_HOME");
        let db_explicit = db_flag.is_some() || fileid_db.is_some() || cffixed_home.is_some();
        let db = if let Some(p) = db_flag {
            p
        } else if let Some(p) = fileid_db {
            p
        } else if let Some(home) = cffixed_home {
            home.join("fileid.sqlite")
        } else if let Some(p) = macos_app_db() {
            p
        } else {
            fileid_engine::paths::db_path().context("resolving default library location")?
        };
        // Honor the de-facto `NO_COLOR` standard (no-color.org): any value,
        // even empty, disables color — in addition to the explicit `--no-color`.
        let color_allowed = !no_color && std::env::var_os("NO_COLOR").is_none();
        let color = color_allowed && std::io::stdout().is_terminal();
        Ok(Self {
            json,
            quiet,
            color,
            color_allowed,
            db,
            db_explicit,
        })
    }

    /// Interactive yes/no gate for destructive actions. Returns true only on an explicit `--yes` (`assume_yes`) or a typed
    /// `y`/`yes` at a TTY; a non-interactive stdin without `--yes` returns false.
    pub fn confirm(&self, prompt: &str, assume_yes: bool) -> bool {
        use std::io::Write as _;
        if assume_yes {
            return true;
        }
        if !std::io::stdin().is_terminal() {
            return false;
        }
        eprint!("{} [y/N] ", terminal_output(prompt));
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
            eprintln!("{}", terminal_output(msg));
        }
    }

    pub fn bold(&self, s: &str) -> String {
        let s = terminal_text(s);
        if self.color {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s
        }
    }

    pub fn dim(&self, s: &str) -> String {
        let s = terminal_text(s);
        if self.color {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s
        }
    }

    /// Brand gold (`#FFCC00` ≈ xterm 220). Pad text to its column width before wrapping, so the invisible escape bytes never throw off `{:<width}` alignment.
    pub fn gold(&self, s: &str) -> String {
        let s = terminal_text(s);
        if self.color {
            format!("\x1b[38;5;220m{s}\x1b[0m")
        } else {
            s
        }
    }

    pub fn green(&self, s: &str) -> String {
        let s = terminal_text(s);
        if self.color {
            format!("\x1b[32m{s}\x1b[0m")
        } else {
            s
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

fn nonempty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The macOS desktop app's library, used only when it exists. `None` off macOS.
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
    use rusqlite::{OptionalExtension, params};
    if let Ok(id) = target.parse::<i64>()
        && let Ok(Some(id)) = conn
            .query_row(
                "SELECT id FROM files WHERE id = ?1 AND failed = 0",
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .optional()
    {
        return Some(id);
    }
    let canon = std::fs::canonicalize(target)
        .map(|p| strip_extended_length(&p).to_string_lossy().into_owned())
        .unwrap_or_else(|_| target.to_string());
    for candidate in [canon.as_str(), target] {
        if let Ok(Some(id)) = conn
            .query_row(
                "SELECT id FROM files WHERE path_text = ?1 AND failed = 0",
                params![candidate],
                |r| r.get::<_, i64>(0),
            )
            .optional()
        {
            return Some(id);
        }
    }
    let basename = escape_like(target.trim_start_matches(['/', '\\']));
    let slash_like = format!("%/{basename}");
    // SQL LIKE uses `\` as its escape character, so a literal Windows path
    // separator in the pattern is represented by two consecutive backslashes.
    let backslash_like = format!("%\\\\{basename}");
    conn.query_row(
        "SELECT id FROM files \
         WHERE failed = 0 AND (path_text LIKE ?1 ESCAPE '\\' OR path_text LIKE ?2 ESCAPE '\\') \
         ORDER BY path_text LIMIT 1",
        params![slash_like, backslash_like],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn print_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

pub fn display_path(p: &str) -> String {
    for name in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(name).filter(|value| !value.is_empty())
            && let Some(shown) = tilde_path(p, &home.to_string_lossy())
        {
            return terminal_text(&shown);
        }
    }
    terminal_text(p)
}

fn tilde_path(p: &str, home: &str) -> Option<String> {
    let home = home.trim_end_matches(['/', '\\']);
    if home.is_empty() {
        return None;
    }
    let rest = p.strip_prefix(home)?;
    (rest.is_empty() || rest.starts_with(['/', '\\'])).then(|| format!("~{rest}"))
}

/// Neutralize control characters before data-derived text reaches a terminal.
/// JSON output keeps the original value and relies on JSON escaping.
pub fn terminal_text(s: &str) -> String {
    s.chars()
        .map(|ch| {
            if matches!(ch, '\n' | '\r' | '\t') {
                ' '
            } else if ch.is_control() {
                '\u{fffd}'
            } else {
                ch
            }
        })
        .collect()
}

fn terminal_output(s: &str) -> String {
    const SAFE_SGR: [&str; 5] = [
        "\x1b[0m",
        "\x1b[1m",
        "\x1b[2m",
        "\x1b[32m",
        "\x1b[38;5;220m",
    ];
    let mut output = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(sequence) = SAFE_SGR
            .iter()
            .find(|sequence| rest.starts_with(**sequence))
        {
            output.push_str(sequence);
            rest = &rest[sequence.len()..];
            continue;
        }
        let ch = rest
            .chars()
            .next()
            .expect("non-empty string has a character");
        if matches!(ch, '\n' | '\r' | '\t') {
            output.push(' ');
        } else if ch.is_control() {
            output.push('\u{fffd}');
        } else {
            output.push(ch);
        }
        rest = &rest[ch.len_utf8()..];
    }
    output
}

#[cfg(test)]
mod tests {
    #[test]
    fn tilde_replaces_home_only_at_a_path_component_boundary() {
        assert_eq!(tilde_path("/home/al/x", "/home/al").as_deref(), Some("~/x"));
        assert_eq!(tilde_path("/home/al", "/home/al/").as_deref(), Some("~"));
        assert_eq!(tilde_path("/home/alice/x", "/home/al"), None);
        assert_eq!(tilde_path("/home/x", "/"), None);
        assert_eq!(
            tilde_path("C:\\Users\\Al\\a", "C:\\Users\\Al").as_deref(),
            Some("~\\a")
        );
        assert_eq!(tilde_path("C:\\Users\\Alan\\a", "C:\\Users\\Al"), None);
    }

    use super::*;
    use rusqlite::Connection;

    fn db_with(rows: &[(i64, &str)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, failed INTEGER NOT NULL DEFAULT 0);",
        )
            .unwrap();
        for (id, p) in rows {
            conn.execute(
                "INSERT INTO files (id, path_text) VALUES (?1, ?2)",
                rusqlite::params![id, p],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn terminal_text_neutralizes_escape_and_line_controls() {
        assert_eq!(
            terminal_text("safe\x1b[2J\nnext\tcell"),
            "safe�[2J next cell"
        );
        assert_eq!(terminal_text("café 📷"), "café 📷");
    }

    #[test]
    fn terminal_output_preserves_only_fileid_sgr_sequences() {
        assert_eq!(
            terminal_output("\x1b[2msafe\x1b[0m\x1b[2J\x1b]0;owned\x07"),
            "\x1b[2msafe\x1b[0m�[2J�]0;owned�"
        );
    }

    #[test]
    fn display_path_neutralizes_filename_escape_sequences() {
        assert_eq!(
            display_path("/tmp/report\x1b[2J.pdf"),
            "/tmp/report�[2J.pdf"
        );
    }

    #[test]
    fn basename_underscore_is_literal_not_a_wildcard() {
        // Only a look-alike is indexed; an unescaped `_` wildcard would wrongly match it.
        let conn = db_with(&[(1, "/seed/reportX2023.pdf")]);
        assert_eq!(resolve_file_id(&conn, "report_2023.pdf"), None);

        // The exact literal basename still resolves once it is present.
        let conn = db_with(&[(1, "/seed/reportX2023.pdf"), (2, "/seed/report_2023.pdf")]);
        assert_eq!(resolve_file_id(&conn, "report_2023.pdf"), Some(2));
    }

    #[test]
    fn basename_percent_is_literal_not_a_wildcard() {
        let conn = db_with(&[(1, "/seed/50-discount-off.png")]);
        assert_eq!(resolve_file_id(&conn, "50%off.png"), None);

        let conn = db_with(&[(1, "/seed/50%off.png")]);
        assert_eq!(resolve_file_id(&conn, "50%off.png"), Some(1));
    }

    #[test]
    fn basename_match_is_deterministic_across_dirs() {
        // Two legitimate matches: the lexicographically-first path wins
        // deterministically, not an arbitrary `LIMIT 1` row (id 7 by rowid).
        let conn = db_with(&[(7, "/b/report.pdf"), (3, "/a/report.pdf")]);
        assert_eq!(resolve_file_id(&conn, "report.pdf"), Some(3));
    }

    #[test]
    fn basename_match_accepts_windows_path_separators() {
        let conn = db_with(&[(7, r"D:\Photos\report.pdf"), (3, r"C:\Archive\report.pdf")]);
        assert_eq!(
            resolve_file_id(&conn, "report.pdf"),
            Some(3),
            "basename lookup must work for Windows path_text rows and remain deterministic"
        );
    }
}
