// Manual Best Takes groups (DESIGN_V3_PORT "Best Takes", manual groups), kept
// free of GTK: the clip pool, when a group can be saved or scored, take card
// wording and the feedback each card button sends. Mirrors the macOS
// TakeWorkbench / TakeCard.

use std::collections::BTreeSet;

use fileid_engine::ipc::{
    CatalogEvent, CatalogHit, CatalogJob, CatalogTake, CatalogTakeFeedback,
    CatalogTakeGroupSuggestion, CatalogTakeRecommendation,
};

use crate::shell::status::Tone;

pub fn is_video_path(path: &str) -> bool {
    use fileid_engine::pipeline::discovery::FileKind;
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(FileKind::from_extension(ext), FileKind::Video))
}

/// Search candidates first, then clips found here, each file once.
pub fn pool(candidates: &[CatalogHit], found: &[CatalogHit]) -> Vec<CatalogHit> {
    let mut seen = BTreeSet::new();
    candidates
        .iter()
        .chain(found)
        .filter(|hit| seen.insert(hit.file_id))
        .cloned()
        .collect()
}

/// Video hits from a clip search that are not already candidates.
pub fn found_videos(hits: &[CatalogHit], candidates: &[CatalogHit]) -> Vec<CatalogHit> {
    let mut seen: BTreeSet<i64> = candidates.iter().map(|hit| hit.file_id).collect();
    hits.iter()
        .filter(|hit| is_video_path(&hit.path) && seen.insert(hit.file_id))
        .cloned()
        .collect()
}

/// The Moments "Best takes" button needs two different files to compare.
pub fn can_open_from_search(hits: &[CatalogHit]) -> bool {
    hits.iter()
        .map(|hit| hit.file_id)
        .collect::<BTreeSet<_>>()
        .len()
        >= 2
}

pub fn search_tooltip(hits: &[CatalogHit]) -> &'static str {
    if hits.len() < 2 {
        "Find at least two clips first"
    } else {
        "Compare these clips as tries of the same shot and pick the best one"
    }
}

pub fn can_save(title: &str, goal: &str, chosen: usize) -> bool {
    !title.trim().is_empty() && !goal.trim().is_empty() && chosen >= 2
}

pub fn save_tooltip(can_save: bool) -> &'static str {
    if can_save {
        "Save (Ctrl+S)"
    } else {
        "Needs a name, a goal and at least two clips"
    }
}

/// The event to save: the open group's id, or a new one.
pub fn event_to_save(
    existing: Option<&CatalogEvent>,
    new_id: String,
    title: &str,
    goal: &str,
    chosen: &BTreeSet<i64>,
) -> CatalogEvent {
    CatalogEvent {
        id: existing.map_or(new_id, |event| event.id.clone()),
        title: title.to_owned(),
        goal: goal.to_owned(),
        file_ids: chosen.iter().copied().collect(),
        user_edited: false,
    }
}

pub fn clips_header(chosen: usize, pool: usize) -> String {
    format!("CLIPS · {chosen} of {pool} chosen")
}

pub fn all_chosen(chosen: &BTreeSet<i64>, pool: &[CatalogHit]) -> bool {
    !pool.is_empty() && pool.iter().all(|hit| chosen.contains(&hit.file_id))
}

pub fn choose_label(chosen: &BTreeSet<i64>, pool: &[CatalogHit]) -> &'static str {
    if all_chosen(chosen, pool) {
        "Choose none"
    } else {
        "Choose all"
    }
}

pub fn toggle_all(chosen: &BTreeSet<i64>, pool: &[CatalogHit]) -> BTreeSet<i64> {
    if all_chosen(chosen, pool) {
        BTreeSet::new()
    } else {
        pool.iter().map(|hit| hit.file_id).collect()
    }
}

pub fn editor_note(pool_empty: bool) -> &'static str {
    if pool_empty {
        "Search on the left to add clips."
    } else {
        "Scores are suggestions from a few frames. Your videos are never changed."
    }
}

pub fn job_active(job: Option<&CatalogJob>) -> bool {
    job.is_some_and(|job| matches!(job.state.as_str(), "queued" | "running" | "paused"))
}

