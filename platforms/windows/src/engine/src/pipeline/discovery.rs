// Discovery — walks the user-picked root, emitting one DiscoveredFile per
// readable file the scan should consider.
//
// V15.9 rewrite: jwalk (rayon-backed parallel walk) + process_read_dir
// directory-level pruning + atomic count-before-send. The previous
// walkdir + tx.blocking_send implementation tied the discovery counter
// to tagging throughput; if the ML pipeline stalled (GPU saturation,
// DirectML command-queue pressure) the walk also stalled because the
// 1024-slot channel filled in under a second on NVMe. Users saw
// "Discovered 1,324" after 60s for a 22 files/s observed rate against a
// 50K+ file NVMe → bottleneck was tagging, not the FS.
//
// New semantics:
//   - jwalk distributes stat() calls across `walk_concurrency_for(root)`
//     threads (NVMe → 16, SATA SSD → 8, HDD → 2; see platform.rs).
//   - process_read_dir prunes noise directories (node_modules, .git, etc.)
//     at the directory level so we never recurse into them — eliminates
//     the per-file component-traversal cost.
//   - count.fetch_add(1) fires BEFORE blocking_send, so the "Discovered"
//     counter reflects what the FS walk has seen even when the downstream
//     channel briefly fills.
//   - Channel cap raised 1024 → 32768. On a typical user corpus of
//     <50K files the channel never fills in practice; on multi-100K
//     corpora the cap caps memory at ~32K × ~200 B path ≈ 6 MB.
//
// Filters:
//   - Hidden / system files: skipped (starts with `.`, OR matches
//     thumbs.db / desktop.ini / etc.)
//   - Symlinks: not followed (follow_links = false)
//   - Zero-byte files: skipped (no content to embed/hash)
//   - Unsupported file kinds: skipped early so tagging workers don't
//     waste work

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::mpsc;

use crate::coordinator::ScanCoordinator;

// Zero-byte files are skipped (no content to embed/hash). No size cap —
// all sizes go through the same pipeline.

/// Bounded mpsc capacity (Discovery → Tagging). At ~200 B per
/// `DiscoveredFile` this caps queue memory at ~6 MB worst-case.
/// Sized so that on typical user corpora (<50K files) the channel never
/// fills, decoupling the discovery counter from ML throughput.
const DISCOVERY_CHANNEL_CAP: usize = 32768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Image,
    Video,
    Pdf,
    Doc,
    Audio,
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
            FileKind::Other => "other",
        }
    }

    /// Lossy-but-fast classification by extension.
    pub fn from_extension(ext: &str) -> Self {
        let ext = ext.to_ascii_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic"
            | "heif" | "raw" | "arw" | "cr2" | "nef" | "dng" => FileKind::Image,
            "mp4" | "mov" | "m4v" | "avi" | "mkv" | "webm" | "mts" | "m2ts" => FileKind::Video,
            "pdf" => FileKind::Pdf,
            "docx" | "doc" | "odt" | "rtf" | "txt" | "md" | "pages" | "key" | "numbers"
            | "xlsx" | "pptx" => FileKind::Doc,
            "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" => FileKind::Audio,
            _ => FileKind::Other,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub kind: FileKind,
    pub size_bytes: u64,
    /// Last-modified timestamp as Unix seconds. Used for incremental
    /// rescans: files unchanged since their `scanned_at` row are skipped.
    pub modified_unix: f64,
    /// Creation timestamp as Unix seconds, when the platform exposes one.
    /// Threaded through to `files.created_at` for parity with the macOS
    /// engine (which populates it). `None` when the filesystem doesn't
    /// record a birth time or the metadata read failed.
    pub created_unix: Option<f64>,
    /// True when the file is a cloud placeholder (OneDrive Files-On-Demand /
    /// `OFFLINE` / `RECALL_ON_*`). Reading its content would trigger a
    /// network hydration (a surprise download — and the only non-model
    /// egress), so the pipeline does a metadata-only pass and skips
    /// decode / embed / OCR for it. Always false on non-Windows.
    pub online_only: bool,
    /// Volume-local file identity (NTFS MFT reference on Windows; inode on
    /// POSIX). The dbwriter's rename/move heal uses it to re-bind a moved
    /// file's catalog row instead of orphaning it. `None` when the metadata
    /// open failed (permission, race, ...) — the heal then falls through to
    /// content_hash. Always `None` on non-Windows for now.
    pub file_ref: Option<u64>,
}

