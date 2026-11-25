// Discovery walks the user-picked root and emits one DiscoveredFile per readable file the scan should consider. jwalk stats in parallel on `walk_concurrency_for(root)` threads (see platform.rs); process_read_dir prunes noise directories so we never recurse into them.
// The counter is bumped BEFORE the blocking send so it tracks the walk even when the channel fills. The channel cap is memory-tier-scaled (see discovery_channel_cap): a corpus that fits walks to completion at filesystem speed, so `done`, the total and DiscoveryComplete are truthful even when the ML stage is far slower.
// Filters: hidden/system files (leading `.`, thumbs.db, desktop.ini, ...), symlinks (not followed), zero-byte files, and unsupported kinds are skipped before tagging.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::coordinator::ScanCoordinator;
use crate::db::zero_byte::ZeroByteObservation;

/// Fixed-size key for the incremental-rescan cache: keeping every catalogued `PathBuf` duplicated the full path string for the whole scan (hundreds of MiB at million-file scale). A 128-bit BLAKE3 fingerprint keeps collision risk negligible with a per-file footprint independent of path length.
pub(crate) type SkipFingerprint = [u8; 16];

pub(crate) fn path_fingerprint(path: &std::path::Path) -> SkipFingerprint {
    path_fingerprint_text(&path.to_string_lossy())
}

pub(crate) fn path_fingerprint_text(path: &str) -> SkipFingerprint {
    let digest = blake3::hash(path.as_bytes());
    let mut fingerprint = [0_u8; 16];
    fingerprint.copy_from_slice(&digest.as_bytes()[..16]);
    fingerprint
}


/// Bounded mpsc capacity (Discovery → Tagging), floor value (MemoryTier::Low). Once the channel fills, `blocking_send` stalls the walker and the discovered counter, `done` flag and DiscoveryComplete advance in lockstep with ML throughput. Per-entry cost is a DiscoveredFile (~300 B with its PathBuf), so even the High-tier cap bounds queue memory at ~300 MB, and only while ML is far behind.
const DISCOVERY_CHANNEL_CAP_LOW: usize = 32_768;
const DISCOVERY_CHANNEL_CAP_BALANCED: usize = 262_144;
const DISCOVERY_CHANNEL_CAP_HIGH: usize = 1_048_576;
const ZERO_BYTE_CHANNEL_CAP: usize = 512;

fn discovery_channel_cap(tier: crate::platform::MemoryTier) -> usize {
    match tier {
        crate::platform::MemoryTier::Low => DISCOVERY_CHANNEL_CAP_LOW,
        crate::platform::MemoryTier::Balanced => DISCOVERY_CHANNEL_CAP_BALANCED,
        crate::platform::MemoryTier::High => DISCOVERY_CHANNEL_CAP_HIGH,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Image,
    Video,
    Pdf,
    Doc,
    Audio,
    /// 3D models are scanned (not dropped like Other) so Deep Analyze can name them from embedded object/material labels. Wavefront `.obj` only for now.
    Model,
    Other,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Image => "image",
            FileKind::Video => "video",
            FileKind::Pdf => "pdf",
            FileKind::Doc => "doc",
            FileKind::Audio => "audio",
            FileKind::Model => "model",
            FileKind::Other => "other",
        }
    }

    pub fn from_db_str(kind: &str) -> Self {
        match kind {
            "image" => FileKind::Image,
            "video" => FileKind::Video,
            "pdf" => FileKind::Pdf,
            "doc" => FileKind::Doc,
            "audio" => FileKind::Audio,
            "model" => FileKind::Model,
            _ => FileKind::Other,
        }
    }

    pub fn from_extension(ext: &str) -> Self {
        let ext = ext.to_ascii_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic"
            | "heif" | "avif" | "raw" | "arw" | "cr2" | "cr3" | "nef" | "dng" | "orf"
            | "rw2" | "raf" | "srw" | "pef" | "3fr" | "erf" | "kdc" | "mef" | "mos"
            | "mrw" | "rwl" | "srf" | "x3f" => FileKind::Image,
            "mp4" | "mov" | "m4v" | "avi" | "mkv" | "webm" | "mts" | "m2ts" | "wmv"
            | "mxf" | "3gp" | "3g2" | "flv" | "ogv" | "mpeg" | "mpg" | "vob"
            | "f4v" | "asf" | "divx" => {
                FileKind::Video
            }
            "pdf" => FileKind::Pdf,
            // Office docs, e-books and source/markup cluster by extracted text (BGE), in lockstep with macOS FileTypes. ppt/xls (legacy binary) have no extractor on either engine → text-less docs routed to Documents/ (text_stage_done stops the backfill re-walk).
            "docx" | "doc" | "odt" | "rtf" | "txt" | "md" | "pages" | "key" | "numbers"
            | "xlsx" | "pptx" | "xls" | "ppt" | "epub"
            | "swift" | "py" | "rb" | "js" | "jsx" | "ts" | "tsx" | "java" | "kt" | "c"
            | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "cs" | "go" | "rs" | "php"
            | "sh" | "bash" | "zsh" | "sql" | "scala" | "m" | "mm" | "r" | "jl" | "lua"
            | "dart" | "vue" | "pl" | "pm" | "ps1" | "tex" | "bib" | "rst" | "org"
            | "adoc" | "csv" | "tsv" | "log" | "markdown" | "text" | "json" | "yaml" | "yml"
            | "toml" | "xml" | "html" | "htm" => FileKind::Doc,
            "mp3" | "mp2" | "mp1" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "m4b"
            | "aac" | "opus" | "aiff" | "aif" | "aifc" | "caf" | "mka" | "spx" | "wma" | "amr"
            | "ape" | "wv" => {
                FileKind::Audio
            }
            // 3D models: `.obj` clusters by its rendered shape (CLIP); the rest group under 3D Models/ and are named by Deep Analyze. Lockstep with macOS FileTypes.models.
            "obj" | "stl" | "ply" | "glb" | "gltf" | "fbx" | "usdz" | "usd" | "usda"
            | "usdc" | "dae" | "3mf" | "3ds" | "off" => FileKind::Model,
            _ => FileKind::Other,
        }
    }
}

/// `SF_DATALESS`: macOS sets it on a file whose contents live only in iCloud (or another File Provider); reading it downloads the file.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const SF_DATALESS: u32 = 0x4000_0000;

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn flags_are_dataless(st_flags: u32) -> bool {
    st_flags & SF_DATALESS != 0
}

/// macOS leaves a cloud-only file out of the scan (the Swift engine's rule) and reports the count; Windows catalogs it from its metadata.
const PLACEHOLDERS_LEFT_OUT: bool = cfg!(target_os = "macos");

#[derive(Debug, PartialEq, Eq)]
enum PlaceholderAction {
    Process,
    LeaveOut,
    AlreadyRecorded,
}

fn placeholder_action(
    online_only: bool,
    left_out: bool,
    recorded: Option<&(i64, Option<f64>)>,
    size: u64,
    modified: f64,
) -> PlaceholderAction {
    if !online_only {
        return PlaceholderAction::Process;
    }
    if left_out {
        return PlaceholderAction::LeaveOut;
    }
    match recorded {
        Some(&(db_size, Some(db_modified))) if db_size == size as i64 && (db_modified - modified).abs() < 0.000_001 => {
            PlaceholderAction::AlreadyRecorded
        }
        _ => PlaceholderAction::Process,
    }
}

pub(crate) const KNOWN_PLACEHOLDER_SQL: &str = "SELECT path_text, size_bytes, modified_at FROM files \
     WHERE failed = 0 AND content_hash IS NULL \
     AND kind IN ('image', 'video', 'pdf', 'doc', 'audio')";

/// Cloud placeholder detection: OneDrive / generic Files-On-Demand mark dehydrated files with these attributes and macOS marks iCloud / File Provider files `SF_DATALESS`; touching their content forces a download.
pub(crate) fn is_online_only(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
        const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
        const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
        metadata.file_attributes()
            & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
            != 0
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        flags_are_dataless(metadata.st_flags())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = metadata;
        false
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub kind: FileKind,
    pub size_bytes: u64,
    /// Used for incremental rescans: files unchanged since their `scanned_at` row are skipped.
    pub modified_unix: f64,
    /// Unix seconds when the platform exposes a birth time (for `files.created_at`, macOS parity); `None` when the filesystem records none or the metadata read failed.
    pub created_unix: Option<f64>,
    /// True for a cloud placeholder (OneDrive Files-On-Demand `OFFLINE` / `RECALL_ON_*`). Reading its content would hydrate it over the network (a surprise download, the only non-model egress), so the pipeline does a metadata-only pass. Windows only: macOS leaves such a file out and Linux has no placeholder flag.
    pub online_only: bool,
    /// Volume-local file identity (NTFS MFT reference on Windows; inode on POSIX) the dbwriter's rename/move heal uses to re-bind a moved file's row instead of orphaning it. `None` when the metadata open failed; the heal then falls through to content_hash. Always `None` on non-Windows for now.
    pub file_ref: Option<u64>,
    /// The catalog already holds this exact file (same file id, size and date) at a path that no longer exists: it moved or was renamed, so its row is re-bound and nothing is read, decoded or analysed again.
    pub moved_from_known_row: bool,
}

