// Best Takes review rules, kept free of GTK so they can be tested: what the
// buttons and toasts say, which tries are kept by default, which files a
// "keep" set sends to Trash, which group opens next and where Zoom lands.
// Mirrors the macOS TakeReview / TakeWorkbench (DESIGN_V3_PORT "Best Takes").

use std::collections::BTreeSet;

use fileid_engine::ipc::{
    BulkActionResult, CatalogTakeAutoGroup, CatalogTakeBox, CatalogTakeDecision, CatalogTakeQuality,
};

pub fn is_video(group: &CatalogTakeAutoGroup) -> bool {
    group.kind == "videos"
}

pub fn noun(group: &CatalogTakeAutoGroup, count: usize) -> &'static str {
    match (is_video(group), count == 1) {
        (false, true) => "photo",
        (false, false) => "photos",
        (true, true) => "clip",
        (true, false) => "clips",
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

/// Which group the page shows after the list reloads (macOS `reloadAuto`).
/// `resolved` is the group just trashed, kept or skipped and its position in
/// the list before the reload: when it was the one on screen, review flows on
/// to the open group now at that position. Otherwise the current group stays,
/// and a vanished one falls back to the first open group.
pub fn next_selection(
    groups: &[CatalogTakeAutoGroup],
    current: Option<&str>,
    resolved: Option<(&str, usize)>,
) -> Option<usize> {
    let index_of = |key: &str| groups.iter().position(|group| group.group_key == key);
    if let Some((key, position)) = resolved
        && (current.is_none() || current == Some(key))
    {
        let open: Vec<usize> = groups
            .iter()
            .enumerate()
            .filter(|(_, group)| is_open(group) && group.group_key != key)
            .map(|(index, _)| index)
            .collect();
        return open
            .get(position.min(open.len().saturating_sub(1)))
            .copied();
    }
    match current {
        Some(key) => index_of(key).or_else(|| first_open(groups)),
        None => first_open(groups),
    }
}

pub fn best(qualities: &[CatalogTakeQuality]) -> Option<&CatalogTakeQuality> {
    qualities
        .iter()
        .find(|quality| quality.recommended)
        .or_else(|| qualities.iter().min_by_key(|quality| quality.rank))
}

pub fn best_index(qualities: &[CatalogTakeQuality]) -> Option<usize> {
    let id = best(qualities)?.primary_file_id;
    qualities
        .iter()
        .position(|quality| quality.primary_file_id == id)
}

/// Everything starts as "keep the best"; the user adds or removes marks.
pub fn default_keep(qualities: &[CatalogTakeQuality]) -> BTreeSet<i64> {
    best(qualities)
        .map(|quality| quality.primary_file_id)
        .into_iter()
        .collect()
}

pub fn toggle(keep: &mut BTreeSet<i64>, primary_file_id: i64) -> bool {
    if keep.remove(&primary_file_id) {
        false
    } else {
        keep.insert(primary_file_id);
        true
    }
}

/// Every file (RAW and JPEG twins included) of the tries that are not kept.
pub fn trash_ids(qualities: &[CatalogTakeQuality], keep: &BTreeSet<i64>) -> Vec<i64> {
    let mut ids: Vec<i64> = victims(qualities, keep)
        .flat_map(|quality| quality.file_ids.iter().copied())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub fn victims<'a>(
    qualities: &'a [CatalogTakeQuality],
    keep: &'a BTreeSet<i64>,
) -> impl Iterator<Item = &'a CatalogTakeQuality> {
    qualities
        .iter()
        .filter(|quality| !keep.contains(&quality.primary_file_id))
}

pub fn kept<'a>(
    qualities: &'a [CatalogTakeQuality],
    keep: &'a BTreeSet<i64>,
) -> impl Iterator<Item = &'a CatalogTakeQuality> {
    qualities
        .iter()
        .filter(|quality| keep.contains(&quality.primary_file_id))
}

pub struct TrashButton {
    pub label: String,
    pub tooltip: String,
    pub enabled: bool,
}