/// Score takes runs only on a saved, unchanged group of videos with no
/// scoring job already going.
pub fn can_score(
    event: &CatalogEvent,
    goal_text: &str,
    chosen: &BTreeSet<i64>,
    takes: &[CatalogTake],
    busy: bool,
    job: Option<&CatalogJob>,
) -> bool {
    !busy
        && goal_text.trim() == event.goal
        && chosen.iter().copied().eq(event.file_ids.iter().copied())
        && takes.len() == event.file_ids.len()
        && takes.iter().all(|take| is_video_path(&take.path))
        && !job_active(job)
}

/// The timeline job a Score takes request just started: a new sampling job
/// over exactly the group's files.
pub fn new_goal_job<'a>(
    jobs: &'a [CatalogJob],
    requested: &BTreeSet<i64>,
    previous: &BTreeSet<String>,
) -> Option<&'a CatalogJob> {
    jobs.iter().find(|job| {
        job.kind == "timelineSample"
            && job.file_ids.iter().copied().collect::<BTreeSet<_>>() == *requested
            && !previous.contains(&job.id)
    })
}

pub fn event_meta(event: &CatalogEvent) -> String {
    format!("{} clips · {}", event.file_ids.len(), event.goal)
}

pub fn event_spoken(event: &CatalogEvent) -> String {
    format!(
        "{}, {} clips, goal: {}",
        event.title,
        event.file_ids.len(),
        event.goal
    )
}

pub fn suggestion_names(suggestion: &CatalogTakeGroupSuggestion) -> Vec<String> {
    suggestion
        .members
        .iter()
        .map(|member| crate::v3::file_name(&member.path))
        .collect()
}

pub fn suggestion_meta(suggestion: &CatalogTakeGroupSuggestion) -> String {
    format!(
        "{} clips · {}% alike",
        suggestion.members.len(),
        (suggestion.similarity * 100.0).round() as i64
    )
}

pub fn suggestion_hits(suggestion: &CatalogTakeGroupSuggestion) -> Vec<CatalogHit> {
    suggestion
        .members
        .iter()
        .map(|member| CatalogHit {
            file_id: member.file_id,
            path: member.path.clone(),
            kind: "video".into(),
            text: String::new(),
            evidence_id: None,
            start_seconds: None,
            page: None,
        })
        .collect()
}

pub fn recommendation_tone(recommendation: &CatalogTakeRecommendation) -> Tone {
    match recommendation.status.to_lowercase().as_str() {
        "ranked" | "preferred" => Tone::Gold,
        "tie" => Tone::Ai,
        _ => Tone::Muted,
    }
}

pub fn recommendation_header(recommendation: &CatalogTakeRecommendation) -> String {
    let mut status = recommendation.status.to_lowercase();
    if let Some(first) = status.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    format!("RECOMMENDATION · {status}")
}

pub fn is_recommended(
    recommendation: Option<&CatalogTakeRecommendation>,
    take: &CatalogTake,
) -> bool {
    recommendation.is_some_and(|r| r.file_ids.contains(&take.file_id))
}

fn reviewed_by_user(take: &CatalogTake) -> bool {
    take.model_version.as_deref() == Some("user")
}

fn is_draft(take: &CatalogTake) -> bool {
    take.model_version
        .as_deref()
        .is_some_and(|version| version.starts_with("take-outcome-sequence-v1/"))
}

/// The card's headline and its tone.
pub fn headline(take: &CatalogTake) -> (String, Tone) {
    if take.preferred {
        return ("Preferred by you".into(), Tone::Gold);
    }
    if take.stale {
        return ("Needs review: the file changed".into(), Tone::Warning);
    }
    let Some(score) = take.outcome_score else {
        if reviewed_by_user(take) {
            return ("Unclear".into(), Tone::Text2);
        }
        let text = if is_draft(take) {
            "Not enough evidence"
        } else {
            "Not reviewed"
        };
        return (text.into(), Tone::Muted);
    };
    let prefix = if is_draft(take) { "Draft · " } else { "" };
    if score >= 0.5 {
        (format!("{prefix}Goal met"), Tone::Done)
    } else {
        (format!("{prefix}Goal missed"), Tone::Danger)
    }
}

