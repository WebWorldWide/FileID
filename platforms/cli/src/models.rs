//! `fileid models` — list + download the engine's AI model weights.
//!
//! Reuses the engine's canonical registry (`models::registry::lookup_full`) for
//! URLs / dests / SHA256 pins / sizes and its SHA256-verified HuggingFace
//! downloader (`downloader::install_model_blocking`) — the SAME code the desktop
//! apps drive over IPC — so the CLI can never drift from the pinned,
//! commercial-clean set. Downloads land in the engine's OWN models dir (see
//! [`crate::ensure_engine_models_dir`]), so `fileid scan <folder> --models` then
//! finds + uses them.
//!
//! Network egress: huggingface.co only (the pinned manifest URLs), triggered by
//! this user-initiated command — the project's single allowed network call.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use fileid_engine::downloader::{install_model_blocking, InstallFileProgress};
use fileid_engine::models::registry::{self, LookupResult, Model};

use crate::context::{human_size, print_json, Ctx};

/// One curated, cross-platform, commercial-clean model the engine uses. `kind`
/// is the registry key passed to `lookup_full`; `license` + `category` are the
/// `shared/docs/MODELS.md` facts the registry doesn't carry. `required` marks the
/// two models the `scan --models` pre-flight gate demands.
struct Catalog {
    name: &'static str,
    kind: &'static str,
    license: &'static str,
    category: &'static str,
    /// Required by the `scan --models` gate (mirror of scan_models::REQUIRED_MODELS).
    required: bool,
}

/// The models `fileid models` lists + downloads, in display order. The Windows
/// accelerator runtime/EP packs (llama.cpp / cuDNN / ORT-CUDA / ORT-OpenVINO)
/// are intentionally absent: they're platform-specific DLL bundles, not
/// cross-platform model weights, and several aren't HuggingFace-hosted. Every
/// entry here is permissively licensed (Apache-2.0 / MIT; Gemma under Google's
/// commercially-usable Gemma Terms) per the commercial-clean posture.
const CATALOG: &[Catalog] = &[
    Catalog { name: "arcface",           kind: "arcface",           license: "MIT + Apache-2.0", category: "Faces (detect + embed)", required: true },
    Catalog { name: "mobileclip_s2",     kind: "mobileclip_s2",     license: "MIT",              category: "Image search (CLIP)",    required: true },
    Catalog { name: "clip_text",         kind: "clip_text",         license: "MIT",              category: "Text→image search",      required: false },
    Catalog { name: "ram_plus",          kind: "ram_plus",          license: "Apache-2.0",       category: "Image tagging",          required: false },
    Catalog { name: "bge_text",          kind: "bge_text",          license: "MIT",              category: "Document search",        required: false },
    Catalog { name: "florence2",         kind: "florence2",         license: "MIT",              category: "Grounded regions",       required: false },
    Catalog { name: "mistral_small_3_2", kind: "mistral_small_3_2", license: "Apache-2.0",       category: "Deep Analyze VLM",       required: false },
    Catalog { name: "qwen2_5_vl_7b",     kind: "qwen2_5_vl_7b",     license: "Apache-2.0",       category: "Deep Analyze VLM",       required: false },
    Catalog { name: "gemma_3_4b",        kind: "gemma_3_4b",        license: "Gemma Terms",      category: "Deep Analyze VLM",       required: false },
];

/// A catalog entry resolved against the live registry + on-disk install state.
struct Resolved {
    cat: &'static Catalog,
    model: Model,
    installed: bool,
    size_bytes: u64,
    repo: String,
}

fn resolve(cat: &'static Catalog) -> Option<Resolved> {
    let model = match registry::lookup_full(cat.kind) {
        LookupResult::Found(m) => m,
        LookupResult::Unknown => return None,
    };
    let installed = registry::sentinel_path(&model).is_some_and(|p| p.exists());
    let size_bytes = model.files.iter().map(|f| f.approx_bytes).sum();
    let repo = hf_repo(model.files.first().map(|f| f.url.as_str()).unwrap_or(""));
    Some(Resolved { cat, model, installed, size_bytes, repo })
}

/// `https://huggingface.co/<org>/<repo>/resolve/...` → `<org>/<repo>`. Falls
/// back to the raw URL for the (cross-platform-excluded) non-HF pack URLs.
fn hf_repo(url: &str) -> String {
    url.strip_prefix("https://huggingface.co/")
        .and_then(|rest| {
            let mut it = rest.split('/');
            Some(format!("{}/{}", it.next()?, it.next()?))
        })
        .unwrap_or_else(|| url.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{keep}…")
    }
}