/// Successfully scanned rows by volume-local file id, for spotting a file that only moved (the Swift engine's `Discovery.isMoveOfKnownRow`). Holds 32 bytes per row; paths are read from the catalog only for a candidate.
pub(crate) struct MoveIndex {
    rows: Vec<(u64, i64, f64, i64)>,
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl MoveIndex {
    const CANDIDATE_LIMIT: usize = 8;

    pub(crate) fn load(conn: &Arc<Mutex<rusqlite::Connection>>) -> Option<Arc<Self>> {
        let mut rows: Vec<(u64, i64, f64, i64)> = {
            let db = conn.lock();
            let mut stmt = db
                .prepare(
                    "SELECT file_ref, size_bytes, modified_at, id FROM files \
                     WHERE failed = 0 AND file_ref IS NOT NULL AND modified_at IS NOT NULL",
                )
                .ok()?;
            stmt.query_map([], |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?, r.get(2)?, r.get(3)?)))
                .ok()?
                .flatten()
                .collect()
        };
        if rows.is_empty() {
            return None;
        }
        rows.sort_unstable_by_key(|row| row.0);
        rows.shrink_to_fit();
        Some(Arc::new(Self { rows, conn: conn.clone() }))
    }

    fn candidates(&self, file_ref: u64, size: u64, modified: f64) -> Vec<i64> {
        let start = self.rows.partition_point(|row| row.0 < file_ref);
        self.rows[start..]
            .iter()
            .take_while(|row| row.0 == file_ref)
            .filter(|row| row.1 == size as i64 && (row.2 - modified).abs() < 0.000_001)
            .map(|row| row.3)
            .take(Self::CANDIDATE_LIMIT)
            .collect()
    }

    /// True when no row sits at `path` and a successfully scanned row with the same file id, size and date sits at a path that is gone from disk. Any error means "not a move".
    pub(crate) fn is_move(&self, path: &str, file_ref: u64, size: u64, modified: f64) -> bool {
        let ids = self.candidates(file_ref, size, modified);
        if ids.is_empty() {
            return false;
        }
        let old_paths: Vec<String> = {
            let db = self.conn.lock();
            let occupied = db
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM files WHERE path_text = ?1)")
                .and_then(|mut stmt| stmt.query_row([path], |r| r.get::<_, bool>(0)))
                .unwrap_or(true);
            if occupied {
                return false;
            }
            let Ok(mut stmt) = db.prepare_cached(
                "SELECT path_text FROM files \
                 WHERE id = ?1 AND file_ref = ?2 AND size_bytes = ?3 AND failed = 0 AND path_text != ?4",
            ) else {
                return false;
            };
            ids.iter()
                .filter_map(|id| {
                    stmt.query_row(rusqlite::params![id, file_ref as i64, size as i64, path], |r| r.get(0)).ok()
                })
                .collect()
        };
        old_paths.iter().any(|old| path_is_gone(old))
    }
}