/// The primary button: its label, its tooltip (why it is disabled, or what
/// Return does) and whether it can run.
pub fn trash_button(
    group: &CatalogTakeAutoGroup,
    qualities: &[CatalogTakeQuality],
    keep: &BTreeSet<i64>,
) -> TrashButton {
    let others = victims(qualities, keep).count();
    let marked = kept(qualities, keep).count();
    let best_only =
        best(qualities).is_some_and(|best| marked <= 1 && keep.contains(&best.primary_file_id));
    let label = if best_only {
        format!("Keep the best, move the other {others} to Trash")
    } else {
        format!("Keep {marked}, move the other {others} to Trash")
    };
    let tooltip = if marked == 0 {
        format!("Mark at least one {} to keep (Space)", noun(group, 1))
    } else if others == 0 {
        "Everything here is marked keep. Use Keep all.".to_owned()
    } else {
        format!(
            "Moves the other {} to the Trash. You can undo right after. (Return)",
            noun(group, 2)
        )
    };
    TrashButton {
        label,
        tooltip,
        enabled: marked > 0 && others > 0,
    }
}

pub fn confirm_heading(
    group: &CatalogTakeAutoGroup,
    qualities: &[CatalogTakeQuality],
    keep: &BTreeSet<i64>,
) -> String {
    let others = victims(qualities, keep).count();
    format!("Move {others} {} to Trash?", noun(group, others))
}

pub fn confirm_body(qualities: &[CatalogTakeQuality], keep: &BTreeSet<i64>) -> String {
    let names: Vec<String> = kept(qualities, keep)
        .map(|quality| crate::v3::file_name(&quality.path))
        .collect();
    let tries = victims(qualities, keep).count();
    let files: usize = victims(qualities, keep)
        .map(|quality| quality.file_ids.len())
        .sum();
    let extra = if files > tries {
        format!(" ({files} files, including RAW copies)")
    } else {
        String::new()
    };
    format!(
        "Keeps {}. The others{extra} go to the Trash. You can undo right after, or put them back from the Trash.",
        names.join(", ")
    )
}

/// How many of the doomed tries had at least one file moved.
pub fn moved_tries(doomed: &[CatalogTakeQuality], result: &BulkActionResult) -> usize {
    let moved: BTreeSet<i64> = result
        .messages
        .iter()
        .filter(|item| item.ok)
        .filter_map(|item| item.file_id)
        .collect();
    doomed
        .iter()
        .filter(|quality| quality.file_ids.iter().any(|id| moved.contains(id)))
        .count()
}

pub fn moved_message(group: &CatalogTakeAutoGroup, moved: usize, failed: u32) -> String {
    let mut text = format!("Moved {moved} {} to Trash", noun(group, moved));
    if failed > 0 {
        text.push_str(&format!(" · {failed} couldn't be moved"));
    }
    text
}

pub fn restored_message(result: &BulkActionResult) -> String {
    let count = result.succeeded;
    if count > 0 {
        format!("Put back {count} file{}", if count == 1 { "" } else { "s" })
    } else {
        result
            .messages
            .iter()
            .find_map(|item| item.message.clone())
            .unwrap_or_else(|| "Nothing was put back. Check the Trash.".to_owned())
    }
}

pub fn decided_message(group: &CatalogTakeAutoGroup, decision: &str) -> String {
    if decision == "keepAll" {
        format!(
            "Kept all {}. This group won't be suggested again.",
            group.shots.len()
        )
    } else {
        "Skipped. It's at the bottom of the list.".to_owned()
    }
}

pub fn decision(group: &CatalogTakeAutoGroup, kind: &str) -> CatalogTakeDecision {
    decision_for(group, kind, group.file_ids())
}

pub fn decision_for(
    group: &CatalogTakeAutoGroup,
    kind: &str,
    file_ids: Vec<i64>,
) -> CatalogTakeDecision {
    CatalogTakeDecision {
        group_key: group.group_key.clone(),
        decision: kind.to_owned(),
        file_ids,
    }
}

