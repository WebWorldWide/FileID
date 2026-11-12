// Library bulk rename and bulk tag, the parts that aren't widgets (macOS
// `BulkRenameSheet`, `BulkTagSheet` and `CatalogWrites.planRenames`): the
// collision-free rename plan, the undo journals kept on disk, why a recorded
// rename can no longer be put back, and the status lines.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Above this many chosen files the rename asks first (macOS `largeBatchThreshold`).
pub(super) const LARGE_BATCH: usize = 50;
/// Files per `renameFiles` command (macOS `renameChunk`).
pub(super) const RENAME_CHUNK: usize = 250;
/// A rename journal older than this stops offering "Undo last rename".
const JOURNAL_MAX_AGE_SECS: f64 = 7.0 * 24.0 * 60.0 * 60.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Candidate {
    pub file_id: i64,
    pub path: String,
    pub proposed: Option<String>,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenamePlan {
    pub file_id: i64,
    pub old_path: String,
    pub new_path: String,
    pub kind: String,
}

impl RenamePlan {
    pub fn old_name(&self) -> String {
        file_name(&self.old_path)
    }

    pub fn new_name(&self) -> String {
        file_name(&self.new_path)
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

/// Each file's suggested name in its own folder, adding `_2`…`_99` when the
/// name is taken on disk or by an earlier file in the batch. A file already
/// carrying its name is left out; one with no suggestion or no free name
/// counts in the second value. `taken(target, current)` says whether another
/// file holds `target` on disk.
pub(super) fn plan_renames(
    files: &[Candidate],
    taken: impl Fn(&Path, &Path) -> bool,
) -> (Vec<RenamePlan>, usize) {
    let mut claimed: HashSet<String> = HashSet::new();
    let mut plans = Vec::new();
    let mut unnamed = 0;
    for file in files {
        let Some(proposed) = file.proposed.as_deref().map(str::trim).filter(|p| !p.is_empty()) else {
            unnamed += 1;
            continue;
        };
        let old = Path::new(&file.path);
        let dir = old.parent().unwrap_or_else(|| Path::new(""));
        let ext = old.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let candidate = |bump: u32| -> PathBuf {
            let stem = if bump == 1 { proposed.to_owned() } else { format!("{proposed}_{bump}") };
            dir.join(if ext.is_empty() { stem } else { format!("{stem}.{ext}") })
        };
        let mut bump = 1;
        let mut target = candidate(bump);
        while claimed.contains(&target.to_string_lossy().to_lowercase()) || taken(&target, old) {
            bump += 1;
            if bump > 99 {
                break;
            }
            target = candidate(bump);
        }
        if bump > 99 {
            unnamed += 1;
            continue;
        }
        claimed.insert(target.to_string_lossy().to_lowercase());
        if target != old {
            plans.push(RenamePlan {
                file_id: file.file_id,
                old_path: file.path.clone(),
                new_path: target.to_string_lossy().into_owned(),
                kind: file.kind.clone(),
            });
        }
    }
    (plans, unnamed)
}

/// Whether `target` is held by a file other than `current` (a rename onto
/// itself, or a case-only change on one inode, is not a collision).
pub(super) fn taken_on_disk(target: &Path, current: &Path) -> bool {
    if !target.exists() {
        return false;
    }
    match (target.canonicalize(), current.canonicalize()) {
        (Ok(a), Ok(b)) => a != b,
        _ => true,
    }
}

pub(super) fn rename_confirm_body(count: usize) -> String {
    format!(
        "This will rename {count} files on disk. “Undo last rename” can revert it, but it won't restore filenames you've moved or renamed again afterwards."
    )
}

pub(super) fn rename_label(count: usize, in_flight: bool) -> String {
    if in_flight {
        "Renaming…".to_owned()
    } else {
        format!("Rename {count} file{}", if count == 1 { "" } else { "s" })
    }
}

pub(super) fn rename_status(renamed: usize, failed: usize, first_error: Option<&str>) -> String {
    if failed == 0 {
        format!("Renamed {renamed} file{}", if renamed == 1 { "" } else { "s" })
    } else {
        let mut line = format!("Renamed {renamed} · {failed} couldn't be renamed");
        if let Some(error) = first_error {
            line.push_str(&format!(" — {error}"));
        }
        line
    }
}

pub(super) fn undo_rename_status(undone: usize, skipped: usize, failed: usize) -> String {
    let mut line = format!("Put back {undone} original name{}", if undone == 1 { "" } else { "s" });
    if skipped > 0 {
        line.push_str(&format!(" · {skipped} skipped because the file moved or changed"));
    }
    if failed > 0 {
        line.push_str(&format!(" · {failed} couldn't be renamed back"));
    }
    line
}

pub(super) fn tag_status(added: usize, unchanged: usize, failed: usize, first_error: Option<&str>) -> String {
    let files = |n: usize| format!("{n} file{}", if n == 1 { "" } else { "s" });
    if failed == 0 && unchanged == 0 {
        format!("Tagged {}", files(added))
    } else if failed == 0 {
        format!("Tagged {added} · {unchanged} already had these tags")
    } else {
        let mut line = format!("Tagged {added} · {unchanged} unchanged · {failed} failed");
        if let Some(error) = first_error {
            line.push_str(&format!(" — {error}"));
        }
        line
    }
}

pub(super) fn undo_tag_status(undone: usize, failed: usize, first_error: Option<&str>) -> String {
    let mut line = format!("Removed tags from {undone} file{}", if undone == 1 { "" } else { "s" });
    if failed > 0 {
        line.push_str(&format!(" · {failed} couldn't be changed"));
    }
    if let Some(error) = first_error {
        line.push_str(&format!(" — {error}"));
    }
    line
}

/// Comma-separated tags, trimmed, empties and repeats dropped.
pub(super) fn parse_tags(raw: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    raw.split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty() && seen.insert((*tag).to_owned()))
        .map(str::to_owned)
        .collect()
}

pub(super) fn tag_title(count: usize) -> String {
    format!("Apply tags to {count} file{}", if count == 1 { "" } else { "s" })
}

// ── Undo journals ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenameOutcome {
    pub file_id: i64,
    pub old_path: String,
    pub new_path: String,
}

/// The tags one bulk-tag batch actually added to a file, so undo strips only those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TagOutcome {
    pub file_id: i64,
    pub tags: Vec<String>,
}

