//! `fileid runtime` — manage the engine's ONNX Runtime shared library.
//!
//! The shared engine is built with `load-dynamic`, so the full-ML `scan
//! --models` path `dlopen`s ONNX Runtime at run time. On Windows the DLLs ship
//! beside the engine; on Linux `download-binaries` statically links the CPU
//! runtime into the engine. On macOS arm64, that feature ships only a STATIC
//! `libonnxruntime.a`, so the dylib must be installed once — this command does
//! that (`install`) and reports state (`status`). It is entirely separate from
//! the AI-model download (`fileid models download`): models are the weights, the
//! runtime is the inference library that loads them.
//!
//! Commercial-clean: ONNX Runtime is MIT-licensed. FileID does not ship a
//! default third-party runtime download URL here; install via Homebrew or a
//! user-supplied HuggingFace mirror (`FILEID_ORT_DYLIB_URL`) so network egress
//! stays within the project privacy rule.

use anyhow::Result;

use crate::context::{print_json, Ctx};

// ── Pinned ONNX Runtime source (macOS arm64) ────────────────────────────────
//
// `ort 2.0.0-rc.10` targets ONNX Runtime 1.22.0 (ort-sys `ONNXRUNTIME_VERSION`)
// and hard-panics if the loaded dylib's minor version is < 22, so any runtime
// installed here must be 1.22.x or newer. The project privacy rule allows only
// user-initiated model/runtime egress to huggingface.co; no GitHub fallback is
// hardcoded. Self-hosters can set `FILEID_ORT_DYLIB_URL` to a HuggingFace-hosted
// `.tgz` or bare `.dylib` plus `FILEID_ORT_DYLIB_SHA256`.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PINNED_DYLIB_URL: Option<&str> = None;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PINNED_DYLIB_SHA256: Option<&str> =
    Some("cab6dcbd77e7ec775390e7b73a8939d45fec3379b017c7cb74f5b204c1a1cc07");
// Second guard: SHA256 of `lib/libonnxruntime.1.22.0.dylib` INSIDE the archive,
// checked after extraction.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PINNED_EXTRACTED_DYLIB_SHA256: Option<&str> =
    Some("2b885992d3d6fa4130d39ec84a80d7504ff52750027c547bb22c86165f19406a");

#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
const PINNED_DYLIB_URL: Option<&str> = None;
#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
const PINNED_DYLIB_SHA256: Option<&str> = None;
#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
const PINNED_EXTRACTED_DYLIB_SHA256: Option<&str> = None;

/// A resolved download source for the macOS ONNX Runtime dylib.
#[cfg(target_os = "macos")]
struct RuntimeSource {
    url: String,
    /// SHA256 of the downloaded artifact (the `.tgz`, or a bare `.dylib`),
    /// verified by the downloader before anything is installed.
    archive_sha256: Option<String>,
    /// SHA256 of the dylib extracted FROM the archive — a post-extract second
    /// guard. Known only for the pinned source (`None` for an env-override URL,
    /// whose inner layout can't be predicted).
    extracted_dylib_sha256: Option<String>,
}

