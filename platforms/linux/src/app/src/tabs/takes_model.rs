// Best Takes review rules, kept free of GTK so they can be tested: what the
// buttons say, which tries are kept by default and which files a "keep" set
// sends to Trash. Mirrors the macOS TakeReview wording (DESIGN_V3_PORT "Best Takes").

use std::collections::BTreeSet;

use fileid_engine::ipc::{CatalogTakeAutoGroup, CatalogTakeDecision, CatalogTakeQuality};

pub fn noun(group: &CatalogTakeAutoGroup, count: usize) -> &'static str {
    match (group.kind == "photos", count == 1) {
        (true, true) => "photo",
        (true, false) => "photos",
        (false, true) => "clip",
        (false, false) => "clips",
    }
}

pub fn group_title(group: &CatalogTakeAutoGroup) -> String {
    format!("{} {}", group.shots.len(), noun(group, group.shots.len()))
}

pub fn group_spoken(group: &CatalogTakeAutoGroup, date: &str) -> String {
    let skipped = if group.status == "skipped" {
        ", skipped"
    } else {
        ""
    };
    format!("{}, {date}{skipped}", group_title(group))
}

pub fn is_open(group: &CatalogTakeAutoGroup) -> bool {
    group.status != "skipped"
}

pub fn open_count(groups: &[CatalogTakeAutoGroup]) -> usize {
    groups.iter().filter(|group| is_open(group)).count()
}

/// The first group still waiting for a decision, else none.
pub fn first_open(groups: &[CatalogTakeAutoGroup]) -> Option<usize> {
    groups.iter().position(is_open)
}

pub fn best(qualities: &[CatalogTakeQuality]) -> Option<&CatalogTakeQuality> {
    qualities
        .iter()
        .find(|quality| quality.recommended)
        .or_else(|| qualities.iter().min_by_key(|quality| quality.rank))
}

/// Everything starts as "keep the best"; the user adds or removes marks.
pub fn default_keep(qualities: &[CatalogTakeQuality]) -> BTreeSet<i64> {
    best(qualities)
        .map(|quality| quality.primary_file_id)
        .into_iter()
        .collect()
}

pub fn toggle(keep: &mut BTreeSet<i64>, primary_file_id: i64) {
    if !keep.remove(&primary_file_id) {
        keep.insert(primary_file_id);
    }
}

/// Every file (RAW and JPEG twins included) of the tries that are not kept.
pub fn trash_ids(qualities: &[CatalogTakeQuality], keep: &BTreeSet<i64>) -> Vec<i64> {
    let mut ids: Vec<i64> = qualities
        .iter()
        .filter(|quality| !keep.contains(&quality.primary_file_id))
        .flat_map(|quality| quality.file_ids.iter().copied())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub fn dropped_tries(qualities: &[CatalogTakeQuality], keep: &BTreeSet<i64>) -> usize {
    qualities
        .iter()
        .filter(|quality| !keep.contains(&quality.primary_file_id))
        .count()
}

/// Label for the primary button and why it is disabled, if it is.
pub fn trash_button(
    group: &CatalogTakeAutoGroup,
    qualities: &[CatalogTakeQuality],
    keep: &BTreeSet<i64>,
) -> (String, Option<&'static str>) {
    let dropped = dropped_tries(qualities, keep);
    let kept = qualities.len() - dropped;
    let best_only =
        best(qualities).is_some_and(|best| keep.len() == 1 && keep.contains(&best.primary_file_id));
    let label = if best_only {
        format!("Keep the best, move the other {dropped} to Trash")
    } else {
        format!("Keep {kept}, move the other {dropped} to Trash")
    };
    let reason = if kept == 0 {
        Some("Mark at least one try to keep.")
    } else if dropped == 0 {
        Some("Every try is marked keep. Use Keep all to close this group.")
    } else {
        None
    };
    let _ = group;
    (label, reason)
}

pub fn confirm_heading(
    group: &CatalogTakeAutoGroup,
    qualities: &[CatalogTakeQuality],
    keep: &BTreeSet<i64>,
) -> String {
    let dropped = dropped_tries(qualities, keep);
    format!("Move {dropped} {} to Trash?", noun(group, dropped))
}

pub fn confirm_body(qualities: &[CatalogTakeQuality], keep: &BTreeSet<i64>) -> String {
    let kept: Vec<String> = qualities
        .iter()
        .filter(|quality| keep.contains(&quality.primary_file_id))
        .map(|quality| crate::v3::file_name(&quality.path))
        .collect();
    let files = trash_ids(qualities, keep).len();
    format!(
        "Keeps {}. The others ({files} file{}, including RAW copies) go to the Trash. You can undo right after, or put them back from the Trash.",
        kept.join(", "),
        if files == 1 { "" } else { "s" },
    )
}

pub fn decision(group: &CatalogTakeAutoGroup, kind: &str) -> CatalogTakeDecision {
    CatalogTakeDecision {
        group_key: group.group_key.clone(),
        decision: kind.to_owned(),
        file_ids: group.file_ids(),
    }
}

/// The two panes of Compare: the best try, and either the focused try or,
/// when the best is focused, the runner-up.
pub fn compare_pair(qualities: &[CatalogTakeQuality], focused: i64) -> Option<(usize, usize)> {
    let best_index = qualities.iter().position(|quality| {
        Some(quality.primary_file_id) == best(qualities).map(|b| b.primary_file_id)
    })?;
    let other = if qualities[best_index].primary_file_id == focused {
        qualities
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != best_index)
            .min_by_key(|(_, quality)| quality.rank)
            .map(|(index, _)| index)?
    } else {
        qualities
            .iter()
            .position(|quality| quality.primary_file_id == focused)?
    };
    Some((best_index, other))
}