/// The 7 px dot: tone, tooltip, spoken label.
pub fn review_dot(take: &CatalogTake) -> (Tone, &'static str, &'static str) {
    if reviewed_by_user(take) {
        (Tone::Gold, "Reviewed by you", "Reviewed by you")
    } else if is_draft(take) {
        (
            Tone::Ai,
            "Suggested by FileID from a few frames",
            "Model draft",
        )
    } else {
        (Tone::Muted, "No score yet", "No score")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Met,
    Missed,
    Unclear,
}

impl Outcome {
    pub const ALL: [Outcome; 3] = [Outcome::Met, Outcome::Missed, Outcome::Unclear];

    pub fn label(self) -> &'static str {
        match self {
            Outcome::Met => "Met",
            Outcome::Missed => "Missed",
            Outcome::Unclear => "Unclear",
        }
    }

    pub fn spoken(self) -> String {
        format!("Goal {}", self.label().to_lowercase())
    }

    fn score(self) -> Option<f64> {
        match self {
            Outcome::Met => Some(1.0),
            Outcome::Missed => Some(0.0),
            Outcome::Unclear => None,
        }
    }
}

/// Which outcome button is on: only the user's own review lights one.
pub fn selected_outcome(take: &CatalogTake) -> Option<Outcome> {
    if !reviewed_by_user(take) {
        return None;
    }
    Some(match take.outcome_score {
        None => Outcome::Unclear,
        Some(score) if score >= 0.5 => Outcome::Met,
        Some(_) => Outcome::Missed,
    })
}

pub fn outcome_feedback(
    event_id: &str,
    take: &CatalogTake,
    outcome: Outcome,
) -> CatalogTakeFeedback {
    CatalogTakeFeedback {
        event_id: event_id.to_owned(),
        file_id: take.file_id,
        outcome_score: outcome.score(),
        preferred: take.preferred,
    }
}

pub fn prefer_feedback(event_id: &str, take: &CatalogTake) -> CatalogTakeFeedback {
    CatalogTakeFeedback {
        event_id: event_id.to_owned(),
        file_id: take.file_id,
        outcome_score: take.outcome_score,
        preferred: !take.preferred,
    }
}

pub fn card_spoken(index: usize, take: &CatalogTake, recommended: bool) -> String {
    format!(
        "Take {index}, {}{}",
        crate::v3::file_name(&take.path),
        if recommended { ", recommended" } else { "" }
    )
}

pub fn percent(value: f64) -> i64 {
    (value.clamp(0.0, 1.0) * 100.0).round() as i64
}

/// Actions whose reply carries the one open group rather than the list.
pub fn replies_with_open_group(action: &str) -> bool {
    matches!(
        action,
        "takeGroup" | "saveEvent" | "setTakeFeedback" | "undoTakeFeedback"
    )
}