/// The download source `install` uses: the `FILEID_ORT_DYLIB_URL` env override
/// (with optional `FILEID_ORT_DYLIB_SHA256`) wins over the pinned constants.
/// Downloads are still refused unless the URL host is HuggingFace-owned.
#[cfg(target_os = "macos")]
fn configured_source() -> Option<RuntimeSource> {
    if let Some(url) = std::env::var_os("FILEID_ORT_DYLIB_URL") {
        let url = url.to_string_lossy().into_owned();
        if !url.is_empty() {
            let override_sha256 = std::env::var("FILEID_ORT_DYLIB_SHA256")
                .ok()
                .filter(|s| !s.is_empty());
            let mirrored_pinned_archive = override_sha256.is_none() && url_is_tarball(&url);
            return Some(RuntimeSource {
                url,
                archive_sha256: override_sha256.or_else(|| {
                    mirrored_pinned_archive
                        .then(|| PINNED_DYLIB_SHA256.map(str::to_string))
                        .flatten()
                }),
                extracted_dylib_sha256: mirrored_pinned_archive
                    .then(|| PINNED_EXTRACTED_DYLIB_SHA256.map(str::to_string))
                    .flatten(),
            });
        }
    }
    PINNED_DYLIB_URL.map(|url| RuntimeSource {
        url: url.to_string(),
        archive_sha256: PINNED_DYLIB_SHA256.map(str::to_string),
        extracted_dylib_sha256: PINNED_EXTRACTED_DYLIB_SHA256.map(str::to_string),
    })
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
    let source = configured_source();

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
            "downloadSource": source.as_ref().map(|s| s.url.clone()),
            "downloadSha256": source.as_ref().and_then(|s| s.archive_sha256.clone()),
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
            println!("    brew install onnxruntime");
            println!("    set FILEID_ORT_DYLIB_URL to a HuggingFace-hosted ONNX Runtime dylib/archive, then run {}", ctx.bold(rt::INSTALL_COMMAND));
        }
    }
    if let Some(s) = &source {
        println!("  {}", ctx.dim(&format!("Download source: {}", s.url)));
        if let Some(sha) = &s.archive_sha256 {
            println!("  {}", ctx.dim(&format!("Pinned SHA256:   {sha}")));
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
/// Without `--force`: already-resolvable (idempotent) → copy from a local source
/// (Homebrew / beside-exe, zero egress) → download + extract the pinned/overridden
/// archive. With `--force`: skip BOTH short-circuits and always download, so the
/// user pins the exact 1.22 build even when a Homebrew/system runtime is present
/// (and so the download path is testable on such a machine). Never silently fails.
#[cfg(target_os = "macos")]
pub fn install(ctx: &Ctx, yes: bool, force: bool) -> Result<()> {
    use fileid_engine::ort_runtime as rt;

    let target = rt::install_path()?;

    if !force {
        // ── Already available? ──
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
                println!(
                    "  {}",
                    ctx.dim(
                        "Pass --force to reinstall the pinned build into the engine runtime dir."
                    )
                );
            }
            return Ok(());
        }

        // ── Local source (zero egress): copy a system/Homebrew dylib in. ──
        if let Some(src) = local_source(&target) {
            copy_into_place(&src, &target)?;
            return report_installed(ctx, &target, &format!("copied from {}", src.display()));
        }
    }

    // ── Download (+ extract, if the source is an archive) the pinned /
    //    overridden runtime via the engine's CA-pinned, allow-listed path. ──
    if let Some(source) = configured_source() {
        if !ctx.confirm(
            &format!(
                "Download ONNX Runtime from {}? (one-time, ~10 MB)",
                source.url
            ),
            yes,
        ) {
            println!("Aborted. (nothing downloaded)");
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        ctx.progress(&format!(
            "  {} {}",
            ctx.bold("Downloading ONNX Runtime"),
            ctx.dim(&source.url)
        ));
        provision_from_source(&source, &target)?;
        return report_installed(ctx, &target, "downloaded");
    }

    // ── Nothing pinned for this arch and no override: print next steps. ──
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

/// Install `src` as `target` atomically: copy to a unique temp file IN THE
/// TARGET'S OWN DIRECTORY (so the final `rename` is same-filesystem and can't
/// EXDEV-fail), then `rename` it into place — an atomic same-FS replace that a
/// concurrent `resolve_dylib()` either misses or sees whole, never truncated.
/// On any failure the temp file is removed, so a mid-copy abort (disk-full,
/// I/O error, SIGINT) never leaves a partial dylib at the resolvable `target`.
#[cfg(target_os = "macos")]
fn copy_into_place(src: &std::path::Path, target: &std::path::Path) -> Result<()> {
    use anyhow::Context as _;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("install target {} has no parent", target.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;

    let tmp = unique_temp_file(parent);
    std::fs::copy(src, &tmp)
        .with_context(|| format!("copying {} -> {}", src.display(), tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e)
            .with_context(|| format!("installing {} -> {}", tmp.display(), target.display()));
    }
    Ok(())
}

/// Provision `target` from a configured source: download + extract when it's a
/// gzipped tarball (the pinned default, and any `.tgz`/`.tar.gz` override), or
/// download straight to `target` when it's a bare dylib (a `.dylib` override).
#[cfg(target_os = "macos")]
fn provision_from_source(source: &RuntimeSource, target: &std::path::Path) -> Result<()> {
    if url_is_tarball(&source.url) {
        download_and_extract(source, target)
    } else {
        download_artifact(&source.url, source.archive_sha256.as_deref(), target)
    }
}

/// Does this URL point at a gzipped tarball (vs. a bare dylib)? Ignores any
/// `?query`/`#fragment` so a signed-URL mirror still classifies correctly.
#[cfg(target_os = "macos")]
fn url_is_tarball(url: &str) -> bool {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    path.ends_with(".tgz") || path.ends_with(".tar.gz")
}

/// Download the archive to a temp dir, SHA256-verify it (the downloader refuses
/// to finalize a mismatched file), extract with the system `tar`, locate the
/// runtime dylib, optionally second-check its own SHA256, then install it as
/// `target`. The temp dir is always removed, success or failure.
#[cfg(target_os = "macos")]
fn download_and_extract(source: &RuntimeSource, target: &std::path::Path) -> Result<()> {
    use anyhow::Context as _;

    let tmp = unique_temp_dir();
    std::fs::create_dir_all(&tmp)
        .with_context(|| format!("creating temp dir {}", tmp.display()))?;

    let outcome = (|| -> Result<()> {
        let archive = tmp.join("onnxruntime.tgz");
        download_artifact(&source.url, source.archive_sha256.as_deref(), &archive)
            .context("downloading the ONNX Runtime archive")?;

        extract_tarball(&archive, &tmp).context("extracting the ONNX Runtime archive")?;

        let dylib = locate_extracted_dylib(&tmp).ok_or_else(|| {
            anyhow::anyhow!("no libonnxruntime*.dylib found inside the downloaded archive")
        })?;

        if let Some(expected) = source.extracted_dylib_sha256.as_deref() {
            verify_sha256(&dylib, expected)
                .context("verifying the extracted ONNX Runtime dylib")?;
        }

        copy_into_place(&dylib, target)
    })();

    let _ = std::fs::remove_dir_all(&tmp);
    outcome
}

/// Extract a `.tgz` with the system `tar` (BSD tar on macOS handles gzip via
/// `-z`), so no `tar`/`flate2` crate is pulled into the CLI.
#[cfg(target_os = "macos")]
fn extract_tarball(archive: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    use anyhow::Context as _;

    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .context("spawning system `tar` to extract the archive")?;
    if !status.success() {
        anyhow::bail!("`tar -xzf {}` exited with {status}", archive.display());
    }
    Ok(())
}

/// Find the core ONNX Runtime dylib inside an extracted archive. Matches
/// `libonnxruntime.<…>.dylib` — the real versioned file AND the bare
/// `libonnxruntime.dylib` alias — but NOT provider side-libs such as
/// `libonnxruntime_providers_shared.dylib` (the `.` after the stem excludes the
/// `_`-suffixed variants), and NOT the `.dSYM` debug companion (a Mach-O with
/// the IDENTICAL name `libonnxruntime.<ver>.dylib` but different bytes, which is
/// why it must be excluded by path, not name). Prefers the longest match so the
/// versioned real file wins over the shorter alias. Pure + deterministic for
/// unit testing.
#[cfg(target_os = "macos")]
fn locate_extracted_dylib(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut best: Option<std::path::PathBuf> = None;
    let mut best_len = 0usize;
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        let path = entry.path();
        // Debug-symbol bundle: `…/libonnxruntime.<ver>.dylib.dSYM/Contents/
        // Resources/DWARF/libonnxruntime.<ver>.dylib` is NOT the loadable dylib.
        if path_has_dsym_component(path) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(name.starts_with("libonnxruntime.") && name.ends_with(".dylib")) {
            continue;
        }
        // `is_file()` follows a symlink to the real dylib; a broken link is skipped.
        if !path.is_file() {
            continue;
        }
        if name.len() > best_len {
            best_len = name.len();
            best = Some(path.to_path_buf());
        }
    }
    best
}

/// True if any path component is a `.dSYM` debug bundle.
#[cfg(target_os = "macos")]
fn path_has_dsym_component(path: &std::path::Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().to_str().is_some_and(|s| s.ends_with(".dSYM")))
}

#[cfg(target_os = "macos")]
fn is_huggingface_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host == "huggingface.co"
        || host.ends_with(".huggingface.co")
        || host == "hf.co"
        || host.ends_with(".hf.co")
}

