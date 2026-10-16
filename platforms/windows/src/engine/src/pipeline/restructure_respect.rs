//! Final Restructure stage: keep what the user already organized.
//! Port of macOS `RestructureRespect.swift` (see shared/docs/RESTRUCTURE.md,
//! 2026-10-15). Files in named folders stay; no move crosses events or lands in
//! another format's folder; folder names never come from camera filenames or
//! tag identifiers; misfiled RAW/JPEG/video files move to their sibling format
//! folder; loose files captured during an event join it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::discovery::FileKind;
use super::restructure::{Confidence, FileForClassify, ProposedMove};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatRole {
    Raw,
    Jpeg,
    Video,
    Photo,
}

const RAW_EXTENSIONS: &[&str] = &[
    "arw", "cr2", "cr3", "crw", "nef", "nrw", "dng", "raf", "orf", "rw2", "pef", "srw", "x3f", "3fr", "iiq",
    "rwl", "erf", "mef", "mos", "srf", "sr2",
];
const JPEG_EXTENSIONS: &[&str] = &["jpg", "jpeg", "jpe", "heic", "heif", "png", "tif", "tiff", "webp"];
const GENERIC_FOLDERS: &[&str] = &[
    "downloads", "downloaded", "new folder", "untitled folder", "untitled", "temp", "tmp", "misc",
    "miscellaneous", "other", "others", "stuff", "things", "files", "unsorted", "desktop", "dump", "inbox",
    "to sort", "to-sort", "import", "imports", "camera uploads", "dcim", "export", "exports", "screenshots",
    "screen shots",
];

pub fn kill_switch_off() -> bool {
    std::env::var("FILEID_RESTRUCTURE_RESPECT").map(|v| v == "0").unwrap_or(false)
}

/// Generic or camera-generated folder names ("DCIM", "100MSDCF", "New Folder 3").
pub fn is_generic_folder_name(raw: &str) -> bool {
    let name = raw.trim().to_lowercase();
    if name.is_empty() || GENERIC_FOLDERS.contains(&name.as_str()) {
        return true;
    }
    for prefix in ["new folder", "untitled folder", "untitled"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            let rest = rest.trim();
            if rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }
    let bytes = name.as_bytes();
    bytes.len() >= 6
        && bytes.len() <= 8
        && bytes[..3].iter().all(u8::is_ascii_digit)
        && bytes[3..].iter().all(|b| b.is_ascii_lowercase() || *b == b'_')
}

fn tokens(raw: &str) -> HashSet<String> {
    raw.to_lowercase()
        .split(|c: char| !c.is_alphabetic())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn folder_role(raw: &str) -> Option<FormatRole> {
    let t = tokens(raw);
    let any = |words: &[&str]| words.iter().any(|w| t.contains(*w));
    if any(&["raw", "raws"]) {
        Some(FormatRole::Raw)
    } else if any(&["jpeg", "jpegs", "jpg", "jpgs"]) {
        Some(FormatRole::Jpeg)
    } else if any(&["video", "videos", "movies", "clips", "footage"]) {
        Some(FormatRole::Video)
    } else if any(&["photo", "photos", "pictures", "images", "stills"]) {
        Some(FormatRole::Photo)
    } else {
        None
    }
}

pub fn file_role(path: &Path, kind: FileKind) -> FormatRole {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    if RAW_EXTENSIONS.contains(&ext.as_str()) {
        FormatRole::Raw
    } else if kind == FileKind::Video {
        FormatRole::Video
    } else if JPEG_EXTENSIONS.contains(&ext.as_str()) {
        FormatRole::Jpeg
    } else {
        FormatRole::Photo
    }
}

pub fn fits(file: FormatRole, folder: Option<FormatRole>) -> bool {
    match folder {
        None => true,
        Some(FormatRole::Raw) => file == FormatRole::Raw,
        Some(FormatRole::Jpeg) => file == FormatRole::Jpeg,
        Some(FormatRole::Video) => file == FormatRole::Video,
        Some(FormatRole::Photo) => file != FormatRole::Video,
    }
}

/// Folder names between the library root and the file, outermost first.
pub fn components(path: &Path, root: &Path) -> Vec<String> {
    let Ok(relative) = path.strip_prefix(root) else { return Vec::new() };
    let mut parts: Vec<String> =
        relative.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    parts.pop();
    parts
}

pub fn is_organized(path: &Path, root: &Path) -> bool {
    components(path, root).iter().any(|f| !is_generic_folder_name(f))
}

pub fn event_root(path: &Path, root: &Path) -> Option<String> {
    components(path, root).into_iter().next()
}

/// A camera filename stem or a raw tag identifier is not a folder name.
pub fn is_usable_folder_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.chars().count() < 3 || trimmed.contains('_') {
        return false;
    }
    !trimmed.to_lowercase().split(|c: char| !c.is_alphanumeric()).any(|word| {
        let letters = word.chars().take_while(|c| c.is_alphabetic()).count();
        let digits = word.chars().skip(letters).take_while(char::is_ascii_digit).count();
        letters <= 5 && digits >= 3 && letters + digits == word.chars().count()
    })
}