/// Actions after which the YOUR GROUPS list is read again.
pub fn relists(action: &str) -> bool {
    matches!(action, "saveEvent" | "deleteEvent" | "undoEventEdit")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::CatalogTakeGroupMember;

    fn hit(id: i64, path: &str) -> CatalogHit {
        CatalogHit {
            file_id: id,
            path: path.into(),
            kind: "passage".into(),
            text: String::new(),
            evidence_id: None,
            start_seconds: None,
            page: None,
        }
    }

    fn take(id: i64, score: Option<f64>, version: Option<&str>) -> CatalogTake {
        CatalogTake {
            event_id: "game".into(),
            file_id: id,
            path: format!("/clips/try{id}.mp4"),
            outcome_score: score,
            quality_score: Some(0.42),
            confidence: None,
            explanation: None,
            source_revision: None,
            model_version: version.map(str::to_owned),
            preferred: false,
            stale: false,
        }
    }

    fn event(goal: &str, ids: &[i64]) -> CatalogEvent {
        CatalogEvent {
            id: "game".into(),
            title: "Baseball".into(),
            goal: goal.into(),
            file_ids: ids.to_vec(),
            user_edited: true,
        }
    }

    fn job(id: &str, state: &str, ids: &[i64]) -> CatalogJob {
        CatalogJob {
            id: id.into(),
            kind: "timelineSample".into(),
            file_ids: ids.to_vec(),
            state: state.into(),
            progress: 0.0,
            error: None,
            created_at: 0.0,
            updated_at: 0.0,
        }
    }

    #[test]
    fn the_pool_keeps_candidates_first_and_searches_add_only_new_videos() {
        let candidates = vec![hit(1, "/a.mp4"), hit(1, "/a.mp4"), hit(2, "/b.jpg")];
        let found = found_videos(
            &[
                hit(2, "/b.mp4"),
                hit(3, "/c.mov"),
                hit(4, "/d.pdf"),
                hit(3, "/c.mov"),
            ],
            &candidates,
        );
        assert_eq!(found.iter().map(|h| h.file_id).collect::<Vec<_>>(), vec![3]);
        let pool = pool(&candidates, &found);
        assert_eq!(
            pool.iter().map(|h| h.file_id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(can_open_from_search(&candidates));
        assert!(!can_open_from_search(&candidates[..2]));
        assert_eq!(
            search_tooltip(&candidates[..1]),
            "Find at least two clips first"
        );
    }

    #[test]
    fn saving_needs_a_name_a_goal_and_two_clips_and_keeps_the_open_id() {
        assert!(can_save("Baseball", "Alex gets a hit", 2));
        assert!(!can_save("  ", "Alex gets a hit", 2));
        assert!(!can_save("Baseball", "", 3));
        assert!(!can_save("Baseball", "goal", 1));
        assert_eq!(save_tooltip(true), "Save (Ctrl+S)");
        let chosen: BTreeSet<i64> = [5, 2].into_iter().collect();
        let fresh = event_to_save(None, "new-id".into(), "T", "G", &chosen);
        assert_eq!(
            (fresh.id.as_str(), fresh.file_ids.clone()),
            ("new-id", vec![2, 5])
        );
        let open = event("G", &[2]);
        assert_eq!(
            event_to_save(Some(&open), "x".into(), "T", "G", &chosen).id,
            "game"
        );
    }

    #[test]
    fn choose_all_flips_between_everything_and_nothing() {
        let pool = vec![hit(1, "/a.mp4"), hit(2, "/b.mp4")];
        let some: BTreeSet<i64> = [1].into_iter().collect();
        assert_eq!(choose_label(&some, &pool), "Choose all");
        let all = toggle_all(&some, &pool);
        assert_eq!(all.len(), 2);
        assert_eq!(choose_label(&all, &pool), "Choose none");
        assert!(toggle_all(&all, &pool).is_empty());
        assert_eq!(clips_header(1, 2), "CLIPS · 1 of 2 chosen");
        assert_eq!(choose_label(&BTreeSet::new(), &[]), "Choose all");
    }

    #[test]
    fn scoring_waits_for_a_saved_unchanged_group_of_videos() {
        let group = event("Alex gets a hit", &[1, 2]);
        let chosen: BTreeSet<i64> = [1, 2].into_iter().collect();
        let takes = vec![take(1, None, None), take(2, None, None)];
        assert!(can_score(
            &group,
            "Alex gets a hit ",
            &chosen,
            &takes,
            false,
            None
        ));
        assert!(!can_score(
            &group,
            "Someone else",
            &chosen,
            &takes,
            false,
            None
        ));
        assert!(!can_score(
            &group,
            "Alex gets a hit",
            &[1].into_iter().collect(),
            &takes,
            false,
            None
        ));
        assert!(!can_score(
            &group,
            "Alex gets a hit",
            &chosen,
            &takes[..1],
            false,
            None
        ));
        assert!(!can_score(
            &group,
            "Alex gets a hit",
            &chosen,
            &takes,
            true,
            None
        ));
        let running = job("j", "running", &[1, 2]);
        assert!(!can_score(
            &group,
            "Alex gets a hit",
            &chosen,
            &takes,
            false,
            Some(&running)
        ));
        let done = job("j", "completed", &[1, 2]);
        assert!(can_score(
            &group,
            "Alex gets a hit",
            &chosen,
            &takes,
            false,
            Some(&done)
        ));
        let mut photo = takes.clone();
        photo[0].path = "/clips/still.jpg".into();
        assert!(!can_score(
            &group,
            "Alex gets a hit",
            &chosen,
            &photo,
            false,
            None
        ));
    }

    #[test]
    fn the_goal_job_is_the_new_sampling_job_over_exactly_the_group() {
        let jobs = vec![
            job("old", "running", &[1, 2]),
            job("other", "queued", &[1, 2, 3]),
            job("new", "queued", &[2, 1]),
        ];
        let previous: BTreeSet<String> = ["old".to_owned()].into_iter().collect();
        let requested: BTreeSet<i64> = [1, 2].into_iter().collect();
        assert_eq!(
            new_goal_job(&jobs, &requested, &previous).map(|j| j.id.as_str()),
            Some("new")
        );
        assert!(new_goal_job(&jobs[..2], &requested, &previous).is_none());
    }

    #[test]
    fn card_headlines_follow_preference_staleness_and_who_scored() {
        let mut t = take(1, Some(0.8), Some("take-outcome-sequence-v1/abc"));
        assert_eq!(headline(&t), ("Draft · Goal met".into(), Tone::Done));
        assert_eq!(review_dot(&t).2, "Model draft");
        t.model_version = Some("user".into());
        t.outcome_score = Some(0.0);
        assert_eq!(headline(&t), ("Goal missed".into(), Tone::Danger));
        assert_eq!(selected_outcome(&t), Some(Outcome::Missed));
        t.outcome_score = None;
        assert_eq!(headline(&t).0, "Unclear");
        assert_eq!(selected_outcome(&t), Some(Outcome::Unclear));
        t.stale = true;
        assert_eq!(headline(&t).1, Tone::Warning);
        t.preferred = true;
        assert_eq!(headline(&t), ("Preferred by you".into(), Tone::Gold));
        let unscored = take(2, None, None);
        assert_eq!(headline(&unscored).0, "Not reviewed");
        assert_eq!(selected_outcome(&unscored), None);
        assert_eq!(review_dot(&unscored).1, "No score yet");
    }

    #[test]
    fn card_buttons_send_the_matching_feedback() {
        let mut t = take(3, Some(0.7), Some("user"));
        let met = outcome_feedback("game", &t, Outcome::Met);
        assert_eq!(
            (met.outcome_score, met.preferred, met.file_id),
            (Some(1.0), false, 3)
        );
        assert_eq!(
            outcome_feedback("game", &t, Outcome::Unclear).outcome_score,
            None
        );
        let prefer = prefer_feedback("game", &t);
        assert_eq!((prefer.outcome_score, prefer.preferred), (Some(0.7), true));
        t.preferred = true;
        assert!(!prefer_feedback("game", &t).preferred);
        assert_eq!(Outcome::Met.spoken(), "Goal met");
        assert_eq!(card_spoken(2, &t, true), "Take 2, try3.mp4, recommended");
        assert_eq!(percent(0.426), 43);
    }

    #[test]
    fn rows_and_recommendations_read_like_the_mac() {
        let group = event("Alex gets a hit", &[1, 2, 3]);
        assert_eq!(event_meta(&group), "3 clips · Alex gets a hit");
        assert_eq!(
            event_spoken(&group),
            "Baseball, 3 clips, goal: Alex gets a hit"
        );
        let suggestion = CatalogTakeGroupSuggestion {
            members: vec![
                CatalogTakeGroupMember {
                    file_id: 1,
                    path: "/c/a.mp4".into(),
                },
                CatalogTakeGroupMember {
                    file_id: 2,
                    path: "/c/b.mp4".into(),
                },
            ],
            similarity: 0.874,
            reason: "Filmed close together".into(),
        };
        assert_eq!(suggestion_meta(&suggestion), "2 clips · 87% alike");
        assert_eq!(suggestion_names(&suggestion), vec!["a.mp4", "b.mp4"]);
        assert_eq!(suggestion_hits(&suggestion)[1].file_id, 2);
        let recommendation = CatalogTakeRecommendation {
            event_id: "game".into(),
            status: "tie".into(),
            file_ids: vec![2],
            reason: "Two takes meet the goal".into(),
        };
        assert_eq!(recommendation_tone(&recommendation), Tone::Ai);
        assert_eq!(
            recommendation_header(&recommendation),
            "RECOMMENDATION · Tie"
        );
        assert!(is_recommended(Some(&recommendation), &take(2, None, None)));
        assert!(!is_recommended(None, &take(2, None, None)));
        assert!(
            replies_with_open_group("setTakeFeedback") && !replies_with_open_group("listEvents")
        );
        assert!(relists("deleteEvent") && !relists("takeGroup"));
    }
}
