// Restructure — pure-logic FolderClassifier.
//
// Inputs are file metadata + tags + (optional) VLM categories from Deep
// Analyze; outputs are proposed destinations. No I/O happens here — this
// module just decides *where* each file should go. The apply layer lives
// in `pipeline/restructure_apply.rs`.

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

/// Per-source-folder classification.
#[derive(Debug, Clone)]
pub struct ClassifiedFolder {
    pub source_folder: PathBuf,
    pub classification: FolderClassification,
}

/// Classify every source folder appearing in `moves`. Returns one
/// `ClassifiedFolder` per distinct source folder. Folders that have NO
/// moves (their files all stay put) are NOT included — they're the
/// implicit "anchor folders intact" tier.
pub fn classify_folders(
    moves: &[ProposedMove],
    library_root: &Path,
) -> Vec<ClassifiedFolder> {
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
        let mut hist: HashMap<&str, u32> = HashMap::new();
        for m in &items {
            *hist.entry(m.category.as_str()).or_insert(0) += 1;
        }
        let total = items.len() as u32;
        let top = hist.values().copied().max().unwrap_or(0);
        let homogeneity = if total > 0 { top as f32 / total as f32 } else { 0.0 };

        // Folder name heuristic — generic names like "Downloads",
        // "Untitled", "New Folder" lean Junk regardless of size.
        let name = folder.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let generic = matches!(
            name.as_str(),
            "downloads"
                | "downloaded"
                | "desktop"
                | "unsorted"
                | "inbox"
                | "new folder"
                | "untitled"
                | "temp"
                | "tmp"
                | "misc"
                | "other"
                | "stuff"
                | "things"
                | "files"
        );

        let is_library_root = folder == library_root
            || folder
                .to_string_lossy()
                .eq_ignore_ascii_case(&library_root.to_string_lossy())
            || matches!(
                (
                    std::fs::canonicalize(&folder),
                    std::fs::canonicalize(library_root)
                ),
                (Ok(folder), Ok(root)) if folder == root
            );
        let classification = if generic || total <= 2 {
            FolderClassification::Junk
        } else if !is_library_root && homogeneity >= 0.80 {
            FolderClassification::Anchor
        } else {
            FolderClassification::Mixed
        };

        out.push(ClassifiedFolder {
            source_folder: folder,
            classification,
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
///
/// Test-only convenience: production now calls [`strip_anchor_folder_moves_except`]
/// directly (the semantic butler exempts only the files it claimed, F-C1-004).
#[cfg(test)]
pub fn strip_anchor_folder_moves(
    moves: Vec<ProposedMove>,
    classified: &[ClassifiedFolder],
) -> Vec<ProposedMove> {
    strip_anchor_folder_moves_except(moves, classified, &std::collections::HashSet::new())
}

/// Like [`strip_anchor_folder_moves`], but never strips a move whose file ID is
/// in `exempt_file_ids`.
///
/// A homogeneous *source* folder whose files all route to a single *destination*
/// group classifies Anchor on category homogeneity — but when that homogeneity
/// is the semantic butler actively relocating every file into one new content
/// group (not the rule cascade computing a stay-put canonical destination),
/// stripping it would silently eat the highest-confidence proposals. The caller
/// passes the exact files the butler claimed so those real relocations survive
/// without exempting unrelated siblings in the same source folder.
/// (audit F-C1-004)
pub fn strip_anchor_folder_moves_except<S: std::hash::BuildHasher>(
    moves: Vec<ProposedMove>,
    classified: &[ClassifiedFolder],
    exempt_file_ids: &std::collections::HashSet<i64, S>,
) -> Vec<ProposedMove> {
    let anchor_folders: std::collections::HashSet<PathBuf> = classified
        .iter()
        .filter(|c| c.classification == FolderClassification::Anchor)
        .map(|c| c.source_folder.clone())
        .collect();
    moves
        .into_iter()
        .filter(|m| {
            exempt_file_ids.contains(&m.file_id)
                || m.source
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
///   6. Audio          → Audio/<Year>/  (flat Audio/ when no date signal)
///   7. Fallback       → Misc/
pub fn classify(
    files: &[FileForClassify],
    library_root: &Path,
) -> Vec<ProposedMove> {
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let ts = f.created_unix.unwrap_or(f.modified_unix);
        // A zero or negative timestamp is not a real date (FAT32 zero-epoch, corrupt
        // mtime). Use Ask confidence and skip year folders so the user isn't misled
        // into accepting 1970 placements silently.
        let date = valid_year_month(ts);
        let ts_valid = date.is_some();
        let (y, m) = date.unwrap_or((1970, 1));
        let mname = month_name(m);

        let (dest, category, confidence, reason) = if let Some(ref name) = f.person_name {
            let safe = sanitize_path_component(name);
            let destination = library_root.join("People").join(&safe);
            let destination = if ts_valid {
                destination.join(format!("{y}"))
            } else {
                destination
            };
            (destination,
             format!("People/{safe}"),
             if ts_valid { Confidence::Auto } else { Confidence::Review },
             if ts_valid {
                 format!("Named person: {safe}")
             } else {
                 format!("Named person: {safe}; no reliable date")
             })
        } else if let Some((lat, lon)) = f
            .location_lat
            .zip(f.location_lon)
            .filter(|(lat, lon)| {
                lat.is_finite()
                    && lon.is_finite()
                    && (-90.0..=90.0).contains(lat)
                    && (-180.0..=180.0).contains(lon)
            })
        {
            let lat_b = (lat * 2.0).round() / 2.0;
            let lon_b = (lon * 2.0).round() / 2.0;
            let bucket = format!("{lat_b:.1}_{lon_b:.1}");
            let destination = library_root.join("Places").join(&bucket);
            let destination = if ts_valid {
                destination.join(format!("{y}"))
            } else {
                destination
            };
            (destination,
             format!("Places/{bucket}"),
             Confidence::Review,
             if ts_valid {
                 "Taken at a shared location".to_string()
             } else {
                 "Taken at a shared location; no reliable date".to_string()
             })
        } else if f.has_text || matches!(f.kind, FileKind::Pdf | FileKind::Doc) {
            let dest = if ts_valid {
                library_root.join("Documents").join(format!("{y}"))
            } else {
                library_root.join("Documents")
            };
            let conf = if ts_valid { Confidence::Review } else { Confidence::Ask };
            let reason = if ts_valid { format!("Document from {y}") } else { "Document — no date signal".to_string() };
            (dest, "document".to_string(), conf, reason)
        } else if matches!(f.kind, FileKind::Image) {
            let dest = if ts_valid {
                library_root.join("Photos").join(format!("{y}")).join(&mname)
            } else {
                library_root.join("Photos")
            };
            let conf = if ts_valid { Confidence::Review } else { Confidence::Ask };
            let reason = if ts_valid { format!("Photo from {mname} {y}") } else { "Photo — no capture date".to_string() };
            (dest, "photo".to_string(), conf, reason)
        } else if matches!(f.kind, FileKind::Video) {
            let dest = if ts_valid {
                library_root.join("Videos").join(format!("{y}"))
            } else {
                library_root.join("Videos")
            };
            let conf = if ts_valid { Confidence::Review } else { Confidence::Ask };
            let reason = if ts_valid { format!("Video from {y}") } else { "Video — no date signal".to_string() };
            (dest, "video".to_string(), conf, reason)
        } else if matches!(f.kind, FileKind::Audio) {
            let dest = if ts_valid {
                library_root.join("Audio").join(format!("{y}"))
            } else {
                library_root.join("Audio")
            };
            let reason = if ts_valid { format!("Audio file from {y}") } else { "Audio file".to_string() };
            (dest, "audio".to_string(), Confidence::Review, reason)
        } else if matches!(f.kind, FileKind::Model) {
            (library_root.join("3D Models"), "model".to_string(),
             Confidence::Review, "3D model".to_string())
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

pub(crate) fn month_name(m: u32) -> String {
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
    let mut buckets: HashMap<&str, u32> = HashMap::new();
    for m in moves {
        *buckets.entry(m.category.as_str()).or_default() += 1;
    }
    let mut out: Vec<_> = buckets
        .into_iter()
        .map(|(category, count)| CategorySummary { category: category.to_string(), count })
        .collect();
    // Count desc, then category asc. The secondary key is load-bearing: the source is
    // a HashMap (arbitrary, run-to-run-randomized iteration), so a count-only sort left
    // equal-count categories in nondeterministic order — both unstable across runs on
    // Windows AND divergent from the macOS engine, which tie-breaks on category. The
    // Sankey ribbon order is now deterministic and lockstep. (audit)
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.category.cmp(&b.category)));
    out
}

/// Convert a Unix-seconds timestamp to (year, month). Drives the
/// Photos/{Year}/{Month}/ tree. Uses chrono so daylight-savings doesn't
/// shift a January-1 photo into the prior December folder.
pub(crate) fn year_month(unix: f64) -> (i32, u32) {
    valid_year_month(unix).unwrap_or((1970, 1))
}

fn valid_year_month(unix: f64) -> Option<(i32, u32)> {
    use chrono::{DateTime, Datelike, Utc};
    if !unix.is_finite() || unix <= 86_400.0 {
        return None;
    }
    let secs = unix as i64;
    let nanos = ((unix - secs as f64) * 1_000_000_000.0)
        .clamp(0.0, 999_999_999.0) as u32;
    DateTime::<Utc>::from_timestamp(secs, nanos).map(|dt| (dt.year(), dt.month()))
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
        let classified = classify_folders(&moves, Path::new("D:/Library"));
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
    fn homogeneous_files_directly_under_library_root_are_never_anchor_stripped() {
        let root = Path::new("D:/Library");
        let ts = 1_710_504_000.0;
        let files = vec![
            img(1, "D:/Library/a.jpg", ts),
            img(2, "D:/Library/b.jpg", ts),
            img(3, "D:/Library/c.jpg", ts),
        ];
        let moves = classify(&files, root);
        let classified = classify_folders(&moves, root);

        assert_eq!(classified.len(), 1);
        assert_eq!(
            classified[0].classification,
            FolderClassification::Mixed,
            "the selected root is the organizational inbox, not a keep-in-place anchor"
        );
        assert_eq!(
            strip_anchor_folder_moves(moves, &classified).len(),
            files.len()
        );
    }

    #[test]
    fn semantic_exemption_is_scoped_to_the_claimed_file() {
        let src_folder = PathBuf::from("D:/Library/inbox/dogs");
        let moves = vec![
            ProposedMove {
                file_id: 1,
                source: src_folder.join("a.jpg"),
                destination: PathBuf::from("D:/Library/Dogs/a.jpg"),
                category: "Dogs".into(),
                confidence: Confidence::Auto,
                reason: None,
            },
            ProposedMove {
                file_id: 2,
                source: src_folder.join("b.jpg"),
                destination: PathBuf::from("D:/Library/Dogs/b.jpg"),
                category: "Dogs".into(),
                confidence: Confidence::Auto,
                reason: None,
            },
            ProposedMove {
                file_id: 3,
                source: src_folder.join("c.jpg"),
                destination: PathBuf::from("D:/Library/Dogs/c.jpg"),
                category: "Dogs".into(),
                confidence: Confidence::Auto,
                reason: None,
            },
        ];
        let classified = classify_folders(&moves, Path::new("D:/Library"));
        assert!(
            classified
                .iter()
                .any(|c| c.classification == FolderClassification::Anchor),
            "homogeneous-destination source folder should classify Anchor: {classified:?}"
        );
        // Without the exemption it is stripped to nothing (the regressing path).
        let stripped = strip_anchor_folder_moves(moves.clone(), &classified);
        assert!(stripped.is_empty(), "baseline anchor-strip eats the moves");
        let mut exempt = std::collections::HashSet::new();
        exempt.insert(1);
        let kept = strip_anchor_folder_moves_except(moves, &classified, &exempt);
        assert_eq!(kept.len(), 1, "only the claimed semantic move survives: {kept:?}");
        assert_eq!(kept[0].file_id, 1);
    }

    #[test]
    fn canonical_in_place_files_still_classify_their_folder_as_anchor() {
        let root = Path::new("D:/Library");
        let ts = 1_710_504_000.0;
        let files = vec![
            img(1, "D:/Library/Photos/2024/March/a.jpg", ts),
            img(2, "D:/Library/Photos/2024/March/b.jpg", ts),
            img(3, "D:/Library/Photos/2024/March/c.jpg", ts),
        ];
        let moves = classify(&files, root);
        assert!(moves.iter().all(|move_| move_.source == move_.destination));

        let classified = classify_folders(&moves, root);
        assert_eq!(classified.len(), 1);
        assert_eq!(classified[0].classification, FolderClassification::Anchor);
    }

    #[test]
    fn desktop_unsorted_and_inbox_are_junk_folders() {
        let root = Path::new("D:/Library");
        for name in ["Desktop", "Unsorted", "Inbox"] {
            let folder = root.join(name);
            let moves = (1..=3)
                .map(|file_id| ProposedMove {
                    file_id,
                    source: folder.join(format!("{file_id}.jpg")),
                    destination: root.join("Photos").join(format!("{file_id}.jpg")),
                    category: "photo".into(),
                    confidence: Confidence::Review,
                    reason: None,
                })
                .collect::<Vec<_>>();
            let classified = classify_folders(&moves, root);
            assert_eq!(
                classified[0].classification,
                FolderClassification::Junk,
                "{name} must never become a Keep/Anchor folder"
            );
        }
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
        let classified = classify_folders(&moves, Path::new("D:/Library"));
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
    fn audio_year_subfolder() {
        let f = FileForClassify {
            file_id: 1,
            source: PathBuf::from("C:/scan/song.mp3"),
            kind: FileKind::Audio,
            modified_unix: 1_710_504_000.0, // 2024
            created_unix: None,
            person_name: None,
            location_lat: None,
            location_lon: None,
            has_text: false,
        };
        let m = classify(&[f], Path::new("D:/Library"));
        let dest = m[0].destination.to_string_lossy();
        assert!(dest.contains("Audio"), "dest={dest}");
        assert!(dest.contains("2024"), "audio should have year sub-folder: dest={dest}");
    }

    #[test]
    fn zero_timestamp_gets_ask_confidence() {
        // A zero timestamp (FAT32 zero-epoch, corrupt mtime) must produce Ask
        // confidence and skip the year sub-folder so the user isn't silently
        // filed under 1970.
        let f = FileForClassify {
            file_id: 1,
            source: PathBuf::from("C:/scan/photo.jpg"),
            kind: FileKind::Image,
            modified_unix: 0.0,
            created_unix: None,
            person_name: None,
            location_lat: None,
            location_lon: None,
            has_text: false,
        };
        let m = classify(&[f], Path::new("D:/Library"));
        assert_eq!(m[0].confidence, Confidence::Ask, "zero-ts must surface as Ask");
        let dest = m[0].destination.to_string_lossy();
        assert!(!dest.contains("1970"), "zero-ts must not land in 1970: dest={dest}");
    }

    #[test]
    fn named_person_without_valid_date_never_creates_a_1970_folder() {
        let mut file = img(1, "C:/scan/face.jpg", f64::INFINITY);
        file.person_name = Some("Alice".to_string());

        let moves = classify(&[file], Path::new("D:/Library"));
        let destination = moves[0].destination.to_string_lossy();

        assert_eq!(moves[0].confidence, Confidence::Review);
        assert!(destination.contains("People"));
        assert!(destination.contains("Alice"));
        assert!(!destination.contains("1970"));
    }

    #[test]
    fn invalid_coordinates_fall_through_instead_of_creating_bogus_places() {
        let mut file = img(1, "C:/scan/photo.jpg", 1_710_504_000.0);
        file.location_lat = Some(999.0);
        file.location_lon = Some(f64::NAN);

        let moves = classify(&[file], Path::new("D:/Library"));

        assert_eq!(moves[0].category, "photo");
        assert!(!moves[0].destination.to_string_lossy().contains("Places"));
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