/// `lstat` says the path does not exist. A dangling symlink is present, and a dead mount (any other error) proves nothing.
fn path_is_gone(path: &str) -> bool {
    let long = crate::util::path_safety::to_extended_length(std::path::Path::new(path));
    matches!(std::fs::symlink_metadata(long), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

pub struct Discovery {
    root: PathBuf,
    coordinator: ScanCoordinator,
    /// Paths already scanned-and-current (DB `scanned_at >= modified_unix`), silently skipped so a re-run of a 1M-file corpus is near-instant. Empty = full scan.
    skip_paths: Arc<HashMap<SkipFingerprint, (i64, Option<f64>)>>,
    known_placeholders: Arc<HashMap<SkipFingerprint, (i64, Option<f64>)>>,
    moves: Option<Arc<MoveIndex>>,
    /// User-excluded folders in `normalize_for_exclusion` form. The walk prunes an excluded directory at its parent's read_dir, so equality matching suffices.
    exclusions: Arc<HashSet<String>>,
}

/// Handle returned from `Discovery::spawn`. `count` is a live counter the orchestrator polls for Progress events; `done` flips the instant the walker exits, so an empty folder (count stays 0) surfaces "no supported files found" instead of hanging.
pub struct DiscoveryHandle {
    pub rx: mpsc::Receiver<DiscoveredFile>,
    pub zero_byte_rx: mpsc::Receiver<ZeroByteObservation>,
    pub count: Arc<AtomicU64>,
    /// Subset of `count` whose kind runs the expensive vision path (Image/Video/Model). Kind-aware ETA needs the remaining-heavy denominator; a blended files/sec swings by an order of magnitude on mixed libraries.
    pub heavy_count: Arc<AtomicU64>,
    pub done: Arc<AtomicBool>,
    /// Stable path hashes of every physically present eligible or skipped file seen by the walk, including zero-byte files.
    pub seen_paths: Arc<Mutex<Vec<i64>>>,
    /// Every directory the walk read, re-statted afterwards to notice files added, renamed or deleted while it ran.
    pub seen_dirs: Arc<Mutex<Vec<PathBuf>>>,
    /// Walk errors swallowed during traversal, surfaced as a non-fatal `discovery_partial` event.
    pub error_count: Arc<AtomicU64>,
    pub cloud_only_count: Arc<AtomicU64>,
    /// Retained by the scan orchestrator so a panic in the blocking walker becomes a failed scan, not an apparently successful empty one.
    pub join: tokio::task::JoinHandle<()>,
}

struct ClassifiedEntry {
    discovered: Option<DiscoveredFile>,
    zero_byte: Option<ZeroByteObservation>,
    seen_path_hash: Option<i64>,
}

struct WalkContext {
    skip_paths: Arc<HashMap<SkipFingerprint, (i64, Option<f64>)>>,
    known_placeholders: Arc<HashMap<SkipFingerprint, (i64, Option<f64>)>>,
    moves: Option<Arc<MoveIndex>>,
    error_count: Arc<AtomicU64>,
    cloud_only_count: Arc<AtomicU64>,
    moved_count: Arc<AtomicU64>,
}

impl WalkContext {
    #[cfg(test)]
    fn for_skip(skip_paths: &HashMap<SkipFingerprint, (i64, Option<f64>)>) -> Self {
        Self {
            skip_paths: Arc::new(skip_paths.clone()),
            known_placeholders: Arc::new(HashMap::new()),
            moves: None,
            error_count: Arc::new(AtomicU64::new(0)),
            cloud_only_count: Arc::new(AtomicU64::new(0)),
            moved_count: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Discovery {
    /// The caller loads the "already current" set from the DB before spawning; a `rescan: true` IPC flag passes an empty set. `exclusions` are user-excluded folders resolved against the root (see `path_safety::resolve_exclusions`), normalized.
    pub fn new_with_skip_and_exclusions(
        root: impl Into<PathBuf>,
        coordinator: ScanCoordinator,
        skip_paths: Arc<HashMap<SkipFingerprint, (i64, Option<f64>)>>,
        exclusions: Arc<Vec<String>>,
    ) -> Self {
        Self {
            root: root.into(),
            coordinator,
            skip_paths,
            known_placeholders: Arc::new(HashMap::new()),
            moves: None,
            exclusions: Arc::new(exclusions.iter().cloned().collect()),
        }
    }

    pub(crate) fn with_move_index(mut self, moves: Option<Arc<MoveIndex>>) -> Self {
        self.moves = moves;
        self
    }

    pub(crate) fn with_known_placeholders(
        mut self,
        placeholders: HashMap<SkipFingerprint, (i64, Option<f64>)>,
    ) -> Self {
        self.known_placeholders = Arc::new(placeholders);
        self
    }

    /// Spawns a tokio blocking task (jwalk's iterator is sync); closes the channel when traversal completes OR cancellation is requested.
    pub fn spawn(self) -> DiscoveryHandle {
        let channel_cap = discovery_channel_cap(crate::platform::memory_tier());
        let (tx, rx) = mpsc::channel(channel_cap);
        let (zero_byte_tx, zero_byte_rx) = mpsc::channel(ZERO_BYTE_CHANNEL_CAP);
        let root = self.root.clone();
        let coordinator = self.coordinator.clone();
        let skip_paths = self.skip_paths.clone();
        let exclusions = self.exclusions.clone();
        let count = Arc::new(AtomicU64::new(0));
        let heavy_count = Arc::new(AtomicU64::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let error_count = Arc::new(AtomicU64::new(0));
        let cloud_only_count = Arc::new(AtomicU64::new(0));
        let moved_count = Arc::new(AtomicU64::new(0));
        let context = Arc::new(WalkContext {
            skip_paths: skip_paths.clone(),
            known_placeholders: self.known_placeholders.clone(),
            moves: self.moves.clone(),
            error_count: error_count.clone(),
            cloud_only_count: cloud_only_count.clone(),
            moved_count: moved_count.clone(),
        });
        let seen_paths = Arc::new(Mutex::new(Vec::new()));
        let seen_dirs: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_dirs_inner = seen_dirs.clone();
        let count_inner = count.clone();
        let heavy_count_inner = heavy_count.clone();
        let done_inner = done.clone();
        let error_count_inner = error_count.clone();
        let seen_paths_inner = seen_paths.clone();

        if !skip_paths.is_empty() {
            tracing::info!(
                skip = skip_paths.len(),
                "[DISCOVERY] skipping N already-scanned files (rescan=false)"
            );
        }

        // Test gate: cap files yielded so a staging run (e.g. N=100) can't wedge the system. Unset / 0 → no cap.
        let test_file_cap: u64 = std::env::var("FILEID_TEST_FILE_CAP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if test_file_cap > 0 {
            tracing::warn!(cap = test_file_cap, "[DISCOVERY] FILEID_TEST_FILE_CAP set; will stop discovery after N files");
        }

        let walk_threads = crate::platform::walk_concurrency_for(&root);
        let storage = crate::platform::storage_type_for_path(&root);
        tracing::info!(
            walk_threads,
            storage = storage.as_str(),
            channel_cap,
            "[DISCOVERY] adaptive parallel walk"
        );

        let join = tokio::task::spawn_blocking(move || {
            // jwalk runs process_read_dir on the rayon pool: directory pruning and the per-file stat/file_ref open happen in parallel, and the consumer loop only sends the precomputed payload in traversal order.
            let coord_for_dir = coordinator.clone();
            let context_for_dir = context.clone();
            let excl_for_dir = exclusions.clone();
            let seen_for_dir = seen_paths_inner.clone();
            let dirs_for_dir = seen_dirs_inner.clone();
            // Walk a verbatim ("\\?\") root so directories past MAX_PATH (260) are traversable (std/jwalk silently fail on them; the engine .exe has no long-path manifest). Children inherit the prefix and it is stripped on emit. Falls back to the plain root if the converted form isn't statable.
            let verbatim_root = crate::util::path_safety::to_extended_length(&root);
            let walk_root = if std::fs::metadata(&verbatim_root).is_ok() {
                verbatim_root
            } else {
                root.clone()
            };
            let walker = jwalk::WalkDirGeneric::<(
                (),
                (Option<DiscoveredFile>, Option<ZeroByteObservation>),
            )>::new(&walk_root)
                .follow_links(false)
                .skip_hidden(false) // own dot-file filter also catches thumbs.db etc.
                .parallelism(jwalk::Parallelism::RayonNewPool(walk_threads))
                .process_read_dir(move |_depth, dir_path, _state, children| {
                    if coord_for_dir.is_cancelled() {
                        children.clear();
                        return;
                    }
                    dirs_for_dir.lock().push(dir_path.to_path_buf());
                    // A directory looks like a dev tree only if a build-system marker sits directly in it; this gates the ambiguous build-dir names (target/build/obj/bin/venv) so a user's ordinary "build" or "bin" folder isn't pruned. has_build_marker costs a lowercase alloc per file, so it is read only when an ambiguous build dir is present.
                    let has_ambiguous_build_dir = children.iter().any(|res| {
                        res.as_ref()
                            .ok()
                            .map(|e| {
                                e.file_type().is_dir()
                                    && is_ambiguous_build_directory(&e.file_name().to_string_lossy())
                            })
                            .unwrap_or(false)
                    });
                    let has_build_marker = has_ambiguous_build_dir
                        && children.iter().any(|res| {
                            res.as_ref()
                                .ok()
                                .map(|e| {
                                    e.file_type().is_file()
                                        && is_build_marker(&e.file_name().to_string_lossy())
                                })
                                .unwrap_or(false)
                        });
                    children.retain(|res| {
                        let Ok(entry) = res.as_ref() else { return true; };
                        let file_type = entry.file_type();
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        if file_type.is_dir() {
                            if is_noise_directory(&name) {
                                return false;
                            }
                            // User exclusions: normalized equality on the child's full path. dir_path carries the walk root's "\\?\" prefix when present; normalize strips it so verbatim and plain roots match the same stored exclusion.
                            if !excl_for_dir.is_empty() {
                                let full = dir_path.join(entry.file_name());
                                let norm = crate::util::path_safety::normalize_for_exclusion_match(&full);
                                if excl_for_dir.contains(&norm) {
                                    return false;
                                }
                            }
                            if is_ambiguous_build_directory(&name) {
                                return !has_build_marker;
                            }
                            true
                        } else if file_type.is_file() {
                            !is_noise_file(&name)
                        } else {
                            false
                        }
                    });
                    // Parallel stage: per-file stat + file_ref open on the rayon pool, stored in client_state. Fingerprints merge once per directory to avoid a global lock per file in million-file incremental walks.
                    let mut seen_in_directory = Vec::new();
                    for res in children.iter_mut() {
                        let Ok(entry) = res.as_mut() else { continue };
                        if !entry.file_type().is_file() {
                            continue;
                        }
                        let entry_path = entry.path();
                        let classified = classify_entry_in(&entry_path, &context_for_dir);
                        if let Some(path_hash) = classified.seen_path_hash {
                            seen_in_directory.push(path_hash);
                        }
                        entry.client_state = (classified.discovered, classified.zero_byte);
                    }
                    if !seen_in_directory.is_empty() {
                        seen_for_dir.lock().extend(seen_in_directory);
                    }
                });

            for entry in walker {
                if coordinator.is_cancelled() {
                    break;
                }
                if test_file_cap > 0 && count_inner.load(Ordering::Relaxed) >= test_file_cap {
                    break;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(error) => {
                        error_count_inner.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            path = %error
                                .path()
                                .map(crate::platform::redact_path_for_log)
                                .unwrap_or_default(),
                            kind = ?error.io_error().map(std::io::Error::kind),
                            "[DISCOVERY] could not read a directory entry (permission denied, \
                             removed during the scan, or a stale network handle); skipping it"
                        );
                        continue;
                    }
                };
                // jwalk stores a failed read_dir on the directory's own entry instead of yielding an Err item. Count it, or an unreadable directory passes as a clean walk and reconciliation soft-hides every catalogued file under it.
                if let Some(error) = entry.read_children_error.as_ref() {
                    error_count_inner.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        path = %crate::platform::redact_path_for_log(entry.path()),
                        kind = ?error.io_error().map(std::io::Error::kind),
                        "[DISCOVERY] could not list a directory (permission denied, \
                         removed during the scan, or a stale network handle); its files \
                         are left as they were in the catalog"
                    );
                }
                // Zero-byte observations use their own bounded path so they never enter decode/model work or inflate content-work totals.
                let (discovered, zero_byte) = entry.client_state;
                if let Some(observation) = zero_byte {
                    if zero_byte_tx.blocking_send(observation).is_err() {
                        break;
                    }
                    continue;
                }
                let Some(discovered) = discovered else { continue };

                // Count before the send so the "Discovered N" counter tracks the walk even when the channel backs up. The count and send stay on this thread so the test cap stop is exact.
                count_inner.fetch_add(1, Ordering::Relaxed);
                if matches!(
                    discovered.kind,
                    FileKind::Image | FileKind::Video | FileKind::Model
                ) {
                    heavy_count_inner.fetch_add(1, Ordering::Relaxed);
                }

                // blocking_send applies backpressure when the channel fills; on corpora that fit the cap it never trips, so the walk and `done` complete at filesystem speed. It errors only if the receiver dropped (shutdown); break out cleanly.
                if tx.blocking_send(discovered).is_err() {
                    break;
                }
            }
            let (cloud_only, moved) =
                (context.cloud_only_count.load(Ordering::Relaxed), context.moved_count.load(Ordering::Relaxed));
            if cloud_only > 0 || moved > 0 {
                tracing::info!(cloud_only, moved, "[DISCOVERY] files left out as cloud-only or re-bound as moved");
            }
            done_inner.store(true, Ordering::Release);
        });

        DiscoveryHandle {
            rx,
            zero_byte_rx,
            count,
            heavy_count,
            done,
            seen_paths,
            seen_dirs,
            error_count,
            cloud_only_count,
            join,
        }
    }
}

/// Builds the `DiscoveredFile` payload for one file (stat + `file_ref` open) on the jwalk/rayon pool so these syscalls fan out instead of serializing on the consumer thread. Also returns a stable path hash for reconciliation: already-current supported files are "seen" even though they skip tagging. `entry_path` is the verbatim-prefixed walk path; FS opens reconvert via `to_extended_length`.
#[cfg(test)]
fn classify_entry(
    entry_path: &std::path::Path,
    skip_paths: &HashMap<SkipFingerprint, (i64, Option<f64>)>,
    error_count: &AtomicU64,
) -> ClassifiedEntry {
    let context = WalkContext::for_skip(skip_paths);
    let classified = classify_entry_in(entry_path, &context);
    error_count.fetch_add(context.error_count.load(Ordering::Relaxed), Ordering::Relaxed);
    classified
}

fn classify_entry_in(entry_path: &std::path::Path, context: &WalkContext) -> ClassifiedEntry {
    let skip_paths = context.skip_paths.as_ref();
    let error_count = context.error_count.as_ref();
    // Strip the verbatim prefix: this is the form stored, displayed and compared against the skip-set. FS access reconverts at the open site.
    let path = crate::util::path_safety::strip_extended_length(entry_path);
    // A name that doesn't round-trip through Unicode (an unpaired UTF-16 surrogate on NTFS) would be catalogued under a U+FFFD-mangled path_text that no later action can resolve, and two such names can collapse into one row. Skip it loudly.
    if is_non_unicode_path(&path) {
        tracing::warn!(
            path = %crate::platform::redact_path_for_log(&path),
            "[DISCOVERY] file name is not valid Unicode (unpaired \
             surrogate?) — skipping; rename the file to include it"
        );
        error_count.fetch_add(1, Ordering::Relaxed);
        return ClassifiedEntry {
            discovered: None,
            zero_byte: None,
            seen_path_hash: None,
        };
    }
    // follow_links is false on the walk, so symlink_metadata matches jwalk's own DirEntry::metadata.
    let metadata = match std::fs::symlink_metadata(entry_path) {
        Ok(m) => m,
        Err(error) => {
            tracing::debug!(
                path = %crate::platform::redact_path_for_log(&path),
                kind = ?error.kind(),
                "[DISCOVERY] file vanished or became unreadable during the scan; skipping"
            );
            error_count.fetch_add(1, Ordering::Relaxed);
            return ClassifiedEntry {
                discovered: None,
                zero_byte: None,
                seen_path_hash: None,
            };
        }
    };
    let size = metadata.len();
    // Compute the reconciliation hash BEFORE any eligibility early-return: "seen" means present under the root, not pipeline-eligible, or a clean-walk reconciliation would soft-hide the catalog row of a zero-byte or unsupported file that still exists.
    let seen_path_hash = crate::util::path_safety::stable_path_hash(&path.to_string_lossy());
    if size == 0 {
        return ClassifiedEntry {
            discovered: None,
            zero_byte: Some(ZeroByteObservation {
                path,
                file_ref: crate::platform::file_ref(entry_path),
            }),
            seen_path_hash: Some(seen_path_hash),
        };
    }
    let fingerprint = path_fingerprint(&path);
    let modified = metadata
        .modified()
        .ok()
        .and_then(crate::util::catalog_unix_seconds)
        .unwrap_or(0.0);
    // Incremental skip: drop a file only when its DB row is current (same size and on-disk mtime), mirroring macOS Discovery.isAlreadyCurrent. A skipped row's modified_at is never refreshed, so path-only membership would strand edited files.
    if let Some(&(db_size, db_modified)) = skip_paths.get(&fingerprint) {
        let unchanged = db_size == size as i64
            && match db_modified {
                Some(m) => (m - modified).abs() < 0.000_001,
                None => false,
            };
        if unchanged {
            return ClassifiedEntry {
                discovered: None,
                zero_byte: None,
                seen_path_hash: Some(seen_path_hash),
            };
        }
    }
    let created_unix = metadata.created().ok().and_then(crate::util::catalog_unix_seconds);

    let online_only = is_online_only(&metadata);

    let mut kind = path
        .extension()
        .and_then(|e| e.to_str())
        .map(FileKind::from_extension)
        .unwrap_or(FileKind::Other);
    if kind == FileKind::Doc {
        let extension = path_extension_lower(entry_path);
        if extension.as_deref() == Some("ts") && file_is_mpeg_transport_stream(entry_path) {
            kind = FileKind::Video;
        } else if extension.as_deref() == Some("json")
            && entry_path.file_name().is_some_and(|n| is_takeout_sidecar(&n.to_string_lossy()))
        {
            return ClassifiedEntry {
                discovered: None,
                zero_byte: None,
                seen_path_hash: Some(seen_path_hash),
            };
        }
    }
    if kind == FileKind::Other && !is_cataloged_other(path_extension_lower(entry_path).as_deref()) {
        // Present but never cataloged (unfinished downloads, scratch and SQLite temp files): kept in `seen` so a row whose kind mapping changed across versions isn't soft-hidden while the file exists.
        return ClassifiedEntry {
            discovered: None,
            zero_byte: None,
            seen_path_hash: Some(seen_path_hash),
        };
    }

    // Reading a cloud-only file downloads it. macOS leaves it out (an existing row stays since the path is still seen); a placeholder Windows already catalogued is rewritten only when its state changes.
    match placeholder_action(
        online_only,
        PLACEHOLDERS_LEFT_OUT,
        context.known_placeholders.get(&fingerprint),
        size,
        modified,
    ) {
        PlaceholderAction::Process => {}
        action => {
            if action == PlaceholderAction::LeaveOut {
                context.cloud_only_count.fetch_add(1, Ordering::Relaxed);
            }
            return ClassifiedEntry {
                discovered: None,
                zero_byte: None,
                seen_path_hash: Some(seen_path_hash),
            };
        }
    }

    // Volume-local file id so the dbwriter can heal a moved file's row instead of recomputing its tags: a cheap metadata-only open via `FILE_FLAG_BACKUP_SEMANTICS`; `None` on permission/race errors.
    let file_ref = crate::platform::file_ref(entry_path);
    let moved_from_known_row = match (file_ref, context.moves.as_deref()) {
        (Some(file_ref), Some(moves)) if !online_only => {
            moves.is_move(&path.to_string_lossy(), file_ref, size, modified)
        }
        _ => false,
    };
    if moved_from_known_row {
        context.moved_count.fetch_add(1, Ordering::Relaxed);
    }
    ClassifiedEntry {
        discovered: Some(DiscoveredFile {
            path,
            kind,
            size_bytes: size,
            modified_unix: modified,
            created_unix,
            online_only,
            file_ref,
            moved_from_known_row,
        }),
        zero_byte: None,
        seen_path_hash: Some(seen_path_hash),
    }
}

/// A transport stream is 188-byte packets that each open with 0x47; TypeScript source never repeats that byte at those offsets.
fn file_is_mpeg_transport_stream(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 188 * 3 + 1];
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return false,
        }
    }
    filled > 188 * 2 && head[0] == 0x47 && head[188] == 0x47 && head[376] == 0x47
}

/// Google Takeout writes `<photo>.<ext>.json`, `<photo>.<ext>.supplemental-metadata.json` (the suffix is cut to fit the name length limit, so any prefix of it counts) and `<photo>.<ext>(1).json` beside each photo or video. They describe a picture, not a document.
fn is_takeout_sidecar(name: &str) -> bool {
    const SUPPLEMENTAL: &str = ".supplemental-metadata";
    let lower = name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".json") else { return false };
    let strip_counter = |text: &str| -> String {
        if let Some(open) = text.rfind('(') {
            let digits = &text[open + 1..];
            if let Some(number) = digits.strip_suffix(')')
                && !number.is_empty()
                && number.bytes().all(|b| b.is_ascii_digit())
            {
                return text[..open].to_string();
            }
        }
        text.to_string()
    };
    let mut stem = strip_counter(stem);
    if let Some(dot) = stem.rfind(".s") {
        let tail = &stem[dot..];
        if tail.len() >= 2 && SUPPLEMENTAL.starts_with(tail) {
            stem.truncate(dot);
        }
    }
    let stem = strip_counter(&stem);
    let Some((_, extension)) = stem.rsplit_once('.') else { return false };
    matches!(
        FileKind::from_extension(extension),
        FileKind::Image | FileKind::Video
    )
}

