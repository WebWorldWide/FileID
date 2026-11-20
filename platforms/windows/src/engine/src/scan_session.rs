use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::Connection;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::coordinator::ScanCoordinator;
use crate::ipc::{
    sink::Sink, BatchSummary, EventPayload, IpcEvent, ScanComplete, ScanPhase, ScanProgress, Wrap,
};
use crate::pipeline::dbwriter::{BatchStats, DbWriter};
use crate::pipeline::discovery::{path_fingerprint_text, Discovery, SkipFingerprint};
use crate::pipeline::scan_models::ModelSource;
use crate::pipeline::tagging::{Tagger, TaggedFile};
use crate::platform::{PriorityBoost, SleepGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Discovering,
    Tagging,
    PostScan,
    Completed,
    Cancelled,
    Failed,
}

impl SessionPhase {
    fn as_ipc_phase(self) -> ScanPhase {
        match self {
            SessionPhase::Discovering => ScanPhase::Discovering,
            SessionPhase::Tagging => ScanPhase::Tagging,
            SessionPhase::PostScan => ScanPhase::PostScan,
            SessionPhase::Completed => ScanPhase::Completed,
            SessionPhase::Cancelled => ScanPhase::Cancelled,
            SessionPhase::Failed => ScanPhase::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SessionResult {
    pub processed: u64,
    pub failed: u64,
    pub elapsed_seconds: f64,
    /// The caller runs one incremental follow-up pass, which reports the totals: files under the root
    /// were added, renamed or deleted while this pass ran, or some files timed out and are read again.
    pub needs_follow_up: bool,
    /// Files this pass stored as failed only because a read timed out and that the follow-up pass reads again;
    /// the follow-up's totals replace theirs.
    pub timed_out: u64,
    /// Every photo's stored date already came from its EXIF in this scan (a
    /// forced rescan, or a root with no catalog rows when it started).
    pub capture_dates_current: bool,
    pub progress_floor: ProgressFloor,
}

/// The largest counts the scan has shown so far. A follow-up pass restarts its own counters, so each of its snapshots
/// is lifted to at least this much and the bar never moves backwards.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressFloor {
    total: u64,
    discovered: u64,
    processed: u64,
}

impl ProgressFloor {
    fn raised_to(self, other: Self) -> Self {
        Self {
            total: self.total.max(other.total),
            discovered: self.discovered.max(other.discovered),
            processed: self.processed.max(other.processed),
        }
    }
}

/// Turns a follow-up pass's own counts into the scan's: what earlier passes finished is added and nothing falls below
/// what was already shown. For the first pass it changes nothing.
#[derive(Debug, Clone, Copy, Default)]
struct ProgressLift {
    carried_processed: u64,
    carried_failed: u64,
    floor: ProgressFloor,
}

impl ProgressLift {
    fn apply(&self, progress: &mut ScanProgress) {
        progress.discovered = (progress.discovered + self.carried_processed).max(self.floor.discovered);
        progress.processed = (progress.processed + self.carried_processed).max(self.floor.processed);
        progress.failed += self.carried_failed;
        let total = if progress.total > 0 { progress.total + self.carried_processed } else { 0 };
        progress.total = total.max(self.floor.total);
    }
}

pub struct ScanSession {
    pub session_id: String,
    coordinator: ScanCoordinator,
    db_conn: Arc<Mutex<Connection>>,
    worker_count: usize,
    sink: Sink,
    models: Arc<ModelSource>,
    /// When true, skip the incremental-rescan skip set and reprocess
    /// every file.
    rescan: bool,
    /// User-excluded folders, raw as received from the app. Resolved
    /// against the root at run() time.
    excluded_paths: Vec<String>,
    /// False for the follow-up pass, so a folder that keeps changing cannot loop.
    allow_follow_up: bool,
    /// The follow-up reads timed-out files again, which a busy machine failed the first time.
    retrying_timeouts: bool,
    /// 0 for the scan's first pass; each pass that re-reads timed-out files counts one more, up to `TIMEOUT_RETRY_ROUNDS`.
    retry_round: u32,
    /// The label the app gave the folder (`startScan.rootDisplay`); Locations shows it instead of the folder's name.
    root_display: Option<String>,
    /// Counts from the pass this one follows up, added to its final totals.
    carried: (u64, u64, f64),
    /// Decided by the first pass; a follow-up starts with that pass's rows.
    capture_dates_current: Option<bool>,
    progress_floor: ProgressFloor,
}

/// True when a directory the walk read was modified after `since` or is gone.
/// Creating, deleting or renaming a file changes its parent directory's mtime on
/// APFS, NTFS, ext4 and the other volumes FileID supports.
fn dirs_changed_since(dirs: &[std::path::PathBuf], since: std::time::SystemTime) -> bool {
    dirs.iter().any(|dir| match std::fs::symlink_metadata(dir).and_then(|m| m.modified()) {
        Ok(modified) => modified >= since,
        Err(_) => true,
    })
}

impl ScanSession {
    pub fn new(
        coordinator: ScanCoordinator,
        db_conn: Arc<Mutex<Connection>>,
        worker_count: usize,
        sink: Sink,
        models: Arc<ModelSource>,
        rescan: bool,
        excluded_paths: Vec<String>,
    ) -> Self {
        Self {
            session_id: Uuid::new_v4().to_string(),
            coordinator,
            db_conn,
            worker_count: worker_count.max(1),
            sink,
            models,
            rescan,
            excluded_paths,
            allow_follow_up: true,
            retrying_timeouts: false,
            retry_round: 0,
            root_display: None,
            carried: (0, 0, 0.0),
            capture_dates_current: None,
            progress_floor: ProgressFloor::default(),
        }
    }

    /// Runs under the id the app was already shown (the baseline progress and a follow-up pass use the same one).
    pub fn with_session_id(mut self, session_id: String) -> Self {
        self.session_id = session_id;
        self
    }

    pub fn with_root_display(mut self, label: Option<String>) -> Self {
        self.root_display = label.map(|label| label.trim().to_owned()).filter(|label| !label.is_empty());
        self
    }

    /// The incremental pass that follows a pass during which files changed or timed out. `round` is 1 for the first
    /// follow-up; every further round of timed-out reads runs with half the workers of the one before.
    pub fn follow_up(mut self, previous: &SessionResult, round: u32) -> Self {
        self.rescan = false;
        self.allow_follow_up = false;
        self.retry_round = round;
        self.carried = (
            previous.processed.saturating_sub(previous.timed_out),
            previous.failed.saturating_sub(previous.timed_out),
            previous.elapsed_seconds,
        );
        if previous.timed_out > 0 {
            self.retrying_timeouts = true;
            self.worker_count = (self.worker_count >> round.min(16)).max(1);
        }
        self.capture_dates_current = Some(previous.capture_dates_current);
        self.progress_floor = previous.progress_floor;
        self
    }

    fn progress_lift(&self) -> ProgressLift {
        ProgressLift { carried_processed: self.carried.0, carried_failed: self.carried.1, floor: self.progress_floor }
    }

    /// Run an end-to-end scan against `root`. Returns when DBWriter has
    /// drained the final batch, every worker has exited, and the
    /// `scanComplete` IPC event has been emitted.
    pub async fn run<F>(self, root: &Path, mut on_phase: F) -> Result<SessionResult>
    where
        F: FnMut(SessionPhase),
    {
        // Boost process priority + prevent sleep for the lifetime of the
        // scan. Both are RAII so a panic / cancel still releases them.
        let _priority = PriorityBoost::acquire();
        let _sleep = SleepGuard::acquire();

        let started = Instant::now();
        let scan_started = std::time::SystemTime::now();
        let session_id = self.session_id.clone();
        let sink = self.sink.clone();

        let emit_phase = |phase: SessionPhase| {
            // Intentional try_send + drop-on-overflow. Phase changes are
            // low-frequency (a few per scan) so the drop is unlikely
            // in practice, but a spawn(async { send.await }) pattern
            // could pile up unbounded tasks waiting on a full sink.
            // try_send bounds the engine's worst-case memory.
            //
            // This droppable path is for NON-terminal phases only.
            // Terminal phases (Cancelled / Failed / Completed) MUST go through
            // `emit_phase_guaranteed` below — a dropped terminal PhaseChanged
            // makes the app render a cancelled/failed scan as Completed and
            // auto-fire face clustering. The engine's "terminal events must
            // not drop" rule.
            let p = phase.as_ipc_phase();
            let _ = sink.try_send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(p))));
        };

