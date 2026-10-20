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

use std::collections::HashSet;
use std::io::{IsTerminal, Write as _};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use fileid_engine::downloader::{install_model_blocking, InstallFileProgress};
use fileid_engine::models::license;
use fileid_engine::models::registry::{self, LookupResult, Model};
use fileid_engine::models::required::REQUIRED_SCAN_MODELS;
use fileid_engine::util::display::{rate, size_pair};

use crate::context::{display_path, file_size_i64, print_json, truncate, Ctx};

/// One curated, cross-platform, commercial-clean model the engine uses. `kind`
/// is the registry key passed to `lookup_full`; `license` + `category` are the
/// `shared/docs/MODELS.md` facts the registry doesn't carry.
struct Catalog {
    name: &'static str,
    kind: &'static str,
    license: &'static str,
    category: &'static str,
}

impl Catalog {
    /// Required by the `scan --models` gate.
    fn required(&self) -> bool {
        REQUIRED_SCAN_MODELS.contains(&self.kind)
    }
}

fn ensure_license_acceptance(ctx: &Ctx, pending: &[&Resolved]) -> Result<()> {
    let state_root = fileid_engine::paths::root()?;
    let mut policies = Vec::new();
    for model in pending {
        let Some(policy) = license::policy_for(model.cat.kind) else {
            continue;
        };
        if !policies
            .iter()
            .any(|existing: &license::LicensePolicy| existing.key == policy.key)
        {
            policies.push(policy);
        }
    }

    for policy in policies {
        if license::is_accepted(&state_root, &policy) {
            continue;
        }
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "models",
                "action": "download",
                "error": "license_acceptance_required",
                "policy": policy.key,
                "reviewedAt": policy.reviewed_at,
                "termsUrl": policy.terms_url,
                "message": "run this command interactively to review and accept the model terms",
            }));
            anyhow::bail!(
                "{} acceptance is required before download",
                policy.display_name
            );
        }
        if !std::io::stdin().is_terminal() {
            anyhow::bail!(
                "{} acceptance is required before download; review {} and run this command interactively (--yes does not accept license terms)",
                policy.display_name,
                policy.terms_url
            );
        }
        eprintln!("{}", ctx.bold("License acceptance required"));
        eprintln!(
            "  This optional model is governed by the {}, not FileID's Apache-2.0 license.",
            policy.display_name
        );
        eprintln!("  Full terms: {}", policy.terms_url);
        eprint!("  Type I ACCEPT to accept and download [cancel]: ");
        let _ = std::io::stderr().flush();
        let mut response = String::new();
        if std::io::stdin().read_line(&mut response).is_err() || response.trim() != "I ACCEPT" {
            anyhow::bail!("license terms were not accepted; nothing was downloaded");
        }
        license::record(&state_root, &policy)
            .with_context(|| format!("recording {} acceptance", policy.key))?;
    }
    Ok(())
}

/// The models `fileid models` lists + downloads, in display order. The Windows
/// accelerator runtime/EP packs (llama.cpp / cuDNN / ORT-CUDA / ORT-OpenVINO)
/// are intentionally absent: they're platform-specific DLL bundles, not
/// cross-platform model weights, and several aren't HuggingFace-hosted. Every
/// entry here is permissively licensed (Apache-2.0 / MIT; Gemma under Google's
/// commercially-usable Gemma Terms) per the commercial-clean posture.
const CATALOG: &[Catalog] = &[
    Catalog {
        name: "arcface",
        kind: "arcface",
        license: "MIT + Apache-2.0",
        category: "Faces (detect + embed)",
    },
    Catalog {
        name: "mobileclip_s2",
        kind: "mobileclip_s2",
        license: "MIT",
        category: "Image search (CLIP)",
    },
    Catalog {
        name: "clip_text",
        kind: "clip_text",
        license: "MIT",
        category: "Text→image search",
    },
    Catalog {
        name: "ram_plus",
        kind: "ram_plus",
        license: "Apache-2.0",
        category: "Image tagging",
    },
    Catalog {
        name: "bge_text",
        kind: "bge_text",
        license: "MIT",
        category: "Document search",
    },
    Catalog {
        name: "mistral_small_3_2",
        kind: "mistral_small_3_2",
        license: "Apache-2.0",
        category: "Deep Analyze VLM",
    },
    Catalog {
        name: "qwen2_5_vl_7b",
        kind: "qwen2_5_vl_7b",
        license: "Apache-2.0",
        category: "Deep Analyze VLM",
    },
    Catalog {
        name: "gemma_3_4b",
        kind: "gemma_3_4b",
        license: "Gemma Terms",
        category: "Deep Analyze VLM",
    },
];