/// Other files (archives, installers, unknown types) are cataloged with name, size, dates and a content hash so they count, search and appear in Duplicates, except these transient ones. Mirrors the Swift `FileTypes.neverCataloged`.
fn is_cataloged_other(extension: Option<&str>) -> bool {
    !matches!(
        extension,
        Some(
            "part" | "partial" | "crdownload" | "download" | "opdownload" | "tmp" | "temp" | "swp" | "swo"
                | "lock" | "sqlite-wal" | "sqlite-shm" | "sqlite-journal" | "db-wal" | "db-shm" | "db-journal"
        )
    )
}

fn path_extension_lower(path: &std::path::Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// True when `to_string_lossy` would mangle this path: the lossy text is the only identity the DB/app layers see, so such a file could never be targeted again.
fn is_non_unicode_path(path: &std::path::Path) -> bool {
    path.to_str().is_none()
}

/// Directory names whose entire subtree is skipped, decided once per directory in `process_read_dir`.
fn is_noise_directory(name: &str) -> bool {
    // Case-insensitive: Windows lists System Volume Information / $RECYCLE.BIN mixed-case at the root.
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "node_modules"
            | ".git"
            | ".svn"
            | ".hg"
            | "__pycache__"
            | "$recycle.bin"
            | "system volume information"
            | ".cache"
            | ".gradle"
            | ".idea"
            | ".fseventsd"
            | ".spotlight-v100"
            | ".trashes"
            | ".temporaryitems"
            | ".documentrevisions-v100"
            | ".pkinstallsandboxmanager"
            | ".trash"
            | "lost+found"
    ) || is_chkdsk_recovery_directory(&lower)
        || is_per_volume_trash_directory(&lower)

}

/// freedesktop per-volume trash: `.Trash-<uid>` at a mount root.
fn is_per_volume_trash_directory(lower: &str) -> bool {
    lower
        .strip_prefix(".trash-")
        .is_some_and(|uid| !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()))
}