/// `fileid models list` — show every model, its installed state, size, license,
/// and HF repo, plus where the engine expects them on disk.
pub fn list(ctx: &Ctx) -> Result<()> {
    crate::ensure_engine_models_dir();
    let resolved: Vec<Resolved> = CATALOG.iter().filter_map(resolve).collect();
    let models_dir = fileid_engine::paths::models_dir().ok();

    if ctx.json {
        let arr: Vec<_> = resolved
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.cat.name,
                    "kind": r.model.id,
                    "installed": r.installed,
                    "required": r.cat.required,
                    "sizeBytes": r.size_bytes,
                    "sizeHuman": human_size(r.size_bytes as i64),
                    "license": r.cat.license,
                    "category": r.cat.category,
                    "repo": r.repo,
                    "files": r.model.files.iter().map(|f| serde_json::json!({
                        "name": f.dest.file_name().and_then(|n| n.to_str()),
                        "url": f.url,
                        "sha256": f.sha256,
                        "bytes": f.approx_bytes,
                        "dest": f.dest.display().to_string(),
                        "present": f.dest.exists(),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        print_json(&serde_json::json!({
            "command": "models",
            "action": "list",
            "modelsDir": models_dir.map(|p| p.display().to_string()),
            "models": arr,
        }));
        return Ok(());
    }

    println!("{}", ctx.bold("FileID engine models"));
    println!(
        "  {:<18} {:<10} {:>10}  {:<16} {}",
        ctx.dim("NAME"),
        ctx.dim("INSTALLED"),
        ctx.dim("SIZE"),
        ctx.dim("LICENSE"),
        ctx.dim("CATEGORY"),
    );
    for r in &resolved {
        let mark = if r.installed { "yes" } else { "no" };
        let name = if r.cat.required {
            format!("{}*", r.cat.name)
        } else {
            r.cat.name.to_string()
        };
        println!(
            "  {:<18} {:<10} {:>10}  {:<16} {}",
            name,
            mark,
            human_size(r.size_bytes as i64),
            r.cat.license,
            r.cat.category,
        );
    }
    println!("  {}", ctx.dim("* required for `fileid scan --models`"));
    if let Some(dir) = models_dir {
        println!("  Models dir: {}", dir.display());
    }
    let any_missing = resolved.iter().any(|r| !r.installed);
    if any_missing {
        println!(
            "  {}",
            ctx.dim("Install with: fileid models download --all   (or name specific models)")
        );
    }
    Ok(())
}

/// `fileid models download [--all | <name>...] [--dry-run] [--yes]`.
pub fn download(ctx: &Ctx, all: bool, dry_run: bool, yes: bool, names: &[String]) -> Result<()> {
    crate::ensure_engine_models_dir();

    // Resolve the requested set of catalog entries.
    let selected: Vec<&'static Catalog> = if all {
        CATALOG.iter().collect()
    } else if names.is_empty() {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "models",
                "action": "download",
                "error": "no_selection",
                "message": "name one or more models, or pass --all",
            }));
            return Ok(());
        }
        println!("{}", ctx.bold("Nothing selected."));
        println!("  Name one or more models, or pass --all to fetch the whole set.");
        println!("    fileid models download arcface mobileclip_s2");
        println!("    fileid models download --all --dry-run   (preview sizes first)");
        println!("  {}", ctx.dim("See `fileid models list` for names."));
        return Ok(());
    } else {
        let mut out = Vec::with_capacity(names.len());
        for n in names {
            match CATALOG.iter().find(|c| c.name == n || c.kind == n) {
                Some(c) => out.push(c),
                None => anyhow::bail!(
                    "unknown model '{n}' — run `fileid models list` to see the available names"
                ),
            }
        }
        out
    };

    let resolved: Vec<Resolved> = selected.iter().copied().filter_map(resolve).collect();
    let pending: Vec<&Resolved> = resolved.iter().filter(|r| !r.installed).collect();
    let pending_bytes: u64 = pending.iter().map(|r| r.size_bytes).sum();

    // ── Dry-run: report what WOULD download (repos + total) and fetch nothing. ──
    if dry_run {
        if ctx.json {
            let arr: Vec<_> = resolved
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "name": r.cat.name,
                        "kind": r.model.id,
                        "installed": r.installed,
                        "sizeBytes": r.size_bytes,
                        "sizeHuman": human_size(r.size_bytes as i64),
                        "repo": r.repo,
                        "license": r.cat.license,
                        "files": r.model.files.iter().map(|f| f.url.clone()).collect::<Vec<_>>(),
                    })
                })
                .collect();
            print_json(&serde_json::json!({
                "command": "models",
                "action": "download",
                "dryRun": true,
                "totalBytes": pending_bytes,
                "totalHuman": human_size(pending_bytes as i64),
                "pendingCount": pending.len(),
                "models": arr,
            }));
            return Ok(());
        }
        println!("{}", ctx.bold("Dry run — nothing will be downloaded."));
        for r in &resolved {
            let state = if r.installed {
                ctx.dim("(installed)").to_string()
            } else {
                human_size(r.size_bytes as i64)
            };
            println!("  {:<18} {:>10}  {}", r.cat.name, state, ctx.dim(&r.repo));
        }
        println!(
            "  {} {} across {} model(s) from huggingface.co",
            ctx.bold("Would download:"),
            human_size(pending_bytes as i64),
            pending.len(),
        );
        return Ok(());
    }

    if pending.is_empty() {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "models",
                "action": "download",
                "installed": [],
                "message": "all selected models already installed",
            }));
        } else {
            println!("{}", ctx.bold("All selected models are already installed."));
        }
        return Ok(());
    }

    // Confirm before pulling a large set (the whole set, or > 1 GiB). `confirm`
    // returns false on a non-interactive stdin without --yes, so CI never
    // unattended-pulls GBs.
    let large = all || pending_bytes > 1_073_741_824;
    if large
        && !ctx.confirm(
            &format!(
                "Download {} model(s), ~{}, from huggingface.co?",
                pending.len(),
                human_size(pending_bytes as i64)
            ),
            yes,
        )
    {
        println!("Aborted. (nothing downloaded)");
        return Ok(());
    }

    // ── Download each pending model in turn, with per-file progress. ──
    let progress = make_progress(ctx);
    let mut installed_now: Vec<&str> = Vec::new();
    for r in &pending {
        ctx.progress(&format!(
            "{} {} ({}, {})",
            ctx.bold("Downloading"),
            r.cat.name,
            human_size(r.size_bytes as i64),
            ctx.dim(&r.repo),
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        install_model_blocking(&r.model, cancel, progress.clone())
            .with_context(|| format!("installing {}", r.cat.name))?;
        ctx.progress(&format!("  {} {} installed", ctx.bold("✓"), r.cat.name));
        installed_now.push(r.cat.name);
    }

    if ctx.json {
        print_json(&serde_json::json!({
            "command": "models",
            "action": "download",
            "installed": installed_now,
            "bytes": pending_bytes,
        }));
    } else {
        println!(
            "{} {} model(s) into {}",
            ctx.bold("Installed"),
            installed_now.len(),
            fileid_engine::paths::models_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "the engine models dir".into()),
        );
        println!(
            "  {}",
            ctx.dim("Run `fileid scan <folder> --models` to scan with the full ML pipeline.")
        );
    }
    Ok(())
}