pub fn filmstrip_header(
    group: &CatalogTakeAutoGroup,
    keep: &BTreeSet<i64>,
    total: usize,
) -> String {
    format!(
        "ALL {total} {} · {} TO KEEP",
        noun(group, total).to_uppercase(),
        keep.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::CatalogTakeShot;

    fn quality(id: i64, rank: usize, recommended: bool, files: &[i64]) -> CatalogTakeQuality {
        CatalogTakeQuality {
            primary_file_id: id,
            file_ids: files.to_vec(),
            path: format!("/shoot/IMG_{id}.jpg"),
            score: 1.0 / rank as f64,
            rank,
            recommended,
            reasons: vec![],
            sharpness: None,
            exposure: None,
            motion_blur: None,
            face_count: None,
            eyes_closed: None,
            face_cut_off: None,
            stability: None,
            action: None,
            peak_seconds: None,
            subject: None,
            faces: vec![],
        }
    }

    fn group(kind: &str, status: &str, ids: &[i64]) -> CatalogTakeAutoGroup {
        CatalogTakeAutoGroup {
            group_key: "takes-1".into(),
            kind: kind.into(),
            status: status.into(),
            captured_at: 0.0,
            similarity: 0.9,
            shots: ids
                .iter()
                .map(|id| CatalogTakeShot {
                    primary_file_id: *id,
                    file_ids: vec![*id],
                    path: format!("/shoot/IMG_{id}.jpg"),
                    captured_at: None,
                })
                .collect(),
        }
    }

    fn three() -> Vec<CatalogTakeQuality> {
        vec![
            quality(1, 2, false, &[1, 11]),
            quality(2, 1, true, &[2, 12]),
            quality(3, 3, false, &[3]),
        ]
    }

    #[test]
    fn titles_use_photos_or_clips_and_skip_marks_the_spoken_form() {
        assert_eq!(
            group_title(&group("photos", "open", &[1, 2, 3])),
            "3 photos"
        );
        assert_eq!(group_title(&group("videos", "open", &[1, 2])), "2 clips");
        assert_eq!(
            group_spoken(&group("photos", "skipped", &[1, 2]), "Sat 19 Sep"),
            "2 photos, Sat 19 Sep, skipped"
        );
    }

    #[test]
    fn the_best_try_is_kept_by_default_and_everything_else_is_trashed_with_its_twins() {
        let qualities = three();
        let keep = default_keep(&qualities);
        assert_eq!(keep.iter().copied().collect::<Vec<_>>(), vec![2]);
        assert_eq!(trash_ids(&qualities, &keep), vec![1, 3, 11]);
        assert_eq!(dropped_tries(&qualities, &keep), 2);
    }

    #[test]
    fn toggling_changes_the_label_and_the_disabled_reasons() {
        let qualities = three();
        let g = group("photos", "open", &[1, 2, 3]);
        let mut keep = default_keep(&qualities);
        assert_eq!(
            trash_button(&g, &qualities, &keep),
            ("Keep the best, move the other 2 to Trash".into(), None)
        );
        toggle(&mut keep, 1);
        assert_eq!(
            trash_button(&g, &qualities, &keep).0,
            "Keep 2, move the other 1 to Trash"
        );
        toggle(&mut keep, 3);
        assert!(trash_button(&g, &qualities, &keep)
            .1
            .unwrap()
            .contains("Keep all"));
        keep.clear();
        assert!(trash_button(&g, &qualities, &keep)
            .1
            .unwrap()
            .contains("at least one"));
    }

    #[test]
    fn confirmation_names_the_kept_file_and_counts_every_file() {
        let qualities = three();
        let keep = default_keep(&qualities);
        let g = group("photos", "open", &[1, 2, 3]);
        assert_eq!(
            confirm_heading(&g, &qualities, &keep),
            "Move 2 photos to Trash?"
        );
        assert!(confirm_body(&qualities, &keep)
            .starts_with("Keeps IMG_2.jpg. The others (3 files, including RAW copies)"));
    }

    #[test]
    fn compare_shows_best_against_the_focused_try_or_the_runner_up() {
        let qualities = three();
        assert_eq!(compare_pair(&qualities, 3), Some((1, 2)));
        assert_eq!(compare_pair(&qualities, 2), Some((1, 0)));
        assert_eq!(compare_pair(&qualities, 99), None);
    }

    #[test]
    fn groups_open_in_engine_order_and_skipped_ones_are_not_open() {
        let groups = vec![
            group("photos", "skipped", &[1, 2]),
            group("photos", "open", &[3, 4]),
            group("videos", "open", &[5, 6]),
        ];
        assert_eq!(open_count(&groups), 2);
        assert_eq!(first_open(&groups), Some(1));
        assert_eq!(first_open(&groups[..1]), None);
    }

    #[test]
    fn decisions_carry_the_whole_group() {
        let g = group("photos", "open", &[1, 2]);
        let d = decision(&g, "skip");
        assert_eq!(
            (
                d.group_key.as_str(),
                d.decision.as_str(),
                d.file_ids.clone()
            ),
            ("takes-1", "skip", vec![1, 2])
        );
    }

    #[test]
    fn filmstrip_header_counts_tries_and_marks() {
        let g = group("photos", "open", &[1, 2, 3]);
        let keep: BTreeSet<i64> = [2].into_iter().collect();
        assert_eq!(filmstrip_header(&g, &keep, 3), "ALL 3 PHOTOS · 1 TO KEEP");
    }
}