/// chkdsk's `FOUND.000`, `FOUND.001`, ... hold recovered `.CHK` fragments, not user files.
fn is_chkdsk_recovery_directory(lower: &str) -> bool {
    lower
        .strip_prefix("found.")
        .is_some_and(|n| n.len() == 3 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Generic names that are build artifacts ONLY inside a dev tree. They double as ordinary folder names (a photographer's "build" set, a "bin" of scans), so the caller prunes them only when a build-system marker sits alongside; otherwise a user's files would vanish from the app.
fn is_ambiguous_build_directory(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "target" | "build" | "obj" | "bin" | "venv" | ".venv"
    )
}

fn is_build_marker(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "cargo.toml"
            | "cmakelists.txt"
            | "makefile"
            | "package.json"
            | "pyproject.toml"
            | "setup.py"
            | "requirements.txt"
            | "pyvenv.cfg"
    ) {
        return true;
    }
    // Compared via the extension (case-insensitively) because clippy's case_sensitive_file_extension_comparisons rejects `ends_with(".sln")` under -D warnings.
    std::path::Path::new(name).extension().is_some_and(|e| {
        let e = e.to_string_lossy();
        e.eq_ignore_ascii_case("sln")
            || e.eq_ignore_ascii_case("csproj")
            || e.eq_ignore_ascii_case("vcxproj")
    })
}

/// Per-file noise filter, separate from `is_noise_directory` because file-vs-dir semantics differ (some libraries live under `.local/share/...`).
fn is_noise_file(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "thumbs.db" | "desktop.ini" | "ehthumbs.db" | "ehthumbs_vista.db"
            | ".ds_store" | "icon\r"
    )
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_extension_image_set() {
        assert_eq!(FileKind::from_extension("jpg"), FileKind::Image);
        assert_eq!(FileKind::from_extension("HEIC"), FileKind::Image);
        assert_eq!(FileKind::from_extension("png"), FileKind::Image);
    }

    fn move_catalog() -> Arc<Mutex<rusqlite::Connection>> {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path_text TEXT UNIQUE, size_bytes INTEGER, \
             modified_at DOUBLE, failed INTEGER NOT NULL DEFAULT 0, file_ref INTEGER);",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn insert_row(conn: &Arc<Mutex<rusqlite::Connection>>, path: &str, size: i64, modified: f64, failed: i64, file_ref: u64) {
        conn.lock()
            .execute(
                "INSERT INTO files (path_text, size_bytes, modified_at, failed, file_ref) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![path, size, modified, failed, file_ref as i64],
            )
            .unwrap();
    }

    #[test]
    fn move_index_is_absent_for_a_catalog_without_file_ids() {
        let conn = move_catalog();
        assert!(MoveIndex::load(&conn).is_none());
        conn.lock().execute("INSERT INTO files (path_text, size_bytes, modified_at) VALUES ('/a', 1, 1.0)", []).unwrap();
        assert!(MoveIndex::load(&conn).is_none());
    }

    #[test]
    fn a_move_needs_the_same_file_id_size_and_date_and_a_vanished_old_path() {
        let dir = std::env::temp_dir().join(format!("fileid-disc-move-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let still_here = dir.join("still-here.jpg");
        std::fs::write(&still_here, b"x").unwrap();
        let gone = dir.join("gone.jpg").to_string_lossy().into_owned();
        let new_path = dir.join("renamed.jpg").to_string_lossy().into_owned();

        let conn = move_catalog();
        insert_row(&conn, &gone, 100, 50.0, 0, 7);
        insert_row(&conn, &still_here.to_string_lossy(), 200, 60.0, 0, 8);
        insert_row(&conn, &dir.join("failed-before.jpg").to_string_lossy(), 300, 70.0, 1, 9);
        insert_row(&conn, &dir.join("high-gone.jpg").to_string_lossy(), 400, 80.0, 0, u64::MAX - 5);
        let index = MoveIndex::load(&conn).expect("rows with file ids");

        assert!(index.is_move(&new_path, 7, 100, 50.0));
        assert!(index.is_move(&new_path, 7, 100, 50.000_000_4), "dates compare within a microsecond");
        assert!(index.is_move(&new_path, u64::MAX - 5, 400, 80.0));
        assert!(!index.is_move(&new_path, 7, 101, 50.0), "a different size is another file on a reused id");
        assert!(!index.is_move(&new_path, 7, 100, 51.0), "a different date is another file on a reused id");
        assert!(!index.is_move(&new_path, 8, 200, 60.0), "the old path still exists: a hard link or copy");
        assert!(!index.is_move(&new_path, 9, 300, 70.0), "a row that never scanned cleanly is analysed");
        assert!(!index.is_move(&new_path, 77, 100, 50.0));
        assert!(!index.is_move(&gone, 7, 100, 50.0), "a row already sits at this path");

        conn.lock().execute("UPDATE files SET failed = 1 WHERE file_ref = 7", []).unwrap();
        assert!(!index.is_move(&new_path, 7, 100, 50.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_renamed_file_is_discovered_as_a_move_and_a_new_file_is_not() {
        let dir = std::env::temp_dir().join(format!("fileid-disc-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("old.jpg");
        std::fs::write(&old, b"jpegbytes").unwrap();
        let before = classify_entry(&old, &HashMap::new(), &AtomicU64::new(0)).discovered.unwrap();
        assert!(!before.moved_from_known_row);

        let conn = move_catalog();
        insert_row(&conn, &old.to_string_lossy(), 9, before.modified_unix, 0, before.file_ref.unwrap());
        let renamed = dir.join("renamed.jpg");
        std::fs::rename(&old, &renamed).unwrap();
        let fresh = dir.join("fresh.jpg");
        std::fs::write(&fresh, b"jpegbytes").unwrap();

        let mut context = WalkContext::for_skip(&HashMap::new());
        context.moves = MoveIndex::load(&conn);
        let moved = classify_entry_in(&renamed, &context).discovered.unwrap();
        assert!(moved.moved_from_known_row);
        assert!(!classify_entry_in(&fresh, &context).discovered.unwrap().moved_from_known_row);
        assert_eq!(context.moved_count.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dataless_flag_marks_a_cloud_only_file() {
        assert!(flags_are_dataless(0x4000_0000));
        assert!(flags_are_dataless(0x4000_0020));
        assert!(!flags_are_dataless(0));
        assert!(!flags_are_dataless(0x0000_8020), "hidden + compressed is a local file");
    }

    #[test]
    fn local_files_are_never_online_only() {
        let dir = std::env::temp_dir().join(format!("fileid-disc-local-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("local.jpg");
        std::fs::write(&file, b"bytes").unwrap();
        assert!(!is_online_only(&std::fs::symlink_metadata(&file).unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn placeholders_are_left_out_on_mac_and_written_once_elsewhere() {
        use PlaceholderAction::{AlreadyRecorded, LeaveOut, Process};
        let recorded = (10_i64, Some(100.0));
        assert_eq!(placeholder_action(false, true, None, 10, 100.0), Process);
        assert_eq!(placeholder_action(false, false, Some(&recorded), 10, 100.0), Process, "downloaded since: analyse it");
        assert_eq!(placeholder_action(true, true, None, 10, 100.0), LeaveOut);
        assert_eq!(placeholder_action(true, true, Some(&recorded), 10, 100.0), LeaveOut);
        assert_eq!(placeholder_action(true, false, None, 10, 100.0), Process, "first sighting writes its metadata row");
        assert_eq!(placeholder_action(true, false, Some(&recorded), 10, 100.0), AlreadyRecorded);
        assert_eq!(placeholder_action(true, false, Some(&recorded), 11, 100.0), Process);
        assert_eq!(placeholder_action(true, false, Some(&recorded), 10, 101.0), Process);
        assert_eq!(placeholder_action(true, false, Some(&(10, None)), 10, 100.0), Process);
    }

    #[test]
    fn known_placeholder_rows_are_metadata_only_content_kinds() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (path_text TEXT, kind TEXT, size_bytes INTEGER, modified_at DOUBLE, failed INTEGER, content_hash BLOB);
             INSERT INTO files VALUES ('/c/placeholder.jpg', 'image', 5, 1.0, 0, NULL);
             INSERT INTO files VALUES ('/c/local.jpg', 'image', 5, 1.0, 0, X'00');
             INSERT INTO files VALUES ('/c/failed.jpg', 'image', 5, 1.0, 1, NULL);
             INSERT INTO files VALUES ('/c/archive.bin', 'other', 5, 1.0, 0, NULL);",
        )
        .unwrap();
        let rows: Vec<String> = conn
            .prepare(KNOWN_PLACEHOLDER_SQL)
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec!["/c/placeholder.jpg".to_string()]);
    }

    #[test]
    fn kind_from_extension_unknown_is_other() {
        assert_eq!(FileKind::from_extension("xyz"), FileKind::Other);
    }

    /// Zero-byte and unsupported-kind files are skipped by the pipeline but still PRESENT, so they must stay in the reconciliation seen-set or a clean walk soft-hides their rows.
    #[test]
    fn skipped_but_present_files_stay_in_the_seen_set() {
        let dir = std::env::temp_dir().join(format!("fileid-disc-seen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zero = dir.join("truncated.jpg");
        std::fs::write(&zero, b"").unwrap();
        let other = dir.join("movie.mp4.part");
        std::fs::write(&other, b"payload").unwrap();
        let archive = dir.join("notes.xyz");
        std::fs::write(&archive, b"payload").unwrap();

        let skips: HashMap<SkipFingerprint, (i64, Option<f64>)> = HashMap::new();
        let errs = AtomicU64::new(0);

        let z = classify_entry(&zero, &skips, &errs);
        assert!(z.discovered.is_none(), "zero-byte file must not be catalogued");
        assert!(z.zero_byte.is_some(), "zero-byte file needs a dormant-row observation");
        assert!(z.seen_path_hash.is_some(), "zero-byte file is present and must be seen");

        let o = classify_entry(&other, &skips, &errs);
        assert!(o.discovered.is_none(), "an unfinished download must not be catalogued");
        assert!(o.zero_byte.is_none());
        assert!(o.seen_path_hash.is_some(), "a skipped file is present and must be seen");
        let a = classify_entry(&archive, &skips, &errs);
        assert_eq!(a.discovered.map(|f| f.kind), Some(FileKind::Other), "unknown types are catalogued as other");

        assert_eq!(errs.load(std::sync::atomic::Ordering::Relaxed), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kind_from_extension_audio_and_model() {
        assert_eq!(FileKind::from_extension("mp3"), FileKind::Audio);
        assert_eq!(FileKind::from_extension("OGG"), FileKind::Audio);
        assert_eq!(FileKind::from_extension("obj"), FileKind::Model);
        assert_eq!(FileKind::from_extension("OBJ"), FileKind::Model);
        assert_eq!(FileKind::Model.as_str(), "model");
        assert_eq!(FileKind::from_extension("py"), FileKind::Doc);
        assert_eq!(FileKind::from_extension("RS"), FileKind::Doc);
        assert_eq!(FileKind::from_extension("swift"), FileKind::Doc);
        assert_eq!(FileKind::from_extension("epub"), FileKind::Doc);
        assert_eq!(FileKind::from_extension("stl"), FileKind::Model);
        assert_eq!(FileKind::from_extension("glb"), FileKind::Model);
        assert_eq!(FileKind::from_extension("usdz"), FileKind::Model);
        assert_eq!(FileKind::from_extension("wmv"), FileKind::Video);
        assert_eq!(FileKind::from_extension("cr3"), FileKind::Image);
        assert_eq!(FileKind::from_extension("mxf"), FileKind::Video);
        assert_eq!(FileKind::from_extension("aiff"), FileKind::Audio);
        assert_eq!(FileKind::from_extension("caf"), FileKind::Audio);
        assert_eq!(FileKind::from_extension("odt"), FileKind::Doc);
        assert_eq!(FileKind::from_extension("xls"), FileKind::Doc);
        assert_eq!(FileKind::from_extension("ppt"), FileKind::Doc);
        assert_eq!(FileKind::from_extension("bin"), FileKind::Other);
    }

    #[test]
    fn noise_directory_recognized() {
        assert!(is_noise_directory("node_modules"));
        assert!(is_noise_directory(".git"));
        assert!(is_noise_directory("__pycache__"));
        assert!(is_noise_directory("$RECYCLE.BIN"));
        assert!(is_noise_directory("System Volume Information"));
        assert!(is_noise_directory(".Trashes") && is_noise_directory(".Spotlight-V100") && is_noise_directory(".TemporaryItems"));
        assert!(!is_noise_directory("Pictures"));
        assert!(!is_noise_directory("Desktop"));
    }

    #[test]
    fn volume_metadata_directories_are_noise() {
        for name in [".fseventsd", ".Spotlight-V100", ".Trashes", ".TemporaryItems", ".DocumentRevisions-V100", "FOUND.000", "found.012"] {
            assert!(is_noise_directory(name), "{name}");
        }
        assert!(!is_noise_directory("found.photos"));
        assert!(!is_noise_directory("FOUND.0000"));
    }

    #[test]
    fn takeout_sidecars_are_recognised_but_real_json_is_not() {
        for name in [
            "IMG_1234.jpg.json",
            "IMG_1234.JPG.supplemental-metadata.json",
            "IMG_1234.jpg.supplemental-metad.json",
            "IMG_1234.jpg.suppl.json",
            "IMG_1234.jpg.s.json",
            "IMG_1234.jpg(1).json",
            "IMG_1234.jpg.supplemental-metadata(2).json",
            "clip.mp4.json",
            "DSC0001.ARW.json",
        ] {
            assert!(is_takeout_sidecar(name), "{name}");
        }
        for name in ["package.json", "notes.txt.json", "data.json", "IMG_1234.jpg", "IMG.jpg.sample.json", "metadata.json"] {
            assert!(!is_takeout_sidecar(name), "{name}");
        }
    }

    #[test]
    fn mpeg_transport_stream_named_ts_is_video_and_typescript_stays_doc() {
        let tmp = tempdir_or_skip();
        let mut ts = vec![0u8; 188 * 4];
        for packet in 0..4 {
            ts[packet * 188] = 0x47;
        }
        std::fs::write(tmp.path().join("clip.ts"), &ts).unwrap();
        std::fs::write(tmp.path().join("app.ts"), b"export const answer: number = 42;\n".repeat(40)).unwrap();
        std::fs::write(tmp.path().join("IMG_1.jpg"), b"jpeg").unwrap();
        std::fs::write(tmp.path().join("IMG_1.jpg.supplemental-metadata.json"), b"{\"photoTakenTime\":{}}").unwrap();
        std::fs::write(tmp.path().join("settings.json"), b"{}").unwrap();
        let (files, errors) = walk(tmp.path());
        assert_eq!(errors, 0);
        let kind_of = |name: &str| files.iter().find(|f| f.path.file_name().unwrap() == name).map(|f| f.kind);
        assert_eq!(kind_of("clip.ts"), Some(FileKind::Video));
        assert_eq!(kind_of("app.ts"), Some(FileKind::Doc));
        assert_eq!(kind_of("settings.json"), Some(FileKind::Doc));
        assert_eq!(kind_of("IMG_1.jpg"), Some(FileKind::Image));
        assert_eq!(kind_of("IMG_1.jpg.supplemental-metadata.json"), None);
    }

    #[test]
    fn volume_metadata_folders_are_not_walked_or_counted() {
        let tmp = tempdir_or_skip();
        for dir in [".fseventsd", ".Spotlight-V100", ".Trashes/501", "FOUND.000"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        std::fs::write(tmp.path().join(".fseventsd/fseventsd-uuid"), b"u").unwrap();
        std::fs::write(tmp.path().join(".fseventsd/000000000d51393f"), b"journal").unwrap();
        std::fs::write(tmp.path().join(".Trashes/501/gone.jpg"), b"jpeg").unwrap();
        std::fs::write(tmp.path().join("FOUND.000/FILE0000.CHK"), b"fragment").unwrap();
        std::fs::write(tmp.path().join("keep.jpg"), b"jpeg").unwrap();
        let (files, errors) = walk(tmp.path());
        assert_eq!(errors, 0);
        let names: Vec<_> = files.iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["keep.jpg".to_string()]);
    }

    #[test]
    fn noise_directory_case_insensitive() {
        assert!(is_noise_directory("NODE_MODULES"));
        assert!(is_noise_directory("Node_Modules"));
    }

    /// Non-Unicode names (unpaired UTF-16 surrogates on NTFS; raw bytes on POSIX) must be detected so discovery can skip-log them instead of cataloguing a mangled path.
    #[test]
    fn non_unicode_path_detected() {
        assert!(!is_non_unicode_path(std::path::Path::new(
            r"C:\Users\René\Bilder\Café.jpg"
        )));
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let units: Vec<u16> = "C:\\pics\\img"
                .encode_utf16()
                .chain([0xD800, 0x002E, 0x006A, 0x0070, 0x0067]) // unpaired high surrogate + ".jpg"
                .collect();
            let os = std::ffi::OsString::from_wide(&units);
            assert!(is_non_unicode_path(std::path::Path::new(&os)));
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let os = std::ffi::OsString::from_vec(b"/pics/img\xFF.jpg".to_vec());
            assert!(is_non_unicode_path(std::path::Path::new(&os)));
        }
    }

    fn walk(root: &std::path::Path) -> (Vec<DiscoveredFile>, u64) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let coord = ScanCoordinator::new();
            let disc = Discovery::new_with_skip_and_exclusions(
                root,
                coord,
                Arc::new(HashMap::new()),
                Arc::new(Vec::new()),
            );
            let handle = disc.spawn();
            let errors = handle.error_count.clone();
            let mut rx = handle.rx;
            let mut got = Vec::new();
            while let Some(file) = rx.recv().await {
                got.push(file);
            }
            (got, errors.load(Ordering::Relaxed))
        })
    }

    /// macOS keeps volume metadata folders it forbids reading (`.Trashes` is mode 0 to everyone): walking them counts a
    /// discovery error every scan and stops missing-file reconciliation for the whole library.
    #[test]
    #[cfg(unix)]
    fn macos_volume_metadata_folders_are_not_walked_so_a_clean_drive_scans_clean() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir_or_skip();
        let root = tmp.path();
        std::fs::write(root.join("a.jpg"), b"jpeg").unwrap();
        let locked = root.join(".Trashes");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&locked).is_ok();
        let (files, errors) = walk(root);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            return;
        }
        assert_eq!(files.len(), 1);
        assert_eq!(errors, 0, "an unreadable metadata folder must be skipped, not counted");
    }

    #[test]
    #[cfg(unix)]
    fn linux_volume_folders_are_not_walked_and_per_volume_trash_is_not_indexed() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir_or_skip();
        let root = tmp.path();
        std::fs::write(root.join("a.jpg"), b"jpeg").unwrap();
        let trash = root.join(".Trash-1000/files");
        std::fs::create_dir_all(&trash).unwrap();
        std::fs::write(trash.join("gone.jpg"), b"jpeg").unwrap();
        std::fs::create_dir_all(root.join(".Trash/1000")).unwrap();
        std::fs::write(root.join(".Trash/1000/gone2.jpg"), b"jpeg").unwrap();
        std::fs::create_dir_all(root.join(".Trash-keep")).unwrap();
        std::fs::write(root.join(".Trash-keep/mine.jpg"), b"jpeg").unwrap();
        let locked = root.join("lost+found");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&locked).is_ok();
        let (files, errors) = walk(root);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(files.len(), 2, "only a.jpg and the non-uid folder's file are library files");
        if !readable {
            assert_eq!(errors, 0, "an unreadable lost+found must be skipped, not counted");
        }
    }

    /// Symlink loops, unreadable directories, hard links and sparse files must neither hang, panic nor vanish silently from the walk.
    #[test]
    #[cfg(unix)]
    fn unix_edge_cases_walk_to_completion() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let tmp = tempdir_or_skip();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/photo.jpg"), b"jpeg").unwrap();
        std::os::unix::fs::symlink(root.join("a"), root.join("a/b/loop")).unwrap();
        std::os::unix::fs::symlink("missing.jpg", root.join("a/dangling.jpg")).unwrap();
        std::fs::hard_link(root.join("a/b/photo.jpg"), root.join("a/twin.jpg")).unwrap();
        let sparse = std::fs::File::create(root.join("a/huge.mp4")).unwrap();
        sparse.set_len(6 * 1024 * 1024 * 1024).unwrap();
        drop(sparse);
        std::fs::create_dir(root.join("locked")).unwrap();
        std::fs::write(root.join("locked/secret.jpg"), b"jpeg").unwrap();
        std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read_dir(root.join("locked")).is_err();

        let (got, errors) = walk(root);
        std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut names: Vec<String> = got
            .iter()
            .map(|file| file.path.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let expected_locked = if unreadable { vec![] } else { vec!["locked/secret.jpg".to_string()] };
        let mut expected = vec![
            "a/b/photo.jpg".to_string(),
            "a/huge.mp4".to_string(),
            "a/twin.jpg".to_string(),
        ];
        expected.extend(expected_locked);
        expected.sort();
        assert_eq!(names, expected);
        if unreadable {
            assert!(errors >= 1, "an unreadable directory must be counted as a walk error");
        }
        let huge = got.iter().find(|file| file.path.ends_with("huge.mp4")).unwrap();
        assert_eq!(huge.size_bytes, 6 * 1024 * 1024 * 1024);
        let photo = got.iter().find(|file| file.path.ends_with("photo.jpg")).unwrap();
        let twin = got.iter().find(|file| file.path.ends_with("twin.jpg")).unwrap();
        assert_eq!(photo.file_ref, twin.file_ref, "hard links share one file id");
        assert_eq!(
            photo.file_ref,
            Some(std::fs::metadata(root.join("a/twin.jpg")).unwrap().ino())
        );
    }

    /// Linux names are bytes: a non-UTF-8 name can't be addressed through the DB's text paths, so it is skipped and counted, never panics.
    #[test]
    #[cfg(target_os = "linux")]
    fn non_utf8_file_names_are_counted_not_catalogued() {
        use std::os::unix::ffi::OsStrExt;
        let tmp = tempdir_or_skip();
        let root = tmp.path();
        let bad = root.join(std::ffi::OsStr::from_bytes(b"caf\xe9.jpg"));
        std::fs::write(&bad, b"jpeg").unwrap();
        std::fs::create_dir(root.join(std::ffi::OsStr::from_bytes(b"dir\xff"))).unwrap();
        std::fs::write(
            root.join(std::ffi::OsStr::from_bytes(b"dir\xff")).join("inside.jpg"),
            b"jpeg",
        )
        .unwrap();
        std::fs::write(root.join("fine.jpg"), b"jpeg").unwrap();
        let (got, errors) = walk(root);
        let names: Vec<_> = got.iter().map(|file| file.path.clone()).collect();
        assert_eq!(names, vec![root.join("fine.jpg")]);
        assert_eq!(errors, 2);
    }

    #[test]
    fn noise_file_recognized() {
        assert!(is_noise_file("Thumbs.db"));
        assert!(is_noise_file("desktop.ini"));
        assert!(is_cataloged_other(Some("zip")) && is_cataloged_other(None) && is_cataloged_other(Some("rpt")));
        assert!(!is_cataloged_other(Some("part")) && !is_cataloged_other(Some("crdownload")) && !is_cataloged_other(Some("sqlite-wal")));
        assert!(is_noise_file(".DS_Store"));
        assert!(is_noise_file(".hidden"));
        assert!(!is_noise_file("vacation.jpg"));
    }

    /// A parallel walk of a small synthetic tree must visit every supported file, prune noise directories and skip zero-byte files. The throughput benchmark lives in tests/discovery_throughput.rs.
    #[test]
    fn synthetic_tree_walk_prunes_noise_and_counts_files() {
        use std::fs;
        let tmp = tempdir_or_skip();
        let root = tmp.path();

        fs::create_dir_all(root.join("pics")).unwrap();
        for i in 0..30 {
            let p = root.join("pics").join(format!("img_{i:03}.jpg"));
            fs::write(&p, b"jpeg").unwrap();
        }
        fs::write(root.join("pics").join("empty.jpg"), b"").unwrap();
        fs::create_dir_all(root.join(".git").join("objects")).unwrap();
        fs::write(root.join(".git").join("objects").join("hidden.jpg"), b"shouldnotsee").unwrap();
        fs::create_dir_all(root.join("node_modules").join("react")).unwrap();
        fs::write(root.join("node_modules").join("react").join("readme.md"), b"# nope").unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (got, count) = rt.block_on(async {
            let coord = ScanCoordinator::new();
            let disc = Discovery::new_with_skip_and_exclusions(root, coord, Arc::new(HashMap::new()), Arc::new(Vec::new()));
            let handle = disc.spawn();
            let mut rx = handle.rx;
            let mut got = Vec::new();
            while let Some(f) = rx.recv().await {
                got.push(f.path);
            }
            (got, handle.count.load(Ordering::Relaxed))
        });

        assert_eq!(got.len(), 30, "expected 30 supported files; got {got:?}");
        assert!(got.iter().all(|p| !p.to_string_lossy().contains("node_modules")));
        assert!(got.iter().all(|p| !p.to_string_lossy().contains(".git")));
        assert_eq!(count, 30);
    }

    /// User folder exclusions prune the excluded subtree at its parent's read_dir: nothing beneath is emitted, siblings are untouched, exclusion strings match after normalization, and an exclusion outside the root changes nothing.
    #[test]
    fn synthetic_tree_walk_honors_user_exclusions() {
        use std::fs;
        let tmp = tempdir_or_skip();
        let root = tmp.path();

        fs::create_dir_all(root.join("keep")).unwrap();
        fs::create_dir_all(root.join("skipme").join("nested")).unwrap();
        fs::create_dir_all(root.join("skipme2")).unwrap();
        fs::create_dir_all(root.join("skipme_not")).unwrap();
        for i in 0..5 {
            fs::write(root.join("keep").join(format!("k{i}.jpg")), b"jpeg").unwrap();
        }
        fs::write(root.join("skipme").join("s.jpg"), b"jpeg").unwrap();
        fs::write(root.join("skipme").join("nested").join("n.jpg"), b"jpeg").unwrap();
        fs::write(root.join("skipme2").join("s2.jpg"), b"jpeg").unwrap();
        fs::write(root.join("skipme_not").join("boundary.jpg"), b"jpeg").unwrap();

        let sep = std::path::MAIN_SEPARATOR;
        let excl_1 = if cfg!(windows) {
            root.join("SKIPME").to_string_lossy().into_owned()
        } else {
            root.join("skipme").to_string_lossy().into_owned()
        };
        let excl_2 = format!("{}{sep}", root.join("skipme2").to_string_lossy());
        let outside = if cfg!(windows) { r"C:\definitely\not\here".to_string() } else { "/definitely/not/here".to_string() };
        let resolved = crate::util::path_safety::resolve_exclusions(
            root,
            &[excl_1, excl_2, outside],
        );
        assert_eq!(resolved.len(), 2);
        let normalized: Vec<String> = resolved.iter().map(|e| e.normalized.clone()).collect();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let got = rt.block_on(async {
            let coord = ScanCoordinator::new();
            let disc = Discovery::new_with_skip_and_exclusions(
                root,
                coord,
                Arc::new(HashMap::new()),
                Arc::new(normalized),
            );
            let handle = disc.spawn();
            let mut rx = handle.rx;
            let mut got = Vec::new();
            while let Some(f) = rx.recv().await {
                got.push(f.path);
            }
            got
        });

        let names: Vec<String> = got.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(got.len(), 6, "5 keepers + skipme_not boundary file; got {names:?}");
        assert!(names.iter().all(|p| !p.contains("nested")));
        assert!(
            names.iter().any(|p| p.contains("boundary.jpg")),
            "exclusion of 'skipme' must not swallow sibling 'skipme_not'"
        );
    }

    /// `count` MUST be incremented for every file BEFORE the channel send, so a slow receiver can't desync the "Discovered N" counter from the walk.
    #[test]
    fn count_increments_before_channel_send() {
        use std::fs;
        let tmp = tempdir_or_skip();
        let root = tmp.path();
        for i in 0..100 {
            fs::write(root.join(format!("f_{i:03}.jpg")), b"x").unwrap();
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut last = 0u64;
            for _attempt in 0..5 {
                let coord = ScanCoordinator::new();
                let disc = Discovery::new_with_skip_and_exclusions(root, coord, Arc::new(HashMap::new()), Arc::new(Vec::new()));
                let handle = disc.spawn();
                let count = handle.count.clone();
                let done = handle.done.clone();
                let seen_paths = handle.seen_paths.clone();
                let mut rx = handle.rx;
                // Wait for the walk to finish; the budget is generous (15s) because the rayon walk shares the thread pool with the rest of the suite, and a tight 2s poll flaked.
                for _ in 0..750 {
                    if done.load(Ordering::Acquire) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                assert!(done.load(Ordering::Acquire), "discovery walk did not finish within budget");
                // Assert count == 100 without draining the receiver: the 32k channel buffers all 100, proving the counter tracks the walk independent of drain.
                last = count.load(Ordering::Relaxed);
                if last == 100 {
                    let mut drained = 0;
                    while rx.try_recv().is_ok() {
                        drained += 1;
                    }
                    assert_eq!(drained, 100);
                    assert_eq!(seen_paths.lock().len(), 100);
                    return;
                }
            }
            panic!("discovery walk under-enumerated after retries: last count = {last}");
        });
    }

    /// The per-file stat + `file_ref` work must live in a standalone function the parallel walk stage calls, not inline on the single consumer loop; calling it directly yields a fully-populated payload.
    #[test]
    fn classify_entry_builds_full_payload_off_consumer() {
        use std::fs;
        let tmp = tempdir_or_skip();
        let root = tmp.path();
        let p = root.join("photo.jpg");
        fs::write(&p, b"jpegbytes").unwrap();

        let skip = HashMap::new();
        let errors = AtomicU64::new(0);
        let classified = classify_entry(&p, &skip, &errors);
        let df = classified.discovered.expect("supported file → Some payload");
        assert_eq!(
            classified.seen_path_hash,
            Some(crate::util::path_safety::stable_path_hash(&p.to_string_lossy()))
        );
        assert_eq!(df.kind, FileKind::Image);
        assert_eq!(df.size_bytes, 9);
        assert!(df.modified_unix > 0.0, "modified timestamp populated by the parallel stage");
        assert_eq!(errors.load(Ordering::Relaxed), 0);

        let empty = root.join("empty.jpg");
        fs::write(&empty, b"").unwrap();
        let empty_result = classify_entry(&empty, &skip, &errors);
        assert!(empty_result.discovered.is_none());
        assert!(empty_result.zero_byte.is_some());
        assert!(empty_result.seen_path_hash.is_some());
        let other = root.join("download.crdownload");
        fs::write(&other, b"data").unwrap();
        let other_result = classify_entry(&other, &skip, &errors);
        assert!(other_result.discovered.is_none());
        assert!(other_result.seen_path_hash.is_some());

        // Skip honors size+mtime, not bare membership: a matching entry is skipped...
        let mut skip2 = HashMap::new();
        skip2.insert(path_fingerprint(&p), (df.size_bytes as i64, Some(df.modified_unix)));
        let unchanged = classify_entry(&p, &skip2, &errors);
        assert!(unchanged.discovered.is_none(), "unchanged file is skipped");
        assert_eq!(
            unchanged.seen_path_hash,
            Some(crate::util::path_safety::stable_path_hash(&p.to_string_lossy())),
            "unchanged file is still observed"
        );
        let mut skip3 = HashMap::new();
        skip3.insert(
            path_fingerprint(&p),
            (df.size_bytes as i64, Some(df.modified_unix + 100.0)),
        );
        assert!(classify_entry(&p, &skip3, &errors).discovered.is_some(), "mtime change defeats the skip");
        let mut skip4 = HashMap::new();
        skip4.insert(
            path_fingerprint(&p),
            (df.size_bytes as i64 + 1, Some(df.modified_unix)),
        );
        assert!(classify_entry(&p, &skip4, &errors).discovered.is_some(), "size change defeats the skip");
    }

    #[test]
    fn the_skip_decision_follows_size_and_modified_time_over_the_whole_matrix() {
        let tmp = tempdir_or_skip();
        let p = tmp.path().join("photo.jpg");
        std::fs::write(&p, b"jpegbytes").unwrap();
        let errors = AtomicU64::new(0);
        let live = classify_entry(&p, &HashMap::new(), &errors).discovered.expect("a new file is discovered");
        let size = live.size_bytes as i64;
        let m = live.modified_unix;
        let discovered = |stored: (i64, Option<f64>)| {
            let mut skips = HashMap::new();
            skips.insert(path_fingerprint(&p), stored);
            classify_entry(&p, &skips, &errors).discovered.is_some()
        };
        assert!(!discovered((size, Some(m))), "same size and time skips");
        assert!(!discovered((size, Some(m + 4e-7))), "drift under a microsecond still skips");
        assert!(discovered((size, Some(m + 3e-6))), "drift over a microsecond re-reads");
        assert!(discovered((size, Some(m - 86_400.0))), "an edit that kept the size but has an older time on disk is re-read");
        assert!(discovered((size, Some(m + 86_400.0))), "a stored time newer than the disk is a change too");
        assert!(discovered((size + 1, Some(m))), "a different size re-reads");
        assert!(discovered((size - 1, Some(m))), "a smaller stored size re-reads");
        assert!(discovered((size, None)), "a row with no stored time is never current");
        assert_eq!(errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn plain_text_data_and_camera_raw_extensions_classify_as_indexed_kinds() {
        for ext in ["csv", "tsv", "json", "yaml", "yml", "log", "toml", "xml", "md", "txt"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::Doc, "{ext}");
        }
        for ext in ["arw", "cr2", "cr3", "nef", "dng", "orf", "rw2", "raf", "srw", "pef", "x3f"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::Image, "{ext}");
        }
        for ext in ["flac", "wav", "m4a", "opus", "aiff"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::Audio, "{ext}");
        }
    }

    /// end-to-end guard: every file the receiver gets must already
    /// carry a complete payload (size + modified + kind) built in the walk
    /// stage, and the whole set must be produced (done flag set) before the
    /// consumer drains a single item. With the stat/file_ref work parallelized
    /// into process_read_dir, the consumer is a pure take-and-send; this pins
    /// that contract so a regression that re-serializes stat work onto recv —
    /// or drops payload fields — is caught.
    #[test]
    fn payloads_ready_before_consumer_drains() {
        use std::fs;
        let tmp = tempdir_or_skip();
        let root = tmp.path();
        for i in 0..200 {
            fs::write(root.join(format!("img_{i:03}.jpg")), b"jpeg").unwrap();
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let coord = ScanCoordinator::new();
            let disc = Discovery::new_with_skip_and_exclusions(root, coord, Arc::new(HashMap::new()), Arc::new(Vec::new()));
            let handle = disc.spawn();
            let mut rx = handle.rx;
            let done = handle.done.clone();
            // Wait for the walk WITHOUT draining the receiver: the 32K-slot channel holds all 200 entries, so reaching `done` proves all payloads were built in the parallel stage.
            for _ in 0..200 {
                if done.load(Ordering::Acquire) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            assert!(done.load(Ordering::Acquire), "walk completed before any recv");

            let mut n = 0;
            while let Ok(f) = rx.try_recv() {
                assert_eq!(f.kind, FileKind::Image, "kind classified in parallel stage");
                assert_eq!(f.size_bytes, 4, "size populated in parallel stage");
                assert!(f.modified_unix > 0.0, "mtime populated in parallel stage");
                n += 1;
            }
            assert_eq!(n, 200);
        });
    }

    /// Tiny RAII tempdir helper that bails the test if /tmp isn't writable; avoids a `tempfile` dependency.
    fn tempdir_or_skip() -> TempDir {
        let base = std::env::temp_dir();
        let unique = format!(
            "fileid-discovery-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let p = base.join(unique);
        std::fs::create_dir_all(&p).expect("tempdir creation");
        TempDir(p)
    }

    struct TempDir(PathBuf);
    impl TempDir { fn path(&self) -> &std::path::Path { &self.0 } }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