fn journal_dir() -> Option<PathBuf> {
    fileid_engine::paths::root().ok()
}

pub(super) fn rename_journal_path() -> Option<PathBuf> {
    journal_dir().map(|dir| dir.join("library-last-rename.json"))
}

pub(super) fn tag_journal_path() -> Option<PathBuf> {
    journal_dir().map(|dir| dir.join("library-last-tags.json"))
}

fn write_atomic(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temp = path.with_extension("json.tmp");
    if std::fs::write(&temp, bytes).is_ok() && std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

/// A batch where every file failed must not replace a still-valid earlier one.
pub(super) fn save_rename_batch(path: &Path, entries: &[RenameOutcome], now: f64) {
    if entries.is_empty() {
        return;
    }
    let entries: Vec<Value> = entries
        .iter()
        .map(|e| json!({"fileID": e.file_id, "oldPath": e.old_path, "newPath": e.new_path}))
        .collect();
    write_atomic(path, json!({"savedAt": now, "entries": entries}).to_string().as_bytes());
}

pub(super) fn load_rename_batch(path: &Path, now: f64) -> Option<Vec<RenameOutcome>> {
    let value: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if now - value.get("savedAt")?.as_f64()? >= JOURNAL_MAX_AGE_SECS {
        return None;
    }
    let entries: Vec<RenameOutcome> = value
        .get("entries")?
        .as_array()?
        .iter()
        .filter_map(|e| {
            Some(RenameOutcome {
                file_id: e.get("fileID")?.as_i64()?,
                old_path: e.get("oldPath")?.as_str()?.to_owned(),
                new_path: e.get("newPath")?.as_str()?.to_owned(),
            })
        })
        .collect();
    (!entries.is_empty()).then_some(entries)
}

pub(super) fn clear_journal(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Successful additions are recorded; a clean all-unchanged batch clears the
/// journal so a stale earlier batch can't be undone by mistake; a batch with
/// failures and nothing added leaves the earlier journal alone.
pub(super) fn record_tag_batch(path: &Path, outcomes: &[TagOutcome], failed: usize) {
    if !outcomes.is_empty() {
        let entries: Vec<Value> = outcomes.iter().map(|o| json!({"fileID": o.file_id, "tags": o.tags})).collect();
        write_atomic(path, json!({"entries": entries}).to_string().as_bytes());
    } else if failed == 0 {
        clear_journal(path);
    }
}

pub(super) fn load_tag_batch(path: &Path) -> Option<Vec<TagOutcome>> {
    let value: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let entries: Vec<TagOutcome> = value
        .get("entries")?
        .as_array()?
        .iter()
        .filter_map(|e| {
            Some(TagOutcome {
                file_id: e.get("fileID")?.as_i64()?,
                tags: e.get("tags")?.as_array()?.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect(),
            })
        })
        .filter(|o| !o.tags.is_empty())
        .collect();
    (!entries.is_empty()).then_some(entries)
}

/// The tags in `wanted` that `existing` lacks, in `wanted` order.
pub(super) fn new_tags(wanted: &[String], existing: &HashSet<String>) -> Vec<String> {
    wanted.iter().filter(|tag| !existing.contains(*tag)).cloned().collect()
}

/// Why a recorded rename can't be put back, or `None` when it can: the file
/// moved or changed since, or its old name is taken by another file.
pub(super) fn undo_blocker(
    outcome: &RenameOutcome,
    catalog_path: Option<&str>,
    exists: impl Fn(&Path) -> bool,
    same_file: impl Fn(&Path, &Path) -> bool,
) -> Option<&'static str> {
    let new = Path::new(&outcome.new_path);
    let old = Path::new(&outcome.old_path);
    if catalog_path != Some(outcome.new_path.as_str()) || !exists(new) || new.parent() != old.parent() {
        return Some("moved");
    }
    let case_only = file_name(&outcome.old_path).to_lowercase() == file_name(&outcome.new_path).to_lowercase();
    if exists(old) && !(case_only && same_file(old, new)) {
        return Some("occupied");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: i64, path: &str, proposed: Option<&str>) -> Candidate {
        Candidate { file_id: id, path: path.into(), proposed: proposed.map(str::to_owned), kind: "image".into() }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fileid-bulk-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names_keep_the_extension_and_bump_on_collisions() {
        let files = [
            cand(1, "/p/IMG_1.heic", Some("Beach day")),
            cand(2, "/p/IMG_2.heic", Some("Beach day")),
            cand(3, "/q/IMG_3.heic", Some("Beach day")),
            cand(4, "/p/IMG_4", Some("No extension")),
            cand(5, "/p/IMG_5.jpg", None),
            cand(6, "/p/Same.jpg", Some("Same")),
        ];
        let (plans, unnamed) = plan_renames(&files, |_, _| false);
        let names: Vec<_> = plans.iter().map(|p| (p.file_id, p.new_name())).collect();
        assert_eq!(
            names,
            vec![
                (1, "Beach day.heic".to_owned()),
                (2, "Beach day_2.heic".to_owned()),
                (3, "Beach day.heic".to_owned()),
                (4, "No extension".to_owned()),
            ]
        );
        assert_eq!(unnamed, 1);
    }

    #[test]
    fn a_name_taken_on_disk_gets_the_next_free_bump_and_case_folds_in_a_batch() {
        let taken = |target: &Path, _: &Path| target == Path::new("/p/Trip.jpg");
        let files = [cand(1, "/p/a.jpg", Some("Trip")), cand(2, "/p/b.jpg", Some("TRIP_2"))];
        let (plans, _) = plan_renames(&files, taken);
        assert_eq!(plans[0].new_name(), "Trip_2.jpg");
        assert_eq!(plans[1].new_name(), "TRIP_2_2.jpg");
    }

    #[test]
    fn running_out_of_free_names_counts_as_unnamed() {
        let (plans, unnamed) = plan_renames(&[cand(1, "/p/a.jpg", Some("X"))], |_, _| true);
        assert!(plans.is_empty());
        assert_eq!(unnamed, 1);
    }

    #[test]
    fn a_file_already_carrying_its_name_is_left_out() {
        let (plans, unnamed) = plan_renames(&[cand(1, "/p/Trip.jpg", Some("Trip"))], |_, _| false);
        assert!(plans.is_empty());
        assert_eq!(unnamed, 0);
    }

    #[test]
    fn taken_on_disk_ignores_the_file_itself() {
        let dir = scratch("taken");
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "y").unwrap();
        assert!(!taken_on_disk(&a, &a));
        assert!(taken_on_disk(&b, &a));
        assert!(!taken_on_disk(&dir.join("free.txt"), &a));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rename_status_lines_read_like_the_mac() {
        assert_eq!(rename_status(1, 0, None), "Renamed 1 file");
        assert_eq!(rename_status(3, 2, Some("busy")), "Renamed 3 · 2 couldn't be renamed — busy");
        assert_eq!(rename_label(1, false), "Rename 1 file");
        assert_eq!(rename_label(7, true), "Renaming…");
        assert_eq!(undo_rename_status(1, 0, 0), "Put back 1 original name");
        assert_eq!(
            undo_rename_status(2, 1, 3),
            "Put back 2 original names · 1 skipped because the file moved or changed · 3 couldn't be renamed back"
        );
    }

    #[test]
    fn tag_status_lines_read_like_the_mac() {
        assert_eq!(tag_status(2, 0, 0, None), "Tagged 2 files");
        assert_eq!(tag_status(1, 0, 0, None), "Tagged 1 file");
        assert_eq!(tag_status(2, 3, 0, None), "Tagged 2 · 3 already had these tags");
        assert_eq!(tag_status(2, 3, 1, Some("x")), "Tagged 2 · 3 unchanged · 1 failed — x");
        assert_eq!(undo_tag_status(2, 1, None), "Removed tags from 2 files · 1 couldn't be changed");
        assert_eq!(tag_title(1), "Apply tags to 1 file");
    }

    #[test]
    fn tags_split_on_commas_and_drop_blanks_and_repeats() {
        assert_eq!(parse_tags(" Vacation, Family ,,Vacation,Important "), vec!["Vacation", "Family", "Important"]);
        assert!(parse_tags(" , ").is_empty());
    }

    #[test]
    fn only_tags_a_file_lacked_are_recorded() {
        let existing: HashSet<String> = ["Family".to_owned()].into_iter().collect();
        let wanted = vec!["Vacation".to_owned(), "Family".to_owned()];
        assert_eq!(new_tags(&wanted, &existing), vec!["Vacation"]);
    }

    #[test]
    fn rename_journal_round_trips_expires_and_ignores_empty_batches() {
        let dir = scratch("rename-journal");
        let path = dir.join("r.json");
        let entries = vec![RenameOutcome { file_id: 1, old_path: "/p/a.jpg".into(), new_path: "/p/b.jpg".into() }];
        save_rename_batch(&path, &entries, 1_000.0);
        assert_eq!(load_rename_batch(&path, 2_000.0), Some(entries.clone()));
        save_rename_batch(&path, &[], 3_000.0);
        assert_eq!(load_rename_batch(&path, 2_000.0), Some(entries));
        assert_eq!(load_rename_batch(&path, 1_000.0 + JOURNAL_MAX_AGE_SECS + 1.0), None);
        clear_journal(&path);
        assert_eq!(load_rename_batch(&path, 2_000.0), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tag_journal_keeps_an_earlier_batch_through_a_total_failure() {
        let dir = scratch("tag-journal");
        let path = dir.join("t.json");
        let first = vec![TagOutcome { file_id: 1, tags: vec!["A".into()] }];
        record_tag_batch(&path, &first, 0);
        assert_eq!(load_tag_batch(&path), Some(first.clone()));
        record_tag_batch(&path, &[], 2);
        assert_eq!(load_tag_batch(&path), Some(first));
        record_tag_batch(&path, &[], 0);
        assert_eq!(load_tag_batch(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_rename_can_only_be_put_back_while_the_file_is_still_there() {
        let outcome = RenameOutcome { file_id: 1, old_path: "/p/a.jpg".into(), new_path: "/p/b.jpg".into() };
        let present = |p: &Path| p == Path::new("/p/b.jpg");
        let never = |_: &Path, _: &Path| false;
        assert_eq!(undo_blocker(&outcome, Some("/p/b.jpg"), present, never), None);
        assert_eq!(undo_blocker(&outcome, Some("/p/moved.jpg"), present, never), Some("moved"));
        assert_eq!(undo_blocker(&outcome, None, present, never), Some("moved"));
        assert_eq!(undo_blocker(&outcome, Some("/p/b.jpg"), |_: &Path| false, never), Some("moved"));
        let both = |_: &Path| true;
        assert_eq!(undo_blocker(&outcome, Some("/p/b.jpg"), both, never), Some("occupied"));
        let case = RenameOutcome { file_id: 1, old_path: "/p/A.jpg".into(), new_path: "/p/a.jpg".into() };
        assert_eq!(undo_blocker(&case, Some("/p/a.jpg"), both, |_, _| true), None);
        let elsewhere = RenameOutcome { file_id: 1, old_path: "/q/a.jpg".into(), new_path: "/p/a.jpg".into() };
        assert_eq!(undo_blocker(&elsewhere, Some("/p/a.jpg"), present, never), Some("moved"));
    }
}