fn is_month_or_year(folder: &str) -> bool {
    (folder.len() == 4 && folder.chars().all(|c| c.is_ascii_digit()))
        || [
            "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
            "November", "December",
        ]
        .contains(&folder)
}

pub fn keep(m: &ProposedMove, kind: FileKind, root: &Path) -> bool {
    if is_organized(&m.source, root) {
        return is_format_correction(m, kind, root);
    }
    let destination = components(&m.destination, root);
    let role = file_role(&m.source, kind);
    if let Some(last) = destination.last() {
        if !fits(role, folder_role(last)) {
            return false;
        }
    }
    let existing: HashSet<String> = components(&m.source, root).into_iter().collect();
    destination.iter().all(|folder| {
        existing.contains(folder)
            || is_usable_folder_name(folder)
            || folder_role(folder).is_some()
            || is_month_or_year(folder)
    })
}

pub fn is_format_correction(m: &ProposedMove, kind: FileKind, root: &Path) -> bool {
    let from = components(&m.source, root);
    let to = components(&m.destination, root);
    if from.len() != to.len() || from.len() < 2 || from[..from.len() - 1] != to[..to.len() - 1] {
        return false;
    }
    let role = file_role(&m.source, kind);
    let (source, destination) = (folder_role(from.last().unwrap()), folder_role(to.last().unwrap()));
    destination.is_some() && !fits(role, source) && fits(role, destination)
}

/// RAW in "JPEGs" (or the reverse) → sibling folder for its format.
pub fn format_corrections(files: &[FileForClassify], root: &Path) -> Vec<ProposedMove> {
    let mut siblings: HashMap<PathBuf, Vec<(String, FormatRole)>> = HashMap::new();
    for f in files {
        let Some(folder) = f.source.parent() else { continue };
        let Some(parent) = folder.parent() else { continue };
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(role) = folder_role(&name).filter(|r| *r != FormatRole::Photo) {
            let list = siblings.entry(parent.to_path_buf()).or_default();
            if !list.iter().any(|(n, _)| *n == name) {
                list.push((name, role));
            }
        }
    }
    for list in siblings.values_mut() {
        list.sort_by(|a, b| a.0.cmp(&b.0));
    }
    let mut out = Vec::new();
    for f in files.iter().filter(|f| is_organized(&f.source, root)) {
        let (Some(folder), Some(name)) = (f.source.parent(), f.source.file_name()) else { continue };
        let Some(parent) = folder.parent() else { continue };
        let folder_name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let role = file_role(&f.source, f.kind);
        let Some(current) = folder_role(&folder_name).filter(|r| *r != FormatRole::Photo) else { continue };
        if fits(role, Some(current)) {
            continue;
        }
        let Some((target, _)) = siblings.get(parent).and_then(|l| l.iter().find(|(_, r)| fits(role, Some(*r))))
        else {
            continue;
        };
        out.push(ProposedMove {
            file_id: f.file_id,
            source: f.source.clone(),
            destination: parent.join(target).join(name),
            category: target.clone(),
            confidence: Confidence::Auto,
            reason: Some(format!("In a {folder_name} folder; moved to its sibling {target} folder.")),
        });
    }
    out
}

