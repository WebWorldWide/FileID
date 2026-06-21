//! `fileid` — the cross-platform FileID command-line front-end.
//!
//! A thin client over the shared Rust engine (`fileid-engine`). It links the
//! engine as a library and reuses its DB schema/migrations, file
//! classification, restructure cascade, and apply/trash code directly
//! (in-process), so the CLI can never drift from the engine contract. Reads and
//! plans are model-free; `dedupe --apply` / `restructure --apply` call the
//! engine's exact apply path; `scan --models` spawns the engine binary to drive
//! the full ML pipeline. Destructive actions are opt-in and prompt unless
//! `--yes`.
//!
//! Cross-OS despite living under `platforms/`: builds and runs identically on
//! macOS, Linux, and Windows.

mod context;
mod dedupe;
mod info;
mod people;
mod restructure;
mod scan;
mod scan_models;
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
    /// Index a directory into the library.
    ///
    /// Default: model-free FTS indexer (filenames + plain-text content).
    /// `--models`: drives the engine's FULL pipeline (image tags, CLIP
    /// embeddings, faces, perceptual + content hashes) by spawning the engine
    /// binary — requires the AI models to be installed.
    Scan {
        /// Directory to scan.
        #[arg(value_name = "PATH")]
        path: PathBuf,
        /// Reprocess every file, even unchanged ones.
        #[arg(long)]
        rescan: bool,
        /// Run the FULL ML pipeline via the engine (needs installed models).
        #[arg(long)]
        models: bool,
    },

    /// Search the library.
    ///
    /// With QUERY terms: model-free FTS keyword search. With `--similar
    /// <path-or-id>`: visual / semantic nearest-neighbor over stored CLIP
    /// embeddings (needs a `scan --models` / desktop scan to have populated
    /// them).
    Search {
        /// Search terms (FTS keyword search). Omit when using `--similar`.
        #[arg(value_name = "QUERY", num_args = 0..)]
        query: Vec<String>,
        /// Find files visually/semantically nearest to this file
        /// (`<path-or-id>`) using stored CLIP embeddings.
        #[arg(long, value_name = "PATH-OR-ID")]
        similar: Option<String>,
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

    /// List (or, with `--apply`, remove) duplicate / near-duplicate groups.
    ///
    /// Listing is read-only. `--apply` keeps the first file of each group and
    /// removes the rest — to the Recycle Bin / Trash by default (recoverable;
    /// Windows + Linux), or permanently with `--delete`. SAFE: nothing is
    /// removed without `--apply`, and `--apply` prompts unless `--yes`.
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
        /// Remove duplicates (keep one per group). Destructive — see `--delete`.
        #[arg(long)]
        apply: bool,
        /// Preview what `--apply` would remove; change nothing.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Permanently delete instead of trashing (irreversible).
        #[arg(long)]
        delete: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },

    /// Compute the proposed reorganization, or (with `--apply`) execute it.
    ///
    /// Without `--apply` this prints a read-only plan. `--apply` moves files
    /// into the proposed layout via the engine's exact apply path; SAFE —
    /// it prompts unless `--yes`, and `--dry-run` previews without moving.
    Restructure {
        /// Produce the read-only plan (default when neither flag is given).
        #[arg(long)]
        plan: bool,
        /// Execute the plan (move files). Destructive unless `--symlinks`.
        #[arg(long)]
        apply: bool,
        /// Preview what `--apply` would move; change nothing.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Create symlinks to the proposed layout instead of moving originals.
        #[arg(long)]
        symlinks: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
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
        Command::Scan { path, rescan, models } => {
            if models {
                scan_models::run(&ctx, &path, rescan)
            } else {
                scan::run(&ctx, &path, rescan)
            }
        }
        Command::Search { query, similar, limit } => {
            search::run(&ctx, &query, similar.as_deref(), limit)
        }
        Command::Info { target } => info::run(&ctx, &target),
        Command::People => people::run(&ctx),
        Command::Dedupe { exact, similar, threshold, apply, dry_run, delete, yes } => {
            dedupe::run(&ctx, exact, similar, threshold, apply, dry_run, delete, yes)
        }
        Command::Restructure { plan, apply, dry_run, symlinks, yes, root } => {
            restructure::run(&ctx, plan, apply, dry_run, symlinks, yes, root)
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