        // Awaits channel capacity rather than dropping, so a cancelled/failed
        // terminal state can never be silently lost under sink backpressure.
        let emit_phase_guaranteed = |sink: &Sink, phase: SessionPhase| {
            let p = phase.as_ipc_phase();
            let sink = sink.clone();
            async move {
                sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(p))))
                    .await;
            }
        };

        // `handle_start_scan` announces Discovering before the models load; announcing it again here
        // would show the app two phase changes for one scan.
        on_phase(SessionPhase::Discovering);

        // Persist a scan_sessions row so Settings → Recent scans can list
        // past scans + offer Re-scan. Uses the engine's single-writer
        // connection — same one DBWriter holds.
        let started_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        {
            let conn = self.db_conn.lock();
            let _ = conn.execute(
                "INSERT OR REPLACE INTO scan_sessions \
                 (id, root_path, started_at, completed_at, last_file_index, total_files, status) \
                 VALUES (?1, ?2, ?3, NULL, NULL, NULL, 'running')",
                rusqlite::params![
                    session_id,
                    root.to_string_lossy(),
                    started_unix,
                ],
            );
            let root_text = root.to_string_lossy();
            let kind = if root_text.starts_with("\\\\") { "network" } else if root_text.starts_with("/Volumes/") { "external" } else { "internal" };
            let _ = conn.execute(
                "INSERT INTO library_roots(path,display,kind,added_at,last_status) VALUES(?1,COALESCE(?4,?1),?2,?3,'running') \
                 ON CONFLICT(path) DO UPDATE SET last_status='running', display=COALESCE(?4,display)",
                rusqlite::params![root_text, kind, started_unix, self.root_display],
            );
        }

        let capture_dates_current = self.capture_dates_current.unwrap_or_else(|| {
            self.rescan || !crate::pipeline::capture_date_backfill::catalog_has_rows(&self.db_conn.lock(), root)
        });

        // Resolve user exclusions against this root, then purge already-
        // cataloged rows beneath them BEFORE the skip-set preload so a
        // stale excluded row can never enter the skip set.
        let exclusions = crate::util::path_safety::resolve_exclusions(root, &self.excluded_paths);
        if !exclusions.is_empty() {
            let conn = self.db_conn.lock();
            let purged = purge_excluded_rows(&conn, &exclusions);
            if purged > 0 {
                tracing::info!("[SCAN] purged {purged} cataloged rows under excluded folders");
            }
        }

        // Pre-load the "already current" set from DB so Discovery can skip
        // files whose `scanned_at >= modified_at`. For a 1M-file repeat
        // scan this turns an 8-hour redo into ~1-second startup + ~0
        // discovery walk. The rescan flag forces a full rescan.
        let skip_paths = if self.rescan {
            tracing::info!("[SCAN] rescan=true; processing every file regardless of prior scan state");
            std::sync::Arc::new(std::collections::HashMap::new())
        } else {
            let conn = self.db_conn.lock();
            // Carry (size_bytes, modified_at) per path so discovery can
            // revalidate against the LIVE file mtime — a bare path set skipped any
            // file edited after its last scan.
            let mut set =
                std::collections::HashMap::<SkipFingerprint, (i64, Option<f64>)>::new();
            let root_prefix = root.to_string_lossy().to_string();
            let total_files: i64 = conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                .unwrap_or(0);
            let lo = root_descendant_prefix(root);
            // A BINARY range seek (`>= lo AND < hi`) is sargable on the
            // implicit UNIQUE index on path_text, unlike `LIKE 'lo%'` (which is
            // non-sargable because LIKE defaults to case-insensitive and forces
            // a full table scan). Stored paths derive from THIS root so they
            // share its exact casing; the skip set is an optimization that fails
            // safe (a miss just re-scans the file).
            let hi = prefix_upper_bound(&lo);
            let prefix_match: i64 = {
                match &hi {
                    Some(hi) => conn.query_row(
                        "SELECT COUNT(*) FROM files WHERE path_text >= ?1 AND path_text < ?2",
                        rusqlite::params![lo, hi],
                        |r| r.get(0),
                    ),
                    None => conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)),
                }
                .unwrap_or(0)
            };
            // failed=0 excludes prior failures (they must retry every scan).
            // modified_at IS NULL rows fall out automatically: NULL comparisons
            // are NULL → treated as false in WHERE, so the row is omitted from
            // the skip set and gets reprocessed.
            //
            // A content-bearing file scanned as an online-only
            // (dehydrated OneDrive Files-On-Demand) placeholder records
            // content_hash=NULL — the tagger skips the content read so it
            // never triggers a network hydration. Hydration does NOT bump
            // modified_at, so the bare `scanned_at >= modified_at` predicate
            // would strand the now-local file in the skip set forever. Exclude
            // any content-bearing kind that lacks a content_hash so a
            // dehydrated→hydrated file is reprocessed; a transient hash failure
            // re-scans too, which is the skip set's documented fail-safe.
            let content_hash_gate = SKIP_SET_CONTENT_HASH_GATE;
            // Lockstep with macOS Discovery: when BGE is installed, keep
            // embeddingless docs/pdfs OUT of the skip set so an install-then-rescan
            // re-emits them and the tagger backfills the BGE embedding. Empty when BGE
            // isn't on disk (no doc could embed → don't force a perpetual re-walk).
            let text_embed_gate = text_embed_gate(bge_installed());
            // CLIP ships by default, so always keep an embeddingless `.obj` in the pipeline
            // to backfill its rendered-shape CLIP vector (lockstep with macOS).
            let model_clip_gate = SKIP_SET_MODEL_CLIP_GATE;
            // Images catalogued while CLIP was missing stay in the pipeline once
            // it is installed so a rescan backfills their embeddings (and faces,
            // which run in the same pass) — lockstep with macOS Discovery.
            let image_clip_gate = if clip_can_embed() { SKIP_SET_IMAGE_CLIP_GATE } else { "" };
            let video_clip_gate = video_clip_gate();
    // Both columns are stored at scan time, so `scanned_at >= modified_at` holds for every
    // scanned file, even after an edit. Compare size+mtime against the live file in discovery.
            let select_sql = if hi.is_some() {
                format!(
                    "SELECT path_text, size_bytes, modified_at FROM files \
                     WHERE path_text >= ?1 AND path_text < ?2 \
                     AND failed = 0 \
                     {content_hash_gate}{model_clip_gate}{image_clip_gate}{video_clip_gate}{text_embed_gate}"
                )
            } else {
                format!(
                    "SELECT path_text, size_bytes, modified_at FROM files \
                     WHERE failed = 0 \
                     {content_hash_gate}{model_clip_gate}{image_clip_gate}{video_clip_gate}{text_embed_gate}"
                )
            };
            if let Ok(mut stmt) = conn.prepare(&select_sql) {
                // One closure literal shared across both arms — two separate
                // closures are distinct types and `match` can't unify them.
                let row_to_entry = |r: &rusqlite::Row| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<f64>>(2)?,
                    ))
                };
                let rows = match &hi {
                    Some(hi) => stmt.query_map(rusqlite::params![lo, hi], row_to_entry),
                    None => stmt.query_map([], row_to_entry),
                };
                if let Ok(rows) = rows {
                    for (p, size, modified) in rows.flatten() {
                        set.insert(path_fingerprint_text(&p), (size, modified));
                    }
                }
            }

            // Files that failed on enough scans, unchanged since, stay failed
            // rows (the Library still lists them) but are not attempted again.
            let given_up = add_given_up_failures(&conn, &mut set, &lo, hi.as_deref());
            if given_up > 0 {
                tracing::info!(given_up, "[SCAN] files that keep failing are left alone until they change");
            }

            // Crash self-heal: face crops are written post-commit outside the
            // writer tx, so a mid-scan kill can leave committed face_prints
            // rows with no on-disk crop JPEG. Those files are size+mtime
            // unchanged, so they'd be skipped forever and show gray faces in
            // the People tab. Only after an unclean prior shutdown, pull the
            // affected files back out of the skip set so they reprocess and
            // re-emit their crops. Bounded to face-bearing files under the
            // scan root; clean shutdowns skip this entirely.
            if crash_self_heal_due(&lo) {
                let removed = reconcile_missing_face_crops(&conn, &mut set, &lo, hi.as_deref());
                if removed > 0 {
                    tracing::info!(
                        removed,
                        "[SCAN] crash self-heal: reprocessing files with missing face crops"
                    );
                }
            }
            tracing::info!(
                already_current = set.len(),
                files_total = total_files,
                files_under_root = prefix_match,
                root = %crate::platform::redact_path_for_log(&root_prefix),
                "[SCAN] preloaded skip set for incremental rescan"
            );
            std::sync::Arc::new(set)
        };

        // Capture the skip-set size before it moves into Discovery: a count==0
        // result with a non-empty skip set means "incremental rescan, all files
        // already current" — NOT an empty/unsupported folder.
        let skip_count = skip_paths.len();
        let excl_norm: Vec<String> = exclusions.iter().map(|e| e.normalized.clone()).collect();
        // Windows catalogs a cloud placeholder from its metadata; the content-hash
        // gate above keeps it out of the skip set so a download is noticed. Hand
        // discovery those rows so a file that is still a placeholder is not
        // re-emitted and re-written on every scan.
        let known_placeholders = if cfg!(windows) && !self.rescan {
            known_placeholders(&self.db_conn.lock(), root)
        } else {
            std::collections::HashMap::new()
        };
        let discovery = Discovery::new_with_skip_and_exclusions(
            root,
            self.coordinator.clone(),
            skip_paths,
            Arc::new(excl_norm),
        )
        .with_known_placeholders(known_placeholders)
        // A forced rescan analyses everything again, moved files included.
        .with_move_index(if self.rescan {
            None
        } else {
            crate::pipeline::discovery::MoveIndex::load(&self.db_conn)
        });
        let handle = discovery.spawn();
        let discovered_count = handle.count.clone();
        let discovered_heavy = handle.heavy_count.clone();
        let discovered_done = handle.done.clone();
        let discovered_errors = handle.error_count.clone();
        let discovered_cloud_only = handle.cloud_only_count.clone();
        let discovered_seen_paths = handle.seen_paths.clone();
        let discovered_seen_dirs = handle.seen_dirs.clone();
        let discovery_join = handle.join;
        let discovered_rx = handle.rx;
        let mut zero_byte_rx = handle.zero_byte_rx;
        let zero_byte_conn = self.db_conn.clone();
        let zero_byte_worker = tokio::task::spawn_blocking(move || -> Result<(u64, u64)> {
            const ZERO_BYTE_BATCH_SIZE: usize = 64;
            let mut observations = Vec::with_capacity(ZERO_BYTE_BATCH_SIZE);
            let mut applied = 0_u64;
            let mut rejected = 0_u64;
            while let Some(observation) = zero_byte_rx.blocking_recv() {
                observations.push(observation);
                if observations.len() < ZERO_BYTE_BATCH_SIZE {
                    continue;
                }
                let validation = crate::db::zero_byte::validate_zero_byte_files(&observations);
                rejected += validation.changed_since_observation;
                let mutation = {
                    let conn = zero_byte_conn.lock();
                    crate::db::zero_byte::deactivate_validated_zero_byte_files(
                        &conn,
                        &validation.observations,
                    )?
                };
                let summary = crate::db::zero_byte::finish_zero_byte_mutation(mutation);
                applied += summary.applied;
                observations.clear();
            }
            if !observations.is_empty() {
                let validation = crate::db::zero_byte::validate_zero_byte_files(&observations);
                rejected += validation.changed_since_observation;
                let mutation = {
                    let conn = zero_byte_conn.lock();
                    crate::db::zero_byte::deactivate_validated_zero_byte_files(
                        &conn,
                        &validation.observations,
                    )?
                };
                let summary = crate::db::zero_byte::finish_zero_byte_mutation(mutation);
                applied += summary.applied;
            }
            Ok((applied, rejected))
        });

        // Emit a live Progress event every 250 ms while discovery walks
        // the tree. Without this, the sidebar stays on "Discovering…" with
        // every stat row reading "—" until the first DBWriter batch flushes
        // (often >1 s on cold cache; never if every file was filtered out).
        // Also surfaces an empty-folder event so the UI doesn't hang.
        let sink_for_tick = sink.clone();
        let session_id_for_tick = session_id.clone();
        let coord_for_tick = self.coordinator.clone();
        let root_for_tick = root.to_string_lossy().into_owned();
        let errors_for_tick = discovered_errors.clone();
        let cloud_only_for_tick = discovered_cloud_only.clone();
        let discovered_count_for_tick = discovered_count.clone();
        let lift_for_tick = self.progress_lift();
        // Single-shot guard shared with the post-drain fallback below: the tick
        // sleeps 250 ms before its first wake-up, so a folder that discovers and
        // drains faster than that (empty folder / fully-current rescan) would be
        // aborted before the tick emits its notice. Whichever path wins the swap
        // emits; the other skips.
        let notice_emitted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notice_emitted_for_tick = notice_emitted.clone();
        // discoveryComplete must fire EXACTLY ONCE on every scan terminal path
        // (normal, empty, sub-250 ms drain, cancel-during-discovery) — the IPC
        // parity invariant the app's progress bar depends on for its file total.
        // The tick emits it on the normal path; the post-drain backstop below
        // covers the paths where the tick is aborted mid-sleep or returns early
        // on cancel. This shared single-shot flag keeps the two from double-
        // emitting.
        let discovery_complete_emitted =
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let discovery_complete_emitted_for_tick = discovery_complete_emitted.clone();
        let discovered_done_post = discovered_done.clone();
        let tick = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if coord_for_tick.is_cancelled() {
                    return;
                }
                let count = discovered_count_for_tick.load(std::sync::atomic::Ordering::Relaxed);
                let finished = discovered_done.load(std::sync::atomic::Ordering::Acquire);
                let mut progress = ScanProgress {
                    session_id: session_id_for_tick.clone(),
                    phase: ScanPhase::Discovering,
                    total: 0,
                    discovered: count,
                    processed: 0,
                    failed: 0,
                    files_per_second: 0.0,
                    eta_seconds: None,
                    resident_mb: crate::platform::process_memory_mb(),
                    available_mb: 0,
                };
                lift_for_tick.apply(&mut progress);
                let _ = sink_for_tick.try_send(IpcEvent::now(EventPayload::Progress(Wrap::new(progress))));
                if finished {
                    // IPC parity with macOS (FileIDEngineMain.swift:486):
                    // announce the discovery total once the walk ends —
                    // macOS emits it even for a cancel-truncated walk. Swap the
                    // shared flag first so the post-drain backstop never re-emits
                    // what the tick already sent.
                    if !discovery_complete_emitted_for_tick
                        .swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        let _ = sink_for_tick.try_send(IpcEvent::now(
                            EventPayload::DiscoveryComplete(
                                crate::ipc::DiscoveryCompletePayload { total_files: count },
                            ),
                        ));
                    }
                    // Empty-folder path: tagging would wait forever on its
                    // input channel if discovery produced zero files.
                    // Close out cleanly with a "no supported files found"
                    // error so the user knows what happened.
                    let errs = errors_for_tick.load(std::sync::atomic::Ordering::Relaxed);
                    // Build the notice this tick would emit (None when there's
                    // nothing to say) so the single-shot flag is only CLAIMED
                    // when a notice is actually delivered.
                    let cloud_only = cloud_only_for_tick.load(std::sync::atomic::Ordering::Relaxed);
                    let reachable = count != 0 || scan_root_reachable(Path::new(&root_for_tick));
                    let notice = discovery_notice(count, skip_count, errs, cloud_only, &root_for_tick, reachable);
                    if let Some(err) = notice {
                        // Claim the single-shot, but RELEASE it if the droppable
                        // try_send drops under sink backpressure — otherwise a
                        // dropped notice would also suppress the reliable
                        // post-drain .await fallback, losing it entirely.
                        if !notice_emitted_for_tick.swap(true, std::sync::atomic::Ordering::SeqCst)
                            && sink_for_tick
                                .try_send(IpcEvent::now(EventPayload::Error(Wrap::new(err))))
                                .is_err()
                        {
                            notice_emitted_for_tick
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                    return;
                }
            }
        });

        on_phase(SessionPhase::Tagging);
        emit_phase(SessionPhase::Tagging);
        let writer = DbWriter::new(self.db_conn.clone(), self.coordinator.clone());
        let known_twins = {
            let conn = self.db_conn.lock();
            crate::pipeline::twin_reuse::known_image_hashes(&conn, clip_can_embed()).unwrap_or_default()
        };
        let tagger = Tagger::with_model_source(self.coordinator.clone(), self.worker_count, self.models.clone())
            .with_scan_root(root.to_path_buf())
            .with_retrying_failures(self.retrying_timeouts)
            .with_legacy_hashes(
                writer.rename_heal_enabled()
                    && legacy_hash_rows_possible(&self.db_conn.lock(), crate::paths::root().ok().as_deref()),
            )
            .with_known_twins(known_twins);
        let (tagged_rx, mut decoder_join): (
            mpsc::Receiver<TaggedFile>,
            tokio::task::JoinHandle<()>,
        ) = tagger.spawn(discovered_rx);

        // Throttle progress emission: at most one event per 100 ms OR
        // every 1000 files (whichever first). Without throttling, a fast
        // scan floods the IPC channel and the app dispatcher.
        let progress_state = Arc::new(Mutex::new(ProgressState::new()));

        let sink_for_batch = sink.clone();
        let session_id_for_batch = session_id.clone();
        let progress_state_for_batch = progress_state.clone();
        // Clone the discovery counter for the tagging callback so Progress
        // events during tagging report the real discovered total — else
        // the sidebar progress bar pegs at 100 % during tagging.
        let discovered_count_for_batch = discovered_count.clone();
        let discovered_heavy_for_batch = discovered_heavy.clone();
        let discovered_done_for_batch = discovered_done_post.clone();
        let mut last_summary_processed = 0u64;
        let cursor_db = self.db_conn.clone();
        let cursor_session = session_id.clone();
        let cursor_carried = self.carried.0;
        let lift_for_batch = self.progress_lift();

        let writer_outcome = writer
            .run(tagged_rx, move |stats: BatchStats| {
                // The Swift engine's resume cursor: files written so far, which Settings ▸ Recent
                // scans shows as "N files" even for a scan that never finished.
                record_scan_cursor(&cursor_db.lock(), &cursor_session, stats.processed_total + cursor_carried);
                if should_emit_batch_summary(stats.batch_index, stats.processed_total, last_summary_processed) {
                    emit_batch_summary(&sink_for_batch, &stats);
                    last_summary_processed = stats.processed_total;
                }
                let discovered = discovered_count_for_batch.load(std::sync::atomic::Ordering::Relaxed);
                let heavy = discovered_heavy_for_batch.load(std::sync::atomic::Ordering::Relaxed);
                let walk_done =
                    discovered_done_for_batch.load(std::sync::atomic::Ordering::Acquire);
                maybe_emit_progress(
                    &sink_for_batch,
                    &progress_state_for_batch,
                    &session_id_for_batch,
                    &stats,
                    discovered,
                    heavy,
                    walk_done,
                    &lift_for_batch,
                );
            })
            .await;
        if writer_outcome.is_err() {
            self.coordinator.request_cancel();
        }

        // Tick exits on its own once discovery's `done` flips true; this
        // abort is belt-and-suspenders for the rare case where DBWriter
        // returns before tick observes the done flag (cancel mid-walk).
        // Must run BEFORE the error propagation below, or a failed writer
        // leaves the tick emitting Discovering progress after the caller's
        // PhaseChanged(Failed).
        tick.abort();

        let decoder_wait = if self.coordinator.is_cancelled() {
            std::time::Duration::from_secs(2)
        } else {
            std::time::Duration::from_secs(60)
        };
        let decoder_outcome = match tokio::time::timeout(decoder_wait, &mut decoder_join).await {
            Ok(result) => result.context("joining decoder pool"),
            Err(_) if self.coordinator.is_cancelled() => {
                tracing::warn!(
                    active = crate::pipeline::tagging::active_decoder_threads(),
                    "cancelled scan left a decoder draining bounded read-only work"
                );
                Ok(())
            }
            Err(_) => Err(anyhow::anyhow!(
                "decoder pool did not quiesce within {} seconds",
                decoder_wait.as_secs()
            )),
        };
        let zero_byte_outcome = zero_byte_worker
            .await
            .context("joining zero-byte catalog worker")
            .and_then(|outcome| outcome);
        let discovery_outcome = discovery_join.await.context("joining discovery worker");
        let pipeline_outcome = match (
            writer_outcome,
            decoder_outcome,
            zero_byte_outcome,
            discovery_outcome,
        ) {
            (Ok((total, failed)), Ok(()), Ok((applied, rejected)), Ok(())) => {
                Ok((total, failed, applied, rejected))
            }
            (Err(err), _, _, _) | (_, _, Err(err), _) => Err(err),
            (_, Err(err), _, _) | (_, _, _, Err(err)) => Err(err),
        };
        // The models could not be loaded: nothing was analysed, and the caller
        // reports the reason instead of a cancelled scan.
        let pipeline_outcome = match self.models.failure() {
            Some(failure) => Err(anyhow::Error::new(failure)),
            None => pipeline_outcome,
        };
        let (total, failed, zero_byte_applied, zero_byte_rejected) = match pipeline_outcome {
            Ok(result) => result,
            Err(err) => {
                // Stamp the row 'failed' so Settings → Recent scans doesn't
                // show it as 'running' forever. Best-effort: the failure may be
                // the DB itself (SQLITE_FULL / unreachable).
                let conn = self.db_conn.lock();
                let completed_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let _ = conn.execute(
                    "UPDATE scan_sessions SET completed_at = ?1, status = 'failed' WHERE id = ?2",
                    rusqlite::params![completed_unix, session_id],
                );
                return Err(err);
            }
        };
        if zero_byte_applied > 0 {
            tracing::info!(
                zero_byte_applied,
                "[SCAN] transitioned existing zero-byte rows to dormant state"
            );
        }
        if zero_byte_rejected > 0 {
            tracing::warn!(
                zero_byte_rejected,
                "[SCAN] files changed after zero-byte discovery; preserving catalog rows"
            );
            sink.send(crate::ipc::EngineError::new(
                "discovery_partial",
                changed_while_scanning_message(zero_byte_rejected),
            )
            .with_path(root.to_string_lossy().into_owned())
            .into_event())
            .await;
        }

        // discoveryComplete backstop: the 250 ms tick emits this
        // on a normal scan, but a sub-250 ms drain (empty folder / fully-current
        // rescan) aborts the tick mid-first-sleep, and a cancel-during-discovery
        // returns the tick early — both BEFORE it emits. The walk has ended by
        // here (writer.run drained tagged_rx), so `discovered_count` is final.
        // Emit unconditionally w.r.t. cancel to match the macOS reference, which
        // announces the total even on a cancel-truncated walk; the shared single-
        // shot flag the tick also swaps guarantees exactly-once across both paths.
        if !discovery_complete_emitted.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let count = discovered_count.load(std::sync::atomic::Ordering::Relaxed);
            sink.send(IpcEvent::now(EventPayload::DiscoveryComplete(
                crate::ipc::DiscoveryCompletePayload { total_files: count },
            )))
            .await;
        }

        // Post-drain fallback for the empty/rescan/partial notice: if discovery
        // finished but the tick was aborted before it could emit (drained inside
        // its 250 ms first sleep — empty folder or fully-current rescan), emit it
        // here so the UI never silently hangs on "Discovering…". Single-shot via
        // the shared flag so we never double-emit what the tick already sent.
        if !self.coordinator.is_cancelled()
            && discovered_done_post.load(std::sync::atomic::Ordering::Acquire)
            && !notice_emitted.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let count = discovered_count.load(std::sync::atomic::Ordering::Relaxed);
            let errs = discovered_errors.load(std::sync::atomic::Ordering::Relaxed);
            let root_display = root.to_string_lossy().into_owned();
            let cloud_only = discovered_cloud_only.load(std::sync::atomic::Ordering::Relaxed);
            let reachable = count != 0 || scan_root_reachable(Path::new(&root_display));
            let notice = discovery_notice(count, skip_count, errs, cloud_only, &root_display, reachable);
            if let Some(err) = notice {
                sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(err))))
                    .await;
            }
        }

        let clean_walk = zero_byte_rejected == 0
            && should_reconcile_missing_rows(
            self.coordinator.is_cancelled(),
            self.coordinator.is_gpu_dead(),
            discovered_done_post.load(std::sync::atomic::Ordering::Acquire),
            discovered_errors.load(std::sync::atomic::Ordering::Relaxed),
            std::env::var("FILEID_TEST_FILE_CAP")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0),
        );
        if clean_walk {
            mark_heal_done(&CRASH_HEALED_ROOTS, &root_descendant_prefix(root));
        }
        // Files a busy machine timed out are read again once, with fewer workers, before the scan reports its totals.
        let timed_out = if self.retry_round < TIMEOUT_RETRY_ROUNDS && !self.coordinator.is_cancelled() && !self.coordinator.is_gpu_dead() {
            count_retryable_timeouts(&self.db_conn.lock(), root, started_unix)
        } else {
            0
        };
        let files_changed =
            clean_walk && self.allow_follow_up && dirs_changed_since(&discovered_seen_dirs.lock(), scan_started);
        if files_changed || timed_out > 0 {
            // Rows of files deleted after they were processed are in the seen set,
            // and renamed files were never visited, so the missing-file pass below
            // would be wrong. Hand over to the follow-up pass, which owns the
            // scan_sessions row, the missing-file pass and the terminal events.
            let _ = self
                .db_conn
                .lock()
                .execute("DELETE FROM scan_sessions WHERE id = ?1", rusqlite::params![session_id]);
            tracing::info!(processed = total, files_changed, timed_out, "[SCAN] running a follow-up pass");
            return Ok(SessionResult {
                processed: total + self.carried.0,
                failed: failed + self.carried.1,
                elapsed_seconds: started.elapsed().as_secs_f64() + self.carried.2,
                needs_follow_up: true,
                timed_out,
                capture_dates_current,
                progress_floor: ProgressFloor {
                    total: total.max(discovered_count.load(std::sync::atomic::Ordering::Relaxed)) + self.carried.0,
                    discovered: discovered_count.load(std::sync::atomic::Ordering::Relaxed) + self.carried.0,
                    processed: total + self.carried.0,
                }
                .raised_to(self.progress_floor),
            });
        }
        // A root that is gone, empty or an unmounted drive's leftover folder lists nothing,
        // which a walk cannot tell from a library whose files were all deleted. Hiding rows
        // (and later deleting the hidden ones) on that evidence would wipe the library of an
        // unplugged drive; the Swift engine's `ScanRootGuard.isReachable` keeps them too.
        let clean_walk = clean_walk && {
            let reachable = scan_root_reachable(root);
            if !reachable {
                tracing::warn!("[SCAN] scan root is missing, empty or unmounted; catalog rows kept");
            }
            reachable
        };
        if clean_walk {
            {
                let mut seen = discovered_seen_paths.lock();
                let mut conn = self.db_conn.lock();
                match soft_hide_missing_rows(&mut conn, root, &mut seen) {
                    Ok(marked) if marked > 0 => {
                        tracing::info!(marked, "[SCAN] soft-hid catalog rows missing from completed walk");
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::error!(error = %err, "[SCAN] missing-file reconciliation failed; preserving catalog rows");
                    }
                }
            }
            let purged = purge_expired_hidden_rows(&self.db_conn, root, unix_now(), hidden_retention_days(), &|| {
                self.coordinator.is_cancelled()
            });
            if purged.removed > 0 || purged.skipped_for_clock {
                tracing::info!(
                    removed = purged.removed,
                    batches = purged.batches,
                    skipped_for_clock = purged.skipped_for_clock,
                    "[STATS] hidden rows of long-gone files removed"
                );
            }
            match crate::paths::root() {
                Ok(state_dir) if capture_dates_current => {
                    crate::pipeline::capture_date_backfill::mark_done(&state_dir, root);
                }
                Ok(state_dir) => {
                    let (database, coordinator, root) = (self.db_conn.clone(), self.coordinator.clone(), root.to_path_buf());
                    let backfill = tokio::task::spawn_blocking(move || {
                        crate::pipeline::capture_date_backfill::run_if_needed(&database, &state_dir, &root, &|| {
                            coordinator.is_cancelled()
                        })
                    });
                    if let Err(error) = backfill.await {
                        tracing::warn!(%error, "[SCAN] capture-date backfill stopped");
                    }
                }
                Err(error) => tracing::warn!(%error, "[SCAN] no state folder for the capture-date backfill marker"),
            }
        } else {
            tracing::info!("[SCAN] skipped missing-file reconciliation because the walk was partial, cancelled, failed, or test-capped");
        }

        if !self.coordinator.is_cancelled() {
            match crate::db::person_counts::recount_stale(&self.db_conn.lock()) {
                Ok(0) => {}
                Ok(fixed) => tracing::info!(fixed, "[SCAN] corrected stale person file counts"),
                Err(err) => tracing::warn!(?err, "[SCAN] person file recount failed"),
            }
        }

        // What the main pass left owed (RAM++ tags in deferred mode, careful
        // text reads), and what an earlier cancelled or interrupted scan of
        // this folder still owes. A cancel here ends the scan as cancelled; the
        // rest stays owed.
        let mut post_scan_announced = false;
        if !self.coordinator.is_cancelled() && !self.coordinator.is_gpu_dead() {
            if self.rescan {
                crate::pipeline::catch_up::forget_careful_reader_failures();
            }
            let (owed_tags, owed_careful) = crate::pipeline::catch_up::owed(&self.db_conn.lock(), Some(root));
            let reader_deferred = owed_tags == 0 && crate::pipeline::catch_up::careful_reader_deferred();
            if reader_deferred {
                tracing::info!(owed_careful, "[SCAN] careful text reads stay owed: the reader stopped answering recently");
            }
            if owed_tags + owed_careful > 0 && !reader_deferred {
                on_phase(SessionPhase::PostScan);
                emit_phase(SessionPhase::PostScan);
                post_scan_announced = true;
                match self.models.get().await {
                    Some(stack) => {
                        let analyzer = crate::pipeline::tagging::catch_up_analyzer(stack, self.coordinator.clone());
                        let report = Arc::new(CatchUpProgress {
                            sink: sink.clone(),
                            session_id: session_id.clone(),
                            discovered: discovered_count.load(std::sync::atomic::Ordering::Relaxed) + self.carried.0,
                            processed: total + self.carried.0,
                            failed: failed + self.carried.1,
                            lift: ProgressLift { floor: self.progress_floor, ..ProgressLift::default() },
                            state: Mutex::new((0, 0, Instant::now())),
                        });
                        let heartbeat = report.clone().spawn_heartbeat(CATCH_UP_HEARTBEAT);
                        let report_pass = report.clone();
                        let late_root = root.to_path_buf();
                        let (conn, coordinator, root) = (self.db_conn.clone(), self.coordinator.clone(), root.to_path_buf());
                        let pass = tokio::task::spawn_blocking(move || {
                            let summary = crate::pipeline::catch_up::run(
                                &conn,
                                Some(&root),
                                analyzer.as_ref(),
                                &|| coordinator.is_cancelled(),
                                &|| coordinator.is_paused(),
                                &|event| report_pass.report(event),
                            );
                            (summary, analyzer)
                        });
                        let finished = pass.await;
                        heartbeat.abort();
                        let _ = heartbeat.await;
                        if finished.as_ref().is_ok_and(|(summary, _)| summary.disk_full) {
                            sink.send(crate::ipc::EngineError::event(
                                "scan_disk_full",
                                crate::util::user_message::DISK_ALMOST_FULL,
                            ))
                            .await;
                        }
                        match finished {
                            Ok((summary, analyzer)) if summary.careful_waiting && !self.coordinator.is_cancelled() => {
                                spawn_late_careful_reads(self.db_conn.clone(), late_root, analyzer);
                            }
                            Ok(_) => {}
                            Err(error) => tracing::warn!(%error, "[SCAN] the catch-up pass stopped"),
                        }
                    }
                    None => tracing::warn!("[SCAN] the catch-up pass was skipped: the models did not load"),
                }
            }
        }

        let elapsed = started.elapsed().as_secs_f64();

        // Stamp the scan_sessions row with completed_at + status. gpu-dead
        // wins over cancelled: a TDR-aborted scan emits PhaseChanged(Failed)
        // below and must not be recorded as completed/cancelled.
        {
            let conn = self.db_conn.lock();
            let final_status = if self.coordinator.is_gpu_dead() {
                "failed"
            } else if self.coordinator.is_cancelled() {
                "cancelled"
            } else {
                "completed"
            };
            let completed_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            stamp_scan_session_end(&conn, &session_id, completed_unix, total + self.carried.0, final_status);
            let _ = conn.execute(
                "UPDATE library_roots SET last_scan_at=?1,last_status=?2 WHERE path=(SELECT root_path FROM scan_sessions WHERE id=?3)",
                rusqlite::params![completed_unix, final_status, session_id],
            );
        }

        if self.coordinator.is_gpu_dead() {
            // Distinct error kind so the C# app can show a
            // "GPU device suspended — restart the app to recover" banner
            // instead of the generic cancelled state. Emit BEFORE
            // PhaseChanged(Failed) so IPC consumers see the cause first.
            sink.send(crate::ipc::EngineError::event(
                "gpu_device_removed",
                crate::coordinator::GPU_DEVICE_REMOVED_MESSAGE,
            ))
            .await;
            on_phase(SessionPhase::Failed);
            sink.send(IpcEvent::now(EventPayload::Progress(Wrap::new(terminal_progress(
                &session_id,
                ScanPhase::Failed,
                discovered_count.load(std::sync::atomic::Ordering::Relaxed) + self.carried.0,
                discovered_count.load(std::sync::atomic::Ordering::Relaxed) + self.carried.0,
                total + self.carried.0,
                failed + self.carried.1,
            )))))
            .await;
            // Terminal Failed phase via the guaranteed (backpressuring)
            // path — a drop here would render the TDR-aborted scan as Completed
            // and auto-fire face clustering on a half-scanned library.
            //
            // Failed emits NO terminal ScanComplete. The app's
            // ScanCompleteEvent handler only suppresses the Phase overwrite for
            // Cancelled, so a ScanComplete on this branch would immediately
            // clobber phaseChanged(Failed) back to Completed (relabeling a failed
            // scan as Completed). The guaranteed phaseChanged(Failed) is the SOLE
            // terminal frame for a failed scan.
            emit_phase_guaranteed(&sink, SessionPhase::Failed).await;
        } else if self.coordinator.is_cancelled() {
            on_phase(SessionPhase::Cancelled);
            sink.send(IpcEvent::now(EventPayload::Progress(Wrap::new(terminal_progress(
                &session_id,
                ScanPhase::Cancelled,
                discovered_count.load(std::sync::atomic::Ordering::Relaxed) + self.carried.0,
                discovered_count.load(std::sync::atomic::Ordering::Relaxed) + self.carried.0,
                total + self.carried.0,
                failed + self.carried.1,
            )))))
            .await;
            // Terminal Cancelled phase via the guaranteed path — a drop
            // makes the app treat a cancelled scan as Completed.
            emit_phase_guaranteed(&sink, SessionPhase::Cancelled).await;
            // IPC parity with macOS (markSessionFinal emits scanComplete
            // unconditionally, pinned by ScanCancellationTests): cancelled
            // scans still get the terminal scanComplete carrying the final
            // counts. Terminal STATE stays the preceding
            // phaseChanged(cancelled) — consumers must not infer
            // "completed" from this event.
            // totalFiles = discovered (can exceed processed when the
            // cancel landed mid-tagging) — macOS emits the same split.
            self.coordinator.release_slot();
            sink.send(IpcEvent::now(EventPayload::ScanComplete(Wrap::new(
                ScanComplete {
                    session_id: session_id.clone(),
                    total_files: discovered_count.load(std::sync::atomic::Ordering::Relaxed) + self.carried.0,
                    processed_files: total + self.carried.0,
                    failed_files: failed + self.carried.1,
                    total_seconds: elapsed + self.carried.2,
                },
            ))))
            .await;
        } else {
            if !post_scan_announced {
                on_phase(SessionPhase::PostScan);
                emit_phase(SessionPhase::PostScan);
            }
            on_phase(SessionPhase::Completed);
            sink.send(IpcEvent::now(EventPayload::Progress(Wrap::new(terminal_progress(
                &session_id,
                ScanPhase::Completed,
                total + self.carried.0,
                discovered_count.load(std::sync::atomic::Ordering::Relaxed) + self.carried.0,
                total + self.carried.0,
                failed + self.carried.1,
            )))))
            .await;
            // Terminal Completed phase via the guaranteed path so the
            // app's terminal state can never be lost under backpressure.
            emit_phase_guaranteed(&sink, SessionPhase::Completed).await;
            self.coordinator.release_slot();
            sink.send(IpcEvent::now(EventPayload::ScanComplete(Wrap::new(
                ScanComplete {
                    session_id: session_id.clone(),
                    total_files: total + self.carried.0,
                    processed_files: total + self.carried.0,
                    failed_files: failed + self.carried.1,
                    total_seconds: elapsed + self.carried.2,
                },
            ))))
            .await;
        }

        Ok(SessionResult {
            processed: total + self.carried.0,
            failed: failed + self.carried.1,
            elapsed_seconds: elapsed + self.carried.2,
            needs_follow_up: false,
            timed_out: 0,
            capture_dates_current,
            progress_floor: self.progress_floor,
        })
    }
}

