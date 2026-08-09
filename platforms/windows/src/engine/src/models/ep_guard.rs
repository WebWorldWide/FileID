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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Per-EP arm refcount. The breadcrumb file (`.ep_attempt`) holds the SET of
/// currently-arming EPs (one per line). A bare counter or a "first arm wins"
/// single name would mis-record WHICH EP was binding when two DIFFERENT guarded
/// EPs overlap (e.g. a scan-startup CUDA bind + a search-query OpenVINO bind),
/// disabling a healthy EP while the real crasher survived. Refcounting per EP
/// also fixes the original race where a sibling bind's disarm removed a
/// breadcrumb a still-in-flight same-EP bind needed. (audit E7 + re-audit)
fn armed() -> &'static Mutex<HashMap<String, usize>> {
    static M: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Rewrite the breadcrumb to the current armed set (one EP per line), or delete
/// it when nothing is armed. Caller holds the `armed()` lock.
fn rewrite_breadcrumb(counts: &HashMap<String, usize>) {
    let Some(p) = attempt_path() else { return };
    let live: Vec<&str> = counts
        .iter()
        .filter(|(_, &c)| c > 0)
        .map(|(ep, _)| ep.as_str())
        .collect();
    if live.is_empty() {
        let _ = std::fs::remove_file(&p);
        return;
    }
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(err) = std::fs::write(&p, live.join("\n")) {
        tracing::warn!(?err, "[EP-GUARD] could not write crash breadcrumb");
    }
}

fn packs_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(base) = tests::packs_dir_override() {
        return Some(base);
    }
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

/// Record that we're about to bind `ep`. No-op for unguarded EPs. Adds `ep` to
/// the armed set so the breadcrumb always names every in-flight guarded EP.
pub fn arm(ep: &str) {
    if !is_guarded(ep) {
        return;
    }
    let mut counts = armed().lock().unwrap_or_else(|e| e.into_inner());
    *counts.entry(ep.to_string()).or_insert(0) += 1;
    rewrite_breadcrumb(&counts);
    tracing::debug!(ep, "[EP-GUARD] armed (will revert to DirectML if the bind crashes)");
}

/// The bind finished (success or a recoverable Rust error). Pass the same `ep`
/// that was armed; the breadcrumb stops naming `ep` only once its LAST
/// outstanding bind disarms, so a concurrent same-EP bind isn't left unguarded
/// and a concurrent DIFFERENT-EP bind keeps its own breadcrumb entry. (audit E7)
pub fn disarm(ep: &str) {
    if !is_guarded(ep) {
        return;
    }
    let mut counts = armed().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = counts.get_mut(ep) {
        // Saturating so a stray disarm without a paired arm can't underflow.
        *c = c.saturating_sub(1);
        if *c == 0 {
            counts.remove(ep);
        }
    }
    rewrite_breadcrumb(&counts);
}

/// RAII disarm: [`arm`]s `ep` on construction and [`disarm`]s on drop — including
/// the drop that happens when the owning future is CANCELLED (a graceful
/// shutdown that lands mid-model-load, where the explicit `disarm` after the
/// `.await` never runs). Without this, a clean shutdown during the first ORT
/// bind left `.ep_attempt` on disk and the NEXT launch false-poisoned a healthy
/// guarded EP. A real native crash still skips the drop (the process is gone),
/// so the over-disable-on-crash breadcrumb is preserved. (audit C1-001)
pub struct ArmGuard {
    ep: String,
    guarded: bool,
}

impl ArmGuard {
    pub fn arm(ep: &str) -> Self {
        arm(ep);
        Self { ep: ep.to_string(), guarded: is_guarded(ep) }
    }
}

impl Drop for ArmGuard {
    fn drop(&mut self) {
        if self.guarded {
            disarm(&self.ep);
        }
    }
}

