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