/// Passes that read timed-out files again after the first one. A busy machine that failed a file once often fails it again straight away,
/// so each pass waits longer (`timeout_retry_backoff`) and runs with fewer workers.
pub const TIMEOUT_RETRY_ROUNDS: u32 = 3;

/// How long to let the machine settle before retry round `round` (1-based).
pub fn timeout_retry_backoff(round: u32) -> Duration {
    Duration::from_secs(match round {
        0 | 1 => 3,
        2 => 8,
        _ => 15,
    })
}

/// Rows under `root` this scan (written at or after `since`) stored as failed because a read timed out and whose retries are not used up.
pub(crate) fn count_retryable_timeouts(conn: &Connection, root: &Path, since: f64) -> u64 {
    let lo = root_descendant_prefix(root);
    let hi = prefix_upper_bound(&lo);
    let [current, per_file, vision] = crate::pipeline::tagging::TIMEOUT_MESSAGES;
    let base = "SELECT COUNT(*) FROM files f WHERE f.failed = 1 AND f.error_message IN (?1, ?2, ?3) AND f.scanned_at >= ?4 \
                AND f.path_text >= ?5 \
                AND COALESCE((SELECT c.attempts FROM scan_failures c WHERE c.file_id = f.id), 0) < ?6";
    let limit = crate::pipeline::catch_up::FAILURE_RETRY_LIMIT;
    let count = match &hi {
        Some(hi) => conn.query_row(
            &format!("{base} AND f.path_text < ?7"),
            rusqlite::params![current, per_file, vision, since, lo, limit, hi],
            |row| row.get::<_, i64>(0),
        ),
        None => conn.query_row(base, rusqlite::params![current, per_file, vision, since, lo, limit], |row| row.get::<_, i64>(0)),
    };
    count.unwrap_or(0).max(0) as u64
}

static LATE_CAREFUL_READS_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The careful text reader was still warming up when the scan ended, so the scan finished without it. This waits for
/// it in the background (one at a time) and reads what was left owed.
fn spawn_late_careful_reads(
    db_conn: Arc<Mutex<Connection>>,
    root: std::path::PathBuf,
    analyzer: Box<dyn crate::pipeline::catch_up::Analyzer + Send>,
) {
    struct Running;
    impl Drop for Running {
        fn drop(&mut self) {
            LATE_CAREFUL_READS_RUNNING.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    if LATE_CAREFUL_READS_RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    tokio::task::spawn_blocking(move || {
        let _running = Running;
        crate::pipeline::catch_up::finish_careful_when_ready(&db_conn, Some(&root), analyzer.as_ref(), &|| false);
    });
}

/// Progress of the catch-up pass, reported in the post-scan phase the way the
/// Swift engine does: `total` grows above `discovered` by the pass's size and
/// `processed` advances by what it has done, so the counters only ever grow.
struct CatchUpProgress {
    sink: Sink,
    session_id: String,
    discovered: u64,
    processed: u64,
    failed: u64,
    lift: ProgressLift,
    /// (owed, done, started)
    state: Mutex<(u64, u64, Instant)>,
}

/// How often the post-scan phase repeats its progress while a read is slow, so the app never sees a silent minute.
const CATCH_UP_HEARTBEAT: Duration = Duration::from_secs(1);

impl CatchUpProgress {
    fn report(&self, event: crate::pipeline::catch_up::Progress) {
        let mut state = self.state.lock();
        match event {
            crate::pipeline::catch_up::Progress::Total(total) => state.0 = total.max(state.0).max(state.1),
            crate::pipeline::catch_up::Progress::Advance(count) => state.1 = (state.1 + count).min(state.0),
        }
        self.send_snapshot(&state);
    }

    fn spawn_heartbeat(self: Arc<Self>, every: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(every);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticks.tick().await;
            loop {
                ticks.tick().await;
                self.send_snapshot(&self.state.lock());
            }
        })
    }

    /// Sent while the caller holds the state lock, so a snapshot never overtakes a newer one.
    fn send_snapshot(&self, state: &(u64, u64, Instant)) {
        let (owed, done, started) = *state;
        let (total, processed) = catch_up_counters(self.discovered, self.processed, owed, done);
        let seconds = started.elapsed().as_secs_f64();
        let rate = if seconds > 0.0 { done as f64 / seconds } else { 0.0 };
        let mut progress = ScanProgress {
            session_id: self.session_id.clone(),
            phase: ScanPhase::PostScan,
            total,
            discovered: self.discovered,
            processed,
            failed: self.failed,
            files_per_second: rate,
            eta_seconds: (rate > 0.01).then(|| (owed - done) as f64 / rate),
            resident_mb: crate::platform::process_memory_mb(),
            available_mb: 0,
        };
        self.lift.apply(&mut progress);
        let _ = self.sink.try_send(IpcEvent::now(EventPayload::Progress(Wrap::new(progress))));
    }
}

/// The last progress snapshot of a scan, sent before its terminal events as the
/// Swift engine does: the terminal phase and the final counts, no rate, no ETA.
fn terminal_progress(session_id: &str, phase: ScanPhase, total: u64, discovered: u64, processed: u64, failed: u64) -> ScanProgress {
    ScanProgress {
        session_id: session_id.into(),
        phase,
        total,
        discovered,
        processed,
        failed,
        files_per_second: 0.0,
        eta_seconds: None,
        resident_mb: crate::platform::process_memory_mb(),
        available_mb: 0,
    }
}

fn catch_up_counters(discovered: u64, processed: u64, owed: u64, done: u64) -> (u64, u64) {
    (discovered.max(processed) + owed, processed + done.min(owed))
}

/// Throttle state for progress emission. The DBWriter callback fires per
/// batch (every 100 files or 200 ms), so we further throttle into the
/// IPC sink to at most ~10 Hz.
struct ProgressState {
    last_emit: Instant,
    last_total: u64,
    // Rolling throughput for the ETA, measured from the processed-count delta
    // over REAL wall-clock time between emits. This replaces the per-batch
    // `files_in_batch / flush_wall` rate, which measured only the DB-INSERT
    // speed (hundreds–thousands of files/s) and produced absurd ETAs — e.g.
    // "13s" for an hour of actual work. EMA-smoothed, mirroring the macOS
    // ScanCoordinator (0.7 old / 0.3 new).
    rate_anchor: Instant,
    rate_anchor_total: u64,
    rolling_fps: f64,
    // Split heavy (Image/Video/Model — the vision path) vs light rates for
    // the ETA. The blended rate swings an order of magnitude between photo
    // bursts (~tens/s) and hash-only stretches (hundreds/s) on mixed
    // libraries, so an ETA computed from it oscillated wildly; each class
    // rate is stable, and remaining work is estimated per class.
    rate_anchor_heavy: u64,
    rolling_heavy_fps: f64,
    rolling_light_fps: f64,
}

