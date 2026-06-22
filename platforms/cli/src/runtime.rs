//! `fileid runtime` — manage the engine's ONNX Runtime shared library.
//!
//! The shared engine is built with `load-dynamic`, so the full-ML `scan
//! --models` path `dlopen`s ONNX Runtime at run time. On Windows the DLLs ship
//! beside the engine; on Linux the system / `download-binaries` path provides
//! the `.so`. On macOS arm64, `ort`'s `download-binaries` ships only a STATIC
//! `libonnxruntime.a`, so the dylib must be installed once — this command does
//! that (`install`) and reports state (`status`). It is entirely separate from
//! the AI-model download (`fileid models download`): models are the weights, the
//! runtime is the inference library that loads them.
//!
//! Commercial-clean: ONNX Runtime is MIT-licensed. The install downloads through
//! the engine's single audited, CA-pinned network path (egress restricted to its
//! redirect allow-list) — see `shared/docs/RUNTIME.md` + `shared/docs/DECISIONS.md`.

use anyhow::Result;

use crate::context::{print_json, Ctx};

// ── Pinned ONNX Runtime dylib source (macOS arm64) ──────────────────────────
//
// `ort 2.0.0-rc.10` targets ONNX Runtime 1.22.0 (ort-sys `ONNXRUNTIME_VERSION`)
// and hard-panics if the loaded dylib's minor version is < 22, so the runtime we
// install must be 1.22.x. The CLI installer fetches a BARE `libonnxruntime.dylib`
// (no archive — the CLI deliberately has no tar/gzip dependency) via the engine's
// audited, CA-pinned downloader, which only follows redirects to its allow-list
// (huggingface.co / github.com / …).
//
// PREFERRED (keeps egress HuggingFace-only, per the project's no-new-egress
// rule): mirror the dylib on huggingface.co and fill these two constants in.
// Until then they stay `None`: `runtime install` provisions from a local source
// (Homebrew / beside-exe) when present, and otherwise prints the exact next
// steps — it never silently fails. A self-hoster can point it anywhere at run
// time with `FILEID_ORT_DYLIB_URL` (+ optional `FILEID_ORT_DYLIB_SHA256`); the
// host must be on the downloader's allow-list.
//
// TODO(runtime-dylib): set these once the 1.22.x dylib is mirrored. The exact
// steps to produce + SHA256 the artifact live in shared/docs/RUNTIME.md.
//
// macOS-only: the download path exists only there (Windows/Linux get the runtime
// from the platform), so gate these so they're not dead code on those targets.
#[cfg(target_os = "macos")]
const PINNED_DYLIB_URL: Option<&str> = None;
#[cfg(target_os = "macos")]
const PINNED_DYLIB_SHA256: Option<&str> = None;

/// The download source actually used: the `FILEID_ORT_DYLIB_URL` env override
/// (with optional `FILEID_ORT_DYLIB_SHA256`) wins over the pinned constants.
#[cfg(target_os = "macos")]
fn configured_source() -> Option<(String, Option<String>)> {
    if let Some(url) = std::env::var_os("FILEID_ORT_DYLIB_URL") {
        let url = url.to_string_lossy().into_owned();
        if !url.is_empty() {
            let sha = std::env::var("FILEID_ORT_DYLIB_SHA256")
                .ok()
                .filter(|s| !s.is_empty());
            return Some((url, sha));
        }
    }
    PINNED_DYLIB_URL.map(|u| (u.to_string(), PINNED_DYLIB_SHA256.map(str::to_string)))
}

// ════════════════════════════════════════════════════════════════════════════
// macOS — real install / status.
// ════════════════════════════════════════════════════════════════════════════