fn is_vlm_kind(kind: &str) -> bool {
    matches!(kind, "mistral_small_3_2" | "qwen2_5_vl_7b" | "gemma_3_4b")
}

fn recommended_vlm_kind() -> &'static str {
    let total = fileid_engine::platform::physical_memory_gb();
    let available = fileid_engine::platform::available_memory_mb() as f64 / 1024.0;
    let vram_mb = fileid_engine::platform::dedicated_vram_mb().unwrap_or(0);
    let has_gpu = matches!(
        fileid_engine::models::runtime::RuntimeProbe::detect().vendor,
        fileid_engine::models::runtime::GpuVendor::Nvidia
            | fileid_engine::models::runtime::GpuVendor::Amd
            | fileid_engine::models::runtime::GpuVendor::Intel
            | fileid_engine::models::runtime::GpuVendor::Qualcomm
    ) && vram_mb > 0;
    let arm64 = std::env::consts::ARCH == "aarch64";
    let free = fileid_engine::paths::models_dir()
        .ok()
        .and_then(|path| fileid_engine::platform::available_disk_bytes(&path));

    recommended_vlm_kind_for(total, available, vram_mb, has_gpu, arm64, free)
}

fn recommended_vlm_kind_for(
    total: f64,
    available: f64,
    vram_mb: u64,
    has_gpu: bool,
    arm64: bool,
    free: Option<u64>,
) -> &'static str {
    let order: &[&str] = if arm64 {
        &["gemma_3_4b"]
    } else if (total >= 29.5 && has_gpu && vram_mb >= 12 * 1024) || total >= 47.5 {
        &["mistral_small_3_2", "qwen2_5_vl_7b", "gemma_3_4b"]
    } else if total >= 15.0 || (total >= 11.5 && has_gpu && vram_mb >= 8 * 1024) {
        &["qwen2_5_vl_7b", "gemma_3_4b"]
    } else {
        &["gemma_3_4b"]
    };

    order
        .iter()
        .copied()
        .find(|kind| cli_vlm_fits(kind, total, available, vram_mb, has_gpu, free))
        .or_else(|| {
            order
                .iter()
                .copied()
                .find(|kind| cli_vlm_fits(kind, total, available, vram_mb, has_gpu, None))
        })
        .unwrap_or("gemma_3_4b")
}

fn cli_vlm_fits(
    kind: &str,
    total_ram_gb: f64,
    available_ram_gb: f64,
    vram_mb: u64,
    has_gpu: bool,
    free_disk_bytes: Option<u64>,
) -> bool {
    let (minimum_total, working_set) = match kind {
        "mistral_small_3_2" => (23.5, 16.0),
        "qwen2_5_vl_7b" => (11.5, 7.0),
        "gemma_3_4b" => (7.5, 4.5),
        _ => return false,
    };
    if total_ram_gb < minimum_total {
        return false;
    }
    let reserve = if total_ram_gb <= 10.0 {
        2.0
    } else if total_ram_gb <= 20.0 {
        4.0
    } else {
        6.0
    };
    let mut usable = (total_ram_gb - reserve).max(0.0);
    if available_ram_gb > 0.0 {
        usable = usable.min((available_ram_gb - 1.5).max(0.0));
    }
    if has_gpu {
        usable += ((vram_mb as f64 / 1024.0) - 1.5).max(0.0) * 0.8;
    }
    if usable < working_set {
        return false;
    }
    let Some(free) = free_disk_bytes else {
        return true;
    };
    let Some(model) = CATALOG
        .iter()
        .find(|entry| entry.kind == kind)
        .and_then(resolve)
        .map(|resolved| resolved.model)
    else {
        return false;
    };
    let bytes: u64 = model.files.iter().map(|file| file.approx_bytes).sum();
    free >= fileid_engine::downloader::required_install_free_bytes(bytes)
}

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
    Some(Resolved {
        cat,
        model,
        installed,
        size_bytes,
        repo,
    })
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