impl ProgressState {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            // -60 s forces the first batch callback past the throttle so the
            // sidebar fills immediately; the rate anchor below uses the real
            // `now` so the first measured interval is honest.
            last_emit: now.checked_sub(Duration::from_secs(60)).unwrap_or(now),
            last_total: 0,
            rate_anchor: now,
            rate_anchor_total: 0,
            rolling_fps: 0.0,
            rate_anchor_heavy: 0,
            rolling_heavy_fps: 0.0,
            rolling_light_fps: 0.0,
        }
    }

    /// Fold a new processed-count sample into the rolling files/sec and return
    /// it. Re-samples only after `MIN_RATE_DT` so a burst of file-triggered
    /// emits a few ms apart can't divide by a near-zero interval; between
    /// re-samples it returns the last rolling value unchanged.
    fn observe_rate(&mut self, processed_total: u64, processed_heavy: u64, now: Instant) -> f64 {
        const MIN_RATE_DT: f64 = 0.5;
        let dt = now.duration_since(self.rate_anchor).as_secs_f64();
        if dt >= MIN_RATE_DT {
            let delta = processed_total.saturating_sub(self.rate_anchor_total) as f64;
            let delta_heavy = processed_heavy.saturating_sub(self.rate_anchor_heavy) as f64;
            let ema = |rolling: f64, instant: f64| {
                if rolling <= 0.0 { instant } else { 0.7 * rolling + 0.3 * instant }
            };
            self.rolling_fps = ema(self.rolling_fps, delta / dt);
            // Only fold a class sample when that class made progress this
            // window — an all-light stretch must not decay the heavy rate
            // toward zero (that's the oscillation this split exists to fix).
            if delta_heavy > 0.0 {
                self.rolling_heavy_fps = ema(self.rolling_heavy_fps, delta_heavy / dt);
            }
            let delta_light = delta - delta_heavy;
            if delta_light > 0.0 {
                self.rolling_light_fps = ema(self.rolling_light_fps, delta_light / dt);
            }
            self.rate_anchor = now;
            self.rate_anchor_total = processed_total;
            self.rate_anchor_heavy = processed_heavy;
        }
        self.rolling_fps
    }

    /// Kind-aware ETA: seconds for the remaining heavy (vision-path) files at
    /// the heavy rate plus the remaining light files at the light rate.
    /// Falls back to the blended rate for any class whose own rate is still
    /// unknown, and to a plain blended estimate before any split data exists.
    fn eta_seconds(&self, remaining_heavy: u64, remaining_light: u64) -> Option<f64> {
        let blended = self.rolling_fps;
        if remaining_heavy + remaining_light == 0 {
            return None;
        }
        let rate_or = |own: f64| if own > 0.01 { own } else { blended };
        let (rh, rl) = (rate_or(self.rolling_heavy_fps), rate_or(self.rolling_light_fps));
        if rh <= 0.01 || rl <= 0.01 {
            return if blended > 0.01 {
                Some((remaining_heavy + remaining_light) as f64 / blended)
            } else {
                None
            };
        }
        Some(remaining_heavy as f64 / rh + remaining_light as f64 / rl)
    }
}

/// Single source for the empty/rescan/partial discovery notice: the 250 ms
/// tick and the post-drain backstop must emit byte-identical IPC, so the kind
/// codes + messages live here once.
fn changed_while_scanning_message(files: u64) -> String {
    format!(
        "{} changed while FileID was scanning {}. {} picked up on the next scan.",
        crate::util::display::count_noun(files as i64, "file", "files"),
        if files == 1 { "it" } else { "them" },
        if files == 1 { "It will be" } else { "They will be" },
    )
}

fn discovery_notice(
    count: u64,
    skip_count: usize,
    errs: u64,
    cloud_only: u64,
    root: &str,
    root_reachable: bool,
) -> Option<crate::ipc::EngineError> {
    let (kind, message) = if cloud_only > 0 {
        // The Swift engine's `Discovery.shortfallNotice`, word for word.
        let mut parts = Vec::new();
        if errs > 0 {
            parts.push(format!(
                "{errs} {} couldn't be opened (no permission, or removed during the scan)",
                if errs == 1 { "item" } else { "items" }
            ));
        }
        parts.push(format!(
            "{cloud_only} {} stored in iCloud and not downloaded to this Mac, so {} left out",
            if cloud_only == 1 { "file is" } else { "files are" },
            if cloud_only == 1 { "it was" } else { "they were" }
        ));
        ("discovery_partial", format!("{}. Everything else was scanned.", parts.join("; ")))
    } else if count == 0 && skip_count > 0 && !root_reachable {
        (
            "empty_folder",
            "This folder looks disconnected or empty, so FileID kept your library as it was. \
             Reconnect the drive and scan again."
                .to_string(),
        )
    } else if count == 0 && skip_count > 0 {
        (
            "rescan_no_changes",
            "Nothing changed in these files since the last scan. FileID is finishing a quick check of the folder.".to_string(),
        )
    } else if count == 0 {
        (
            "empty_folder",
            format!(
                "No supported files found in {}.\n\
                 Pick a folder with images, videos, PDFs, or documents.",
                root
            ),
        )
    } else if errs > 0 {
        (
            "discovery_partial",
            format!(
                "Scanned {}; {} couldn’t be read \
                 (permission denied or removed mid-scan). Scan continues.",
                crate::util::display::count_noun(count as i64, "file", "files"),
                crate::util::display::count_noun(errs as i64, "item", "items")
            ),
        )
    } else {
        return None;
    };
    Some(crate::ipc::EngineError::new(kind, message).with_path(root.to_string()))
}

fn should_emit_batch_summary(batch_index: u32, processed_total: u64, last_emitted: u64) -> bool {
    batch_index == 0 || processed_total.saturating_sub(last_emitted) >= 100
}

fn emit_batch_summary(sink: &Sink, stats: &BatchStats) {
    let summary = BatchSummary {
        batch_index: stats.batch_index,
        files_in_batch: stats.files_in_batch,
        processed_total: stats.processed_total,
        wall_seconds: stats.wall_seconds,
        files_per_second: stats.files_per_second,
        utilization: stats.utilization,
        vision_p50_ms: stats.vision_p50_ms,
        vision_p95_ms: stats.vision_p95_ms,
        clip_p50_ms: stats.clip_p50_ms,
        clip_p95_ms: stats.clip_p95_ms,
        store_insert_p50_ms: stats.store_insert_p50_ms,
        store_insert_p95_ms: stats.store_insert_p95_ms,
        resident_mb: crate::platform::process_memory_mb(),
        available_mb: 0,
    };
    // Intentional try_send. Summaries are sampled every ~100 processed files;
    // progress and terminal events carry the authoritative lifecycle. Dropping
    // a diagnostic sample is preferable to backpressuring the scan.
    let _ = sink.try_send(IpcEvent::now(EventPayload::BatchSummary(Wrap::new(summary))));
}

static CRASH_HEALED_ROOTS: std::sync::LazyLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

/// The crash self-heal runs until one scan of the root walks it to the end, then stops for this process. A build
/// that writes face crops after the scan (the Mac adapters path) never has a crop to find, so without the stop every
/// scan after a crash reprocessed every face-bearing file; a cancelled scan leaves the root due so the repair is
/// not lost.
fn crash_self_heal_due(root_prefix: &str) -> bool {
    heal_due(
        crate::db::UNCLEAN_PRIOR_SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed),
        &CRASH_HEALED_ROOTS,
        root_prefix,
    )
}

fn heal_due(unclean: bool, healed: &Mutex<std::collections::HashSet<String>>, root_prefix: &str) -> bool {
    unclean && !healed.lock().contains(root_prefix)
}

fn mark_heal_done(healed: &Mutex<std::collections::HashSet<String>>, root_prefix: &str) {
    healed.lock().insert(root_prefix.to_string());
}

/// skip-set guard: drop any content-bearing row that was scanned
/// without a content_hash (the signature of an online-only OneDrive
/// placeholder, whose content read is skipped to avoid a network hydration).
/// Because hydration doesn't bump `modified_at`, this is the only durable
/// signal that a now-local file still needs ML processing.
/// Crash self-heal (only after an unclean shutdown — see `UNCLEAN_PRIOR_SHUTDOWN`).
/// Face-crop JPEGs are written post-commit outside the writer tx, so a kill can
/// leave committed `face_prints` rows with no on-disk crop. Such a file is
/// size+mtime unchanged, so the skip set would drop it forever and the People
/// tab shows gray faces. This pulls every affected file (any face missing its
/// `<face_id>.jpg`) back OUT of `set` so the next scan reprocesses it and
/// re-emits the crops. Bounded to face-bearing files under the scan root;
/// returns how many files were reopened for reprocessing. Best-effort: a query
/// or FS error just leaves the skip set as-is (the file stays gray, no worse
/// than before).
fn reconcile_missing_face_crops(
    conn: &rusqlite::Connection,
    set: &mut std::collections::HashMap<SkipFingerprint, (i64, Option<f64>)>,
    lo: &str,
    hi: Option<&str>,
) -> usize {
    let Ok(faces_dir) = crate::paths::faces_dir() else {
        return 0;
    };
    reconcile_missing_face_crops_in(conn, set, lo, hi, &faces_dir)
}

fn reconcile_missing_face_crops_in(
    conn: &rusqlite::Connection,
    set: &mut std::collections::HashMap<SkipFingerprint, (i64, Option<f64>)>,
    lo: &str,
    hi: Option<&str>,
    faces_dir: &std::path::Path,
) -> usize {
    let sql = if hi.is_some() {
        "SELECT f.path_text, fp.id FROM face_prints fp \
         JOIN files f ON f.id = fp.file_id \
         WHERE f.path_text >= ?1 AND f.path_text < ?2"
    } else {
        "SELECT f.path_text, fp.id FROM face_prints fp \
         JOIN files f ON f.id = fp.file_id"
    };
    let Ok(mut stmt) = conn.prepare(sql) else {
        return 0;
    };
    let row = |r: &rusqlite::Row| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?));
    let rows = match hi {
        Some(hi) => stmt.query_map(rusqlite::params![lo, hi], row),
        None => stmt.query_map([], row),
    };
    let Ok(rows) = rows else { return 0 };
    let mut missing: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in rows.flatten() {
        let (path, face_id) = entry;
        if missing.contains(&path) {
            continue;
        }
        if !faces_dir.join(format!("{face_id}.jpg")).is_file() {
            missing.insert(path);
        }
    }
    let mut removed = 0;
    for path in &missing {
        if set.remove(&path_fingerprint_text(path)).is_some() {
            removed += 1;
        }
    }
    removed
}

/// Whether a row stamped with a pre-SHA-256 content hash can still be in the
/// catalog, checked once per scan. The rename heal probes the frozen BLAKE3
/// recipes only for such rows, and computing them costs a second hash of every
/// image. Rows carry no recipe marker, so the state folder records when this
/// engine (which stamps SHA-256, as the Swift engine does) first scanned: any
/// hash on a row scanned before then may be a legacy one. Unknown means yes.
fn legacy_hash_rows_possible(conn: &Connection, state_dir: Option<&Path>) -> bool {
    let Some(marker) = state_dir.map(|dir| dir.join("sha256-content-hash-since")) else {
        return true;
    };
    let since = match std::fs::read_to_string(&marker).ok().and_then(|text| text.trim().parse::<f64>().ok()) {
        Some(since) => since,
        None => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let written = marker
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&marker, format!("{now}")));
            if written.is_err() || now <= 0.0 {
                return true;
            }
            now
        }
    };
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE scanned_at < ?1 AND content_hash IS NOT NULL)",
        [since],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(true)
}

/// Adds the failed rows that used up their retries to the skip set; discovery
/// then skips each one only while its size and date still match.
fn add_given_up_failures(
    conn: &Connection,
    set: &mut std::collections::HashMap<SkipFingerprint, (i64, Option<f64>)>,
    lo: &str,
    hi: Option<&str>,
) -> usize {
    let base = crate::pipeline::catch_up::GIVEN_UP_SQL;
    let context = crate::pipeline::catch_up::failure_context();
    let sql = if hi.is_some() { format!("{base} AND f.path_text >= ?2 AND f.path_text < ?3") } else { base.to_string() };
    let row = |r: &rusqlite::Row| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<f64>>(2)?));
    let Ok(mut stmt) = conn.prepare(&sql) else { return 0 };
    let rows = match hi {
        Some(hi) => stmt.query_map(rusqlite::params![context, lo, hi], row),
        None => stmt.query_map(rusqlite::params![context], row),
    };
    let mut added = 0;
    if let Ok(rows) = rows {
        for (path, size, modified) in rows.flatten() {
            set.insert(path_fingerprint_text(&path), (size, modified));
            added += 1;
        }
    }
    added
}

/// Rows under `root` that were catalogued as cloud placeholders, keyed like the
/// skip set.
fn known_placeholders(
    conn: &Connection,
    root: &Path,
) -> std::collections::HashMap<SkipFingerprint, (i64, Option<f64>)> {
    let lo = root_descendant_prefix(root);
    let hi = prefix_upper_bound(&lo);
    let base = crate::pipeline::discovery::KNOWN_PLACEHOLDER_SQL;
    let sql = if hi.is_some() { format!("{base} AND path_text >= ?1 AND path_text < ?2") } else { base.to_string() };
    let row = |r: &rusqlite::Row| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<f64>>(2)?));
    let mut set = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare(&sql) {
        let rows = match &hi {
            Some(hi) => stmt.query_map(rusqlite::params![lo, hi], row),
            None => stmt.query_map([], row),
        };
        if let Ok(rows) = rows {
            for (path, size, modified) in rows.flatten() {
                set.insert(path_fingerprint_text(&path), (size, modified));
            }
        }
    }
    set
}

pub(crate) const SKIP_SET_CONTENT_HASH_GATE: &str =
    "AND NOT (content_hash IS NULL \
      AND kind IN ('image', 'video', 'pdf', 'doc', 'audio'))";

/// Lockstep with macOS `DBWriter.skipSetTextBackfillExclusionSQL`: keep any
/// doc/pdf that still LACKS a `text_embeddings` row OUT of the skip set, so an
/// install-then-rescan re-emits it and the tagger backfills its BGE embedding. BGE is
/// opt-in, so the first scan usually predates it — without this, those docs would be
/// stranded on the weaker filename-bag-of-words clustering forever (the doc-content
/// pass has no plan-time fallback). Appended ONLY when BGE is on disk (see
/// `bge_installed`); leading space because it concatenates after the content gate.
/// The `text_stage_done = 0` clause stops the re-walk once a doc has been text-extracted
/// but yielded no embeddable text (image-only PDF, iWork, empty file) — otherwise such a
/// doc, never able to get a `text_embeddings` row, would re-walk forever.
pub(crate) const SKIP_SET_TEXT_EMBED_GATE: &str =
    " AND NOT (kind IN ('doc', 'pdf') AND text_stage_done = 0 \
      AND NOT EXISTS (SELECT 1 FROM text_embeddings \
                      WHERE text_embeddings.file_id = files.id))";

/// Lockstep with macOS `DBWriter.skipSetModelClipBackfillExclusionSQL`: keep a `.obj`
/// 3D model still LACKING a `clip_embeddings` row OUT of the skip set so its rendered-shape
/// CLIP vector backfills on a rescan after the render→CLIP feature shipped. Limited to
/// `extension = 'obj'` (the only format `obj_render` rasterizes). The `text_stage_done = 0`
/// clause stops the re-walk once a render has been ATTEMPTED but failed (a corrupt /
/// geometry-less .obj) — otherwise that .obj would re-walk forever. CLIP ships by default,
/// so this is appended unconditionally; leading space concatenates after the content gate.
pub(crate) const SKIP_SET_MODEL_CLIP_GATE: &str =
    " AND NOT (kind = 'model' AND extension = 'obj' AND text_stage_done = 0 \
      AND NOT EXISTS (SELECT 1 FROM clip_embeddings \
                      WHERE clip_embeddings.file_id = files.id))";

/// macOS `DBWriter.skipSetClipBackfillExclusionSQL`: keep an image that still
/// LACKS a `clip_embeddings` row OUT of the skip set while CLIP is installed, so a
/// scan run before the model was installed is backfilled by the next rescan.
pub(crate) const SKIP_SET_IMAGE_CLIP_GATE: &str =
    " AND NOT (kind = 'image' AND NOT EXISTS (SELECT 1 FROM clip_embeddings \
                                            WHERE clip_embeddings.file_id = files.id))";

/// A macOS video catalogued while FileID's FFmpeg runtime was absent has no
/// frame and no CLIP vector; keep it out of the skip set once frames are
/// available so a rescan backfills it. Appended only on macOS (see `video_clip_gate`).
pub(crate) const SKIP_SET_VIDEO_CLIP_GATE: &str =
    " AND NOT (kind = 'video' AND NOT EXISTS (SELECT 1 FROM clip_embeddings \
                                            WHERE clip_embeddings.file_id = files.id))";

fn video_clip_gate() -> &'static str {
    if cfg!(all(not(windows), not(target_os = "linux")))
        && clip_can_embed()
        && crate::pipeline::tagging::video_frames_available()
    {
        SKIP_SET_VIDEO_CLIP_GATE
    } else {
        ""
    }
}

/// Whether this scan can give a picture its vector: the CLIP weights are installed.
fn clip_can_embed() -> bool {
    clip_installed()
}

fn clip_installed() -> bool {
    crate::models::mobileclip::default_weights_path().is_ok_and(|p| p.exists())
}

/// True once the BGE document embedder's weights are on disk — the same probe the
/// tagger uses to decide whether to build the embedder (tagging.rs). Gates
/// `SKIP_SET_TEXT_EMBED_GATE`: with no model installed no doc could embed, so forcing
/// embeddingless docs out of the skip set would re-walk every doc on every scan.
fn bge_installed() -> bool {
    crate::models::bge_text::default_weights_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// The gate applies only when a scan can actually embed a document: the weights are on disk.
fn text_embed_gate(installed: bool) -> &'static str {
    if installed {
        SKIP_SET_TEXT_EMBED_GATE
    } else {
        ""
    }
}