/// Run once at startup. If a guarded EP's `arm` was never followed by a
/// `disarm`, the prior run crashed during that EP's bind: promote it to a
/// persistent disable. The breadcrumb can name MORE THAN ONE EP (heterogeneous
/// overlapping binds) — disable EVERY guarded EP it names so the actual crasher
/// can't survive; over-disabling a concurrently-arming healthy EP is recoverable
/// (re-enable in Settings), a crash-loop is not. Returns the first EP disabled,
/// if any (for the caller's banner). (audit E7 re-audit)
pub fn resolve_poison_at_startup() -> Option<String> {
    let attempt = attempt_path()?;
    let contents = std::fs::read_to_string(&attempt).ok()?;
    // Stale attempt on disk == last run died mid-bind.
    let _ = std::fs::remove_file(&attempt);
    let mut first: Option<String> = None;
    for line in contents.lines() {
        let ep = line.trim().to_ascii_lowercase();
        if ep.is_empty() || !is_guarded(&ep) {
            continue;
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
        if first.is_none() {
            first = Some(ep);
        }
    }
    first
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    // The breadcrumb file, the `.ep_disabled_*` markers, and the global
    // `armed()` refcount map are all process-global, so ep_guard tests must run
    // one at a time and reset that shared state between them.
    fn guard() -> &'static Mutex<()> {
        static G: OnceLock<Mutex<()>> = OnceLock::new();
        G.get_or_init(|| Mutex::new(()))
    }

    fn override_cell() -> &'static Mutex<Option<PathBuf>> {
        static C: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
        C.get_or_init(|| Mutex::new(None))
    }

    pub(super) fn packs_dir_override() -> Option<PathBuf> {
        override_cell().lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Clears the `packs_dir()` override on drop so the real path is restored for
    /// any other test that reaches an `ep_guard` public fn after this one.
    struct OverrideReset(PathBuf);
    impl Drop for OverrideReset {
        fn drop(&mut self) {
            *override_cell().lock().unwrap_or_else(|e| e.into_inner()) = None;
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Point `packs_dir()` at a fresh empty temp dir and clear all in-memory
    /// arm state so each test starts from a clean slate.
    fn fresh_packs() -> OverrideReset {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join("fileid-ep-guard-test")
            .join(format!("{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        *override_cell().lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.clone());
        armed().lock().unwrap_or_else(|e| e.into_inner()).clear();
        OverrideReset(dir)
    }

    #[test]
    fn graceful_shutdown_mid_load_clears_breadcrumb() {
        let _serial = guard().lock().unwrap_or_else(|e| e.into_inner());
        let packs = fresh_packs();
        let crumb = packs.0.join(".ep_attempt");

        // Arm as a scan's first ORT bind would. The breadcrumb names the EP.
        let g = ArmGuard::arm("cuda");
        assert!(crumb.exists(), "arming should drop the crash breadcrumb");

        // Graceful shutdown mid-load == the owning future is dropped before the
        // explicit disarm runs. The RAII guard's drop must clear the breadcrumb,
        // so the NEXT launch does not false-poison this healthy EP.
        drop(g);
        assert!(!crumb.exists(), "graceful drop should clear the crash breadcrumb");

        // And the next-launch resolver finds nothing to disable.
        assert_eq!(resolve_poison_at_startup(), None);
        assert!(!is_disabled("cuda"), "healthy EP must not be disabled after a clean shutdown");
    }

    #[test]
    fn actual_crash_still_over_disables() {
        let _serial = guard().lock().unwrap_or_else(|e| e.into_inner());
        let packs = fresh_packs();
        let crumb = packs.0.join(".ep_attempt");

        // A native crash == arm() ran but neither disarm() nor the guard's drop
        // did (the process is gone). The breadcrumb persists to disk.
        arm("cuda");
        assert!(crumb.exists(), "a crash must leave the breadcrumb for the next launch");

        // Next launch promotes the stale breadcrumb to a persistent disable.
        assert_eq!(resolve_poison_at_startup().as_deref(), Some("cuda"));
        assert!(is_disabled("cuda"), "crashed EP must stay disabled until re-enabled");
        assert!(!crumb.exists(), "resolver consumes the breadcrumb");

        reenable_ep("cuda");
        assert!(!is_disabled("cuda"));
    }
}
