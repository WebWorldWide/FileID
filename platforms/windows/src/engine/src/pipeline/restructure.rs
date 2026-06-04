// Restructure — pure-logic FolderClassifier.
//
// Inputs are file metadata + tags + (optional) VLM categories from Deep
// Analyze; outputs are proposed destinations. No I/O happens here — this
// module just decides *where* each file should go. The apply layer lives
// in `shell/restructure_apply.rs`.

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
    /// Butler confidence band (RESTRUCTURE.md §6): drives auto-file / review /
    /// ask routing in the UI.
    pub confidence: Confidence,
    /// Plain-language "why filed here" (RESTRUCTURE.md §6 trust mechanics).
    pub reason: Option<String>,
}

/// Three-band autonomy tier for a single proposed move.
///
/// - **Auto**   = high confidence; safe to auto-file (still fully reversible).
/// - **Review** = medium; show for one-click confirm.
/// - **Ask**    = low; hold and ask, or leave in place.
///
/// Orthogonal to [`FolderClassification`] (which scores a *source folder's*
/// homogeneity); this scores a *single move's* placement certainty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Confidence {
    Auto,
    #[default]
    Review,
    Ask,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Auto => "auto",
            Confidence::Review => "review",
            Confidence::Ask => "ask",
        }
    }
}

/// Three-tier folder classification.
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
#[allow(dead_code)]
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

/// Drop every move whose source folder was classified
/// [`FolderClassification::Anchor`]. Anchor folders are deliberately left
/// untouched — the macOS reference emits NO proposals for them ("Files inside
/// Anchor folders stay put") — so their moves must never reach the plan the app
/// applies, even though the per-file classifier computes a canonical destination
/// for them. The anchor COUNT for the informational "Keep" tile is the caller's
/// responsibility, computed from the same `classified` slice BEFORE stripping.
/// (audit A1/A3)
pub fn strip_anchor_folder_moves(
    moves: Vec<ProposedMove>,
    classified: &[ClassifiedFolder],
) -> Vec<ProposedMove> {
    let anchor_folders: std::collections::HashSet<PathBuf> = classified
        .iter()
        .filter(|c| c.classification == FolderClassification::Anchor)
        .map(|c| c.source_folder.clone())
        .collect();
    moves
        .into_iter()
        .filter(|m| {
            m.source
                .parent()
                .map(|p| !anchor_folders.contains(p))
                .unwrap_or(true)
        })
        .collect()
}