/// The two panes of Compare: the best try, and either the focused try or,
/// when the best is focused, the runner-up.
pub fn compare_pair(qualities: &[CatalogTakeQuality], focused: usize) -> Option<(usize, usize)> {
    let best_index = best_index(qualities)?;
    let other = if focused == best_index || focused >= qualities.len() {
        qualities
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != best_index)
            .min_by_key(|(_, quality)| quality.rank)
            .map(|(index, _)| index)?
    } else {
        focused
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

/// What "Ask about…" attaches to a Chat question: the kept tries when only
/// some are kept, otherwise every try; and its label.
pub fn ask_targets(qualities: &[CatalogTakeQuality], keep: &BTreeSet<i64>) -> (Vec<i64>, String) {
    let subset = !keep.is_empty() && keep.len() < qualities.len();
    let ids: Vec<i64> = qualities.iter().map(|q| q.primary_file_id).filter(|id| !subset || keep.contains(id)).collect();
    let label = if subset { format!("Ask about {} kept", ids.len()) } else { format!("Ask about all {}", ids.len()) };
    (ids, label)
}

pub fn hint_line(group: &CatalogTakeAutoGroup) -> String {
    format!(
        "← → move · Space keep · Z zoom · Return keeps the marked {} and moves the rest to Trash",
        noun(group, 2)
    )
}

pub fn zoom_label(group: &CatalogTakeAutoGroup, zoomed: bool) -> String {
    if zoomed {
        format!("Show whole {}", noun(group, 1))
    } else if is_video(group) {
        "Zoom in".to_owned()
    } else {
        "Zoom to face".to_owned()
    }
}

pub fn score_percent(quality: &CatalogTakeQuality) -> i64 {
    (quality.score.clamp(0.0, 1.0) * 100.0).round() as i64
}

pub fn has_raw_twin(quality: &CatalogTakeQuality) -> bool {
    quality.file_ids.len() > 1
}

pub fn step_focus(index: usize, delta: i64, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (index as i64 + delta).clamp(0, len as i64 - 1) as usize
}

pub fn tile_spoken(
    group: &CatalogTakeAutoGroup,
    quality: &CatalogTakeQuality,
    index: usize,
    count: usize,
    best: bool,
    kept: bool,
) -> String {
    let mut noun = noun(group, 1).to_owned();
    noun[..1].make_ascii_uppercase();
    format!(
        "{noun} {} of {count}, {}{}{}, score {}",
        index + 1,
        crate::v3::file_name(&quality.path),
        if best { ", recommended" } else { "" },
        if kept { ", keep" } else { "" },
        score_percent(quality)
    )
}

pub fn keep_announcement(quality: &CatalogTakeQuality, kept: bool) -> String {
    let name = crate::v3::file_name(&quality.path);
    if kept {
        format!("Keeping {name}")
    } else {
        format!("Not keeping {name}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasonTone {
    Positive,
    Goal,
    Neutral,
}

const POSITIVE_REASONS: [&str; 9] = [
    "Sharpest face",
    "Sharpest",
    "Eyes open",
    "Every face visible",
    "Best exposure",
    "Steadiest",
    "Most action",
    "Shows the goal",
    "Closest to the goal",
];

pub fn reason_tone(reason: &str) -> ReasonTone {
    if !POSITIVE_REASONS.contains(&reason) {
        ReasonTone::Neutral
    } else if reason.contains("goal") {
        ReasonTone::Goal
    } else {
        ReasonTone::Positive
    }
}

pub fn reasons(quality: &CatalogTakeQuality) -> Vec<String> {
    quality.reasons.iter().take(4).cloned().collect()
}

/// The box Zoom shows: the largest face × 2.6, else the sharp subject × 1.2,
/// at least 12 % of the frame, kept inside the image.
pub fn zoom_box(quality: &CatalogTakeQuality) -> Option<CatalogTakeBox> {
    let face = quality
        .faces
        .iter()
        .copied()
        .max_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)));
    let (base, factor) = match face {
        Some(face) => (face, 2.6),
        None => (quality.subject?, 1.2),
    };
    let width = (base.width * factor).clamp(0.12, 1.0);
    let height = (base.height * factor).clamp(0.12, 1.0);
    let x = (base.x + base.width / 2.0 - width / 2.0).clamp(0.0, 1.0 - width);
    let y = (base.y + base.height / 2.0 - height / 2.0).clamp(0.0, 1.0 - height);
    Some(CatalogTakeBox {
        x,
        y,
        width,
        height,
    })
}