/// A `Send + Sync` progress sink for [`install_model_blocking`]. Prints a single
/// in-place updating line per file to stderr (never stdout, so `--json` on
/// stdout stays clean); silent under `--quiet`/`--json`. No mutable state — each
/// call renders the current totals; the downloader throttles emission to ~20 Hz.
fn make_progress(ctx: &Ctx) -> Arc<dyn Fn(InstallFileProgress) + Send + Sync> {
    let silent = ctx.quiet || ctx.json;
    Arc::new(move |p: InstallFileProgress| {
        if silent {
            return;
        }
        use std::io::Write as _;
        let pct = match p.bytes_total {
            Some(t) if t > 0 => (p.bytes_done as f64 / t as f64 * 100.0).min(100.0),
            _ => 0.0,
        };
        let total_h = p
            .bytes_total
            .map(|t| human_size(t as i64))
            .unwrap_or_else(|| "?".to_string());
        let mbps = p.bytes_per_second / (1024.0 * 1024.0);
        let mut err = std::io::stderr();
        let _ = write!(
            err,
            "\r  [{}/{}] {:<28} {:>10} / {:<10} {:>5.1}% ({:>5.1} MB/s)   ",
            p.file_index + 1,
            p.file_count,
            truncate(&p.file_name, 28),
            human_size(p.bytes_done as i64),
            total_h,
            pct,
            mbps,
        );
        let _ = err.flush();
        // File complete → end the in-place line so the next file starts fresh.
        if p.bytes_total.is_some_and(|t| p.bytes_done >= t) {
            let _ = writeln!(err);
        }
    })
}