/// `fileid models list` — show every model, its installed state, size, license,
/// and HF repo, plus where the engine expects them on disk.
pub fn list(ctx: &Ctx) -> Result<()> {
    crate::ensure_engine_models_dir();
    let resolved: Vec<Resolved> = CATALOG.iter().filter_map(resolve).collect();
    let models_dir = fileid_engine::paths::models_dir().ok();
    let recommended_vlm = recommended_vlm_kind();

    if ctx.json {
        let arr: Vec<_> = resolved
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.cat.name,
                    "kind": r.model.id,
                    "installed": r.installed,
                    "required": r.cat.required(),
                    "recommended": r.cat.kind == recommended_vlm,
                    "sizeBytes": r.size_bytes,
                    "sizeHuman": file_size_i64(r.size_bytes as i64),
                    "license": r.cat.license,
                    "termsRequired": license::policy_for(r.cat.kind).is_some(),
                    "termsUrl": license::policy_for(r.cat.kind).map(|policy| policy.terms_url),
                    "externalRuntimeRequired": cfg!(target_os = "linux") && is_vlm_kind(r.cat.kind),
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

    println!(
        "{}   {}",
        ctx.bold("FileID engine models"),
        ctx.dim(&format!(
            "{} = required for AI scans · {} = recommended VLM for this machine",
            ctx.gold("★"),
            ctx.gold("◆")
        )),
    );
    // Name field sized to the longest model id (`mistral_small_3_2`, 17 chars).
    println!(
        "{}",
        ctx.dim(&format!(
            "  {:<19} {:<11} {:>9}  {:<16} {}",
            "MODEL", "STATUS", "SIZE", "LICENSE", "CATEGORY"
        ))
    );
    for r in &resolved {
        // Pad cells as plain text FIRST, then color — so the escape bytes never
        // skew the column widths.
        let marker = if r.cat.required() {
            "★"
        } else if r.cat.kind == recommended_vlm {
            "◆"
        } else {
            " "
        };
        let name_plain = format!("{marker} {:<17}", r.cat.name);
        let name_cell = if r.cat.required() {
            ctx.gold(&name_plain)
        } else {
            name_plain
        };

        let status_plain = if r.installed {
            "✓ installed"
        } else {
            "· missing"
        };
        let status_cell = format!("{status_plain:<11}");
        let status_cell = if r.installed {
            ctx.green(&status_cell)
        } else {
            ctx.dim(&status_cell)
        };

        let license = if license::policy_for(r.cat.kind).is_some() {
            format!("{} *", r.cat.license)
        } else {
            r.cat.license.to_string()
        };
        println!(
            "  {name_cell} {status_cell} {:>9}  {:<16} {}",
            file_size_i64(r.size_bytes as i64),
            license,
            r.cat.category,
        );
    }

    if resolved
        .iter()
        .any(|r| license::policy_for(r.cat.kind).is_some())
    {
        println!(
            "  {}",
            ctx.dim("* Separate model terms require explicit acceptance before download.")
        );
    }
    #[cfg(target_os = "linux")]
    println!(
        "  {}",
        ctx.dim("Deep Analyze VLM weights require a compatible `llama-mtmd-cli` on PATH; Linux packages do not bundle it.")
    );

    let total: u64 = resolved.iter().map(|r| r.size_bytes).sum();
    let installed_count = resolved.iter().filter(|r| r.installed).count();
    let missing: Vec<&Resolved> = resolved.iter().filter(|r| !r.installed).collect();
    let missing_bytes: u64 = missing.iter().map(|r| r.size_bytes).sum();

    println!();
    let tail = if missing.is_empty() {
        " — all installed".to_string()
    } else {
        format!(" ({} to download)", file_size_i64(missing_bytes as i64))
    };
    println!(
        "  {} models · {} total · {} installed · {} missing{}",
        resolved.len(),
        file_size_i64(total as i64),
        installed_count,
        missing.len(),
        tail,
    );
    if let Some(dir) = models_dir {
        println!(
            "  Models dir: {}",
            display_path(dir.to_string_lossy().as_ref())
        );
    }
    if !missing.is_empty() {
        let req_missing: Vec<&str> = missing
            .iter()
            .filter(|r| r.cat.required())
            .map(|r| r.cat.name)
            .collect();
        if !req_missing.is_empty() {
            println!(
                "  Required for AI scans:  {}",
                ctx.bold(&format!("fileid models download {}", req_missing.join(" ")))
            );
        }
        println!(
            "  Install everything:     {}",
            ctx.bold("fileid models download --all")
        );
        println!(
            "  {}",
            ctx.dim("Preview sizes first with `fileid models download --all --dry-run`.")
        );
    }
    Ok(())
}

/// `fileid models download [--all | <name>...] [--dry-run] [--yes]`.
///
pub fn download(ctx: &Ctx, all: bool, dry_run: bool, yes: bool, names: &[String]) -> Result<()> {
    crate::ensure_engine_models_dir();

    // Resolve the requested set of catalog entries.
    let selected: Vec<&'static Catalog> = if all {
        CATALOG.iter().collect()
    } else if names.is_empty() {
        anyhow::bail!(
            "no models selected; name one or more models, or pass --all (see `fileid models list`)"
        );
    } else {
        let mut out = Vec::with_capacity(names.len());
        let mut seen = HashSet::with_capacity(names.len());
        for n in names {
            match CATALOG.iter().find(|c| c.name == n || c.kind == n) {
                Some(c) if seen.insert(c.kind) => out.push(c),
                Some(_) => {}
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
                        "sizeHuman": file_size_i64(r.size_bytes as i64),
                        "repo": r.repo,
                        "license": r.cat.license,
                        "termsRequired": license::policy_for(r.cat.kind).is_some(),
                        "termsUrl": license::policy_for(r.cat.kind).map(|policy| policy.terms_url),
                        "externalRuntimeRequired": cfg!(target_os = "linux") && is_vlm_kind(r.cat.kind),
                        "files": r.model.files.iter().map(|f| f.url.clone()).collect::<Vec<_>>(),
                    })
                })
                .collect();
            print_json(&serde_json::json!({
                "command": "models",
                "action": "download",
                "dryRun": true,
                "totalBytes": pending_bytes,
                "totalHuman": file_size_i64(pending_bytes as i64),
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
                file_size_i64(r.size_bytes as i64)
            };
            println!("  {:<18} {:>10}  {}", r.cat.name, state, ctx.dim(&r.repo));
        }
        println!(
            "  {} {} across {} model(s) from huggingface.co",
            ctx.bold("Would download:"),
            file_size_i64(pending_bytes as i64),
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
                file_size_i64(pending_bytes as i64)
            ),
            yes,
        )
    {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "models",
                "action": "download",
                "installed": [],
                "aborted": true,
                "reason": "confirmation_required",
                "message": "large download not confirmed; pass --yes to install non-interactively",
            }));
        } else {
            println!("Aborted. (nothing downloaded)");
        }
        return Ok(());
    }

    ensure_license_acceptance(ctx, &pending)?;

    // ── Download each pending model in turn, with overall progress. ──
    // The bar spans ALL pending models: `pending_bytes` is the
    // denominator and `completed_bytes` accumulates as each finishes.
    let ui = ProgressUi::resolve(ctx, pending_bytes, pending.len());
    let mut installed_now: Vec<&str> = Vec::new();
    let mut completed_bytes: u64 = 0;
    for (i, r) in pending.iter().enumerate() {
        ui.pre_model(
            ctx,
            &format!(
                "{} {} ({}, {})",
                ctx.bold("Downloading"),
                r.cat.name,
                file_size_i64(r.size_bytes as i64),
                ctx.dim(&r.repo),
            ),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let cb = ui.callback(i, r.cat.name, completed_bytes, r.size_bytes);
        if let Err(e) = install_model_blocking(&r.model, cancel, cb) {
            let e = e.context(format!("installing {}", r.cat.name));
            ui.abort();
            return Err(e);
        }
        ui.model_done(ctx, r.cat.name);
        installed_now.push(r.cat.name);
        completed_bytes += r.size_bytes;
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
                .map(|p| display_path(p.to_string_lossy().as_ref()))
                .unwrap_or_else(|_| "the engine models dir".into()),
        );
        println!(
            "  {}",
            ctx.dim("Run `fileid scan <folder> --models` to scan with the full ML pipeline.")
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Download progress rendering.
//
// One OVERALL bar/stream spans every pending model. The byte-weighted fraction
// is monotonic by construction (file_index + bytes climb within a model;
// completed models accumulate) and additionally clamped to a running max — the
// downloader may report bundle files concurrently, so a raw sample can dip.
// ─────────────────────────────────────────────────────────────────────────

/// How `models download` renders progress, resolved once per invocation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProgressMode {
    /// `--json`/`--quiet`: no live bar; milestones still route via `ctx.progress`
    /// (so `--json` keeps its stderr chrome and `--quiet` stays silent).
    Plainish,
    /// stderr is a TTY: live carriage-return bar on stderr.
    Bar,
    /// stderr is not a TTY: milestone lines only, no carriage-return spam.
    NonTty,
}

/// Renders [`install_model_blocking`] progress across the whole pending set.
struct ProgressUi {
    mode: ProgressMode,
    /// Color the bar (color permitted AND stderr is a TTY).
    color: bool,
    /// Overall denominator: Σ sizes of all pending models.
    pending_bytes: u64,
    model_count: usize,
    /// Monotonic overall fraction in basis points (0..=10000), shared across the
    /// per-model callbacks so the displayed value never goes backwards.
    max_bp: Arc<AtomicU64>,
}

impl ProgressUi {
    fn resolve(ctx: &Ctx, pending_bytes: u64, model_count: usize) -> Self {
        let mode = if ctx.json || ctx.quiet {
            ProgressMode::Plainish
        } else if std::io::stderr().is_terminal() {
            ProgressMode::Bar
        } else {
            ProgressMode::NonTty
        };
        let color = ctx.color_allowed && std::io::stderr().is_terminal();
        Self {
            mode,
            color,
            pending_bytes,
            model_count,
            max_bp: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The "Downloading <name> (size, repo)" provenance line — human stderr only.
    fn pre_model(&self, ctx: &Ctx, msg: &str) {
        ctx.progress(msg);
    }

    /// `✓ <name> installed` milestone.
    fn model_done(&self, ctx: &Ctx, name: &str) {
        if self.mode == ProgressMode::Bar {
            clear_bar_line();
        }
        ctx.progress(&format!("  {} {} installed", ctx.bold("✓"), name));
    }

    /// Build the per-model progress sink. Captures the model's slice of the
    /// overall bar (`completed_bytes` before it, its own `current_size`).
    fn callback(
        &self,
        model_index: usize,
        model_name: &str,
        completed_bytes: u64,
        current_size: u64,
    ) -> Arc<dyn Fn(InstallFileProgress) + Send + Sync> {
        let mode = self.mode;
        let color = self.color;
        let pending = self.pending_bytes;
        let count = self.model_count;
        let max_bp = Arc::clone(&self.max_bp);
        let name = model_name.to_string();
        Arc::new(move |p: InstallFileProgress| {
            if mode != ProgressMode::Bar {
                return;
            }
            let raw = overall_basis_points(completed_bytes, current_size, pending, &p);
            // fetch_max keeps the shared value monotonic across concurrent files.
            let bp = max_bp.fetch_max(raw, Ordering::Relaxed).max(raw);
            let remaining = pending.max(1) as f64 * (1.0 - bp as f64 / 10_000.0);
            let eta = if p.bytes_per_second > 0.0 {
                human_eta(remaining / p.bytes_per_second)
            } else {
                "—".to_string()
            };
            let line = render_bar(color, bp, &name, model_index, count, &p, &eta);
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r{line}\x1b[K");
            let _ = err.flush();
        })
    }

    /// An install failed: surface it where the consumer is watching, and stop
    /// the bar from dangling. (`main` also prints the error to stderr + exits 1.)
    fn abort(&self) {
        if self.mode == ProgressMode::Bar {
            clear_bar_line();
        }
    }
}

/// Carriage-return to column 0 and clear to end of line, erasing the live bar.
fn clear_bar_line() {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "\r\x1b[K");
    let _ = err.flush();
}

/// Round basis points (0..=10000) to an integer percent (0..=100), monotonic
/// with the input.
fn bp_to_percent(bp: u64) -> u64 {
    ((bp + 50) / 100).min(100)
}

/// Overall download fraction in basis points (0..=10000), byte-weighted across
/// the whole pending set. `completed_bytes` = Σ sizes of models already finished
/// this run; `current_size` = the in-flight model's size; `pending_bytes` = the
/// run's denominator. The per-model term is `(file_index + bytes_done/total) /
/// file_count`, which climbs within a model.
fn overall_basis_points(
    completed_bytes: u64,
    current_size: u64,
    pending_bytes: u64,
    p: &InstallFileProgress,
) -> u64 {
    let file_frac = if p.file_count == 0 {
        0.0
    } else {
        let within = match p.bytes_total {
            Some(t) if t > 0 => (p.bytes_done as f64 / t as f64).clamp(0.0, 1.0),
            _ => 0.0,
        };
        ((p.file_index as f64 + within) / p.file_count as f64).clamp(0.0, 1.0)
    };
    let done = completed_bytes as f64 + current_size as f64 * file_frac;
    // `.max(1)` guards the degenerate all-zero-sizes case (never seen with the
    // real registry, which always carries `approx_bytes`).
    let denom = pending_bytes.max(1) as f64;
    ((done / denom).clamp(0.0, 1.0) * 10_000.0).round() as u64
}

/// The live one-line human bar: `[████░░░░] 41% w600k.onnx · 182/271 MB · …`.
fn render_bar(
    color: bool,
    bp: u64,
    model_name: &str,
    model_index: usize,
    model_count: usize,
    p: &InstallFileProgress,
    eta: &str,
) -> String {
    const WIDTH: usize = 20;
    let frac = bp as f64 / 10_000.0;
    let filled = ((frac * WIDTH as f64).round() as usize).min(WIDTH);
    let empty = WIDTH - filled;
    let pct = bp_to_percent(bp);
    let (bar, pct_s) = if color {
        (
            // Brand gold (#FFCC00 ≈ xterm 220) for the filled cells.
            format!(
                "[\x1b[38;5;220m{}\x1b[0m{}]",
                "█".repeat(filled),
                "░".repeat(empty)
            ),
            format!("\x1b[1m{pct:>3}%\x1b[0m"),
        )
    } else {
        (
            format!("[{}{}]", "█".repeat(filled), "░".repeat(empty)),
            format!("{pct:>3}%"),
        )
    };
    format!(
        "{bar} {pct_s}  {} · {} · {} · {} · model {}/{}",
        truncate(model_name, 20),
        size_pair(p.bytes_done, p.bytes_total),
        rate(p.bytes_per_second),
        eta,
        model_index + 1,
        model_count,
    )
}

/// `~45s` / `~2m10s` / `~1h05m`; `—` when not estimable.
fn human_eta(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "—".to_string();
    }
    let s = seconds.round() as u64;
    if s < 60 {
        format!("~{s}s")
    } else if s < 3600 {
        format!("~{}m{:02}s", s / 60, s % 60)
    } else {
        format!("~{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlm_fit_keeps_a_system_ram_floor_even_with_large_vram() {
        let plenty = Some(200 * 1024 * 1024 * 1024);
        assert!(cli_vlm_fits(
            "gemma_3_4b",
            8.0,
            6.0,
            16 * 1024,
            true,
            plenty
        ));
        assert!(!cli_vlm_fits(
            "qwen2_5_vl_7b",
            8.0,
            6.0,
            16 * 1024,
            true,
            plenty
        ));
        assert!(!cli_vlm_fits(
            "mistral_small_3_2",
            8.0,
            6.0,
            16 * 1024,
            true,
            plenty
        ));
    }

    #[test]
    fn live_host_profile_recommends_mistral() {
        assert_eq!(
            recommended_vlm_kind_for(
                30.9,
                13.5,
                15_977,
                true,
                false,
                Some(200 * 1024 * 1024 * 1024),
            ),
            "mistral_small_3_2"
        );
    }

    fn prog(
        file_index: usize,
        file_count: usize,
        bytes_done: u64,
        bytes_total: Option<u64>,
        bytes_per_second: f64,
    ) -> InstallFileProgress {
        InstallFileProgress {
            file_index,
            file_count,
            file_name: "w600k_r50.onnx".to_string(),
            bytes_done,
            bytes_total,
            bytes_per_second,
        }
    }

    const MB: u64 = 1024 * 1024;

    /// The guarded overall fraction never decreases — even when bundle files
    /// report concurrently / out of order — and reaches 100% at completion.
    #[test]
    fn overall_fraction_is_monotonic_and_reaches_full() {
        // Two pending models: A = 100 MB, B = 300 MB; denominator = 400 MB.
        let a = 100 * MB;
        let b = 300 * MB;
        let pending = a + b;

        // (completed_before, current_size, InstallFileProgress) — note the
        // deliberately jittery / out-of-order samples within model B.
        let samples = [
            (0, a, prog(0, 1, 10 * MB, Some(a), 0.0)),
            (0, a, prog(0, 1, 90 * MB, Some(a), 0.0)),
            (0, a, prog(0, 1, a, Some(a), 0.0)), // A complete
            (a, b, prog(0, 2, 10 * MB, Some(150 * MB), 0.0)),
            (a, b, prog(1, 2, 5 * MB, Some(150 * MB), 0.0)), // file 2 starts
            (a, b, prog(0, 2, 140 * MB, Some(150 * MB), 0.0)), // file 1 lags in late
            (a, b, prog(1, 2, 150 * MB, Some(150 * MB), 0.0)), // B complete
        ];

        let max = std::sync::atomic::AtomicU64::new(0);
        let mut last = 0u64;
        let mut displayed_final = 0u64;
        for (completed, current, p) in samples {
            let raw = overall_basis_points(completed, current, pending, &p);
            let bp = max.fetch_max(raw, Ordering::Relaxed).max(raw);
            assert!(bp >= last, "fraction went backwards: {last} -> {bp}");
            last = bp;
            displayed_final = bp;
        }
        assert_eq!(
            displayed_final, 10_000,
            "must reach 100% when the last model completes"
        );
        assert_eq!(bp_to_percent(displayed_final), 100);
    }
}