/// Download a file to `dest` through the engine's audited, CA-pinned downloader,
/// rendering a simple stderr progress line. `sha256` (when set) is verified by
/// the downloader before the atomic rename, so a mismatched artifact never lands.
#[cfg(target_os = "macos")]
fn download_artifact(url: &str, sha256: Option<&str>, dest: &std::path::Path) -> Result<()> {
    if !is_huggingface_url(url) {
        anyhow::bail!("runtime downloads must come from huggingface.co or hf.co; got {url}");
    }
    let sha256 = sha256.ok_or_else(|| {
        anyhow::anyhow!(
            "runtime download is not hash-pinned; set FILEID_ORT_DYLIB_SHA256 to the expected SHA256"
        )
    })?;
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
    let r = download_file_blocking(url, dest, Some(sha256), cancel, progress);
    eprintln!();
    r
}

/// Lowercase-hex SHA256 of a file, streamed in 64 KB chunks.
#[cfg(target_os = "macos")]
fn sha256_hex(path: &std::path::Path) -> Result<String> {
    use anyhow::Context as _;
    use sha2::{Digest, Sha256};

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Error unless `path`'s SHA256 equals `expected` (case-insensitive hex).
#[cfg(target_os = "macos")]
fn verify_sha256(path: &std::path::Path, expected: &str) -> Result<()> {
    let got = sha256_hex(path)?;
    if !got.eq_ignore_ascii_case(expected) {
        anyhow::bail!(
            "SHA256 mismatch for {}: expected {expected}, got {got}",
            path.display()
        );
    }
    Ok(())
}

/// A process-unique temp dir under the OS temp root (pid + nanos + counter), so
/// concurrent installs / tests never collide.
#[cfg(target_os = "macos")]
fn unique_temp_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "fileid-ort-install-{}-{nanos}-{seq}",
        std::process::id()
    ))
}