/// `scan_sessions.last_file_index` is the Swift engine's resume cursor: files written so far.
/// The Mac app shows it as "N files" under Settings ▸ Recent scans.
fn record_scan_cursor(conn: &Connection, session_id: &str, files_written: u64) {
    let _ = conn.execute(
        "UPDATE scan_sessions SET last_file_index = ?1 WHERE id = ?2",
        rusqlite::params![files_written as i64, session_id],
    );
}

/// The end of a session fills `total_files` (read by the Windows and Linux apps) and
/// `last_file_index` (read by the Mac app) with the same count.
fn stamp_scan_session_end(conn: &Connection, session_id: &str, completed_unix: f64, files_written: u64, status: &str) {
    let _ = conn.execute(
        "UPDATE scan_sessions SET completed_at = ?1, total_files = ?2, last_file_index = ?2, status = ?3 WHERE id = ?4",
        rusqlite::params![completed_unix, files_written as i64, status, session_id],
    );
}

fn should_reconcile_missing_rows(
    cancelled: bool,
    gpu_dead: bool,
    discovery_done: bool,
    discovery_errors: u64,
    test_file_cap: u64,
) -> bool {
    !cancelled && !gpu_dead && discovery_done && discovery_errors == 0 && test_file_cap == 0
}

/// False when the scan root cannot be trusted to reflect the library: missing, not a
/// directory, empty or unreadable, or (macOS) a leftover `/Volumes/<name>` folder on the
/// boot volume after the drive was unplugged. Port of the Swift `ScanRootGuard.isReachable`.
pub(crate) fn scan_root_reachable(root: &Path) -> bool {
    root_is_trustworthy(root, &device_of)
}

#[cfg(target_os = "macos")]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|meta| meta.dev())
}

#[cfg(not(target_os = "macos"))]
fn device_of(_path: &Path) -> Option<u64> {
    None
}

/// `device_of` is the seam for the mount-point check (see [`is_unmounted_volume_stub`]).
fn root_is_trustworthy(root: &Path, device_of: &dyn Fn(&Path) -> Option<u64>) -> bool {
    if !root.is_dir() {
        return false;
    }
    if !std::fs::read_dir(root).is_ok_and(|mut entries| entries.next().is_some()) {
        return false;
    }
    !(cfg!(target_os = "macos") && is_unmounted_volume_stub(root, device_of))
}

/// A folder under `/Volumes` that sits on the same device as `/Volumes` itself is not a
/// mounted drive: macOS leaves it behind when a drive is unplugged. A symlink there (the
/// boot volume's own entry) is not a stub.
fn is_unmounted_volume_stub(root: &Path, device_of: &dyn Fn(&Path) -> Option<u64>) -> bool {
    let volumes = Path::new("/Volumes");
    if root == volumes || !root.starts_with(volumes) {
        return false;
    }
    if std::fs::canonicalize(root).is_ok_and(|resolved| resolved != root) {
        return false;
    }
    matches!((device_of(root), device_of(volumes)), (Some(here), Some(there)) if here == there)
}

pub(crate) fn root_descendant_prefix(root: &Path) -> String {
    let mut prefix = root.to_string_lossy().trim_end_matches(['\\', '/']).to_string();
    prefix.push(std::path::MAIN_SEPARATOR);
    prefix
}

fn soft_hide_missing_rows(
    conn: &mut Connection,
    root: &Path,
    seen: &mut Vec<i64>,
) -> rusqlite::Result<usize> {
    seen.sort_unstable();
    seen.dedup();
    let lo = root_descendant_prefix(root);
    let Some(hi) = prefix_upper_bound(&lo) else { return Ok(0) };
    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS fileid_seen_path_hashes (
             path_hash INTEGER PRIMARY KEY
         ) WITHOUT ROWID;
         DELETE FROM fileid_seen_path_hashes;",
    )?;
    {
        let mut insert = tx.prepare_cached(
            "INSERT OR IGNORE INTO fileid_seen_path_hashes (path_hash) VALUES (?1)",
        )?;
        for path_hash in seen.iter() {
            insert.execute([path_hash])?;
        }
    }
    let marked = tx.execute(
        "UPDATE files
         SET failed = 1,
             error_message = 'File is no longer present under the completed scan root.',
             scanned_at = ?3
         WHERE path_text >= ?1 AND path_text < ?2
           AND failed = 0
           AND NOT EXISTS (
               SELECT 1 FROM fileid_seen_path_hashes seen
               WHERE seen.path_hash = files.path_hash
           )",
        rusqlite::params![lo, hi, unix_now()],
    )?;
    tx.execute("DELETE FROM fileid_seen_path_hashes", [])?;
    tx.commit()?;
    Ok(marked)
}

/// Days a row hidden by [`soft_hide_missing_rows`] is kept before a later clean
/// scan of its location removes it. A file that comes back before then (same path,
/// or found again by rename-heal) re-binds to its row and keeps its tags and people.
/// A rescan does not touch the `scanned_at` of an unchanged file, so it means "last
/// analysed", not "last seen"; hiding therefore stamps `scanned_at` with the hide time
/// (the file was there at the scan before), and that is what the retention counts from.
/// Rows hidden by builds before this one keep their old `scanned_at` and are removed by
/// the first clean scan of their location after the upgrade.
/// `FILEID_HIDDEN_RETENTION_DAYS` overrides it (tests and the soak; 0 removes at the
/// end of the scan that hid the row, as the Swift engine does).
pub(crate) const HIDDEN_ROW_RETENTION_DAYS: f64 = 30.0;
const HIDDEN_PURGE_BATCH: usize = 200;
pub(crate) const HIDDEN_ROW_MESSAGE: &str = "File is no longer present under the completed scan root.";

pub(crate) fn hidden_retention_days() -> f64 {
    parse_retention_days(std::env::var("FILEID_HIDDEN_RETENTION_DAYS").ok().as_deref())
}

fn parse_retention_days(value: Option<&str>) -> f64 {
    value
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d >= 0.0)
        .unwrap_or(HIDDEN_ROW_RETENTION_DAYS)
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct HiddenPurge {
    pub removed: u64,
    pub batches: u32,
    pub skipped_for_clock: bool,
}

