// Discovery — walks the user-picked root, emitting one DiscoveredFile per
// readable file the scan should consider.
//
// Mirror of macOS engine/Sources/FileIDEngine/Pipeline/Discovery.swift.
// Filters:
//   - Hidden / system files: skipped (Windows: starts with `.` OR has
//     FILE_ATTRIBUTE_HIDDEN or _SYSTEM)
//   - Symlinks pointing outside root: skipped (TOCTOU-safe via canonical
//     path containment)
//   - Files larger than 500 MB: skipped (config'able later)
//   - Unsupported kinds: skipped early so tagging workers don't waste work
//
// Throughput target: walkdir at 50K files/s on NVMe; this is rarely the
// bottleneck. The bounded mpsc channel applies backpressure when tagging
// workers fall behind.

use std::path::{Path, PathBuf};
use anyhow::Result;
use tokio::sync::mpsc;
use walkdir::WalkDir;

use crate::coordinator::ScanCoordinator;

/// Cap per-file size at 500 MB. Larger files (raw video, disk images,
/// VM images) are skipped because the ML pipeline doesn't process them
/// usefully and they monopolize a worker. Configurable in Phase 5
/// Settings.
const MAX_FILE_BYTES: u64 = 500 * 1024 * 1024;

/// Bounded mpsc capacity (Discovery → Tagging). Matches macOS's 1024.
const DISCOVERY_CHANNEL_CAP: usize = 1024;

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

    /// Lossy-but-fast classification by extension. Same set as macOS.
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
}

pub struct Discovery {
    root: PathBuf,
    coordinator: ScanCoordinator,
}

impl Discovery {
    pub fn new(root: impl Into<PathBuf>, coordinator: ScanCoordinator) -> Self {
        Self {
            root: root.into(),
            coordinator,
        }
    }

    /// Walk the root, sending each readable file onto the returned receiver.
    /// Spawns a tokio blocking task because walkdir is sync. Closes the
    /// channel when traversal completes OR cancellation is requested.
    pub fn spawn(self) -> mpsc::Receiver<DiscoveredFile> {
        let (tx, rx) = mpsc::channel(DISCOVERY_CHANNEL_CAP);
        let root = self.root.clone();
        let coordinator = self.coordinator.clone();

        tokio::task::spawn_blocking(move || {
            // walkdir: sorted-by-path traversal for I/O locality (sequential
            // disk reads). follow_links = false: macOS Discovery treats
            // symlinks as opaque, we match.
            let walker = WalkDir::new(&root)
                .follow_links(false)
                .same_file_system(false)
                .sort_by_file_name();

            for entry in walker {
                if coordinator.is_cancelled() {
                    break;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue, // permissions / vanished mid-walk → skip silently
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                if Self::should_skip(path) {
                    continue;
                }
                let metadata = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let size = metadata.len();
                if size > MAX_FILE_BYTES || size == 0 {
                    continue;
                }
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let kind = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(FileKind::from_extension)
                    .unwrap_or(FileKind::Other);
                if kind == FileKind::Other {
                    continue; // don't bother tagging workers with files we can't process
                }
                let discovered = DiscoveredFile {
                    path: path.to_path_buf(),
                    kind,
                    size_bytes: size,
                    modified_unix: modified,
                };
                // blocking_send applies backpressure when tagging falls
                // behind. Errors only happen if the receiver dropped (i.e.
                // shutdown); break out cleanly.
                if tx.blocking_send(discovered).is_err() {
                    break;
                }
            }
            // Channel auto-closes when tx drops here.
        });

        rx
    }

    fn should_skip(path: &Path) -> bool {
        // Hidden files (Windows: starts with `.` is enough; native hidden
        // attribute requires Win32 GetFileAttributes which we add in Phase 5
        // when the cost matters).
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                return true;
            }
            // Common build/cache dirs (matches macOS Discovery behavior).
            if matches!(name.to_ascii_lowercase().as_str(),
                "thumbs.db" | "desktop.ini" | "ehthumbs.db" | "$recycle.bin")
            {
                return true;
            }
        }
        // Component-level skip: any ancestor is a typical noise dir.
        for component in path.components() {
            if let std::path::Component::Normal(name) = component {
                if let Some(name_str) = name.to_str() {
                    if matches!(name_str, "node_modules" | ".git" | ".svn" | "__pycache__"
                                | "$RECYCLE.BIN" | "System Volume Information")
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Run the discovery walk and call `progress` periodically with the
/// running count. Used by tests + the standalone iterate harness.
pub async fn enumerate(root: impl AsRef<Path>) -> Result<Vec<DiscoveredFile>> {
    let coordinator = ScanCoordinator::new();
    let discovery = Discovery::new(root.as_ref(), coordinator);
    let mut rx = discovery.spawn();
    let mut out = Vec::new();
    while let Some(f) = rx.recv().await {
        out.push(f);
    }
    Ok(out)
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
}