pub const WHOLE: CatalogTakeBox = CatalogTakeBox {
    x: 0.0,
    y: 0.0,
    width: 1.0,
    height: 1.0,
};

pub fn lerp_box(from: CatalogTakeBox, to: CatalogTakeBox, t: f64) -> CatalogTakeBox {
    let mix = |a: f64, b: f64| a + (b - a) * t;
    CatalogTakeBox {
        x: mix(from.x, to.x),
        y: mix(from.y, to.y),
        width: mix(from.width, to.width),
        height: mix(from.height, to.height),
    }
}

/// Scale and top-left offset that fit `region` (normalized) of an image of
/// `image` size inside a `view`, centred: `WHOLE` is an aspect fit, or an
/// aspect fill with `cover`.
pub fn view_transform(
    image: (f64, f64),
    view: (f64, f64),
    region: CatalogTakeBox,
    cover: bool,
) -> (f64, f64, f64) {
    let (iw, ih) = image;
    let (vw, vh) = view;
    if iw <= 0.0 || ih <= 0.0 || region.width <= 0.0 || region.height <= 0.0 {
        return (1.0, 0.0, 0.0);
    }
    let (sx, sy) = (vw / (region.width * iw), vh / (region.height * ih));
    let scale = if cover { sx.max(sy) } else { sx.min(sy) };
    let (w, h) = (iw * scale, ih * scale);
    let x = vw / 2.0 - (region.x + region.width / 2.0) * w;
    let y = vh / 2.0 - (region.y + region.height / 2.0) * h;
    (scale, x, y)
}