/// `fileid runtime status` — where the ONNX Runtime is (or isn't) and how to
/// install it.
#[cfg(target_os = "macos")]
pub fn status(ctx: &Ctx) -> Result<()> {
    use fileid_engine::ort_runtime as rt;

    let resolved = rt::resolve_dylib();
    let install_path = rt::install_path().ok();
    let search: Vec<std::path::PathBuf> = rt::search_locations();

    if ctx.json {
        print_json(&serde_json::json!({
            "command": "runtime",
            "action": "status",
            "os": "macos",
            "installed": resolved.is_some(),
            "resolvedPath": resolved.as_ref().map(|p| p.display().to_string()),
            "installPath": install_path.as_ref().map(|p| p.display().to_string()),
            "searchLocations": search.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "installCommand": rt::INSTALL_COMMAND,
        }));
        return Ok(());
    }

    println!("{}", ctx.bold("ONNX Runtime (macOS)"));
    match &resolved {
        Some(p) => {
            println!(
                "  {} {}",
                ctx.green("✓ installed"),
                ctx.dim(&p.display().to_string())
            );
            println!(
                "  {}",
                ctx.dim("Full-ML `fileid scan --models` can load its models.")
            );
        }
        None => {
            println!("  {}", ctx.bold("· not installed"));
            println!(
                "  {}",
                ctx.dim("Full-ML `fileid scan --models` can't load models until it's installed.")
            );
            println!("  {}", ctx.bold("To install (any one):"));
            println!("    {}", ctx.bold(rt::INSTALL_COMMAND));
            println!("    brew install onnxruntime");
            println!("    shared/scripts/install_onnxruntime_macos.sh");
        }
    }
    println!("  {}", ctx.dim("Searched (in order):"));
    for p in &search {
        println!("    {}", ctx.dim(&p.display().to_string()));
    }
    Ok(())
}