/// A process-unique temp file path inside `dir` (so a later `rename` onto a
/// target in the same `dir` is same-filesystem and atomic). Hidden + suffixed
/// so a partial copy is obviously not the real dylib.
#[cfg(target_os = "macos")]
fn unique_temp_file(dir: &std::path::Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        ".libonnxruntime.dylib.tmp-{}-{nanos}-{seq}",
        std::process::id()
    ))
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
        println!(
            "  {}",
            ctx.dim("Run `fileid scan <folder> --models` for a full AI scan.")
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn no_source_guidance(ctx: &Ctx) {
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "runtime", "action": "install",
            "installed": false, "error": "no_source_configured",
            "message": "no HuggingFace runtime source configured; install via Homebrew or set FILEID_ORT_DYLIB_URL",
            "options": [
                "brew install onnxruntime",
                "set FILEID_ORT_DYLIB_URL (+ FILEID_ORT_DYLIB_SHA256) to a HuggingFace-hosted 1.22.x .tgz or bare dylib",
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
        "  {}",
        ctx.dim("or set FILEID_ORT_DYLIB_URL (+ FILEID_ORT_DYLIB_SHA256) to a HuggingFace-hosted 1.22.x .tgz or bare dylib")
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
            "Provided with the engine on {os} (bundled DLLs or a statically linked runtime); nothing to install."
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn verify_sha256_accepts_match_and_rejects_mismatch() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("vec.bin");
        // NIST FIPS 180-2 test vector: SHA256("abc").
        std::fs::write(&f, b"abc").unwrap();
        let correct = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

        assert!(verify_sha256(&f, correct).is_ok());
        assert!(
            verify_sha256(&f, &correct.to_uppercase()).is_ok(),
            "hex comparison must be case-insensitive"
        );
        assert!(
            verify_sha256(&f, &"f".repeat(64)).is_err(),
            "a wrong hash must be rejected, never installed"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn locate_extracted_dylib_prefers_versioned_real_file() {
        let dir = unique_temp_dir();
        let lib = dir.join("onnxruntime-osx-arm64-1.22.0").join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        // The two names ORT's archive really ships (the versioned real file +
        // the shorter alias) plus decoys that must be ignored.
        std::fs::write(lib.join("libonnxruntime.1.22.0.dylib"), b"real").unwrap();
        std::fs::write(lib.join("libonnxruntime.dylib"), b"alias").unwrap();
        std::fs::write(lib.join("libonnxruntime_providers_shared.dylib"), b"decoy").unwrap();
        std::fs::write(lib.join("libonnxruntime.so"), b"decoy").unwrap();
        std::fs::write(dir.join("VERSION_NUMBER"), b"1.22.0").unwrap();
        // The `.dSYM` debug companion: a same-named file (`libonnxruntime.1.22.0
        // .dylib`) that is NOT the loadable dylib — must be excluded by path.
        let dwarf = lib
            .join("libonnxruntime.1.22.0.dylib.dSYM")
            .join("Contents")
            .join("Resources")
            .join("DWARF");
        std::fs::create_dir_all(&dwarf).unwrap();
        std::fs::write(
            dwarf.join("libonnxruntime.1.22.0.dylib"),
            b"dwarf-not-the-real-dylib",
        )
        .unwrap();

        let found = locate_extracted_dylib(&dir).expect("a core dylib must be located");
        assert_eq!(
            found.file_name().and_then(|n| n.to_str()),
            Some("libonnxruntime.1.22.0.dylib"),
            "the versioned real file must win over the bare alias and side-libs"
        );
        assert!(
            !path_has_dsym_component(&found),
            "the located dylib must not be the .dSYM DWARF companion"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn locate_extracted_dylib_none_when_absent() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("readme.txt"), b"no dylib here").unwrap();
        assert!(locate_extracted_dylib(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copy_into_place_installs_atomically_replacing_old() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.dylib");
        std::fs::write(&src, b"new-runtime-bytes").unwrap();
        let target = dir.join("libonnxruntime.dylib");
        std::fs::write(&target, b"old").unwrap();

        copy_into_place(&src, &target).expect("install must succeed");
        assert_eq!(std::fs::read(&target).unwrap(), b"new-runtime-bytes");
        // Only `src` + `target` remain — the temp file was renamed, not left behind.
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names.len(),
            2,
            "a successful install must leave no temp file: {names:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copy_into_place_failed_copy_leaves_no_resolvable_target() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("libonnxruntime.dylib");
        let missing_src = dir.join("does-not-exist.dylib");

        assert!(
            copy_into_place(&missing_src, &target).is_err(),
            "copying a missing source must fail"
        );
        assert!(
            !target.exists(),
            "a failed install must never leave a (truncated) dylib at the resolvable target"
        );
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed install must clean up its temp file: {leftovers:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn url_is_tarball_detects_archives_vs_bare_dylib() {
        assert!(url_is_tarball(
            "https://huggingface.co/fileid/runtime/resolve/main/onnxruntime-osx-arm64-1.22.0.tgz"
        ));
        assert!(url_is_tarball("https://example.test/foo.tar.gz"));
        assert!(url_is_tarball(
            "https://example.test/foo.TGZ?token=abc#frag"
        ));
        assert!(!url_is_tarball("https://example.test/libonnxruntime.dylib"));
        assert!(!url_is_tarball(
            "https://huggingface.co/fileid/runtime/resolve/main/libonnxruntime.dylib"
        ));
    }

    #[test]
    fn runtime_download_hosts_are_huggingface_only() {
        assert!(is_huggingface_url(
            "https://huggingface.co/fileid/runtime/resolve/main/libonnxruntime.dylib"
        ));
        assert!(is_huggingface_url(
            "https://cdn-lfs.huggingface.co/repos/foo"
        ));
        assert!(is_huggingface_url(
            "https://hf.co/fileid/runtime/resolve/main/runtime.tgz"
        ));
        assert!(!is_huggingface_url(
            "http://huggingface.co/fileid/runtime.tgz"
        ));
        assert!(!is_huggingface_url(
            "https://github.com/microsoft/onnxruntime/releases/foo.tgz"
        ));
        assert!(!is_huggingface_url("https://evilhuggingface.co/foo.tgz"));
    }
}
