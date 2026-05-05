// Restructure — pure-logic FolderClassifier port.
//
// Mirror of macOS engine/Sources/FileIDEngine/Pipeline/Restructure.swift
// + FolderClassifier.swift. Inputs are file metadata + tags + (optional)
// VLM categories from Deep Analyze; outputs are proposed destinations.
// No I/O happens here — this module just decides *where* each file
// should go. Phase 7 also adds the apply layer that does the real
// `MoveFileExW` (default) or `CreateSymbolicLinkW` (advanced).
//
// Phase 7 cut: the classifier rule set + tree-build helpers. Apply
// happens in `shell/restructure_apply.rs` (Phase 7.x).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::pipeline::discovery::FileKind;

/// One proposed move (or stay-in-place) for a single file.
#[derive(Debug, Clone)]
pub struct ProposedMove {
    pub file_id: i64,
    pub source: PathBuf,
    pub destination: PathBuf,
    /// Logical category that drove the destination — surfaced in the
    /// Sankey ribbon hover tooltip.
    pub category: String,
}

/// V14.7.2: three-tier folder classification.
/// Mirrors macOS engine `Restructure.swift` `FolderClassification` enum.
///
/// - **Anchor** = source folder where ≥80% of moves go to ONE destination
///   category (homogeneous; folder gets renamed in place).
/// - **Mixed**  = source folder where moves span multiple destination
///   categories (some files extracted as outliers).
/// - **Junk**   = source folder with ≤2 files OR a folder name that
///   matches the generic-name pattern (folder dissolves).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderClassification {
    Anchor,
    Mixed,
    Junk,
}

/// Per-source-folder classification + the dominant destination category.
#[derive(Debug, Clone)]
pub struct ClassifiedFolder {
    pub source_folder: PathBuf,
    pub classification: FolderClassification,
    pub move_count: u32,
    pub dominant_category: String,
}

/// Classify every source folder appearing in `moves`. Returns one
/// `ClassifiedFolder` per distinct source folder. Folders that have NO
/// moves (their files all stay put) are NOT included — they're the
/// implicit "anchor folders intact" tier.
pub fn classify_folders(moves: &[ProposedMove]) -> Vec<ClassifiedFolder> {
    use std::collections::BTreeMap;

    // Group moves by source folder (parent dir of `source`).
    let mut by_folder: BTreeMap<PathBuf, Vec<&ProposedMove>> = BTreeMap::new();
    for m in moves {
        let parent = m.source.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        by_folder.entry(parent).or_default().push(m);
    }

    let mut out = Vec::with_capacity(by_folder.len());
    for (folder, items) in by_folder {
        // Per-folder category histogram.
        let mut hist: HashMap<String, u32> = HashMap::new();
        for m in &items {
            *hist.entry(m.category.clone()).or_insert(0) += 1;
        }
        let total = items.len() as u32;
        let (dominant, top) = hist
            .iter()
            .max_by_key(|(_, c)| **c)
            .map(|(k, v)| (k.clone(), *v))
            .unwrap_or_default();
        let homogeneity = if total > 0 { top as f32 / total as f32 } else { 0.0 };

        // Folder name heuristic — generic names like "Downloads",
        // "Untitled", "New Folder" lean Junk regardless of size.
        let name = folder.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let generic = matches!(
            name.as_str(),
            "downloads" | "downloaded" | "new folder" | "untitled" | "temp" | "tmp"
                | "misc" | "other" | "stuff" | "things" | "files"
        );

        let classification = if generic || total <= 2 {
            FolderClassification::Junk
        } else if homogeneity >= 0.80 {
            FolderClassification::Anchor
        } else {
            FolderClassification::Mixed
        };

        out.push(ClassifiedFolder {
            source_folder: folder,
            classification,
            move_count: total,
            dominant_category: dominant,
        });
    }
    out
}