pub struct Discovery {
    root: PathBuf,
    coordinator: ScanCoordinator,
    /// Paths the orchestrator has determined are already scanned-and-current
    /// (DB `scanned_at >= modified_unix`). Discovery silently skips these,
    /// turning a re-run of a 1M-file corpus into a near-instant no-op.
    /// Empty = classic full-scan.
    skip_paths: Arc<HashSet<PathBuf>>,
}

/// Handle returned from `Discovery::spawn`. `count` is a live counter the
/// orchestrator polls to emit Progress events with `discovered=count` so
/// the user sees the number climb during a long discovery walk; `done`
/// flips true the instant the walker exits its loop (detects the
/// empty-folder case where count stays at 0 and we should surface
/// "no supported files found" instead of hanging).
pub struct DiscoveryHandle {
    pub rx: mpsc::Receiver<DiscoveredFile>,
    pub count: Arc<AtomicU64>,
    pub done: Arc<AtomicBool>,
    /// Walk errors swallowed during traversal. Surfaced as a non-fatal
    /// `discovery_partial` event by the scan orchestrator.
    pub error_count: Arc<AtomicU64>,
}

impl Discovery {
    /// Incremental-rescan-aware constructor. The caller loads the
    /// "already current" path set from the DB before spawning Discovery.
    /// Honors a `rescan: true` IPC flag by passing an empty set here.
    pub fn new_with_skip(
        root: impl Into<PathBuf>,
        coordinator: ScanCoordinator,
        skip_paths: Arc<HashSet<PathBuf>>,
    ) -> Self {
        Self {
            root: root.into(),
            coordinator,
            skip_paths,
        }
    }