/// `fileid runtime install` — provision the macOS ONNX Runtime dylib.
///
/// Order: already-resolvable (idempotent) → copy from a local source (Homebrew /
/// beside-exe, zero egress) → download a pinned/overridden bare dylib → print
/// exact next steps. Never silently fails.
#[cfg(target_os = "macos")]
pub fn install(ctx: &Ctx, yes: bool, force: bool) -> Result<()> {
    use fileid_engine::ort_runtime as rt;

    let target = rt::install_path()?;

    // ── Already available (and not forcing a refresh)? ──
    if !force {
        if let Some(p) = rt::resolve_dylib() {
            if ctx.json {
                print_json(&serde_json::json!({
                    "command": "runtime", "action": "install",
                    "installed": true, "resolvedPath": p.display().to_string(),
                    "message": "ONNX Runtime already available",
                }));
            } else {
                println!(
                    "{} {}",
                    ctx.bold("ONNX Runtime already available:"),
                    ctx.dim(&p.display().to_string())
                );
                println!("  {}", ctx.dim("Pass --force to reinstall into the engine runtime dir."));
            }
            return Ok(());
        }
    }

    // ── Local source (zero egress): copy a system/Homebrew dylib in. ──
    if let Some(src) = local_source(&target) {
        copy_into_place(&src, &target)?;
        return report_installed(ctx, &target, &format!("copied from {}", src.display()));
    }

    // ── Download a pinned / overridden bare dylib via the engine's
    //    CA-pinned, allow-listed network path. ──
    if let Some((url, sha)) = configured_source() {
        if !ctx.confirm(
            &format!("Download the ONNX Runtime dylib (~30 MB) from {url}?"),
            yes,
        ) {
            println!("Aborted. (nothing downloaded)");
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        ctx.progress(&format!("  {} {}", ctx.bold("Downloading ONNX Runtime"), ctx.dim(&url)));
        download_dylib(&url, sha.as_deref(), &target)?;
        return report_installed(ctx, &target, "downloaded");
    }

    // ── Nothing pinned yet: print the exact, actionable next steps. ──
    no_source_guidance(ctx);
    Ok(())
}

/// First existing dylib among the system/Homebrew sources (NOT the install
/// target itself), suitable to copy into place with no network access.
#[cfg(target_os = "macos")]
fn local_source(target: &std::path::Path) -> Option<std::path::PathBuf> {
    fileid_engine::ort_runtime::search_locations()
        .into_iter()
        .find(|p| p != target && p.is_file())
}

#[cfg(target_os = "macos")]
fn copy_into_place(src: &std::path::Path, target: &std::path::Path) -> Result<()> {
    use anyhow::Context as _;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::copy(src, target)
        .with_context(|| format!("copying {} -> {}", src.display(), target.display()))?;
    Ok(())
}

/// Download a bare dylib to `target` through the engine's audited downloader,
/// rendering a simple stderr progress line.
#[cfg(target_os = "macos")]
fn download_dylib(url: &str, sha256: Option<&str>, target: &std::path::Path) -> Result<()> {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use fileid_engine::downloader::{download_file_blocking, DownloadProgress};

    let cancel = Arc::new(AtomicBool::new(false));
    let progress: Arc<dyn Fn(DownloadProgress) + Send + Sync> = Arc::new(|p: DownloadProgress| {
        if let Some(total) = p.bytes_total {
            if total > 0 {
                let pct = (p.bytes_done as f64 / total as f64 * 100.0).min(100.0);
                eprint!("\r  {pct:.0}% \x1b[K");
                let _ = std::io::Write::flush(&mut std::io::stderr());
            }
        }
    });
    let r = download_file_blocking(url, target, sha256, cancel, progress);
    eprintln!();
    r
}

#[cfg(target_os = "macos")]
fn report_installed(ctx: &Ctx, target: &std::path::Path, how: &str) -> Result<()> {
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "runtime", "action": "install",
            "installed": true, "path": target.display().to_string(), "how": how,
        }));
    } else {
        println!(
            "{} {} {}",
            ctx.bold("✓ ONNX Runtime installed"),
            ctx.dim(&format!("({how})")),
            ctx.dim(&target.display().to_string()),
        );
        println!("  {}", ctx.dim("Run `fileid scan <folder> --models` for a full AI scan."));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn no_source_guidance(ctx: &Ctx) {
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "runtime", "action": "install",
            "installed": false, "error": "no_source_configured",
            "message": "no pinned download source yet; install via Homebrew or the shell script",
            "options": [
                "brew install onnxruntime",
                "shared/scripts/install_onnxruntime_macos.sh",
                "set FILEID_ORT_DYLIB_URL (+ FILEID_ORT_DYLIB_SHA256) to a 1.22.x bare dylib",
            ],
        }));
        return;
    }
    println!("{}", ctx.bold("ONNX Runtime install — choose one:"));
    println!(
        "  {}   {}",
        ctx.bold("brew install onnxruntime"),
        ctx.dim("(simplest; the engine finds /opt/homebrew/lib/libonnxruntime.dylib)")
    );
    println!(
        "  {}   {}",
        ctx.bold("shared/scripts/install_onnxruntime_macos.sh"),
        ctx.dim("(downloads the official MIT ONNX Runtime 1.22.0, verifies, installs)")
    );
    println!(
        "  {}",
        ctx.dim("or set FILEID_ORT_DYLIB_URL (+ FILEID_ORT_DYLIB_SHA256) to a 1.22.x bare dylib")
    );
    println!(
        "  {}",
        ctx.dim("Then re-run `fileid runtime status`. See shared/docs/RUNTIME.md.")
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Windows / Linux — the runtime is provided by the platform; nothing to install.
// ════════════════════════════════════════════════════════════════════════════

#[cfg(not(target_os = "macos"))]
pub fn status(ctx: &Ctx) -> Result<()> {
    let os = std::env::consts::OS;
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "runtime", "action": "status", "os": os,
            "installed": true,
            "message": "ONNX Runtime is provided by the platform on this OS",
        }));
        return Ok(());
    }
    println!("{}", ctx.bold("ONNX Runtime"));
    println!(
        "  {}",
        ctx.dim(&format!(
            "Provided by the platform on {os} (bundled DLLs / system library); nothing to install."
        ))
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn install(ctx: &Ctx, _yes: bool, _force: bool) -> Result<()> {
    let os = std::env::consts::OS;
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "runtime", "action": "install", "os": os,
            "installed": true, "message": "nothing to install on this OS",
        }));
        return Ok(());
    }
    println!(
        "{}",
        ctx.bold(&format!(
            "Nothing to install — ONNX Runtime is provided by the platform on {os}."
        ))
    );
    Ok(())
}