/// RGBA rows to Cairo's native-endian premultiplied ARGB32.
pub fn rgba_to_argb32(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for pixel in rgba.as_chunks::<4>().0 {
        let alpha = u32::from(pixel[3]);
        let premultiply = |channel: u8| (u32::from(channel) * alpha + 127) / 255;
        let value = (alpha << 24)
            | (premultiply(pixel[0]) << 16)
            | (premultiply(pixel[1]) << 8)
            | premultiply(pixel[2]);
        out.extend_from_slice(&value.to_ne_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::{BulkActionItem, CatalogTakeShot};

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

    fn keyed(key: &str, kind: &str, status: &str, ids: &[i64]) -> CatalogTakeAutoGroup {
        CatalogTakeAutoGroup {
            group_key: key.into(),
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
            relation: None,
            evidence: None,
        }
    }

    fn group(kind: &str, status: &str, ids: &[i64]) -> CatalogTakeAutoGroup {
        keyed("takes-1", kind, status, ids)
    }

    fn three() -> Vec<CatalogTakeQuality> {
        vec![
            quality(1, 2, false, &[1, 11]),
            quality(2, 1, true, &[2, 12]),
            quality(3, 3, false, &[3]),
        ]
    }

    fn bulk(ok: &[i64], failed: u32) -> BulkActionResult {
        BulkActionResult {
            action: "trashFiles:batch-1".into(),
            succeeded: ok.len() as u32,
            failed,
            messages: ok
                .iter()
                .map(|id| BulkActionItem {
                    file_id: Some(*id),
                    ok: true,
                    message: None,
                    reason: None,
                    operation_id: None,
                })
                .collect(),
        }
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
        assert_eq!(victims(&qualities, &keep).count(), 2);
    }

    #[test]
    fn toggling_changes_the_label_and_the_disabled_reasons() {
        let qualities = three();
        let g = group("photos", "open", &[1, 2, 3]);
        let mut keep = default_keep(&qualities);
        let button = trash_button(&g, &qualities, &keep);
        assert_eq!(button.label, "Keep the best, move the other 2 to Trash");
        assert!(button.enabled);
        assert!(button.tooltip.ends_with("(Return)"));
        assert!(toggle(&mut keep, 1));
        assert_eq!(
            trash_button(&g, &qualities, &keep).label,
            "Keep 2, move the other 1 to Trash"
        );
        toggle(&mut keep, 3);
        let all = trash_button(&g, &qualities, &keep);
        assert!(!all.enabled);
        assert!(all.tooltip.contains("Keep all"));
        keep.clear();
        let none = trash_button(&g, &qualities, &keep);
        assert!(!none.enabled);
        assert_eq!(none.tooltip, "Mark at least one photo to keep (Space)");
        assert!(!toggle(&mut [2].into_iter().collect(), 2));
    }

    #[test]
    fn confirmation_names_the_kept_file_and_counts_raw_copies_only_when_there_are_some() {
        let qualities = three();
        let keep = default_keep(&qualities);
        let g = group("photos", "open", &[1, 2, 3]);
        assert_eq!(
            confirm_heading(&g, &qualities, &keep),
            "Move 2 photos to Trash?"
        );
        assert!(
            confirm_body(&qualities, &keep)
                .starts_with("Keeps IMG_2.jpg. The others (3 files, including RAW copies) go")
        );
        let plain = vec![quality(1, 1, true, &[1]), quality(2, 2, false, &[2])];
        assert!(
            confirm_body(&plain, &default_keep(&plain))
                .starts_with("Keeps IMG_1.jpg. The others go to the Trash.")
        );
    }

    #[test]
    fn compare_shows_best_against_the_focused_try_or_the_runner_up() {
        let qualities = three();
        assert_eq!(compare_pair(&qualities, 2), Some((1, 2)));
        assert_eq!(compare_pair(&qualities, 1), Some((1, 0)));
        assert_eq!(compare_pair(&qualities, 99), Some((1, 0)));
        assert_eq!(compare_pair(&[], 0), None);
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
    fn a_resolved_group_hands_over_to_the_open_group_at_its_position() {
        let groups = vec![
            keyed("a", "photos", "open", &[1, 2]),
            keyed("c", "photos", "open", &[5, 6]),
            keyed("b", "photos", "skipped", &[3, 4]),
        ];
        assert_eq!(next_selection(&groups, Some("b"), Some(("b", 1))), Some(1));
        assert_eq!(next_selection(&groups, Some("c"), Some(("c", 9))), Some(0));
        assert_eq!(next_selection(&groups, Some("a"), Some(("b", 1))), Some(0));
        assert_eq!(next_selection(&groups, Some("c"), None), Some(1));
        assert_eq!(next_selection(&groups, Some("gone"), None), Some(0));
        assert_eq!(next_selection(&groups, None, None), Some(0));
        let only = vec![keyed("b", "photos", "skipped", &[3, 4])];
        assert_eq!(next_selection(&only, Some("b"), Some(("b", 0))), None);
        assert_eq!(next_selection(&[], None, None), None);
    }

    #[test]
    fn decisions_carry_the_whole_group_or_the_kept_files() {
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
        assert_eq!(decision_for(&g, "keepAll", vec![2]).file_ids, vec![2]);
    }

    #[test]
    fn toasts_count_tries_not_files() {
        let g = group("photos", "open", &[1, 2, 3]);
        let qualities = three();
        let doomed: Vec<_> = victims(&qualities, &default_keep(&qualities))
            .cloned()
            .collect();
        let result = bulk(&[1, 11, 3], 0);
        assert_eq!(moved_tries(&doomed, &result), 2);
        assert_eq!(moved_message(&g, 2, 0), "Moved 2 photos to Trash");
        assert_eq!(
            moved_message(&g, 1, 2),
            "Moved 1 photo to Trash · 2 couldn't be moved"
        );
        assert_eq!(moved_tries(&doomed, &bulk(&[11], 1)), 1);
        assert_eq!(restored_message(&bulk(&[1, 3], 0)), "Put back 2 files");
        assert_eq!(
            restored_message(&bulk(&[], 0)),
            "Nothing was put back. Check the Trash."
        );
        assert_eq!(
            decided_message(&g, "keepAll"),
            "Kept all 3. This group won't be suggested again."
        );
        assert_eq!(
            decided_message(&g, "skip"),
            "Skipped. It's at the bottom of the list."
        );
    }

    #[test]
    fn filmstrip_labels_follow_the_group_kind() {
        let g = group("photos", "open", &[1, 2, 3]);
        let v = group("videos", "open", &[1, 2]);
        let keep: BTreeSet<i64> = [2].into_iter().collect();
        assert_eq!(filmstrip_header(&g, &keep, 3), "ALL 3 PHOTOS · 1 TO KEEP");
        assert_eq!(zoom_label(&g, false), "Zoom to face");
        assert_eq!(zoom_label(&v, false), "Zoom in");
        assert_eq!(zoom_label(&v, true), "Show whole clip");
        assert!(hint_line(&v).ends_with("keeps the marked clips and moves the rest to Trash"));
        let q = quality(2, 1, true, &[2, 12]);
        assert_eq!(
            tile_spoken(&g, &q, 2, 8, true, true),
            "Photo 3 of 8, IMG_2.jpg, recommended, keep, score 100"
        );
        assert_eq!(keep_announcement(&q, false), "Not keeping IMG_2.jpg");
        assert!(has_raw_twin(&q));
        assert_eq!(step_focus(0, -1, 3), 0);
        assert_eq!(step_focus(1, 1, 3), 2);
        assert_eq!(step_focus(2, 1, 3), 2);
        assert_eq!(step_focus(0, 1, 0), 0);
    }

    #[test]
    fn reasons_are_toned_like_the_mac_chips() {
        assert_eq!(reason_tone("Sharpest face"), ReasonTone::Positive);
        assert_eq!(reason_tone("Closest to the goal"), ReasonTone::Goal);
        assert_eq!(reason_tone("Eyes closed"), ReasonTone::Neutral);
        let mut q = quality(1, 1, true, &[1]);
        q.reasons = (0..6).map(|n| n.to_string()).collect();
        assert_eq!(reasons(&q).len(), 4);
    }

    #[test]
    fn zoom_frames_the_largest_face_or_the_subject_inside_the_image() {
        let face = |x: f64, y: f64, w: f64, h: f64| CatalogTakeBox {
            x,
            y,
            width: w,
            height: h,
        };
        let mut q = quality(1, 1, true, &[1]);
        assert_eq!(zoom_box(&q), None);
        q.subject = Some(face(0.4, 0.4, 0.2, 0.2));
        let subject = zoom_box(&q).unwrap();
        assert!((subject.width - 0.24).abs() < 1e-9 && (subject.x - 0.38).abs() < 1e-9);
        q.faces = vec![face(0.0, 0.0, 0.05, 0.05), face(0.9, 0.1, 0.1, 0.1)];
        let zoomed = zoom_box(&q).unwrap();
        assert!((zoomed.width - 0.26).abs() < 1e-9);
        assert!((zoomed.x - 0.74).abs() < 1e-9, "kept inside the right edge");
        assert!((zoomed.y - 0.02).abs() < 1e-9);
    }

    #[test]
    fn the_view_transform_fits_the_whole_image_or_centres_the_zoom_box() {
        let (scale, x, y) = view_transform((200.0, 100.0), (100.0, 100.0), WHOLE, false);
        assert_eq!((scale, x, y), (0.5, 0.0, 25.0));
        let (scale, x, y) = view_transform((200.0, 100.0), (100.0, 100.0), WHOLE, true);
        assert_eq!((scale, x, y), (1.0, -50.0, 0.0));
        let quarter = CatalogTakeBox {
            x: 0.5,
            y: 0.5,
            width: 0.5,
            height: 0.5,
        };
        let (scale, x, y) = view_transform((100.0, 100.0), (100.0, 100.0), quarter, false);
        assert_eq!((scale, x, y), (2.0, -100.0, -100.0));
        let half = lerp_box(WHOLE, quarter, 0.5);
        assert_eq!((half.x, half.width), (0.25, 0.75));
        assert_eq!(
            view_transform((0.0, 1.0), (1.0, 1.0), WHOLE, false),
            (1.0, 0.0, 0.0)
        );
    }

    #[test]
    fn rgba_becomes_premultiplied_native_argb() {
        let out = rgba_to_argb32(&[255, 0, 0, 255, 255, 255, 255, 0, 200, 100, 50, 128]);
        let word =
            |index: usize| u32::from_ne_bytes(out[index * 4..index * 4 + 4].try_into().unwrap());
        assert_eq!(word(0), 0xFFFF_0000);
        assert_eq!(word(1), 0);
        assert_eq!(word(2), 0x8064_3219);
    }
}