/// Remove rows under `root` hidden as "no longer present" whose file was last seen
/// (`scanned_at`) more than `retention_days` before `now`. Call only after a walk of
/// `root` that was complete and clean. Nothing is removed when any row under `root`
/// was last seen in the future (a wrong clock). Batches of `HIDDEN_PURGE_BATCH`
/// rows, each in its own transaction with the writer lock released between them.
/// Tags, text, embeddings, faces and the other child rows go through the
/// foreign-key cascades (as in `purge_excluded_rows` and Trash); face crop files are
/// deleted after the commit and each affected person's count and representative are
/// recomputed in the same transaction, as the Swift engine's missing-file sweep does.
pub(crate) fn purge_expired_hidden_rows(
    db: &Mutex<Connection>,
    root: &Path,
    now: f64,
    retention_days: f64,
    cancelled: &dyn Fn() -> bool,
) -> HiddenPurge {
    let mut out = HiddenPurge::default();
    let lo = root_descendant_prefix(root);
    let Some(hi) = prefix_upper_bound(&lo) else { return out };
    let cutoff = now - retention_days * 86_400.0;
    {
        let conn = db.lock();
        let future: rusqlite::Result<bool> = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE path_text >= ?1 AND path_text < ?2 AND scanned_at > ?3)",
            rusqlite::params![lo, hi, now + 300.0],
            |r| r.get(0),
        );
        match future {
            Ok(false) => {}
            Ok(true) => {
                out.skipped_for_clock = true;
                return out;
            }
            Err(err) => {
                tracing::warn!(?err, "hidden-row retention check failed; keeping rows");
                return out;
            }
        }
    }
    while !cancelled() {
        let mut conn = db.lock();
        let batch = (|| -> rusqlite::Result<usize> {
            let ids: Vec<i64> = conn
                .prepare_cached(
                    "SELECT id FROM files WHERE path_text >= ?1 AND path_text < ?2 \
                     AND failed = 1 AND error_message = ?3 AND scanned_at <= ?4 ORDER BY id LIMIT ?5",
                )?
                .query_map(rusqlite::params![lo, hi, HIDDEN_ROW_MESSAGE, cutoff, HIDDEN_PURGE_BATCH as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            if ids.is_empty() {
                return Ok(0);
            }
            let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            let tx = conn.transaction()?;
            let persons: Vec<i64> = tx
                .prepare(&format!("SELECT DISTINCT person_id FROM face_prints WHERE person_id IS NOT NULL AND file_id IN ({list})"))?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let crops: Vec<i64> = tx
                .prepare(&format!("SELECT id FROM face_prints WHERE file_id IN ({list})"))?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            tx.execute(&format!("DELETE FROM files WHERE id IN ({list}) AND failed = 1 AND error_message = ?1"), [HIDDEN_ROW_MESSAGE])?;
            for pid in persons {
                tx.execute(
                    "UPDATE persons SET file_count = (SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id = ?1 AND COALESCE(excluded,0)=0) WHERE id = ?1",
                    [pid],
                )?;
                tx.execute(
                    "UPDATE persons SET representative_face_id = (SELECT id FROM face_prints WHERE person_id = ?1 ORDER BY id LIMIT 1) \
                     WHERE id = ?1 AND (representative_face_id IS NULL OR representative_face_id NOT IN (SELECT id FROM face_prints WHERE person_id = ?1))",
                    [pid],
                )?;
            }
            tx.commit()?;
            for id in crops {
                crate::pipeline::dbwriter::remove_face_crop(id);
            }
            Ok(ids.len())
        })();
        drop(conn);
        match batch {
            Ok(0) => break,
            Ok(n) => {
                out.removed += n as u64;
                out.batches += 1;
            }
            Err(err) => {
                tracing::warn!(?err, "hidden-row retention batch failed; keeping the rest");
                break;
            }
        }
    }
    out
}

/// Delete cataloged rows under each excluded folder. Child rows
/// (tags / face prints / captions / embeddings) go via ON DELETE CASCADE
/// (`PRAGMA foreign_keys = ON` on every engine connection); face-crop JPEGs
/// are pruned best-effort after each delete. Range-matches `path_text` with
/// the exclusion's original casing — same walk-derived casing contract as
/// the skip-set preload above; a mismatch fails safe (rows survive until a
/// future exact-cased scan). Returns the number of `files` rows purged.
pub(crate) fn purge_excluded_rows(
    conn: &Connection,
    exclusions: &[crate::util::path_safety::ResolvedExclusion],
) -> u64 {
    let mut purged = 0u64;
    for excl in exclusions {
        let lo = format!("{}{}", excl.original, std::path::MAIN_SEPARATOR);
        let Some(hi) = prefix_upper_bound(&lo) else { continue };
        let crop_ids: Vec<i64> = conn
            .prepare(
                "SELECT id FROM face_prints WHERE file_id IN \
                 (SELECT id FROM files WHERE path_text >= ?1 AND path_text < ?2)",
            )
            .and_then(|mut st| {
                st.query_map(rusqlite::params![lo, hi], |r| r.get(0))
                    .map(|rows| rows.filter_map(Result::ok).collect())
            })
            .unwrap_or_default();
        let persons: Vec<i64> = conn
            .prepare(
                "SELECT DISTINCT person_id FROM face_prints WHERE person_id IS NOT NULL AND file_id IN \
                 (SELECT id FROM files WHERE path_text >= ?1 AND path_text < ?2)",
            )
            .and_then(|mut st| {
                st.query_map(rusqlite::params![lo, hi], |r| r.get(0))
                    .map(|rows| rows.filter_map(Result::ok).collect())
            })
            .unwrap_or_default();
        match conn.execute(
            "DELETE FROM files WHERE path_text >= ?1 AND path_text < ?2",
            rusqlite::params![lo, hi],
        ) {
            Ok(n) => {
                purged += n as u64;
                if let Err(err) = crate::db::person_counts::reconcile(conn, &persons) {
                    tracing::warn!(?err, "person recount after the excluded-rows purge failed");
                }
                for id in crop_ids {
                    crate::pipeline::dbwriter::remove_face_crop(id);
                }
            }
            Err(err) => tracing::warn!(?err, "excluded-rows purge failed"),
        }
    }
    purged
}

/// Exclusive upper bound for a sargable prefix range: `prefix` with its last
/// Unicode scalar incremented, so `path_text >= prefix AND path_text < upper`
/// selects exactly the rows `LIKE 'prefix%'` would (BINARY collation). Returns
/// None when no finite bound exists.
pub(crate) fn prefix_upper_bound(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(last) = chars.pop() {
        let mut cp = last as u32 + 1;
        // Jump the UTF-16 surrogate gap — those code points aren't scalars.
        if (0xD800..=0xDFFF).contains(&cp) {
            cp = 0xE000;
        }
        if let Some(next) = char::from_u32(cp) {
            chars.push(next);
            return Some(chars.iter().collect());
        }
        // cp > 0x10FFFF (last was char::MAX) — carry into the previous char.
    }
    None
}

const PROGRESS_THROTTLE_MS: u128 = 100;
const PROGRESS_THROTTLE_FILES: u64 = 1000;

fn maybe_emit_progress(
    sink: &Sink,
    state: &Arc<Mutex<ProgressState>>,
    session_id: &str,
    stats: &BatchStats,
    discovered_total: u64,
    discovered_heavy: u64,
    discovery_done: bool,
    lift: &ProgressLift,
) {
    let now = Instant::now();
    let should_emit = {
        let st = state.lock();
        let elapsed_ms = now.duration_since(st.last_emit).as_millis();
        let files_since = stats.processed_total.saturating_sub(st.last_total);
        elapsed_ms >= PROGRESS_THROTTLE_MS || files_since >= PROGRESS_THROTTLE_FILES
    };
    if !should_emit {
        return;
    }
    let (fps, eta_kind_aware) = {
        let mut st = state.lock();
        st.last_emit = now;
        st.last_total = stats.processed_total;
        let fps = st.observe_rate(stats.processed_total, stats.processed_heavy, now);
        let eta = if discovery_done {
            let total_now = discovered_total.max(stats.processed_total);
            let remaining = total_now.saturating_sub(stats.processed_total);
            let remaining_heavy = discovered_heavy.saturating_sub(stats.processed_heavy);
            let remaining_light = remaining.saturating_sub(remaining_heavy);
            st.eta_seconds(remaining_heavy, remaining_light)
        } else {
            None
        };
        (fps, eta)
    };
    // Progress payload fields:
    //   total            = 0 until the discovery walk completes (the IPC
    //                      schema contract: "0 until discovery completes"),
    //                      then the final discovered count. Emitting the
    //                      still-climbing counter as `total` made both the
    //                      progress-bar denominator and the ETA moving
    //                      targets — the overnight "17.0h → 16.4h after 10
    //                      hours" symptom.
    //   files_per_second = the ROLLING wall-clock rate (real end-to-end
    //                      throughput), NOT the per-batch DB-flush rate.
    //   eta_seconds      = (total - processed) / rolling_fps when known;
    //                      None during ramp-up or while total is unknown.
    //   failed           = cumulative failed-file count from DBWriter.
    //   resident_mb      = process RSS via Win32 GetProcessMemoryInfo.
    let total = if discovery_done {
        discovered_total.max(stats.processed_total)
    } else {
        0
    };
    let eta_seconds = if total > 0 { eta_kind_aware } else { None };
    let mut progress = ScanProgress {
        session_id: session_id.into(),
        phase: ScanPhase::Tagging,
        total,
        discovered: discovered_total,
        processed: stats.processed_total,
        failed: stats.failed_total,
        files_per_second: fps,
        eta_seconds,
        resident_mb: crate::platform::process_memory_mb(),
        available_mb: 0,
    };
    lift.apply(&mut progress);
    // Intentional try_send. Progress events are throttled to 10 Hz / 1k
    // files, so dropping is rare in practice — and the next emit (≤100ms
    // later) brings the UI back in sync.
    let _ = sink.try_send(IpcEvent::now(EventPayload::Progress(Wrap::new(progress))));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_row_carries_the_file_count_in_both_columns() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute("INSERT INTO scan_sessions (id, root_path, started_at, status) VALUES ('s', '/lib', 1.0, 'running')", [])
            .unwrap();
        let read = || -> (Option<i64>, Option<i64>, String) {
            conn.query_row("SELECT last_file_index, total_files, status FROM scan_sessions WHERE id = 's'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
        };
        assert_eq!(read(), (None, None, "running".into()));
        record_scan_cursor(&conn, "s", 100);
        assert_eq!(read(), (Some(100), None, "running".into()), "a scan cut short still shows how far it got");
        stamp_scan_session_end(&conn, "s", 2.0, 137, "completed");
        assert_eq!(read(), (Some(137), Some(137), "completed".into()));
    }

    #[test]
    fn a_scan_root_must_exist_be_a_folder_and_list_something() {
        let dir = std::env::temp_dir().join(format!("fileid-root-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let no_device = |_: &Path| None;
        assert!(!root_is_trustworthy(&dir.join("missing"), &no_device), "missing");
        let file = dir.join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(!root_is_trustworthy(&file, &no_device), "a file");
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(!root_is_trustworthy(&empty, &no_device), "empty");
        assert!(root_is_trustworthy(&dir, &no_device), "a folder with entries");
        assert!(scan_root_reachable(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_leftover_volumes_folder_on_the_boot_device_is_a_stub() {
        let same = |_: &Path| Some(7);
        let mounted = |path: &Path| Some(if path == Path::new("/Volumes") { 7 } else { 9 });
        let root = Path::new("/Volumes/Stub/Photos");
        assert!(is_unmounted_volume_stub(root, &same));
        assert!(!is_unmounted_volume_stub(root, &mounted), "a mounted drive has its own device");
        assert!(!is_unmounted_volume_stub(Path::new("/Users/me/Photos"), &same), "only /Volumes");
        assert!(!is_unmounted_volume_stub(Path::new("/Volumes"), &same));
        assert!(!is_unmounted_volume_stub(root, &|_: &Path| None), "no device: do not guess");
    }

    #[test]
    fn an_unreachable_root_keeps_the_notice_for_a_disconnected_drive() {
        let notice = discovery_notice(0, 40, 0, 0, "/lib", false).unwrap();
        assert_eq!(notice.kind, "empty_folder");
        assert!(notice.message.contains("disconnected"));
        assert_eq!(discovery_notice(0, 0, 0, 0, "/lib", false).unwrap().kind, "empty_folder");
    }

    #[test]
    fn counts_in_scan_notices_read_singular_at_one() {
        assert_eq!(
            changed_while_scanning_message(1),
            "1 file changed while FileID was scanning it. It will be picked up on the next scan."
        );
        assert_eq!(
            changed_while_scanning_message(2),
            "2 files changed while FileID was scanning them. They will be picked up on the next scan."
        );
        let partial = discovery_notice(1, 0, 1, 0, "/lib", true).unwrap();
        assert_eq!(partial.kind, "discovery_partial");
        assert!(partial.message.starts_with("Scanned 1 file; 1 item couldn’t be read"), "{}", partial.message);
        let partial = discovery_notice(1200, 0, 3, 0, "/lib", true).unwrap();
        assert!(partial.message.starts_with("Scanned 1,200 files; 3 items couldn’t be read"), "{}", partial.message);
    }

    #[test]
    fn cloud_only_files_are_reported_in_the_swift_engines_words() {
        let one = discovery_notice(12, 0, 0, 1, "/lib", true).unwrap();
        assert_eq!(one.kind, "discovery_partial");
        assert_eq!(
            one.message,
            "1 file is stored in iCloud and not downloaded to this Mac, so it was left out. Everything else was scanned."
        );
        let many = discovery_notice(0, 40, 2, 3, "/lib", true).unwrap();
        assert_eq!(many.kind, "discovery_partial");
        assert_eq!(
            many.message,
            "2 items couldn't be opened (no permission, or removed during the scan); 3 files are stored in iCloud and not downloaded to this Mac, so they were left out. Everything else was scanned."
        );
        assert_eq!(discovery_notice(0, 40, 0, 0, "/lib", true).unwrap().kind, "rescan_no_changes");
        assert!(discovery_notice(5, 0, 0, 0, "/lib", true).is_none());
    }

    #[test]
    fn files_that_keep_failing_join_the_skip_set_with_their_stored_size_and_date() {
        let conn = crate::db::test_support::memory_db();
        let sep = std::path::MAIN_SEPARATOR;
        let path = |name: &str| format!("{sep}lib{sep}{name}");
        let context = crate::pipeline::catch_up::failure_context();
        let add = |id: i64, name: &str, failed: i64, owes: bool, attempts: i64, build: &str| {
            crate::db::test_support::insert_file(&conn, id, &path(name), "image");
            conn.execute("UPDATE files SET failed = ?2 WHERE id = ?1", rusqlite::params![id, failed]).unwrap();
            let sql = if owes {
                "INSERT INTO scan_catchup (file_id, stages, size_bytes, modified_at, vision, face_count, attempts, created_at) VALUES (?1, 1, 100, 10, ?3, 0, ?2, 0)"
            } else {
                "INSERT INTO scan_failures (file_id, size_bytes, modified_at, engine_build, attempts, failed_at) VALUES (?1, 100, 10, ?3, ?2, 0)"
            };
            conn.execute(sql, rusqlite::params![id, attempts, build]).unwrap();
        };
        add(1, "given-up.jpg", 1, false, 3, context);
        add(2, "two-tries.jpg", 1, false, 2, context);
        add(3, "other-build.jpg", 1, false, 5, "fileid-engine 0.0.0 portable");
        add(4, "owes-tags.jpg", 0, true, 3, "{}");
        add(5, "fine-now.jpg", 0, false, 3, context);
        crate::db::test_support::insert_file(&conn, 6, &format!("{sep}elsewhere{sep}given-up.jpg"), "image");
        conn.execute("UPDATE files SET failed = 1 WHERE id = 6", []).unwrap();
        conn.execute(
            "INSERT INTO scan_failures (file_id, size_bytes, modified_at, engine_build, attempts, failed_at) VALUES (6, 100, 10, ?1, 3, 0)",
            [context],
        )
        .unwrap();

        let lo = root_descendant_prefix(Path::new(&format!("{sep}lib")));
        let hi = prefix_upper_bound(&lo);
        let mut set = std::collections::HashMap::new();
        assert_eq!(add_given_up_failures(&conn, &mut set, &lo, hi.as_deref()), 1);
        assert_eq!(set.get(&path_fingerprint_text(&path("given-up.jpg"))), Some(&(100, Some(10.0))));

        let mut everywhere = std::collections::HashMap::new();
        assert_eq!(add_given_up_failures(&conn, &mut everywhere, "", None), 2);
    }

    #[test]
    fn only_this_scans_timed_out_files_with_retries_left_are_read_again() {
        let conn = crate::db::test_support::memory_db();
        let sep = std::path::MAIN_SEPARATOR;
        let path = |name: &str| format!("{sep}lib{sep}{name}");
        let [current, per_file, vision] = crate::pipeline::tagging::TIMEOUT_MESSAGES;
        let add = |id: i64, name: &str, message: &str, scanned_at: f64, attempts: Option<i64>| {
            crate::db::test_support::insert_file(&conn, id, &path(name), "image");
            conn.execute(
                "UPDATE files SET failed = 1, error_message = ?2, scanned_at = ?3 WHERE id = ?1",
                rusqlite::params![id, message, scanned_at],
            )
            .unwrap();
            if let Some(attempts) = attempts {
                conn.execute(
                    "INSERT INTO scan_failures (file_id, size_bytes, modified_at, engine_build, attempts, failed_at) VALUES (?1, 100, 10, 'build', ?2, 0)",
                    rusqlite::params![id, attempts],
                )
                .unwrap();
            }
        };
        add(1, "vision.jpg", vision, 500.0, Some(1));
        add(2, "per-file.jpg", per_file, 500.0, Some(2));
        add(3, "transient.jpg", vision, 500.0, None);
        add(4, "given-up.jpg", vision, 500.0, Some(3));
        add(5, "earlier-scan.jpg", vision, 100.0, Some(1));
        add(6, "corrupt.jpg", "Could not decode image", 500.0, Some(1));
        add(9, "ran-out-of-time.jpg", current, 500.0, Some(1));
        crate::db::test_support::insert_file(&conn, 7, &path("fine.jpg"), "image");
        crate::db::test_support::insert_file(&conn, 8, &format!("{sep}elsewhere{sep}slow.jpg"), "image");
        conn.execute("UPDATE files SET failed = 1, error_message = ?1, scanned_at = 500.0 WHERE id = 8", [vision]).unwrap();
        assert_eq!(count_retryable_timeouts(&conn, Path::new(&format!("{sep}lib")), 400.0), 4);
    }

    fn tagging_snapshot(total: u64, discovered: u64, processed: u64, failed: u64) -> ScanProgress {
        ScanProgress {
            session_id: "s".into(),
            phase: ScanPhase::Tagging,
            total,
            discovered,
            processed,
            failed,
            files_per_second: 0.0,
            eta_seconds: None,
            resident_mb: 0,
            available_mb: 0,
        }
    }

    #[test]
    fn the_first_pass_shows_its_own_counts() {
        let mut snapshot = tagging_snapshot(4, 4, 3, 1);
        ProgressLift::default().apply(&mut snapshot);
        assert_eq!((snapshot.total, snapshot.discovered, snapshot.processed, snapshot.failed), (4, 4, 3, 1));
    }

    #[tokio::test]
    async fn a_timed_out_retry_round_continues_the_scans_counts_instead_of_restarting_them() {
        let first = SessionResult {
            processed: 4,
            failed: 2,
            elapsed_seconds: 1.0,
            needs_follow_up: true,
            timed_out: 2,
            capture_dates_current: true,
            progress_floor: ProgressFloor { total: 4, discovered: 4, processed: 4 },
        };
        let (sink, _writer) = crate::ipc::sink::Sink::spawn();
        let session = ScanSession::new(
            ScanCoordinator::new(),
            Arc::new(Mutex::new(crate::db::test_support::memory_db())),
            8,
            sink,
            ModelSource::lazy(|| unreachable!("no model is needed to plan a follow-up")),
            false,
            Vec::new(),
        )
        .follow_up(&first, 1);
        let lift = session.progress_lift();
        let mut last = (4, 4, 4);
        for (total, discovered, processed) in [(0, 0, 0), (0, 2, 0), (2, 2, 0), (2, 2, 1), (2, 2, 2)] {
            let mut snapshot = tagging_snapshot(total, discovered, processed, 0);
            lift.apply(&mut snapshot);
            assert!(snapshot.discovered >= last.0 && snapshot.processed >= last.2, "{snapshot:?} fell below {last:?}");
            assert!(snapshot.total == 0 || snapshot.total >= last.1, "{snapshot:?} fell below {last:?}");
            if total == 0 {
                assert_eq!(snapshot.total, 4, "the total shown before stays shown while the pass walks the folder");
            }
            last = (snapshot.discovered, snapshot.total, snapshot.processed);
        }
        assert_eq!(last, (4, 4, 4), "two files read again finish the four of the scan");
    }

    #[test]
    fn a_later_round_with_more_new_files_raises_the_total() {
        let lift = ProgressLift { carried_processed: 2, carried_failed: 0, floor: ProgressFloor { total: 4, discovered: 4, processed: 4 } };
        let mut snapshot = tagging_snapshot(5, 5, 5, 0);
        lift.apply(&mut snapshot);
        assert_eq!((snapshot.total, snapshot.discovered, snapshot.processed), (7, 7, 7));
    }

    #[test]
    fn each_timed_out_retry_round_waits_longer_and_there_are_three() {
        let waits: Vec<_> = (1..=TIMEOUT_RETRY_ROUNDS).map(timeout_retry_backoff).collect();
        assert_eq!(TIMEOUT_RETRY_ROUNDS, 3);
        assert!(waits.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[tokio::test]
    async fn the_follow_up_replaces_the_timed_out_files_counts_and_uses_fewer_workers() {
        let first = SessionResult {
            processed: 40,
            failed: 14,
            elapsed_seconds: 30.0,
            needs_follow_up: true,
            timed_out: 12,
            capture_dates_current: true,
            progress_floor: ProgressFloor::default(),
        };
        let (sink, _writer) = crate::ipc::sink::Sink::spawn();
        let session = ScanSession::new(
            ScanCoordinator::new(),
            Arc::new(Mutex::new(crate::db::test_support::memory_db())),
            8,
            sink,
            ModelSource::lazy(|| unreachable!("no model is needed to plan a follow-up")),
            true,
            Vec::new(),
        )
        .follow_up(&first, 2);
        assert_eq!((session.carried.0, session.carried.1), (28, 2));
        assert_eq!(session.worker_count, 2);
        assert!(session.retrying_timeouts && !session.allow_follow_up && !session.rescan);
        assert_eq!(session.retry_round, 2);
    }

    #[test]
    fn legacy_hashes_are_computed_only_while_a_row_predates_this_engines_first_scan() {
        let dir = std::env::temp_dir().join(format!("fileid-legacy-marker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let conn = crate::db::test_support::memory_db();
        assert!(legacy_hash_rows_possible(&conn, None), "no state folder: assume yes");

        // A catalog that already has hashed rows when the marker is first written.
        crate::db::test_support::insert_file(&conn, 1, "/lib/old.jpg", "image");
        conn.execute("UPDATE files SET content_hash = X'01', scanned_at = 1000 WHERE id = 1", []).unwrap();
        assert!(legacy_hash_rows_possible(&conn, Some(&dir)));
        let since: f64 = std::fs::read_to_string(dir.join("sha256-content-hash-since")).unwrap().parse().unwrap();
        assert!(since > 1000.0);
        assert!(legacy_hash_rows_possible(&conn, Some(&dir)), "the old row is still there");

        // Once every hashed row has been scanned since, the probe is retired.
        conn.execute("UPDATE files SET scanned_at = ?1 WHERE id = 1", [since + 1.0]).unwrap();
        assert!(!legacy_hash_rows_possible(&conn, Some(&dir)));
        crate::db::test_support::insert_file(&conn, 2, "/lib/unhashed.bin", "other");
        assert!(!legacy_hash_rows_possible(&conn, Some(&dir)), "a row without a hash cannot heal by hash");

        // A new library never computes them.
        let fresh = dir.join("fresh");
        assert!(!legacy_hash_rows_possible(&crate::db::test_support::memory_db(), Some(&fresh)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catch_up_progress_only_grows_past_the_scans_own_counts() {
        assert_eq!(catch_up_counters(100, 100, 40, 0), (140, 100));
        assert_eq!(catch_up_counters(100, 100, 40, 25), (140, 125));
        assert_eq!(catch_up_counters(100, 100, 40, 99), (140, 140));
        // A rescan that found nothing new but still owes an earlier scan's files.
        assert_eq!(catch_up_counters(0, 0, 12, 4), (12, 4));
    }

    #[test]
    fn the_last_snapshot_carries_the_terminal_phase_and_the_final_counts() {
        for phase in [ScanPhase::Completed, ScanPhase::Cancelled, ScanPhase::Failed] {
            let progress = terminal_progress("s1", phase, 959, 959, 930, 2);
            assert_eq!(progress.phase, phase);
            assert_eq!((progress.total, progress.discovered, progress.processed, progress.failed), (959, 959, 930, 2));
            assert_eq!(progress.files_per_second.to_bits(), 0.0f64.to_bits());
            assert!(progress.eta_seconds.is_none());
        }
    }

    #[test]
    fn known_placeholders_are_scoped_to_the_root() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (path_text TEXT, kind TEXT, size_bytes INTEGER, modified_at DOUBLE, failed INTEGER, content_hash BLOB);",
        )
        .unwrap();
        let sep = std::path::MAIN_SEPARATOR;
        let inside = format!("{sep}lib{sep}cloud.jpg");
        let outside = format!("{sep}library2{sep}cloud.jpg");
        for path in [&inside, &outside] {
            conn.execute("INSERT INTO files VALUES (?1, 'image', 7, 3.0, 0, NULL)", [path]).unwrap();
        }
        let root = format!("{sep}lib");
        let known = known_placeholders(&conn, Path::new(&root));
        assert_eq!(known.len(), 1);
        assert_eq!(known.get(&path_fingerprint_text(&inside)), Some(&(7, Some(3.0))));
    }

    #[test]
    fn batch_summary_sampling_bounds_ipc_volume() {
        assert!(should_emit_batch_summary(0, 5, 0));
        assert!(!should_emit_batch_summary(1, 99, 5));
        assert!(should_emit_batch_summary(20, 105, 5));
        assert!(!should_emit_batch_summary(21, 150, 105));
        assert!(should_emit_batch_summary(40, 205, 105));
    }

    /// Kind-aware ETA: the heavy and light classes each use their own rate,
    /// so a hash-only stretch (light files flying by at hundreds/s) must not
    /// collapse the ETA for tens of thousands of remaining images — the
    /// "ETA jumping everywhere" regression on mixed libraries.
    #[test]
    fn eta_uses_per_class_rates() {
        let mut st = ProgressState::new();
        let t0 = Instant::now();
        st.rate_anchor = t0;
        let _ = st.observe_rate(10, 10, t0 + Duration::from_secs(1));
        let _ = st.observe_rate(410, 10, t0 + Duration::from_secs(2));
        assert!((st.rolling_heavy_fps - 10.0).abs() < 1.0, "heavy rate held: {}", st.rolling_heavy_fps);
        assert!(st.rolling_light_fps > 100.0, "light rate: {}", st.rolling_light_fps);
        // 1000 heavy + 1000 light remaining: ETA is ~100s of images plus a
        // few seconds of light files — NOT (2000 / blended≈210) ≈ 9.5s.
        let eta = st.eta_seconds(1000, 1000).unwrap();
        assert!(eta > 90.0 && eta < 130.0, "eta: {eta}");
        assert!(st.eta_seconds(0, 0).is_none());
    }

    /// The rolling fps must measure real wall-clock throughput, not the
    /// per-batch DB-flush rate — the regression that produced "13s" ETAs for
    /// an hour of work.
    #[test]
    fn rolling_fps_measures_wallclock_throughput() {
        let mut st = ProgressState::new();
        let t0 = Instant::now();
        st.rate_anchor = t0;
        st.rate_anchor_total = 0;
        st.rolling_fps = 0.0;

        // 5 files in the first second → ~5 files/s (the real pipeline rate),
        // NOT the thousands/s a DB-flush measurement reports.
        let fps1 = st.observe_rate(5, 0, t0 + Duration::from_secs(1));
        assert!((fps1 - 5.0).abs() < 0.01, "first-interval rate should be 5/s, got {fps1}");

        let fps2 = st.observe_rate(10, 0, t0 + Duration::from_secs(2));
        assert!((fps2 - 5.0).abs() < 0.01, "sustained rate ~5/s, got {fps2}");

        // An ETA built on this rate is hours for tens of thousands remaining,
        let eta = 18_000.0_f64 / fps2;
        assert!(eta > 3000.0, "eta should be ~hours ({eta}s)");
    }

    /// The prefix upper bound must select exactly the rows `LIKE
    /// 'prefix%'` would under BINARY collation — i.e. lo <= x < hi iff x starts
    /// with prefix.
    #[test]
    fn prefix_upper_bound_brackets_the_prefix() {
        let lo = r"C:\Users\a\Photos";
        let hi = prefix_upper_bound(lo).unwrap();
        assert!(lo < hi.as_str(), "lo must sort before hi");
        let child = r"C:\Users\a\Photos\2024\img.jpg";
        assert!(lo <= child && child < hi.as_str());
        let sibling = r"C:\Users\a\Pictures\x.jpg";
        assert!(!(lo <= sibling && sibling < hi.as_str()));
        assert_eq!(prefix_upper_bound(""), None);
        assert_eq!(prefix_upper_bound("abc").as_deref(), Some("abd"));
    }

    #[test]
    fn completed_walk_soft_hides_only_unseen_active_rows_and_preserves_metadata() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        for pragma in crate::db::SETUP_PRAGMAS {
            let _ = conn.execute_batch(pragma);
        }
        crate::db::migrations::apply(&conn).unwrap();

        let root = if cfg!(windows) { r"C:\library" } else { "/library" };
        let separator = std::path::MAIN_SEPARATOR;
        let path = |tail: &str| format!("{root}{separator}{tail}");
        let sibling_root = if cfg!(windows) { r"C:\library-old" } else { "/library-old" };
        let insert = |conn: &Connection, value: &str, failed: i64, message: Option<&str>| -> i64 {
            conn.execute(
                "INSERT INTO files \
                 (path_text, path_hash, size_bytes, scanned_at, kind, extension, failed, error_message) \
                 VALUES (?1, ?2, 10, 100.0, 'image', 'jpg', ?3, ?4)",
                rusqlite::params![
                    value,
                    crate::util::path_safety::stable_path_hash(value),
                    failed,
                    message,
                ],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let seen_path = path("seen.jpg");
        let missing_path = path("missing.jpg");
        let failed_path = path("already-failed.jpg");
        let seen_id = insert(&conn, &seen_path, 0, None);
        let missing_id = insert(&conn, &missing_path, 0, None);
        let failed_id = insert(&conn, &failed_path, 1, Some("decode failed"));
        let sibling_id = insert(&conn, &format!("{sibling_root}{separator}sibling.jpg"), 0, None);
        conn.execute(
            "INSERT INTO tags (file_id, tag, source) VALUES (?1, 'family', 'user')",
            [missing_id],
        )
        .unwrap();

        let mut seen = vec![crate::util::path_safety::stable_path_hash(&seen_path)];
        let marked = soft_hide_missing_rows(&mut conn, Path::new(root), &mut seen).unwrap();
        assert_eq!(marked, 1);
        let state = |id: i64| {
            conn.query_row(
                "SELECT failed, error_message FROM files WHERE id = ?1",
                [id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .unwrap()
        };
        assert_eq!(state(seen_id).0, 0);
        assert_eq!(state(missing_id).0, 1);
        assert!(state(missing_id).1.unwrap().contains("no longer present"));
        assert_eq!(state(failed_id), (1, Some("decode failed".into())));
        assert_eq!(state(sibling_id).0, 0, "prefix-sharing sibling root must survive");
        let tag_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tags WHERE file_id = ?1", [missing_id], |row| row.get(0))
            .unwrap();
        assert_eq!(tag_count, 1, "soft hide must preserve user tags for recovery");
    }

    fn retention_db() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        for pragma in crate::db::SETUP_PRAGMAS {
            let _ = conn.execute_batch(pragma);
        }
        crate::db::migrations::apply(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    const NOW: f64 = 2_000_000_000.0;
    const DAY: f64 = 86_400.0;

    fn hidden_row(db: &Arc<Mutex<Connection>>, name: &str, seen_days_ago: f64, hidden: bool, message: Option<&str>) -> i64 {
        let root = if cfg!(windows) { r"C:\lib" } else { "/lib" };
        let path = format!("{root}{}{name}", std::path::MAIN_SEPARATOR);
        let conn = db.lock();
        conn.execute(
            "INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension, failed, error_message) \
             VALUES (?1, ?2, 10, ?3, 'image', 'jpg', ?4, ?5)",
            rusqlite::params![path, crate::util::path_safety::stable_path_hash(&path), NOW - seen_days_ago * DAY, hidden as i64, message],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn row_exists(db: &Arc<Mutex<Connection>>, id: i64) -> bool {
        db.lock().query_row("SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1)", [id], |r| r.get(0)).unwrap()
    }

    fn lib_root() -> &'static Path {
        Path::new(if cfg!(windows) { r"C:\lib" } else { "/lib" })
    }

    #[test]
    fn a_hidden_row_last_seen_31_days_ago_is_removed_with_what_hangs_off_it_and_its_person_is_reconciled() {
        let db = retention_db();
        let old = hidden_row(&db, "old.jpg", 31.0, true, Some(HIDDEN_ROW_MESSAGE));
        let keep = hidden_row(&db, "keep.jpg", 1.0, false, None);
        {
            let conn = db.lock();
            conn.execute("INSERT INTO persons (id, name, file_count, created_at) VALUES (7, 'Pat', 2, 0)", []).unwrap();
            conn.execute("INSERT INTO tags (file_id, tag, source) VALUES (?1, 'beach', 'user')", [old]).unwrap();
            conn.execute("INSERT INTO doc_text (file_id, text) VALUES (?1, 'hello')", [old]).unwrap();
            conn.execute("INSERT INTO clip_embeddings (file_id, embedding, model) VALUES (?1, X'00', 'm')", [old]).unwrap();
            conn.execute("INSERT INTO face_prints (id, file_id, print_data, bbox, person_id) VALUES (50, ?1, X'00', '[]', 7)", [old]).unwrap();
            conn.execute("INSERT INTO face_prints (id, file_id, print_data, bbox, person_id) VALUES (51, ?1, X'00', '[]', 7)", [keep]).unwrap();
            conn.execute("UPDATE persons SET representative_face_id = 50 WHERE id = 7", []).unwrap();
        }
        let out = purge_expired_hidden_rows(&db, lib_root(), NOW, 30.0, &|| false);
        assert_eq!(out.removed, 1);
        assert!(!row_exists(&db, old) && row_exists(&db, keep));
        let conn = db.lock();
        for table in ["tags", "doc_text", "clip_embeddings"] {
            let n: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE file_id = ?1"), [old], |r| r.get(0)).unwrap();
            assert_eq!(n, 0, "{table} rows go with the file");
        }
        let faces: i64 = conn.query_row("SELECT COUNT(*) FROM face_prints WHERE file_id = ?1", [old], |r| r.get(0)).unwrap();
        assert_eq!(faces, 0);
        let person: (i64, Option<i64>) = conn.query_row("SELECT file_count, representative_face_id FROM persons WHERE id = 7", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(person, (1, Some(51)), "count recomputed and the representative moved to a surviving face");
    }

    #[test]
    fn a_person_left_with_no_faces_has_a_zero_count_and_no_representative() {
        let db = retention_db();
        let old = hidden_row(&db, "old.jpg", 40.0, true, Some(HIDDEN_ROW_MESSAGE));
        {
            let conn = db.lock();
            conn.execute("INSERT INTO persons (id, name, file_count, created_at) VALUES (7, 'Pat', 1, 0)", []).unwrap();
            conn.execute("INSERT INTO face_prints (id, file_id, print_data, bbox, person_id) VALUES (50, ?1, X'00', '[]', 7)", [old]).unwrap();
            conn.execute("UPDATE persons SET representative_face_id = 50 WHERE id = 7", []).unwrap();
        }
        purge_expired_hidden_rows(&db, lib_root(), NOW, 30.0, &|| false);
        let person: (i64, Option<i64>) = db.lock().query_row("SELECT file_count, representative_face_id FROM persons WHERE id = 7", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(person, (0, None));
    }

    #[test]
    fn a_hidden_row_last_seen_29_days_ago_stays() {
        let db = retention_db();
        let id = hidden_row(&db, "recent.jpg", 29.0, true, Some(HIDDEN_ROW_MESSAGE));
        assert_eq!(purge_expired_hidden_rows(&db, lib_root(), NOW, 30.0, &|| false).removed, 0);
        assert!(row_exists(&db, id));
    }

    #[test]
    fn a_row_that_failed_for_another_reason_stays_however_old() {
        let db = retention_db();
        let id = hidden_row(&db, "corrupt.jpg", 400.0, true, Some("Could not decode image"));
        assert_eq!(purge_expired_hidden_rows(&db, lib_root(), NOW, 30.0, &|| false).removed, 0);
        assert!(row_exists(&db, id));
    }

    #[test]
    fn nothing_outside_the_walked_location_is_removed() {
        let db = retention_db();
        let id = hidden_row(&db, "old.jpg", 90.0, true, Some(HIDDEN_ROW_MESSAGE));
        let other = Path::new(if cfg!(windows) { r"C:\elsewhere" } else { "/elsewhere" });
        assert_eq!(purge_expired_hidden_rows(&db, other, NOW, 30.0, &|| false).removed, 0);
        assert!(row_exists(&db, id));
    }

    #[test]
    fn a_cancelled_scan_removes_nothing() {
        let db = retention_db();
        let id = hidden_row(&db, "old.jpg", 90.0, true, Some(HIDDEN_ROW_MESSAGE));
        assert_eq!(purge_expired_hidden_rows(&db, lib_root(), NOW, 30.0, &|| true).removed, 0);
        assert!(row_exists(&db, id));
    }

    #[test]
    fn a_last_seen_time_in_the_future_removes_nothing_at_all() {
        let db = retention_db();
        let old = hidden_row(&db, "old.jpg", 90.0, true, Some(HIDDEN_ROW_MESSAGE));
        hidden_row(&db, "future.jpg", -2.0, false, None);
        let out = purge_expired_hidden_rows(&db, lib_root(), NOW, 30.0, &|| false);
        assert!(out.skipped_for_clock && out.removed == 0);
        assert!(row_exists(&db, old));
    }

    #[test]
    fn a_large_purge_runs_in_bounded_batches() {
        let db = retention_db();
        for i in 0..(HIDDEN_PURGE_BATCH * 2 + 5) {
            hidden_row(&db, &format!("f{i}.jpg"), 60.0, true, Some(HIDDEN_ROW_MESSAGE));
        }
        let out = purge_expired_hidden_rows(&db, lib_root(), NOW, 30.0, &|| false);
        assert_eq!((out.removed, out.batches), ((HIDDEN_PURGE_BATCH * 2 + 5) as u64, 3));
    }

    #[test]
    fn a_file_that_returns_within_the_period_rebinds_to_its_hidden_row() {
        let mut conn = Connection::open_in_memory().unwrap();
        for pragma in crate::db::SETUP_PRAGMAS {
            let _ = conn.execute_batch(pragma);
        }
        crate::db::migrations::apply(&conn).unwrap();
        let root = lib_root();
        let path = format!("{}{}back.jpg", root.display(), std::path::MAIN_SEPARATOR);
        conn.execute(
            "INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES (?1, ?2, 10, ?3, 'image', 'jpg')",
            rusqlite::params![path, crate::util::path_safety::stable_path_hash(&path), unix_now() - 5.0 * DAY],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute("INSERT INTO tags (file_id, tag, source) VALUES (?1, 'family', 'user')", [id]).unwrap();
        let marked = soft_hide_missing_rows(&mut conn, root, &mut Vec::new()).unwrap();
        assert_eq!(marked, 1);
        let db = Arc::new(Mutex::new(conn));
        assert_eq!(purge_expired_hidden_rows(&db, root, unix_now(), 30.0, &|| false).removed, 0, "hiding restarts the clock");
        assert!(row_exists(&db, id));
        let tags: i64 = db.lock().query_row("SELECT COUNT(*) FROM tags WHERE file_id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(tags, 1);
        assert_eq!(purge_expired_hidden_rows(&db, root, unix_now() + 31.0 * DAY, 30.0, &|| false).removed, 1, "and 31 days on it goes");
    }

    #[test]
    fn the_retention_override_accepts_zero_and_ignores_junk() {
        assert!(parse_retention_days(Some("0")).abs() < f64::EPSILON);
        assert!((parse_retention_days(Some(" 2.5 ")) - 2.5).abs() < f64::EPSILON);
        for bad in [Some("junk"), Some("-1"), Some("NaN"), None] {
            assert!((parse_retention_days(bad) - HIDDEN_ROW_RETENTION_DAYS).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn missing_reconciliation_requires_a_complete_uncapped_error_free_walk() {
        assert!(should_reconcile_missing_rows(false, false, true, 0, 0));
        assert!(!should_reconcile_missing_rows(true, false, true, 0, 0));
        assert!(!should_reconcile_missing_rows(false, true, true, 0, 0));
        assert!(!should_reconcile_missing_rows(false, false, false, 0, 0));
        assert!(!should_reconcile_missing_rows(false, false, true, 1, 0));
        assert!(!should_reconcile_missing_rows(false, false, true, 0, 1));
    }

    #[test]
    #[ignore = "manual 1M-row performance gate"]
    fn million_row_reconciliation_uses_set_based_update() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (
                 id INTEGER PRIMARY KEY,
                 path_text TEXT NOT NULL UNIQUE,
                 path_hash INTEGER NOT NULL,
                 failed INTEGER NOT NULL DEFAULT 0,
                 error_message TEXT
             );
             CREATE INDEX files_path_hash ON files(path_hash);",
        )
        .unwrap();
        let root = if cfg!(windows) { r"C:\million" } else { "/million" };
        let prefix = format!("{root}{}", std::path::MAIN_SEPARATOR);
        conn.execute(
            "WITH RECURSIVE seq(x) AS (
                 SELECT 1 UNION ALL SELECT x + 1 FROM seq WHERE x < 1000000
             )
             INSERT INTO files (id, path_text, path_hash)
             SELECT x, ?1 || x || '.jpg', x FROM seq",
            [prefix],
        )
        .unwrap();
        let mut seen: Vec<i64> = (1..=1_000_000).step_by(2).collect();
        let started = Instant::now();
        let marked = soft_hide_missing_rows(&mut conn, Path::new(root), &mut seen).unwrap();
        assert_eq!(marked, 500_000);
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    /// purge_excluded_rows deletes exactly the rows strictly under each
    /// excluded folder (BINARY prefix range) and cascades child rows; a
    /// sibling folder sharing the excluded name as a prefix survives.
    #[test]
    fn purge_excluded_rows_deletes_subtree_and_cascades() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for pragma in crate::db::SETUP_PRAGMAS {
            let _ = conn.execute_batch(pragma);
        }
        crate::db::migrations::apply(&conn).unwrap();

        let insert = |path: &str| -> i64 {
            conn.execute(
                "INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension) \
                 VALUES (?1, 1, 10, 100.0, 'image', 'jpg')",
                rusqlite::params![path],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        // Platform-appropriate absolute paths: resolve_exclusions drops
        // non-absolute entries, and these tests also run on Linux CI.
        let j = |tail: &str| {
            if cfg!(windows) {
                format!(r"C:\a{}", tail.replace('/', "\\"))
            } else {
                format!("/a{tail}")
            }
        };
        let in_a = insert(&j("/b/one.jpg"));
        let in_b = insert(&j("/b/deep/two.jpg"));
        let boundary = insert(&j("/bc/three.jpg")); // shares '…\a\b' as string prefix
        let keeper = insert(&j("/keep/four.jpg"));
        conn.execute(
            "INSERT INTO tags (file_id, tag, source) VALUES (?1, 'cat', 'test')",
            rusqlite::params![in_a],
        )
        .unwrap();
        conn.execute("INSERT INTO persons (id, created_at, file_count) VALUES (5, 0, 2)", []).unwrap();
        conn.execute(
            "INSERT INTO face_prints (file_id, print_data, bbox, person_id) VALUES (?1, X'00', '[]', 5), (?2, X'00', '[]', 5)",
            rusqlite::params![in_b, keeper],
        )
        .unwrap();

        let root = if cfg!(windows) { r"C:\a".to_string() } else { "/a".to_string() };
        let exclusions = crate::util::path_safety::resolve_exclusions(
            std::path::Path::new(&root),
            &[j("/b")],
        );
        assert_eq!(exclusions.len(), 1);
        let purged = purge_excluded_rows(&conn, &exclusions);
        assert_eq!(purged, 2);

        let remaining: Vec<i64> = conn
            .prepare("SELECT id FROM files ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(remaining, vec![boundary, keeper]);
        let tag_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0)).unwrap();
        let face_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM face_prints", [], |r| r.get(0)).unwrap();
        assert_eq!((tag_count, face_count), (0, 1), "cascade must clear child rows");
        let person_files: i64 = conn.query_row("SELECT file_count FROM persons WHERE id = 5", [], |r| r.get(0)).unwrap();
        assert_eq!(person_files, 1, "the purge recounts the person whose face went");
    }

    /// Two emits closer than MIN_RATE_DT must not re-divide and spike the rate.
    #[test]
    fn rolling_fps_holds_between_rapid_samples() {
        let mut st = ProgressState::new();
        let t0 = Instant::now();
        st.rate_anchor = t0;
        st.rate_anchor_total = 0;
        st.rolling_fps = 0.0;

        let _ = st.observe_rate(10, 0, t0 + Duration::from_secs(1)); // seeds ~10/s
        let held = st.observe_rate(1000, 0, t0 + Duration::from_millis(1100)); // only +0.1 s
        assert!(held < 50.0, "a rapid sample (<0.5s) must not spike the rate, got {held}");
    }

    #[test]
    fn text_embed_gate_needs_the_weights() {
        assert_eq!(text_embed_gate(true), SKIP_SET_TEXT_EMBED_GATE);
        assert_eq!(text_embed_gate(false), "");
    }

    #[test]
    fn crash_self_heal_stops_after_one_finished_scan_of_the_root() {
        let healed = Mutex::new(std::collections::HashSet::new());
        assert!(!heal_due(false, &healed, "/a/"), "clean shutdown: never");
        assert!(heal_due(true, &healed, "/a/"), "first scan of the root after a crash");
        assert!(heal_due(true, &healed, "/a/"), "a scan that was cancelled leaves the root due");
        mark_heal_done(&healed, "/a/");
        assert!(!heal_due(true, &healed, "/a/"), "after a finished scan the root is not reopened again");
        assert!(heal_due(true, &healed, "/b/"), "another root still gets its own pass");
    }

    /// Crash self-heal: after an unclean shutdown, a file with committed
    /// face_prints rows but a missing crop JPEG must be pulled OUT of the skip
    /// set (so it reprocesses), while a file whose crops all exist stays in it.
    #[test]
    fn reconcile_missing_face_crops_reopens_only_crop_missing_files() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path_text TEXT NOT NULL UNIQUE);
             CREATE TABLE face_prints (id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute("INSERT INTO files (id, path_text) VALUES (1, 'C:\\Lib\\a.jpg')", [])
            .unwrap();
        conn.execute("INSERT INTO files (id, path_text) VALUES (2, 'C:\\Lib\\b.jpg')", [])
            .unwrap();
        // a.jpg: two faces (10, 11). b.jpg: one face (20).
        for (fid, file) in [(10, 1), (11, 1), (20, 2)] {
            conn.execute(
                "INSERT INTO face_prints (id, file_id) VALUES (?1, ?2)",
                rusqlite::params![fid, file],
            )
            .unwrap();
        }
        let tmp = std::env::temp_dir().join(format!("fileid-crop-test-{}", std::process::id()));
        let faces = tmp.join("face_crops");
        std::fs::create_dir_all(&faces).unwrap();
        // b.jpg's crop exists; a.jpg is missing crop 11 (killed mid-crop-write).
        std::fs::write(faces.join("20.jpg"), b"x").unwrap();
        std::fs::write(faces.join("10.jpg"), b"x").unwrap();

        let mut set = std::collections::HashMap::new();
        set.insert(path_fingerprint_text("C:\\Lib\\a.jpg"), (1i64, None));
        set.insert(path_fingerprint_text("C:\\Lib\\b.jpg"), (2i64, None));

        let lo = "C:\\Lib\\";
        let hi = prefix_upper_bound(lo);
        let removed =
            reconcile_missing_face_crops_in(&conn, &mut set, lo, hi.as_deref(), &faces);
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(removed, 1, "only a.jpg (missing a crop) should be reopened");
        assert!(!set.contains_key(&path_fingerprint_text("C:\\Lib\\a.jpg")), "a.jpg reopened");
        assert!(set.contains_key(&path_fingerprint_text("C:\\Lib\\b.jpg")), "b.jpg stays skipped");
    }

    /// The incremental skip-set must NOT skip a file whose
    /// reparse/placeholder state changed (dehydrated→hydrated). A OneDrive
    /// online-only placeholder is recorded with `content_hash = NULL` and
    /// `scanned_at >= modified_at`; after the user hydrates it, the modified
    /// time is unchanged, so the bare `scanned_at >= modified_at` predicate
    /// would strand it forever. The content_hash gate must drop it from the
    /// skip set while keeping fully-scanned files in it.
    #[test]
    fn skip_set_excludes_hydrated_placeholder_keeps_scanned_file() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (
                 path_text    TEXT NOT NULL UNIQUE,
                 kind         TEXT NOT NULL,
                 modified_at  DOUBLE,
                 scanned_at   DOUBLE NOT NULL,
                 failed       INTEGER NOT NULL DEFAULT 0,
                 content_hash BLOB
             );",
        )
        .unwrap();
        // A dehydrated image scanned as an online-only placeholder: no
        // content_hash, scanned_at >= modified_at, failed=0. After hydration
        // modified_at is unchanged — the row below is exactly that state.
        conn.execute(
            "INSERT INTO files (path_text, kind, modified_at, scanned_at, failed, content_hash) \
             VALUES ('C:\\OneDrive\\dehydrated.jpg', 'image', 100.0, 200.0, 0, NULL)",
            [],
        )
        .unwrap();
        // A fully-scanned local image: has a content_hash → belongs in the skip set.
        conn.execute(
            "INSERT INTO files (path_text, kind, modified_at, scanned_at, failed, content_hash) \
             VALUES ('C:\\Local\\photo.jpg', 'image', 100.0, 200.0, 0, X'00112233')",
            [],
        )
        .unwrap();
        // A non-content kind (e.g. 'other') legitimately lacks a content_hash
        // and must NOT be force-rescanned by the gate.
        conn.execute(
            "INSERT INTO files (path_text, kind, modified_at, scanned_at, failed, content_hash) \
             VALUES ('C:\\Local\\notes.bin', 'other', 100.0, 200.0, 0, NULL)",
            [],
        )
        .unwrap();

        // Run the EXACT production skip-set predicate (no path-prefix arm).
        let sql = format!(
            "SELECT path_text FROM files \
             WHERE failed = 0 \
             AND scanned_at >= modified_at \
             {SKIP_SET_CONTENT_HASH_GATE}"
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let skip: std::collections::HashSet<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();

        assert!(
            !skip.contains("C:\\OneDrive\\dehydrated.jpg"),
            "a dehydrated→hydrated placeholder (content_hash NULL) must be reprocessed, not skipped"
        );
        assert!(
            skip.contains("C:\\Local\\photo.jpg"),
            "a fully-scanned content-bearing file must stay in the skip set"
        );
        assert!(
            skip.contains("C:\\Local\\notes.bin"),
            "a non-content kind without a content_hash must not be force-rescanned"
        );
    }

    /// Lockstep with macOS `skipSetTextBackfillExclusionSQL`: the BGE-text gate
    /// must keep a doc/pdf still lacking a `text_embeddings` row OUT of the skip set
    /// (so an install-then-rescan backfills it), while leaving docs that already have
    /// one — and non-doc kinds entirely — in the skip set.
    #[test]
    fn text_embed_gate_reprocesses_embeddingless_docs_only() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (
                 id              INTEGER PRIMARY KEY,
                 path_text       TEXT NOT NULL UNIQUE,
                 kind            TEXT NOT NULL,
                 modified_at     DOUBLE,
                 scanned_at      DOUBLE NOT NULL,
                 failed          INTEGER NOT NULL DEFAULT 0,
                 content_hash    BLOB,
                 text_stage_done INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE text_embeddings (
                 file_id   INTEGER PRIMARY KEY,
                 embedding BLOB,
                 model     TEXT
             );",
        )
        .unwrap();
        // All rows carry a content_hash so the content gate keeps them all — isolating
        // the text gate's effect. id=5 is a text-less doc whose text stage already ran
        // (text_stage_done=1) — it must NOT be re-walked (no embeddable text to find).
        conn.execute(
            "INSERT INTO files (id, path_text, kind, modified_at, scanned_at, failed, content_hash, text_stage_done) VALUES \
                 (1, 'C:\\Docs\\no_embed.docx',  'doc',   100.0, 200.0, 0, X'00', 0), \
                 (2, 'C:\\Docs\\embedded.docx',  'doc',   100.0, 200.0, 0, X'00', 0), \
                 (3, 'C:\\Docs\\no_embed.pdf',   'pdf',   100.0, 200.0, 0, X'00', 0), \
                 (4, 'C:\\Pics\\photo.jpg',      'image', 100.0, 200.0, 0, X'00', 0), \
                 (5, 'C:\\Docs\\scanned.pdf',    'pdf',   100.0, 200.0, 0, X'00', 1)",
            [],
        )
        .unwrap();
        // Only doc id=2 has a stored embedding.
        conn.execute(
            "INSERT INTO text_embeddings (file_id, embedding, model) \
             VALUES (2, X'00', 'bge_small_en_v1_5')",
            [],
        )
        .unwrap();

        // Production no-prefix predicate with BOTH gates appended (BGE installed).
        let sql = format!(
            "SELECT path_text FROM files \
             WHERE failed = 0 \
             {SKIP_SET_CONTENT_HASH_GATE}{SKIP_SET_TEXT_EMBED_GATE}"
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let skip: std::collections::HashSet<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();

        assert!(
            !skip.contains("C:\\Docs\\no_embed.docx"),
            "an embeddingless doc must be reprocessed so its BGE embedding backfills"
        );
        assert!(
            !skip.contains("C:\\Docs\\no_embed.pdf"),
            "an embeddingless pdf must be reprocessed too"
        );
        assert!(
            skip.contains("C:\\Docs\\embedded.docx"),
            "a doc that already has a text_embeddings row stays in the skip set"
        );
        assert!(
            skip.contains("C:\\Pics\\photo.jpg"),
            "the text gate must not touch non-doc kinds (images use clip_embeddings)"
        );
        assert!(
            skip.contains("C:\\Docs\\scanned.pdf"),
            "a text-less doc whose text stage already ran (text_stage_done=1) must NOT re-walk forever"
        );
    }

    /// Lockstep with macOS `skipSetModelClipBackfillExclusionSQL`: the model-CLIP
    /// gate must keep a `.obj` still lacking a `clip_embeddings` row OUT of the skip set
    /// (so a rescan backfills its rendered-shape vector), while leaving an embedded `.obj`,
    /// a non-`.obj` 3D format (not renderable), and non-model kinds in the skip set.
    #[test]
    fn model_clip_gate_reprocesses_embeddingless_obj_only() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (
                 id              INTEGER PRIMARY KEY,
                 path_text       TEXT NOT NULL UNIQUE,
                 kind            TEXT NOT NULL,
                 extension       TEXT NOT NULL,
                 modified_at     DOUBLE,
                 scanned_at      DOUBLE NOT NULL,
                 failed          INTEGER NOT NULL DEFAULT 0,
                 content_hash    BLOB,
                 text_stage_done INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE clip_embeddings (
                 file_id   INTEGER PRIMARY KEY,
                 embedding BLOB,
                 model     TEXT
             );",
        )
        .unwrap();
        // All rows carry a content_hash so the content gate keeps them — isolating the
        // model gate's effect. id=5 is an un-renderable .obj whose render already ran
        // (text_stage_done=1) — it must NOT be re-walked (no CLIP vector to ever produce).
        conn.execute(
            "INSERT INTO files (id, path_text, kind, extension, modified_at, scanned_at, failed, content_hash, text_stage_done) VALUES \
                 (1, 'C:\\M\\no_embed.obj', 'model', 'obj',  100.0, 200.0, 0, X'00', 0), \
                 (2, 'C:\\M\\embedded.obj', 'model', 'obj',  100.0, 200.0, 0, X'00', 0), \
                 (3, 'C:\\M\\shape.stl',    'model', 'stl',  100.0, 200.0, 0, X'00', 0), \
                 (4, 'C:\\P\\photo.jpg',    'image', 'jpg',  100.0, 200.0, 0, X'00', 0), \
                 (5, 'C:\\M\\corrupt.obj',  'model', 'obj',  100.0, 200.0, 0, X'00', 1)",
            [],
        )
        .unwrap();
        // Only obj id=2 has a stored CLIP embedding.
        conn.execute(
            "INSERT INTO clip_embeddings (file_id, embedding, model) VALUES (2, X'00', 'mobileclip_s2')",
            [],
        )
        .unwrap();

        let sql = format!(
            "SELECT path_text FROM files \
             WHERE failed = 0 \
             {SKIP_SET_CONTENT_HASH_GATE}{SKIP_SET_MODEL_CLIP_GATE}"
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let skip: std::collections::HashSet<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();

        assert!(
            !skip.contains("C:\\M\\no_embed.obj"),
            "an embeddingless .obj must be reprocessed so its rendered-shape CLIP backfills"
        );
        assert!(
            skip.contains("C:\\M\\embedded.obj"),
            "an .obj that already has a clip_embeddings row stays in the skip set"
        );
        assert!(
            skip.contains("C:\\M\\shape.stl"),
            "a non-.obj 3D format isn't renderable, so the gate must not re-walk it"
        );
        assert!(
            skip.contains("C:\\P\\photo.jpg"),
            "the model gate must not touch images"
        );
        assert!(
            skip.contains("C:\\M\\corrupt.obj"),
            "an un-renderable .obj whose render already ran (text_stage_done=1) must NOT re-walk forever"
        );
    }

    #[test]
    fn video_clip_gate_reprocesses_frameless_videos_only() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path_text TEXT NOT NULL UNIQUE, kind TEXT NOT NULL,
                 failed INTEGER NOT NULL DEFAULT 0, content_hash BLOB);
             CREATE TABLE clip_embeddings (file_id INTEGER PRIMARY KEY, embedding BLOB, model TEXT);
             INSERT INTO files (id, path_text, kind, content_hash) VALUES
                 (1, '/v/frameless.mov', 'video', X'00'), (2, '/v/embedded.mov', 'video', X'00'),
                 (3, '/a/song.m4a', 'audio', X'00');
             INSERT INTO clip_embeddings (file_id, embedding, model) VALUES (2, X'00', 'clip');",
        )
        .unwrap();
        let sql = format!("SELECT path_text FROM files WHERE failed = 0 {SKIP_SET_CONTENT_HASH_GATE}{SKIP_SET_VIDEO_CLIP_GATE}");
        let skip: std::collections::HashSet<String> =
            conn.prepare(&sql).unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
        assert!(!skip.contains("/v/frameless.mov"));
        assert!(skip.contains("/v/embedded.mov"));
        assert!(skip.contains("/a/song.m4a"));
    }

    fn post_scan_progress(event: IpcEvent) -> ScanProgress {
        match event.payload {
            EventPayload::Progress(progress) => progress.inner,
            other => panic!("expected progress, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_catch_up_repeats_its_progress_while_a_read_is_slow() {
        let (sink, mut events) = Sink::channel_for_test(256);
        let report = Arc::new(CatchUpProgress {
            sink,
            session_id: "one-session".into(),
            discovered: 100,
            processed: 100,
            failed: 2,
            lift: ProgressLift::default(),
            state: Mutex::new((0, 0, Instant::now())),
        });
        let heartbeat = report.clone().spawn_heartbeat(Duration::from_millis(20));
        report.report(crate::pipeline::catch_up::Progress::Total(30));
        tokio::time::sleep(Duration::from_millis(300)).await;
        report.report(crate::pipeline::catch_up::Progress::Advance(10));
        report.report(crate::pipeline::catch_up::Progress::Total(25));
        tokio::time::sleep(Duration::from_millis(100)).await;
        heartbeat.abort();
        let _ = heartbeat.await;

        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(post_scan_progress(event));
        }
        assert!(seen.len() >= 8, "a progress event about every tick, got {}", seen.len());
        assert!(seen.iter().all(|p| p.session_id == "one-session" && p.phase == ScanPhase::PostScan && p.failed == 2 && p.discovered == 100));
        for pair in seen.windows(2) {
            assert!(pair[1].total >= pair[0].total, "total never shrinks: {} then {}", pair[0].total, pair[1].total);
            assert!(pair[1].processed >= pair[0].processed, "processed never shrinks: {} then {}", pair[0].processed, pair[1].processed);
        }
        let last = seen.last().unwrap();
        assert_eq!((last.total, last.processed), (130, 110), "a smaller later estimate does not shrink the total");
        assert!(seen.iter().all(|p| p.processed <= p.total));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn catch_up_snapshots_from_the_pass_and_the_heartbeat_never_go_backwards() {
        let (sink, mut events) = Sink::channel_for_test(1 << 16);
        let report = Arc::new(CatchUpProgress {
            sink,
            session_id: "one-session".into(),
            discovered: 10,
            processed: 10,
            failed: 0,
            lift: ProgressLift::default(),
            state: Mutex::new((0, 0, Instant::now())),
        });
        let heartbeat = report.clone().spawn_heartbeat(Duration::from_millis(1));
        report.report(crate::pipeline::catch_up::Progress::Total(2000));
        let passes: Vec<_> = (0..4)
            .map(|_| {
                let report = report.clone();
                tokio::task::spawn_blocking(move || {
                    for _ in 0..400 {
                        report.report(crate::pipeline::catch_up::Progress::Advance(1));
                    }
                })
            })
            .collect();
        for pass in passes {
            pass.await.unwrap();
        }
        heartbeat.abort();
        let _ = heartbeat.await;

        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(post_scan_progress(event));
        }
        assert!(seen.len() >= 1600);
        for pair in seen.windows(2) {
            assert!(pair[1].processed >= pair[0].processed, "processed went back: {} then {}", pair[0].processed, pair[1].processed);
            assert!(pair[1].total >= pair[0].total);
        }
        assert_eq!(seen.last().unwrap().processed, 10 + 1600);
    }

    #[tokio::test]
    async fn no_catch_up_progress_follows_an_awaited_heartbeat_abort() {
        let (sink, mut events) = Sink::channel_for_test(1 << 12);
        let report = Arc::new(CatchUpProgress {
            sink,
            session_id: "one-session".into(),
            discovered: 10,
            processed: 10,
            failed: 0,
            lift: ProgressLift::default(),
            state: Mutex::new((0, 0, Instant::now())),
        });
        let heartbeat = report.clone().spawn_heartbeat(Duration::from_millis(1));
        tokio::time::sleep(Duration::from_millis(30)).await;
        heartbeat.abort();
        let _ = heartbeat.await;
        while events.try_recv().is_ok() {}
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(events.try_recv().is_err(), "the heartbeat stopped before the terminal event");
    }
}