/// Loose photos and videos split into events wherever consecutive capture
/// times are more than `gap` seconds apart. Events of at least `min_files` get
/// a dated folder (`Photos/2026/2026-09-19`, or a range for multi-day events);
/// the rest go to `Photos/<year>/<Month>`. Day names use `offset` (the machine's
/// local offset in production) so an evening photo isn't filed under the next day.
pub fn loose_event_destinations(
    files: &[FileForClassify],
    root: &Path,
    gap: f64,
    min_files: usize,
    offset: chrono::FixedOffset,
) -> HashMap<i64, (PathBuf, String)> {
    use chrono::{Datelike, TimeZone};
    let mut loose: Vec<(&FileForClassify, f64)> = files
        .iter()
        .filter(|f| matches!(f.kind, FileKind::Image | FileKind::Video) && !is_organized(&f.source, root))
        .map(|f| (f, f.created_unix.unwrap_or(f.modified_unix)))
        .filter(|(_, t)| *t > 0.0)
        .collect();
    loose.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.file_id.cmp(&b.0.file_id)));
    let mut groups: Vec<Vec<(&FileForClassify, f64)>> = Vec::new();
    for item in loose {
        match groups.last_mut() {
            Some(group) if item.1 - group.last().unwrap().1 <= gap => group.push(item),
            _ => groups.push(vec![item]),
        }
    }
    let local = |t: f64| offset.timestamp_opt(t as i64, 0).single();
    let mut out = HashMap::new();
    for group in groups {
        let (Some(start), Some(end)) = (local(group[0].1), local(group[group.len() - 1].1)) else { continue };
        let (folder, reason) = if group.len() >= min_files {
            let same_day = start.date_naive() == end.date_naive();
            let name = if same_day {
                start.format("%Y-%m-%d").to_string()
            } else {
                format!("{} to {}", start.format("%Y-%m-%d"), end.format("%m-%d"))
            };
            let when = if same_day {
                format!("on {}", start.format("%b %-d, %Y"))
            } else {
                format!("from {} to {}", start.format("%b %-d, %Y"), end.format("%b %-d, %Y"))
            };
            (
                root.join("Photos").join(start.year().to_string()).join(name),
                format!("One of {} photos taken {when}; kept together as an event.", group.len()),
            )
        } else {
            (
                root.join("Photos").join(start.year().to_string()).join(start.format("%B").to_string()),
                format!("Taken in {} {}.", start.format("%B"), start.year()),
            )
        };
        for (file, _) in group {
            if let Some(name) = file.source.file_name() {
                out.insert(file.file_id, (folder.join(name), reason.clone()));
            }
        }
    }
    out
}

pub struct EventSpan {
    pub folder: PathBuf,
    pub start: f64,
    pub end: f64,
    pub format_folders: Vec<(PathBuf, FormatRole)>,
}