/// Priority-based restructure matching macOS Restructure.swift. First
/// match wins:
///   1. Named person  → People/<Name>/<Year>/
///   2. GPS location  → Places/<lat,lon-bucketed>/<Year>/
///   3. Document       → Documents/<Year>/
///   4. Image          → Photos/<Year>/<MonthName>/
///   5. Video          → Videos/<Year>/
///   6. Audio          → Audio/
///   7. Fallback       → Misc/
pub fn classify(
    files: &[FileForClassify],
    library_root: &Path,
) -> Vec<ProposedMove> {
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let ts = f.created_unix.unwrap_or(f.modified_unix);
        let (y, m) = year_month(ts);
        let mname = month_name(m);

        let (dest, category, confidence, reason) = if let Some(ref name) = f.person_name {
            let safe = sanitize_path_component(name);
            (library_root.join("People").join(&safe).join(format!("{y}")),
             format!("People/{safe}"),
             Confidence::Auto,
             format!("Named person: {safe}"))
        } else if let (Some(lat), Some(lon)) = (f.location_lat, f.location_lon) {
            let lat_b = (lat * 2.0).round() / 2.0;
            let lon_b = (lon * 2.0).round() / 2.0;
            let bucket = format!("{lat_b:.1}_{lon_b:.1}");
            (library_root.join("Places").join(&bucket).join(format!("{y}")),
             format!("Places/{bucket}"),
             Confidence::Review,
             "Taken at a shared location".to_string())
        } else if f.has_text || matches!(f.kind, FileKind::Pdf | FileKind::Doc) {
            (library_root.join("Documents").join(format!("{y}")),
             "document".to_string(),
             Confidence::Review,
             format!("Document from {y}"))
        } else if matches!(f.kind, FileKind::Image) {
            (library_root.join("Photos").join(format!("{y}")).join(&mname),
             "photo".to_string(),
             Confidence::Review,
             format!("Photo from {mname} {y}"))
        } else if matches!(f.kind, FileKind::Video) {
            (library_root.join("Videos").join(format!("{y}")),
             "video".to_string(),
             Confidence::Review,
             format!("Video from {y}"))
        } else if matches!(f.kind, FileKind::Audio) {
            (library_root.join("Audio"), "audio".to_string(),
             Confidence::Review, "Audio file".to_string())
        } else {
            (library_root.join("Misc"), "misc".to_string(),
             Confidence::Ask, "No strong signal — left for you to decide".to_string())
        };

        out.push(ProposedMove {
            file_id: f.file_id,
            source: f.source.clone(),
            destination: dest.join(f.source.file_name().unwrap_or_default()),
            category,
            confidence,
            reason: Some(reason),
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
    pub created_unix: Option<f64>,
    pub person_name: Option<String>,
    pub location_lat: Option<f64>,
    pub location_lon: Option<f64>,
    pub has_text: bool,
}

fn sanitize_path_component(s: &str) -> String {
    // PAR-69/PAR-96: byte-faithful with macOS componentSafe (replace illegal +
    // control chars with `_`, handle Windows reserved names / trailing dots /
    // length / empty). The old version only DELETED illegal chars, which
    // diverged from macOS and produced NTFS-invalid names (e.g. a category
    // "CON" failed MoveFileExW with a cryptic error).
    crate::util::path_safety::safe_filename_component(s)
}

fn month_name(m: u32) -> String {
    match m {
        1 => "January", 2 => "February", 3 => "March", 4 => "April",
        5 => "May", 6 => "June", 7 => "July", 8 => "August",
        9 => "September", 10 => "October", 11 => "November", 12 => "December",
        _ => "Unknown",
    }.to_string()
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
    out.sort_by_key(|s| std::cmp::Reverse(s.count));
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
            created_unix: None,
            person_name: None,
            location_lat: None,
            location_lon: None,
            has_text: false,
        }
    }

    #[test]
    fn images_routed_to_photos_year_month() {
        let f = img(1, "C:/scan/foo.jpg", 1_710_504_000.0);
        let m = classify(&[f], Path::new("D:/Library"));
        assert_eq!(m.len(), 1);
        let dest = m[0].destination.to_string_lossy();
        assert!(dest.contains("Photos"), "dest={dest}");
        assert!(dest.contains("2024"), "dest={dest}");
        assert!(dest.contains("March"), "dest={dest}");
        assert_eq!(m[0].category, "photo");
    }

    #[test]
    fn anchor_folder_moves_are_stripped_from_plan() {
        // Three same-month photos in one well-named folder => that folder
        // classifies Anchor (>=80% one category, >2 files, non-generic name).
        // The macOS reference leaves anchor folders untouched, so their moves
        // must be dropped before the plan reaches the app. (audit A1/A3)
        let ts = 1_710_504_000.0;
        let files = vec![
            img(1, "D:/Library/Vacation2019/a.jpg", ts),
            img(2, "D:/Library/Vacation2019/b.jpg", ts),
            img(3, "D:/Library/Vacation2019/c.jpg", ts),
        ];
        let moves = classify(&files, Path::new("D:/Library"));
        assert_eq!(moves.len(), 3);
        let classified = classify_folders(&moves);
        assert!(
            classified
                .iter()
                .any(|c| c.classification == FolderClassification::Anchor),
            "Vacation2019 should classify Anchor: {classified:?}"
        );
        let kept = strip_anchor_folder_moves(moves, &classified);
        assert!(kept.is_empty(), "anchor-folder moves must be dropped: {kept:?}");
    }

    #[test]
    fn non_anchor_folder_moves_survive_the_strip() {
        // A generic-named folder ("Downloads") classifies Junk, not Anchor, so
        // its moves must survive the strip — guards against over-stripping.
        let ts = 1_710_504_000.0;
        let files = vec![
            img(1, "D:/Library/Downloads/a.jpg", ts),
            img(2, "D:/Library/Downloads/b.jpg", ts),
            img(3, "D:/Library/Downloads/c.jpg", ts),
        ];
        let moves = classify(&files, Path::new("D:/Library"));
        let classified = classify_folders(&moves);
        assert!(classified
            .iter()
            .all(|c| c.classification != FolderClassification::Anchor));
        let kept = strip_anchor_folder_moves(moves, &classified);
        assert_eq!(kept.len(), 3);
    }

    #[test]
    fn person_priority_over_date() {
        let mut f = img(1, "C:/scan/face.jpg", 1_710_504_000.0);
        f.person_name = Some("Alice".to_string());
        let m = classify(&[f], Path::new("D:/Library"));
        let dest = m[0].destination.to_string_lossy();
        assert!(dest.contains("People"), "dest={dest}");
        assert!(dest.contains("Alice"), "dest={dest}");
        assert!(m[0].category.starts_with("People/"));
    }

    #[test]
    fn gps_priority_over_kind() {
        let mut f = img(1, "C:/scan/geo.jpg", 1_710_504_000.0);
        f.location_lat = Some(37.7749);
        f.location_lon = Some(-122.4194);
        let m = classify(&[f], Path::new("D:/Library"));
        let dest = m[0].destination.to_string_lossy();
        assert!(dest.contains("Places"), "dest={dest}");
        assert!(m[0].category.starts_with("Places/"));
    }

    #[test]
    fn document_priority_over_year_month() {
        let f = FileForClassify {
            file_id: 1,
            source: PathBuf::from("C:/scan/report.pdf"),
            kind: FileKind::Pdf,
            modified_unix: 1_710_504_000.0,
            created_unix: None,
            person_name: None,
            location_lat: None,
            location_lon: None,
            has_text: false,
        };
        let m = classify(&[f], Path::new("D:/Library"));
        let dest = m[0].destination.to_string_lossy();
        assert!(dest.contains("Documents"), "dest={dest}");
        assert!(dest.contains("2024"), "dest={dest}");
        assert_eq!(m[0].category, "document");
    }

    #[test]
    fn video_year_only() {
        let f = FileForClassify {
            file_id: 1,
            source: PathBuf::from("C:/scan/clip.mp4"),
            kind: FileKind::Video,
            modified_unix: 1_710_504_000.0,
            created_unix: None,
            person_name: None,
            location_lat: None,
            location_lon: None,
            has_text: false,
        };
        let m = classify(&[f], Path::new("D:/Library"));
        let dest = m[0].destination.to_string_lossy();
        assert!(dest.contains("Videos"), "dest={dest}");
        assert!(dest.contains("2024"), "dest={dest}");
        assert!(!dest.contains("March"), "videos should not have month: dest={dest}");
    }

    #[test]
    fn has_text_routes_to_documents() {
        let mut f = img(1, "C:/scan/screenshot.png", 1_710_504_000.0);
        f.has_text = true;
        let m = classify(&[f], Path::new("D:/Library"));
        let dest = m[0].destination.to_string_lossy();
        assert!(dest.contains("Documents"), "dest={dest}");
        assert_eq!(m[0].category, "document");
    }

    #[test]
    fn category_counts_summed_and_sorted() {
        let mv = |id: i64, category: &str| ProposedMove {
            file_id: id,
            source: PathBuf::new(),
            destination: PathBuf::new(),
            category: category.into(),
            confidence: Confidence::default(),
            reason: None,
        };
        let moves = vec![mv(1, "photo"), mv(2, "photo"), mv(3, "video")];
        let cats = category_counts(&moves);
        assert_eq!(cats[0].category, "photo");
        assert_eq!(cats[0].count, 2);
        assert_eq!(cats[1].category, "video");
        assert_eq!(cats[1].count, 1);
    }
}
