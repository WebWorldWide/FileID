//! `fileid` — the cross-platform FileID command-line front-end.
//!
//! A thin client over the shared Rust engine (`fileid-engine`). It links the
//! engine as a library and reuses its DB schema/migrations, file
//! classification, and restructure rule cascade directly (in-process), so the
//! CLI can never drift from the engine contract. The MVP is read/query + plan
//! only — no destructive apply.
//!
//! Cross-OS despite living under `platforms/`: builds and runs identically on
//! macOS, Linux, and Windows.

mod context;
mod dedupe;
mod info;
mod people;
mod restructure;
mod scan;
mod search;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use context::Ctx;

#[derive(Parser)]
#[command(
    name = "fileid",
    version,
    about = "FileID — on-device AI file organizer (CLI front-end over the Rust engine)",
    long_about = None,
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct GlobalArgs {
    /// Library SQLite path. Overrides $FILEID_DB / $CFFIXED_USER_HOME /
    /// the engine default ($XDG_DATA_HOME or %LOCALAPPDATA%).
    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,

    /// Emit machine-readable JSON instead of human tables.
    #[arg(long, global = true)]
    json: bool,

    /// Suppress progress and non-essential output.
    #[arg(long, global = true)]
    quiet: bool,

    /// Disable ANSI color.
    #[arg(long = "no-color", global = true)]
    no_color: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Index a directory into the library (model-free FTS indexer).
    Scan {
        /// Directory to scan.
        #[arg(value_name = "PATH")]
        path: PathBuf,
        /// Reprocess every file, even unchanged ones.
        #[arg(long)]
        rescan: bool,
    },

    /// Full-text keyword search (model-free). `--similar` needs ML models.
    Search {
        /// Search terms.
        #[arg(value_name = "QUERY", required = true, num_args = 1..)]
        query: Vec<String>,
        /// Semantic / similarity search (requires CLIP models; not in MVP).
        #[arg(long)]
        similar: bool,
        /// Maximum results.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },

    /// Show a file's metadata, tags, people, and a text snippet.
    Info {
        /// A file path or numeric file id.
        #[arg(value_name = "PATH-OR-ID")]
        target: String,
    },

    /// List person clusters (id, name, face count).
    People,

    /// List duplicate / near-duplicate groups (read-only).
    Dedupe {
        /// Group byte-identical files (BLAKE3 content hash). Default.
        #[arg(long)]
        exact: bool,
        /// Group near-duplicates by perceptual-hash Hamming distance.
        #[arg(long)]
        similar: bool,
        /// Near-dup Hamming threshold (bits).
        #[arg(long, default_value_t = 8)]
        threshold: u32,
    },

    /// Compute + print the proposed reorganization (read-only).
    Restructure {
        /// Required: produce the plan. (Apply is a documented follow-on.)
        #[arg(long)]
        plan: bool,
        /// Library root the plan organizes into. Defaults to the indexed
        /// files' common ancestor.
        #[arg(value_name = "ROOT")]
        root: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let ctx = match Ctx::resolve(
        cli.global.db,
        cli.global.json,
        cli.global.quiet,
        cli.global.no_color,
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    let result = match cli.command {
        Command::Scan { path, rescan } => scan::run(&ctx, &path, rescan),
        Command::Search { query, similar, limit } => search::run(&ctx, &query, similar, limit),
        Command::Info { target } => info::run(&ctx, &target),
        Command::People => people::run(&ctx),
        Command::Dedupe { exact, similar, threshold } => {
            dedupe::run(&ctx, exact, similar, threshold)
        }
        Command::Restructure { plan, root } => restructure::run(&ctx, plan, root),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