pub fn event_spans(files: &[FileForClassify], root: &Path) -> Vec<EventSpan> {
    let mut times: HashMap<String, Vec<f64>> = HashMap::new();
    let mut formats: HashMap<String, HashSet<PathBuf>> = HashMap::new();
    for f in files {
        let folders = components(&f.source, root);
        let Some(top) = folders.first() else { continue };
        if is_generic_folder_name(top) {
            continue;
        }
        let t = f.created_unix.unwrap_or(f.modified_unix);
        if t > 0.0 {
            times.entry(top.clone()).or_default().push(t);
        }
        if folders.last().and_then(|n| folder_role(n)).is_some() {
            if let Some(parent) = f.source.parent() {
                formats.entry(top.clone()).or_default().insert(parent.to_path_buf());
            }
        }
    }
    let mut spans: Vec<EventSpan> = times
        .into_iter()
        .filter_map(|(top, mut values)| {
            if values.len() < 5 {
                return None;
            }
            values.sort_by(|a, b| a.total_cmp(b));
            let lo = values[((values.len() - 1) as f64 * 0.1) as usize];
            let hi = values[((values.len() - 1) as f64 * 0.9) as usize];
            if hi - lo > 4.0 * 86_400.0 {
                return None;
            }
            let mut format_folders: Vec<(PathBuf, FormatRole)> = formats
                .remove(&top)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|p| {
                    let role = folder_role(&p.file_name()?.to_string_lossy())?;
                    Some((p, role))
                })
                .collect();
            format_folders.sort_by(|a, b| a.0.cmp(&b.0));
            Some(EventSpan { folder: root.join(&top), start: lo, end: hi, format_folders })
        })
        .collect();
    spans.sort_by(|a, b| a.start.total_cmp(&b.start));
    spans
}

pub fn event_destination(f: &FileForClassify, spans: &[EventSpan]) -> Option<PathBuf> {
    let t = f.created_unix.unwrap_or(f.modified_unix);
    if t <= 0.0 {
        return None;
    }
    let slack = 3.0 * 3_600.0;
    let matches: Vec<&EventSpan> = spans.iter().filter(|s| t >= s.start - slack && t <= s.end + slack).collect();
    let [span] = matches.as_slice() else { return None };
    let role = file_role(&f.source, f.kind);
    let exact: Vec<_> = span.format_folders.iter().filter(|(_, r)| *r == role).collect();
    let general: Vec<_> = span
        .format_folders
        .iter()
        .filter(|(_, r)| *r == FormatRole::Photo && role != FormatRole::Video)
        .collect();
    let folder = match (exact.as_slice(), general.as_slice()) {
        ([one], _) => one.0.clone(),
        (_, [one]) => one.0.clone(),
        _ => span.folder.clone(),
    };
    Some(folder.join(f.source.file_name()?))
}

