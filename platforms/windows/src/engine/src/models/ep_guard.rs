//! Execution-provider crash-safety gate.
//!
//! Auto-enabling a GPU execution provider whose DLLs we haven't verified on
//! *this* machine (an installed CUDA / OpenVINO Performance Pack) risks a
//! native crash at session-bind time — a bad `onnxruntime_providers_*.dll`,
//! a driver mismatch, or an OOM the EP can't survive. That crash takes the
//! whole engine down before any Rust error handling runs.
//!
//! This gate bounds the blast radius to **one** crash:
//!
//! - [`arm`] drops `packs/.ep_attempt` (containing the EP name) right before
//!   the first ORT session is created, and [`disarm`] deletes it right after
//!   the session pool binds successfully.
//! - [`resolve_poison_at_startup`] runs once at engine start: if `.ep_attempt`
//!   is still on disk, the previous run crashed *during* the bind, so we
//!   promote it to a persistent `packs/.ep_disabled` and never auto-try that
//!   EP again until the user re-enables it.
//! - [`is_disabled`] is consulted by the provider picker (treat the pack as
//!   absent → fall through to DirectML) and by the `ORT_DYLIB_PATH` pin.
//! - [`reenable`] clears the disable (wired to the Settings "Verify install"
//!   action / an explicit provider override / a pack reinstall).
//!
//! Only pack-backed GPU EPs are guarded (`cuda`, `openvino`); DirectML and CPU
//! are the always-safe fallbacks and are never armed.

use std::path::PathBuf;

fn packs_dir() -> Option<PathBuf> {
    let root = crate::paths::models_dir().ok()?;
    Some(root.join("packs"))
}

fn attempt_path() -> Option<PathBuf> {
    Some(packs_dir()?.join(".ep_attempt"))
}

/// Per-EP disable marker so two different packs can BOTH be crash-disabled at
/// once. The old single `.ep_disabled` file held one EP name and was overwritten
/// by the next poison, silently un-disabling the first. (ENG-59)
fn disabled_marker(ep: &str) -> Option<PathBuf> {
    Some(packs_dir()?.join(format!(".ep_disabled_{}", ep.to_ascii_lowercase())))
}

/// EPs that are guarded. DirectML/CPU are the safe fallbacks — never armed.
fn is_guarded(ep: &str) -> bool {
    matches!(ep, "cuda" | "openvino")
}

fn read_trimmed(path: &PathBuf) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_ascii_lowercase())
}

/// Record that we're about to bind `ep`. No-op for unguarded EPs.
pub fn arm(ep: &str) {
    if !is_guarded(ep) {
        return;
    }
    let Some(p) = attempt_path() else { return };
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(err) = std::fs::write(&p, ep) {
        tracing::warn!(?err, ep, "[EP-GUARD] could not arm crash breadcrumb");
    } else {
        tracing::debug!(ep, "[EP-GUARD] armed (will revert to DirectML if the bind crashes)");
    }
}

/// The bind succeeded — clear the breadcrumb so an unrelated later crash
/// can't poison this EP.
pub fn disarm() {
    if let Some(p) = attempt_path() {
        let _ = std::fs::remove_file(p);
    }
}

/// Run once at startup. If a guarded EP's `arm` was never followed by a
/// `disarm`, the prior run crashed during that EP's bind: promote it to a
/// persistent disable. Returns the EP that was just disabled, if any.
pub fn resolve_poison_at_startup() -> Option<String> {
    let attempt = attempt_path()?;
    let ep = read_trimmed(&attempt)?;
    // Stale attempt on disk == last run died mid-bind.
    let _ = std::fs::remove_file(&attempt);
    if !is_guarded(&ep) {
        return None;
    }
    if let Some(dis) = disabled_marker(&ep) {
        if let Some(parent) = dis.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&dis, &ep);
    }
    tracing::error!(
        ep = %ep,
        "[EP-GUARD] previous run crashed while binding the {ep} execution provider; \
         disabling it and falling back to DirectML. Re-enable in Settings → Performance \
         (Verify install) once the pack is fixed."
    );
    Some(ep)
}

/// True when `ep` was disabled by a prior crash and not yet re-enabled.
pub fn is_disabled(ep: &str) -> bool {
    if !is_guarded(ep) {
        return false;
    }
    // Per-EP marker is authoritative; the legacy single `.ep_disabled` file
    // (one EP name) is still honored for in-place upgrades.
    if disabled_marker(ep).map(|p| p.exists()).unwrap_or(false) {
        return true;
    }
    packs_dir()
        .map(|d| d.join(".ep_disabled"))
        .and_then(|p| read_trimmed(&p))
        .map(|d| d == ep.to_ascii_lowercase())
        .unwrap_or(false)
}

/// Clear the persistent disable for ONE execution provider only — the targeted
/// "Verify install" / per-pack reinstall path. Unlike [`reenable`] (clear-all),
/// this does NOT touch sibling EPs' disables, so verifying/installing the CUDA
/// pack can't silently re-arm a separately crash-poisoned OpenVINO EP and send
/// the next scan straight back into its bad bind (ENG-59's per-EP isolation).
/// Clears the legacy single-file marker only when it names `ep`.
pub fn reenable_ep(ep: &str) {
    if !is_guarded(ep) {
        return;
    }
    let Some(dir) = packs_dir() else { return };
    let mut cleared = false;
    if let Some(p) = disabled_marker(ep) {
        if p.exists() {
            let _ = std::fs::remove_file(&p);
            cleared = true;
        }
    }
    // Legacy single-file format — only if it names THIS ep.
    let legacy = dir.join(".ep_disabled");
    if read_trimmed(&legacy).map(|d| d == ep.to_ascii_lowercase()).unwrap_or(false) {
        let _ = std::fs::remove_file(&legacy);
        cleared = true;
    }
    if cleared {
        tracing::info!(ep, "[EP-GUARD] re-enabling previously crash-disabled execution provider");
    }
}
