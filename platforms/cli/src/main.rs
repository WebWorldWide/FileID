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
mod models;
mod people;
mod restructure;
mod runtime;
mod scan;
mod scan_models;
mod search;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use context::Ctx;

/// Shown at the foot of `fileid --help`: a few copy-pasteable starting points.
const HELP_FOOTER: &str = "\
Examples:
  fileid scan ~/Pictures              fast index — names + text, no models needed
  fileid search \"invoice 2023\"        keyword-search the library
  fileid models download --all        install the AI models (from huggingface.co)
  fileid scan ~/Pictures --models     full AI scan — tags · faces · visual search

AI scans need two models (mobileclip_s2 + arcface); see `fileid models list`.
Everything runs on-device — no cloud, no telemetry. Run a bare `fileid` for a tour.";

#[derive(Parser)]
#[command(
    name = "fileid",
    version,
    about = "FileID — on-device AI file organizer (CLI front-end over the Rust engine)",
    long_about = None,
    after_help = HELP_FOOTER,
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    // Optional so bare `fileid` prints a friendly getting-started intro (FIX 4)
    // instead of clap's terse usage error. `--help` / `--version` still work
    // (clap intercepts them); an *unknown* subcommand still errors.
    #[command(subcommand)]
    command: Option<Command>,
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
    ///
    /// Example: fileid scan ~/Pictures --models
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
    ///
    /// Example: fileid search "vacation 2023"   ·   fileid search --similar 1234
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
    ///
    /// Example: fileid info ~/Pictures/IMG_1234.jpg   (or a numeric file id)
    Info {
        /// A file path or numeric file id.
        #[arg(value_name = "PATH-OR-ID")]
        target: String,
    },

    /// List person clusters (id, name, face count).
    ///
    /// Example: fileid people
    People,

    /// List (or, with `--apply`, remove) duplicate / near-duplicate groups.
    ///
    /// Listing is read-only. `--apply` keeps the first file of each group and
    /// removes the rest — to the Recycle Bin / Trash by default (recoverable;
    /// Windows + Linux), or permanently with `--delete`. SAFE: nothing is
    /// removed without `--apply`, and `--apply` prompts unless `--yes`.
    ///
    /// Example: fileid dedupe --exact            (list byte-identical groups)
    ///          fileid dedupe --exact --apply    (trash all but one per group)
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
    ///
    /// Example: fileid restructure --plan            (preview only)
    ///          fileid restructure --apply --dry-run (preview the moves)
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

    /// List or download the engine's AI models (for full-ML `scan --models`).
    ///
    /// `list` shows the commercial-clean model set, each model's installed
    /// state, size, license, and HuggingFace repo. `download` fetches +
    /// installs them into the engine's own models dir so `scan --models` can
    /// use them — user-initiated downloads from huggingface.co (the only
    /// network egress).
    ///
    /// Example: fileid models list   ·   fileid models download --all
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },

    /// Manage the ONNX Runtime library the full-ML scan loads (`status` /
    /// `install`).
    ///
    /// Separate from `models` (the AI weights): this is the inference library
    /// that loads them. On macOS it must be installed once (`ort` ships only a
    /// static lib for arm64); on Windows/Linux the platform provides it.
    ///
    /// Example: fileid runtime status   ·   fileid runtime install
    Runtime {
        #[command(subcommand)]
        cmd: RuntimeCmd,
    },
}

#[derive(Subcommand)]
enum RuntimeCmd {
    /// Show whether ONNX Runtime is installed + where the engine looks for it.
    ///
    /// Example: fileid runtime status
    Status,