/// Applies the stage to a full proposal set. Returns the kept/added moves and
/// the source folders that stay untouched (reported as Keep).
pub fn apply(proposed: Vec<ProposedMove>, files: &[FileForClassify], root: &Path) -> (Vec<ProposedMove>, HashSet<PathBuf>) {
    let kinds: HashMap<i64, &FileForClassify> = files.iter().map(|f| (f.file_id, f)).collect();
    let spans = event_spans(files, root);
    let mut kept: Vec<ProposedMove> = proposed
        .into_iter()
        .filter(|m| keep(m, kinds.get(&m.file_id).map(|f| f.kind).unwrap_or(FileKind::Other), root))
        .collect();
    let mut planned: HashSet<i64> = kept.iter().map(|m| m.file_id).collect();
    for fix in format_corrections(files, root) {
        if planned.insert(fix.file_id) {
            kept.push(fix);
        }
    }
    let unmatched: Vec<FileForClassify> =
        files.iter().filter(|f| event_destination(f, &spans).is_none()).cloned().collect();
    let offset = *chrono::Local::now().offset();
    let loose = loose_event_destinations(&unmatched, root, 8.0 * 3_600.0, 5, offset);
    let mut index_by_id: HashMap<i64, usize> = kept.iter().enumerate().map(|(i, m)| (m.file_id, i)).collect();
    let mut loose_ids: Vec<&i64> = loose.keys().collect();
    loose_ids.sort();
    for id in loose_ids {
        let (Some(file), Some((target, reason))) = (kinds.get(id), loose.get(id)) else { continue };
        if file.source == *target {
            continue;
        }
        let category = target
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let proposal = ProposedMove {
            file_id: *id,
            source: file.source.clone(),
            destination: target.clone(),
            category,
            confidence: Confidence::Auto,
            reason: Some(reason.clone()),
        };
        if let Some(&i) = index_by_id.get(id) {
            kept[i] = proposal;
        } else {
            index_by_id.insert(*id, kept.len());
            kept.push(proposal);
        }
    }
    for m in &mut kept {
        if is_organized(&m.source, root) {
            continue;
        }
        let Some(f) = kinds.get(&m.file_id) else { continue };
        if let Some(destination) = event_destination(f, &spans) {
            let event = event_root(&destination, root).unwrap_or_default();
            m.category = destination
                .parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            m.destination = destination;
            m.confidence = Confidence::Auto;
            m.reason = Some(format!("Captured during {event}; filed with it."));
        }
    }
    let moving: HashSet<PathBuf> = kept.iter().filter_map(|m| m.source.parent().map(Path::to_path_buf)).collect();
    let untouched = files
        .iter()
        .filter(|f| is_organized(&f.source, root))
        .filter_map(|f| f.source.parent().map(Path::to_path_buf))
        .filter(|p| !moving.contains(p))
        .collect();
    (kept, untouched)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/Volumes/Adlon/Camera";

    fn file(id: i64, rel: &str, kind: FileKind, t: Option<f64>) -> FileForClassify {
        FileForClassify {
            file_id: id,
            source: Path::new(ROOT).join(rel),
            kind,
            modified_unix: t.unwrap_or(0.0),
            created_unix: t,
            person_name: None,
            location_lat: None,
            location_lon: None,
            has_text: false,
            vlm_proposed: None,
            identity: None,
        }
    }

    fn mv(id: i64, from: &str, to: &str) -> ProposedMove {
        ProposedMove {
            file_id: id,
            source: Path::new(ROOT).join(from),
            destination: Path::new(ROOT).join(to),
            category: "x".into(),
            confidence: Confidence::Auto,
            reason: None,
        }
    }

    #[test]
    fn generic_names_and_roles() {
        for n in ["Downloads", "DCIM", "100MSDCF", "New Folder 2", "untitled folder", "Misc"] {
            assert!(is_generic_folder_name(n), "{n}");
        }
        for n in ["Kyle Baseball Game 9-19-26", "Hunter", "Photos", "RAWs"] {
            assert!(!is_generic_folder_name(n), "{n}");
        }
        assert_eq!(folder_role("RAWs"), Some(FormatRole::Raw));
        assert_eq!(folder_role("Edited JPEGs"), Some(FormatRole::Jpeg));
        assert_eq!(folder_role("Videos"), Some(FormatRole::Video));
        assert_eq!(folder_role("Hunter"), None);
    }

    #[test]
    fn organized_files_stay_and_events_never_mix() {
        let root = Path::new(ROOT);
        assert!(!keep(&mv(1, "Kyle Baseball Game 9-19-26/Photos/RAWs/D1.ARW", "Kyle Baseball Game 9-20-26/Hunter/RAW/D1.ARW"), FileKind::Image, root));
        assert!(!keep(&mv(2, "Eckerts 10-4-26/Photos/JPEGs/D2.JPG", "SIUE Homecoming 9-26-26/Photos/Edited JPEGs/D2.JPG"), FileKind::Image, root));
    }

    #[test]
    fn loose_files_organized_but_not_into_wrong_formats_or_junk_names() {
        let root = Path::new(ROOT);
        assert!(keep(&mv(1, "IMG_1.JPG", "Photos/2026/September/IMG_1.JPG"), FileKind::Image, root));
        assert!(!keep(&mv(2, "Downloads/IMG_2.JPG", "Kyle Baseball Game 9-20-26/Hunter/RAW/IMG_2.JPG"), FileKind::Image, root));
        assert!(!keep(&mv(3, "IMG_3.JPG", "Dsc01548 Sunglasses/IMG_3.JPG"), FileKind::Image, root));
        assert!(!keep(&mv(4, "Downloads/C1.MP4", "Consumer_electronics Computer/C1.MP4"), FileKind::Video, root));
    }

    #[test]
    fn misfiled_formats_move_to_sibling() {
        let files = vec![
            file(1, "Eckerts 10-4-26/Photos/JPEGs/D9.ARW", FileKind::Image, None),
            file(2, "Eckerts 10-4-26/Photos/JPEGs/D9.JPG", FileKind::Image, None),
            file(3, "Eckerts 10-4-26/Photos/RAWs/D8.ARW", FileKind::Image, None),
        ];
        let fixes = format_corrections(&files, Path::new(ROOT));
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].destination, Path::new(ROOT).join("Eckerts 10-4-26/Photos/RAWs/D9.ARW"));
        assert!(keep(&fixes[0], FileKind::Image, Path::new(ROOT)));
    }

    #[test]
    fn loose_files_join_their_event() {
        let day = 1_790_000_000.0;
        let mut files: Vec<_> = (0..6)
            .map(|i| file(i, &format!("Kyle Baseball Game 9-19-26/Photos/RAWs/K{i}.ARW"), FileKind::Image, Some(day + i as f64 * 600.0)))
            .collect();
        files.extend((0..6).map(|i| file(20 + i, &format!("Eckerts 10-4-26/Photos/JPEGs/E{i}.JPG"), FileKind::Image, Some(day + 15.0 * 86_400.0 + i as f64 * 600.0))));
        let spans = event_spans(&files, Path::new(ROOT));
        assert_eq!(spans.len(), 2);
        let loose = file(99, "Downloads/D5.ARW", FileKind::Image, Some(day + 1_200.0));
        assert_eq!(event_destination(&loose, &spans), Some(Path::new(ROOT).join("Kyle Baseball Game 9-19-26/Photos/RAWs/D5.ARW")));
        let old = file(98, "Downloads/old.JPG", FileKind::Image, Some(day - 200.0 * 86_400.0));
        assert_eq!(event_destination(&old, &spans), None);
    }

    #[test]
    fn loose_photos_group_into_dated_events() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let sep19 = 1_789_812_000.0;
        let oct4 = sep19 + 15.0 * 86_400.0;
        let mut files: Vec<_> = (0..6).map(|i| file(i, &format!("Downloads/A{i}.JPG"), FileKind::Image, Some(sep19 + i as f64 * 900.0))).collect();
        files.extend((0..5).map(|i| file(10 + i, &format!("B{i}.JPG"), FileKind::Image, Some(oct4 + i as f64 * 1_800.0))));
        files.push(file(20, "Downloads/lonely.JPG", FileKind::Image, Some(oct4 + 40.0 * 86_400.0)));
        files.push(file(21, "Kyle Baseball Game 9-19-26/Photos/JPEGs/K.JPG", FileKind::Image, Some(sep19)));
        let out = loose_event_destinations(&files, Path::new(ROOT), 8.0 * 3_600.0, 5, utc);
        let root = Path::new(ROOT);
        assert_eq!(out[&0].0, root.join("Photos/2026/2026-09-19/A0.JPG"));
        assert_eq!(out[&14].0, root.join("Photos/2026/2026-10-04/B4.JPG"));
        assert_eq!(out[&20].0, root.join("Photos/2026/November/lonely.JPG"));
        assert!(!out.contains_key(&21));
        let trip: Vec<_> = (0..6).map(|i| file(30 + i, &format!("trip{i}.JPG"), FileKind::Image, Some(sep19 + i as f64 * 5.0 * 3_600.0))).collect();
        let out = loose_event_destinations(&trip, root, 8.0 * 3_600.0, 5, utc);
        assert_eq!(out[&30].0, root.join("Photos/2026/2026-09-19 to 09-20/trip0.JPG"));
    }

    #[test]
    fn camera_names_and_tag_ids_are_not_folder_names() {
        for n in ["Dsc01548 Sunglasses", "Consumer_electronics Computer", "IMG 4821"] {
            assert!(!is_usable_folder_name(n), "{n}");
        }
        for n in ["Dogs", "Lake Trip"] {
            assert!(is_usable_folder_name(n), "{n}");
        }
    }
}