/// Heuristic root-level layout. Mirrors macOS:
///   - Photos/{Year}/{Month}/         for image kind
///   - Videos/{Year}/                 for video kind
///   - Documents/                     for doc/pdf kind
///   - Audio/                         for audio kind
///   - Misc/                          for everything else
///
/// Year + month derived from EXIF DateTimeOriginal (image), Media
/// Foundation creation date (video), or modified_at as the fallback.
pub fn classify(
    files: &[FileForClassify],
    library_root: &Path,
) -> Vec<ProposedMove> {
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let dest = match f.kind {
            FileKind::Image => {
                let (y, m) = year_month(f.modified_unix);
                library_root.join("Photos").join(format!("{y}")).join(format!("{m:02}"))
            }
            FileKind::Video => {
                let (y, _) = year_month(f.modified_unix);
                library_root.join("Videos").join(format!("{y}"))
            }
            FileKind::Pdf | FileKind::Doc => library_root.join("Documents"),
            FileKind::Audio => library_root.join("Audio"),
            FileKind::Other => library_root.join("Misc"),
        };
        let category = match f.kind {
            FileKind::Image => "photo".to_string(),
            FileKind::Video => "video".to_string(),
            FileKind::Pdf | FileKind::Doc => "document".to_string(),
            FileKind::Audio => "audio".to_string(),
            FileKind::Other => "misc".to_string(),
        };
        let dest_with_name = dest.join(f.source.file_name().unwrap_or_default());
        out.push(ProposedMove {
            file_id: f.file_id,
            source: f.source.clone(),
            destination: dest_with_name,
            category,
        });
    }
    out
}

#[derive(Debug, Clone)]
pub struct FileForClassify {
    pub file_id: i64,
    pub source: PathBuf,
    pub kind: FileKind,
    pub modified_unix: f64,
}

#[derive(Debug, Clone)]
pub struct CategorySummary {
    pub category: String,
    pub count: u32,
}

/// Aggregate ProposedMoves for the Sankey diagram: source-folder rollup
/// → category. The macOS implementation does the same on the app side.
pub fn category_counts(moves: &[ProposedMove]) -> Vec<CategorySummary> {
    let mut buckets: HashMap<String, u32> = HashMap::new();
    for m in moves {
        *buckets.entry(m.category.clone()).or_default() += 1;
    }
    let mut out: Vec<_> = buckets
        .into_iter()
        .map(|(category, count)| CategorySummary { category, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count));
    out
}

/// Convert a Unix-seconds timestamp to (year, month). Drives the
/// Photos/{Year}/{Month}/ tree. Uses chrono so daylight-savings doesn't
/// shift a January-1 photo into the prior December folder.
fn year_month(unix: f64) -> (i32, u32) {
    use chrono::{DateTime, Datelike, Utc};
    let secs = unix as i64;
    let nanos = ((unix - secs as f64) * 1_000_000_000.0) as u32;
    if let Some(dt) = DateTime::<Utc>::from_timestamp(secs, nanos) {
        return (dt.year(), dt.month());
    }
    (1970, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(id: i64, path: &str, ts: f64) -> FileForClassify {
        FileForClassify {
            file_id: id,
            source: PathBuf::from(path),
            kind: FileKind::Image,
            modified_unix: ts,
        }
    }

    #[test]
    fn images_routed_to_photos_year_month() {
        // 2024-03-15 12:00 UTC ≈ 1710504000
        let f = img(1, "C:/scan/foo.jpg", 1_710_504_000.0);
        let m = classify(&[f], Path::new("D:/Library"));
        assert_eq!(m.len(), 1);
        let dest = m[0].destination.to_string_lossy();
        assert!(dest.contains("Photos"));
        assert!(dest.contains("2024"));
        assert!(dest.contains("03"));
        assert_eq!(m[0].category, "photo");
    }

    #[test]
    fn category_counts_summed_and_sorted() {
        let moves = vec![
            ProposedMove { file_id: 1, source: PathBuf::new(), destination: PathBuf::new(), category: "photo".into() },
            ProposedMove { file_id: 2, source: PathBuf::new(), destination: PathBuf::new(), category: "photo".into() },
            ProposedMove { file_id: 3, source: PathBuf::new(), destination: PathBuf::new(), category: "video".into() },
        ];
        let cats = category_counts(&moves);
        assert_eq!(cats[0].category, "photo");
        assert_eq!(cats[0].count, 2);
        assert_eq!(cats[1].category, "video");
        assert_eq!(cats[1].count, 1);
    }
}