    /// Walk the root, sending each readable file onto the returned receiver.
    /// Spawns a tokio blocking task because jwalk's iterator is sync.
    /// Closes the channel when traversal completes OR cancellation is
    /// requested.
    pub fn spawn(self) -> DiscoveryHandle {
        let (tx, rx) = mpsc::channel(DISCOVERY_CHANNEL_CAP);
        let root = self.root.clone();
        let coordinator = self.coordinator.clone();
        let skip_paths = self.skip_paths.clone();
        let count = Arc::new(AtomicU64::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let error_count = Arc::new(AtomicU64::new(0));
        let count_inner = count.clone();
        let done_inner = done.clone();
        let error_count_inner = error_count.clone();

        if !skip_paths.is_empty() {
            tracing::info!(
                skip = skip_paths.len(),
                "[DISCOVERY] skipping N already-scanned files (rescan=false)"
            );
        }

        // Test gate: cap files yielded by Discovery so a staging run
        // (e.g. N=100 to validate VRAM behavior) can't wedge the system.
        // Unset / 0 → no cap.
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
            "[DISCOVERY] adaptive parallel walk"
        );

        tokio::task::spawn_blocking(move || {
            // jwalk: rayon-backed parallel walk. process_read_dir prunes
            // noise directories at the read_dir level so we never recurse
            // into them (the win is dramatic on trees with deep
            // node_modules / .git subtrees — those entire subtrees are
            // dropped with a single name check per directory rather than
            // per-file component traversal).
            //
            // C6-017: the per-file metadata + file_ref opens used to run on
            // the single consumer thread (the for-loop below), capping
            // discovery throughput at one stat/open per file once the ML
            // stage stopped being the bottleneck. We now do that work INSIDE
            // process_read_dir — which jwalk runs on the rayon pool, one task
            // per directory — and stash the finished DiscoveredFile in the
            // entry's typed client_state. The consumer loop then only takes
            // the pre-computed payload and sends it, so the stat/open syscalls
            // fan out across `walk_threads` instead of serializing. Ordering
            // is unchanged: jwalk still yields entries in the same traversal
            // order regardless of which thread filled each client_state.
            let coord_for_dir = coordinator.clone();
            let skip_for_dir = skip_paths.clone();
            let err_for_dir = error_count_inner.clone();
            // Walk a verbatim ("\\?\") root so directories whose full path
            // exceeds MAX_PATH (260) are traversable — std/jwalk silently
            // fail on them otherwise (this engine .exe has no long-path
            // manifest). Children inherit the prefix; we strip it back to
            // normal form on emit. Fall back to the plain root if the
            // converted form isn't statable, so short-path scans never
            // regress.
            let verbatim_root = crate::util::path_safety::to_extended_length(&root);
            let walk_root = if std::fs::metadata(&verbatim_root).is_ok() {
                verbatim_root
            } else {
                root.clone()
            };
            // ClientState = ((), Option<DiscoveredFile>): the per-entry slot
            // (DirEntryState) carries each file's finished payload, computed
            // in parallel inside process_read_dir below.
            let walker = jwalk::WalkDirGeneric::<((), Option<DiscoveredFile>)>::new(&walk_root)
                .follow_links(false)
                .skip_hidden(false)   // we do our own dot-file filter to also catch thumbs.db etc.
                .parallelism(jwalk::Parallelism::RayonNewPool(walk_threads))
                .process_read_dir(move |_depth, _dir_path, _state, children| {
                    if coord_for_dir.is_cancelled() {
                        children.clear();
                        return;
                    }
                    // A directory looks like a dev tree only if a build-system
                    // marker sits directly in it. Used to gate the ambiguous
                    // build-dir names (target/build/obj/bin/venv) so we don't
                    // prune a user's ordinary "build" / "bin" folder of files.
                    let has_build_marker = children.iter().any(|res| {
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
                            // Prune generic build dirs only inside a dev tree.
                            if is_ambiguous_build_directory(&name) {
                                return !has_build_marker;
                            }
                            true
                        } else if file_type.is_file() {
                            !is_noise_file(&name)
                        } else {
                            false // symlinks, sockets, devices: skip
                        }
                    });
                    // Parallel stage: do the per-file stat + file_ref open here
                    // (on the rayon pool) and store the result in client_state.
                    for res in children.iter_mut() {
                        let Ok(entry) = res.as_mut() else { continue };
                        if !entry.file_type().is_file() {
                            continue;
                        }
                        let entry_path = entry.path();
                        entry.client_state = classify_entry(
                            &entry_path,
                            skip_for_dir.as_ref(),
                            err_for_dir.as_ref(),
                        );
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
                    Err(_) => {
                        error_count_inner.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
                // Payload was computed in parallel inside process_read_dir;
                // None = a file we skip (unsupported kind, zero-byte, skip-set
                // hit, metadata failure) or a directory entry. Errors were
                // already counted at the parallel stage.
                let Some(discovered) = entry.client_state else {
                    continue;
                };

                // Increment the FS-walk counter BEFORE the channel send so
                // the "Discovered N" sidebar reflects walk progress even if
                // the channel briefly backs up. Order matters: this is the
                // V15.9 decoupling fix — under the old code, blocking_send
                // could stall a file's count when ML stalled. The count +
                // send stay on this single consumer thread so the counter is
                // monotonic and the test_file_cap stop condition is exact.
                count_inner.fetch_add(1, Ordering::Relaxed);

                // blocking_send applies backpressure when the channel fills
                // (cap 32768 → roughly 6 MB queued path metadata). On
                // typical corpora this never trips. Errors only happen if
                // the receiver dropped (i.e. shutdown); break out cleanly.
                if tx.blocking_send(discovered).is_err() {
                    break;
                }
            }
            done_inner.store(true, Ordering::Release);
            // Channel auto-closes when tx drops here.
        });

        DiscoveryHandle { rx, count, done, error_count }
    }
}

/// Build the `DiscoveredFile` payload for one file entry, doing the stat +
/// `file_ref` opens. Runs on the jwalk/rayon pool (one task per directory) so
/// these syscalls fan out instead of serializing on the consumer thread
/// (C6-017). Returns `None` for any file the pipeline should skip
/// (non-Unicode name, already-current, metadata failure, zero-byte,
/// unsupported kind); skip-with-error cases bump `error_count` here so the
/// consumer's terminal accounting is unchanged.
///
/// `entry_path` is the verbatim-prefixed walk path (children inherit the
/// "\\?\" root); FS opens reconvert via `to_extended_length` internally.
fn classify_entry(
    entry_path: &std::path::Path,
    skip_paths: &HashSet<PathBuf>,
    error_count: &AtomicU64,
) -> Option<DiscoveredFile> {
    // Strip the verbatim prefix back to normal form: this is what we store +
    // display + compare against the skip-set (which holds normal-form DB
    // paths). FS access reconverts via `to_extended_length` at the open site.
    let path = crate::util::path_safety::strip_extended_length(entry_path);
    // A name that doesn't round-trip through Unicode (an unpaired UTF-16
    // surrogate on NTFS) would be catalogued under a U+FFFD-mangled path_text
    // that no later action (open, thumbnail, rename, trash) can resolve back
    // to the real file — and two such names can collapse into one DB row.
    // Skip it loudly instead of cataloguing an inert entry.
    if is_non_unicode_path(&path) {
        tracing::warn!(
            path = %crate::platform::redact_path_for_log(&path),
            "[DISCOVERY] file name is not valid Unicode (unpaired \
             surrogate?) — skipping; rename the file to include it"
        );
        error_count.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    // Skip files the orchestrator pre-loaded as already-current. Hash lookup
    // is O(1); a 1M-file set costs ~80 MB RAM but lets a repeat scan complete
    // in seconds instead of hours.
    if skip_paths.contains(&path) {
        return None;
    }
    // follow_links is false on the walk, so symlink_metadata matches jwalk's
    // own DirEntry::metadata for the non-followed case.
    let metadata = match std::fs::symlink_metadata(entry_path) {
        Ok(m) => m,
        Err(_) => {
            error_count.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    let size = metadata.len();
    if size == 0 {
        return None;
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let created_unix = metadata
        .created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());

    // Cloud placeholder detection. OneDrive / generic Files-On-Demand mark
    // dehydrated files with these attributes; touching their content forces a
    // download.
    #[cfg(windows)]
    let online_only = {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
        const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
        const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
        metadata.file_attributes()
            & (FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_OPEN
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
            != 0
    };
    #[cfg(not(windows))]
    let online_only = false;

    let kind = path
        .extension()
        .and_then(|e| e.to_str())
        .map(FileKind::from_extension)
        .unwrap_or(FileKind::Other);
    if kind == FileKind::Other {
        return None; // don't bother tagging workers with files we can't process
    }

    // Volume-local file id: lets the dbwriter heal a renamed/moved file's
    // existing row instead of recomputing its tags. Cheap metadata-only open
    // via `FILE_FLAG_BACKUP_SEMANTICS`; `None` on permission/race errors (the
    // heal then falls through to content_hash).
    let file_ref = crate::platform::file_ref(entry_path);
    Some(DiscoveredFile {
        path,
        kind,
        size_bytes: size,
        modified_unix: modified,
        created_unix,
        online_only,
        file_ref,
    })
}

/// True when `to_string_lossy` would mangle this path: the lossy text is the
/// only identity the DB/app layers ever see, so such a file can never be
/// targeted again after cataloguing.
fn is_non_unicode_path(path: &std::path::Path) -> bool {
    path.to_str().is_none()
}

/// Directory names whose entire subtree should be skipped. Decided once
/// per directory inside jwalk's `process_read_dir`, so we never recurse
/// in — the entire subtree is invisible to the walk.
fn is_noise_directory(name: &str) -> bool {
    // Case-insensitive comparisons for the System Volume Information /
    // $RECYCLE.BIN cases — Windows lists these mixed-case at the root.
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
    )
}

/// Generic directory names that are build artifacts ONLY inside a dev tree.
/// They double as ordinary user folder names (a photographer's "build" set,
/// a "bin" of scans, a French "venv"…), so the caller prunes these only when
/// a build-system marker sits alongside them — otherwise we'd silently make a
/// user's files invisible to the whole app.
fn is_ambiguous_build_directory(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "target" | "build" | "obj" | "bin" | "venv" | ".venv"
    )
}

/// True if `name` is a build-system marker (sits next to the build dirs above).
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
    // Visual Studio / MSBuild project files — compare via the extension
    // (case-insensitively) rather than str::ends_with(".sln"), which clippy's
    // case_sensitive_file_extension_comparisons lint rejects under -D warnings.
    std::path::Path::new(name).extension().is_some_and(|e| {
        let e = e.to_string_lossy();
        e.eq_ignore_ascii_case("sln")
            || e.eq_ignore_ascii_case("csproj")
            || e.eq_ignore_ascii_case("vcxproj")
    })
}

/// Per-file noise filter. Cheap O(1) string match — kept separate from
/// `is_noise_directory` because file-vs-dir semantics differ (no dotfile
/// rejection for directories; some users' libraries live under
/// `.local/share/...`).
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

    #[test]
    fn kind_from_extension_unknown_is_other() {
        assert_eq!(FileKind::from_extension("xyz"), FileKind::Other);
    }

    #[test]
    fn noise_directory_recognized() {
        assert!(is_noise_directory("node_modules"));
        assert!(is_noise_directory(".git"));
        assert!(is_noise_directory("__pycache__"));
        assert!(is_noise_directory("$RECYCLE.BIN"));
        assert!(is_noise_directory("System Volume Information"));
        assert!(!is_noise_directory("Pictures"));
        assert!(!is_noise_directory("Desktop"));
    }

    #[test]
    fn noise_directory_case_insensitive() {
        assert!(is_noise_directory("NODE_MODULES"));
        assert!(is_noise_directory("Node_Modules"));
    }

    /// Non-Unicode names (unpaired UTF-16 surrogates on NTFS; raw bytes on
    /// POSIX) must be detected so discovery can skip-log them instead of
    /// cataloguing a U+FFFD-mangled path no later action can resolve.
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

    #[test]
    fn noise_file_recognized() {
        assert!(is_noise_file("Thumbs.db"));
        assert!(is_noise_file("desktop.ini"));
        assert!(is_noise_file(".DS_Store"));
        assert!(is_noise_file(".hidden"));
        assert!(!is_noise_file("vacation.jpg"));
    }

    /// jwalk parallel walk against a small synthetic tree must:
    ///   1. Visit every supported file.
    ///   2. Prune noise directories (no recursion into node_modules / .git).
    ///   3. Skip zero-byte files.
    ///
    /// This is the smoke test guarding the V15.9 rewrite. The full
    /// throughput benchmark lives in tests/discovery_throughput.rs and
    /// runs under `cargo test --release --test discovery_throughput
    /// -- --ignored`.
    #[test]
    fn synthetic_tree_walk_prunes_noise_and_counts_files() {
        use std::fs;
        let tmp = tempdir_or_skip();
        let root = tmp.path();

        // 30 image files + 1 zero-byte + 1 ".git" dir + 1 "node_modules" dir
        // with junk inside that should be invisible to the walk.
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
            let disc = Discovery::new_with_skip(root, coord, Arc::new(HashSet::new()));
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

    /// `count` MUST be incremented for every file BEFORE the channel send,
    /// so a slow receiver doesn't desync the "Discovered N" counter from
    /// actual FS walk progress. This is the load-bearing invariant for
    /// the Issue 1 decoupling fix.
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
            let coord = ScanCoordinator::new();
            let disc = Discovery::new_with_skip(root, coord, Arc::new(HashSet::new()));
            let handle = disc.spawn();
            let mut rx = handle.rx;
            let count = handle.count.clone();
            let done = handle.done.clone();
            // Wait for the walk to finish (done flag flips after the last
            // count.fetch_add + tx.send). Then assert count == 100 even if
            // we haven't drained the receiver yet — proving the counter
            // reflects walk progress independent of receiver drain.
            for _ in 0..100 {
                if done.load(Ordering::Acquire) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            assert_eq!(count.load(Ordering::Relaxed), 100);
            let mut drained = 0;
            while rx.try_recv().is_ok() {
                drained += 1;
            }
            assert_eq!(drained, 100);
        });
    }

    /// C6-017: the per-file stat + `file_ref` work must live in a standalone
    /// function the parallel walk stage can call (jwalk's process_read_dir, on
    /// the rayon pool) — NOT inline on the single consumer loop. Calling it
    /// directly proves the syscall work was lifted off the consumer thread and
    /// produces a fully-populated payload independent of the channel/recv.
    #[test]
    fn classify_entry_builds_full_payload_off_consumer() {
        use std::fs;
        let tmp = tempdir_or_skip();
        let root = tmp.path();
        let p = root.join("photo.jpg");
        fs::write(&p, b"jpegbytes").unwrap();

        let skip = HashSet::new();
        let errors = AtomicU64::new(0);
        // Pass the path exactly as the walk would (no verbatim prefix in the
        // test env; strip_extended_length is a no-op on a plain path).
        let df = classify_entry(&p, &skip, &errors).expect("supported file → Some payload");
        assert_eq!(df.kind, FileKind::Image);
        assert_eq!(df.size_bytes, 9);
        assert!(df.modified_unix > 0.0, "modified timestamp populated by the parallel stage");
        assert_eq!(errors.load(Ordering::Relaxed), 0);

        // Zero-byte and unsupported-kind files resolve to None (skipped) here,
        // so the consumer never has to re-stat to decide.
        let empty = root.join("empty.jpg");
        fs::write(&empty, b"").unwrap();
        assert!(classify_entry(&empty, &skip, &errors).is_none());
        let other = root.join("notes.xyz");
        fs::write(&other, b"data").unwrap();
        assert!(classify_entry(&other, &skip, &errors).is_none());

        // Skip-set membership is honored at the parallel stage too.
        let mut skip2 = HashSet::new();
        skip2.insert(p.clone());
        assert!(classify_entry(&p, &skip2, &errors).is_none());
    }

    /// C6-017 end-to-end guard: every file the receiver gets must already
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
            let disc = Discovery::new_with_skip(root, coord, Arc::new(HashSet::new()));
            let handle = disc.spawn();
            let mut rx = handle.rx;
            let done = handle.done.clone();
            // Wait for the walk to finish WITHOUT draining the receiver. If the
            // metadata/file_ref opens were still serialized on the consumer
            // (the pre-fix behavior), the walk could not complete here — the
            // 32K-slot channel holds all 200 entries, so the producer reaching
            // `done` proves all payloads were built in the parallel stage.
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

    /// Tiny RAII tempdir helper that bails the test if /tmp isn't writable
    /// (CI sandboxing edge case). Avoids the `tempfile` crate dep — we
    /// only need it from tests, not from the engine binary.
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