    /// Install the ONNX Runtime library where the engine loads it (macOS).
    ///
    /// Idempotent: reports + exits if it's already available (e.g. via
    /// Homebrew). Provisions from a local copy when possible (no network), else
    /// guides you through the one-time install. On Windows/Linux there's nothing
    /// to install — the platform provides the runtime.
    ///
    /// Example: fileid runtime install
    Install {
        /// Reinstall into the engine runtime dir even if one is already found.
        #[arg(long)]
        force: bool,
        /// Skip the confirmation prompt for a download.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ModelsCmd {
    /// List the engine's AI models: installed state, size, license, HF repo.
    ///
    /// Example: fileid models list
    List,

    /// Download + install models into the engine's models dir.
    ///
    /// Name specific models, or `--all` for the whole set. `--dry-run` previews
    /// the repos + total size without fetching anything (the set is tens of GB
    /// with the Deep Analyze VLMs). A large or `--all` download prompts for
    /// confirmation unless `--yes`.
    ///
    /// Example: fileid models download arcface mobileclip_s2   (the two required)
    Download {
        /// Download the entire commercial-clean model set.
        #[arg(long)]
        all: bool,
        /// Show what WOULD download (repos + total size); fetch nothing.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Skip the confirmation prompt for a large / `--all` download.
        #[arg(long)]
        yes: bool,
        /// Machine-readable progress on stdout (`PROGRESS\t<pct>\t<label>`) for
        /// the TUI installer; suppresses the human bar. Hidden from `--help`.
        #[arg(long = "porcelain-progress", hide = true)]
        porcelain_progress: bool,
        /// Model names to download (e.g. `arcface mobileclip_s2 ram_plus`).
        #[arg(value_name = "NAME", num_args = 0..)]
        names: Vec<String>,
    },
}

/// Friendly getting-started shown for a bare `fileid` (no subcommand) — what it
/// is, the handful of commands worth trying first (copy-pasteable), and the
/// model story. Replaces clap's terse "USAGE:" error.
fn intro() -> &'static str {
    "FileID — on-device AI file organizer.\n\
     Tag, search, dedupe, and tidy your files locally — no cloud, no telemetry.\n\
     \n\
     Get started\n  \
     fileid scan ~/Pictures            index a folder fast (names + text; no models)\n  \
     fileid search \"invoice 2023\"       keyword-search what you indexed\n  \
     fileid models list                see the AI models and what's installed\n  \
     fileid models download --all      install them once (from huggingface.co)\n  \
     fileid scan ~/Pictures --models   full AI scan: image tags, faces, visual search\n\
     \n\
     More\n  \
     fileid search --similar 1234      files that look like file #1234   (needs an AI scan)\n  \
     fileid people                     people found by face clustering   (needs an AI scan)\n  \
     fileid dedupe --similar           find visually-similar duplicates\n  \
     fileid restructure --plan         preview a tidy folder layout\n  \
     fileid info <path-or-id>          everything indexed about one file\n\
     \n\
     The AI scan needs two models — mobileclip_s2 and arcface; `fileid models download`\n\
     fetches them. Without models, `fileid scan` still does a fast names + plain-text index.\n\
     \n\
     Run `fileid <command> --help` for details and an example.\n\
     (macOS: with no --db, commands read your FileID desktop app's library.)"
}

/// Point the engine at its OWN writable models dir for this process (and any
/// engine subprocess we spawn), unless the user already pinned
/// `FILEID_MODELS_DIR`. On macOS this is the XDG `~/.local/share/FileID/Models`,
/// NOT the desktop app's read-only CoreML `~/Library/Application Support/...` —
/// so `fileid models download` writes the engine's ONNX/GGUF weights somewhere
/// it can also read for `scan --models`. On Windows/Linux this is exactly the
/// default the engine already resolves to (so app-installed models are still
/// found); it only makes that choice explicit + inheritable by the spawned engine.
pub(crate) fn ensure_engine_models_dir() {
    if std::env::var_os("FILEID_MODELS_DIR").is_some_and(|v| !v.is_empty()) {
        return;
    }
    if let Ok(dir) = fileid_engine::paths::engine_models_dir() {
        std::env::set_var("FILEID_MODELS_DIR", dir);
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Bare `fileid`: print the friendly intro to stdout and exit cleanly.
    let Some(command) = cli.command else {
        println!("{}", intro());
        return ExitCode::SUCCESS;
    };

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

    let result = match command {
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
        Command::Models { cmd } => match cmd {
            ModelsCmd::List => models::list(&ctx),
            ModelsCmd::Download { all, dry_run, yes, porcelain_progress, names } => {
                models::download(&ctx, all, dry_run, yes, porcelain_progress, &names)
            }
        },
        Command::Runtime { cmd } => match cmd {
            RuntimeCmd::Status => runtime::status(&ctx),
            RuntimeCmd::Install { force, yes } => runtime::install(&ctx, yes, force),
        },
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
