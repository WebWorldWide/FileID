// What a Chat action card says and whether it may run: the client-side copy of
// the engine's action rules (macOS `ChatActions`), plan summaries and the
// result lines. No widgets here.

use std::path::Path;

use fileid_engine::ipc::{
    BulkActionResult, ChatAction, DeepAnalyzeComplete, RestructureApplyResult, RestructureMove,
    RestructurePlan, ScanPhase, ScanProgress, CHAT_ACTION_KINDS, CHAT_JOBS, CHAT_PAGES,
    CHAT_SETTINGS,
};

pub(super) const CONFIRMING: [&str; 6] = [
    "moveDuplicatesToTrash",
    "applyFolderLayout",
    "undoLastMove",
    "renameToSuggested",
    "namePerson",
    "mergePeople",
];

pub(super) const SETTING_UNAVAILABLE: &str =
    "This setting isn't on Linux yet. Nothing will change.";

pub(super) fn needs_confirmation(kind: &str) -> bool {
    CONFIRMING.contains(&kind)
}

fn present(ids: &Option<Vec<i64>>) -> bool {
    ids.as_ref().is_some_and(|ids| !ids.is_empty())
}

/// A reply whose confirmation flag disagrees with the fixed table, whose kind
/// is unknown or whose fields are missing never gets a card.
pub(super) fn runnable(action: &ChatAction) -> bool {
    let listed = |value: &Option<String>, allowed: &[&str]| {
        value.as_deref().is_none_or(|value| allowed.contains(&value))
    };
    let basics = CHAT_ACTION_KINDS.contains(&action.kind.as_str())
        && action.requires_confirmation == needs_confirmation(&action.kind)
        && !action.id.is_empty()
        && !action.title.trim().is_empty()
        && listed(&action.page, &CHAT_PAGES)
        && listed(&action.job, &CHAT_JOBS)
        && listed(&action.setting, &CHAT_SETTINGS);
    if !basics {
        return false;
    }
    match action.kind.as_str() {
        "scanFolder" | "rescan" | "suggestFolderLayout" | "applyFolderLayout" | "undoLastMove" => {
            action.path.as_deref().is_some_and(|path| !path.is_empty())
        }
        "moveDuplicatesToTrash" => match (&action.file_ids, &action.keeper_file_ids) {
            (Some(files), Some(keepers)) => {
                !files.is_empty()
                    && files.len() == keepers.len()
                    && files.iter().all(|file| !keepers.contains(file))
            }
            _ => false,
        },
        "renameToSuggested" => action.renames.as_ref().is_some_and(|renames| {
            !renames.is_empty()
                && renames.iter().all(|rename| {
                    let name = rename.new_name.as_str();
                    !name.is_empty() && !name.contains(['/', '\0']) && name != "." && name != ".."
                })
        }),
        "namePerson" => {
            action.person_id.is_some()
                && action.name.as_deref().is_some_and(|name| !name.trim().is_empty())
        }
        "mergePeople" => match (action.person_id, &action.person_ids) {
            (Some(target), Some(sources)) => !sources.is_empty() && !sources.contains(&target),
            _ => false,
        },
        "openPage" => action.page.is_some(),
        "search" => action.query.as_deref().is_some_and(|query| !query.trim().is_empty()),
        "changeSetting" => action.setting.is_some() && action.value.is_some(),
        "stopJob" => action.job.is_some(),
        "exportFiles" => present(&action.file_ids),
        _ => true,
    }
}

/// Chat's page names → the GTK shell's page ids.
pub(super) fn page_id(page: &str) -> &'static str {
    match page {
        "people" => "people",
        "cleanup" => "cleanup",
        "deepAnalyze" => "deep-analyze",
        "restructure" => "restructure",
        "settings" => "settings",
        "search" => "moments",
        "tools" => "tools",
        "chat" => "chat",
        _ => "library",
    }
}

/// The GTK shell's page ids → the page names `ChatContext` carries.
pub(super) fn context_page(shell_page: &str) -> &'static str {
    match shell_page {
        "people" => "people",
        "cleanup" => "cleanup",
        "deep-analyze" => "deepAnalyze",
        "restructure" => "restructure",
        "settings" => "settings",
        "moments" | "takes" => "search",
        "tools" => "tools",
        "chat" => "chat",
        _ => "library",
    }
}

pub(super) fn page_title(page: &str) -> &'static str {
    match page {
        "people" => "People",
        "cleanup" => "Duplicates",
        "deepAnalyze" => "Describe Files",
        "restructure" => "Organize Folders",
        "settings" => "Settings",
        "search" => "Search & Moments",
        "tools" => "File Tools",
        "chat" => "Chat",
        _ => "Library",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Accent {
    Danger,
    Gold,
    Info,
}

impl Accent {
    pub(super) fn css(self) -> &'static str {
        match self {
            Accent::Danger => "tone-danger",
            Accent::Gold => "tone-gold",
            Accent::Info => "tone-info",
        }
    }
}

pub(super) fn accent(action: &ChatAction) -> Accent {
    if action.kind == "moveDuplicatesToTrash" {
        Accent::Danger
    } else if action.requires_confirmation {
        Accent::Gold
    } else {
        Accent::Info
    }
}

pub(super) fn section(action: &ChatAction) -> &'static str {
    if action.requires_confirmation {
        "Needs your OK"
    } else {
        "Suggested"
    }
}

pub(super) fn can_stop(kind: &str) -> bool {
    matches!(
        kind,
        "scanFolder" | "rescan" | "describeFiles" | "applyFolderLayout" | "undoLastMove" | "suggestFolderLayout"
    )
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        crate::shell::grouped(count),
        if count == 1 { one } else { many }
    )
}

fn files(count: u64) -> String {
    plural(count, "file", "files")
}

#[derive(Clone, Debug)]
pub(super) struct PlanSummary {
    pub root: String,
    pub count: u64,
    pub examples: Vec<String>,
    pub plan_id: Option<String>,
    pub moves: Vec<RestructureMove>,
}

impl PlanSummary {
    pub(super) fn same(&self, other: &PlanSummary) -> bool {
        (&self.root, self.count, &self.examples, &self.plan_id)
            == (&other.root, other.count, &other.examples, &other.plan_id)
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into_owned())
}

/// The current layout for the action's folder, narrowed to its Auto tier so
/// Chat never moves files the plan marked for review.
pub(super) fn plan_summary(plan: &RestructurePlan, path: Option<&str>) -> Option<PlanSummary> {
    if path.is_some_and(|path| path != plan.library_root) {
        return None;
    }
    let auto: Vec<&RestructureMove> = plan.moves.iter().filter(|m| m.confidence == "auto").collect();
    let count = match &plan.plan_id {
        Some(_) => plan
            .confidence_counts
            .as_ref()
            .map(|counts| counts.auto)
            .or(plan.total_moves)
            .unwrap_or(plan.moves.len() as u64),
        None => auto.len() as u64,
    };
    if count == 0 {
        return None;
    }
    let root = plan.library_root.trim_end_matches('/');
    let examples = auto
        .iter()
        .take(3)
        .map(|m| {
            let folder = Path::new(&m.destination)
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default();
            let relative = folder
                .strip_prefix(root)
                .map_or(folder.as_str(), |rest| rest.trim_matches('/'));
            let relative = if relative.is_empty() { "top folder" } else { relative };
            format!("{} → {relative}", file_name(&m.source))
        })
        .collect();
    Some(PlanSummary {
        root: plan.library_root.clone(),
        count,
        examples,
        plan_id: plan.plan_id.clone(),
        moves: if plan.plan_id.is_some() {
            Vec::new()
        } else {
            auto.into_iter().cloned().collect()
        },
    })
}

pub(super) fn card_detail(action: &ChatAction, plan: Option<&PlanSummary>) -> String {
    if action.kind == "applyFolderLayout" {
        return match plan {
            Some(plan) => format!(
                "Moves {} in {} into the suggested folders. Files the plan marked for review stay where they are.",
                files(plan.count),
                file_name(&plan.root)
            ),
            None => "There's no suggested layout for this folder yet. Ask Chat to organize it first; nothing moves until you apply it.".to_owned(),
        };
    }
    action.detail.clone()
}

pub(super) fn primary_title(action: &ChatAction, plan: Option<&PlanSummary>) -> String {
    match plan {
        Some(plan) if action.kind == "applyFolderLayout" => format!("Move {}", files(plan.count)),
        _ => action.title.clone(),
    }
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Facts {
    pub files: Option<String>,
    pub size: Option<String>,
    pub examples: Vec<String>,
    pub more: Option<String>,
    pub undo: Option<String>,
}

pub(super) fn example_label(example: &str) -> String {
    if example.starts_with('/') {
        file_name(example)
    } else {
        example.to_owned()
    }
}

/// The well under a confirming card: counts, up to three examples, the rest
/// as "and N more", and the undo story.
pub(super) fn facts(action: &ChatAction, plan: Option<&PlanSummary>) -> Facts {
    let count = plan
        .map(|plan| plan.count as i64)
        .or(action.file_count)
        .map(|count| count.max(0));
    let examples: Vec<String> = plan
        .map(|plan| plan.examples.clone())
        .or_else(|| action.examples.clone())
        .unwrap_or_default()
        .iter()
        .take(3)
        .map(|example| example_label(example))
        .collect();
    let shown = examples.len() as i64;
    Facts {
        files: count.map(|count| crate::shell::grouped(count as u64)),
        size: action
            .byte_count
            .map(fileid_engine::util::display::file_size_i64),
        more: count
            .filter(|count| !examples.is_empty() && *count > shown)
            .map(|count| format!("and {} more", crate::shell::grouped((count - shown) as u64))),
        examples,
        undo: action.undo.as_ref().map(|undo| format!("Undo: {undo}")),
    }
}

/// Whether the card's button stays disabled, and why.
pub(super) fn unavailable(action: &ChatAction, plan: Option<&PlanSummary>, describe_runtime: bool) -> Option<&'static str> {
    match action.kind.as_str() {
        "applyFolderLayout" if plan.is_none() => {
            Some("Ask Chat to organize this folder first.")
        }
        "describeFiles" if !describe_runtime => Some(
            "Describing needs the llama.cpp runtime, which isn't in this installation.",
        ),
        "changeSetting" => Some(SETTING_UNAVAILABLE),
        _ => None,
    }
}

/// "Ada Lovelace King" → first "Ada", last "Lovelace King".
pub(super) fn split_name(name: &str) -> (Option<String>, Option<String>) {
    let mut words = name.split_whitespace();
    let first = words.next().map(str::to_owned);
    let rest = words.collect::<Vec<_>>().join(" ");
    (first, (!rest.is_empty()).then_some(rest))
}

pub(super) fn trash_batch(result: &BulkActionResult) -> Option<String> {
    result
        .action
        .strip_prefix("trashFiles:")
        .filter(|batch| !batch.is_empty())
        .map(str::to_owned)
}

pub(super) fn is_trash_result(result: &BulkActionResult) -> bool {
    result.action == "trashFiles" || result.action.starts_with("trashFiles:")
}

pub(super) fn copies(count: u64) -> String {
    plural(count, "copy", "copies")
}

pub(super) fn trash_summary(trashed: u64, freed_bytes: i64, changed: u64, failed: u64) -> String {
    let mut text = format!(
        "Moved {} to Trash · freed {}",
        copies(trashed),
        fileid_engine::util::display::file_size_i64(freed_bytes)
    );
    if changed > 0 {
        text.push_str(&format!(
            " · {} skipped because the file changed since the scan",
            crate::shell::grouped(changed)
        ));
    }
    if failed > 0 {
        text.push_str(&format!(" · {} couldn't be moved", crate::shell::grouped(failed)));
    }
    text.push('.');
    text
}

pub(super) fn rename_summary(renamed: u64, failed: u64, skipped: u64) -> String {
    let mut text = format!("Renamed {}", files(renamed));
    if failed > 0 {
        text.push_str(&format!(" · {} failed", crate::shell::grouped(failed)));
    }
    if skipped > 0 {
        text.push_str(&format!(
            " · {} skipped because their suggestion changed",
            crate::shell::grouped(skipped)
        ));
    }
    text.push('.');
    text
}

/// A suggestion still matches the card when its proposed name plus the
/// file's extension is the name the card showed.
pub(super) fn suggestion_matches(proposed: Option<&str>, path: &str, wanted: &str) -> bool {
    let Some(proposed) = proposed.map(str::trim).filter(|p| !p.is_empty()) else {
        return false;
    };
    let name = match Path::new(path).extension() {
        Some(ext) if !ext.is_empty() => format!("{proposed}.{}", ext.to_string_lossy()),
        _ => proposed.to_owned(),
    };
    name == wanted
}

pub(super) fn apply_summary(result: &RestructureApplyResult) -> String {
    if let Some(message) = result.privilege_error.as_deref().filter(|m| !m.is_empty()) {
        return message.to_owned();
    }
    if result.cancelled {
        return format!("Stopped after moving {}.", files(result.applied.into()));
    }
    let mut text = format!("Moved {}", files(result.applied.into()));
    if result.failed > 0 {
        text.push_str(&format!(" · {} couldn't move", crate::shell::grouped(result.failed.into())));
    }
    text.push('.');
    text
}

pub(super) fn undo_move_summary(result: &RestructureApplyResult) -> String {
    let mut text = format!("Moved {} back", files(result.applied.into()));
    if result.failed > 0 {
        text.push_str(&format!(" · {} couldn't move back", crate::shell::grouped(result.failed.into())));
    }
    text.push('.');
    text
}

pub(super) fn plan_ready(summary: Option<&PlanSummary>) -> String {
    match summary {
        Some(summary) => format!(
            "Layout ready: {} would move. Nothing has moved yet; review it in Organize Folders or ask Chat to apply it.",
            files(summary.count)
        ),
        None => "Nothing needs to move.".to_owned(),
    }
}

pub(super) fn scan_progress(progress: &ScanProgress) -> Option<String> {
    match progress.phase {
        ScanPhase::Discovering => Some(format!(
            "Finding files · {} so far",
            crate::shell::grouped(progress.discovered)
        )),
        ScanPhase::Tagging => Some(format!(
            "Scanning · {} of {} files",
            crate::shell::grouped(progress.processed),
            crate::shell::grouped(progress.total)
        )),
        ScanPhase::PostScan => Some("Finishing up".to_owned()),
        _ => None,
    }
}

pub(super) fn scan_finished(phase: ScanPhase, progress: Option<&ScanProgress>, processed: Option<u64>) -> String {
    match phase {
        ScanPhase::Cancelled => "Scan stopped. Files found so far stay in the library.".to_owned(),
        ScanPhase::Failed => "The scan didn't finish. Scan again to pick up where it left off.".to_owned(),
        _ => {
            let processed = processed.or(progress.map(|p| p.processed));
            let failed = progress.map_or(0, |p| p.failed);
            let Some(processed) = processed else {
                return "Scan finished.".to_owned();
            };
            let mut text = format!("Scan finished · {}", files(processed));
            if failed > 0 {
                text.push_str(&format!(" · {} couldn't be read", crate::shell::grouped(failed)));
            }
            text.push('.');
            text
        }
    }
}

pub(super) fn describe_finished(done: &DeepAnalyzeComplete) -> String {
    if done.cancelled {
        return format!(
            "Stopped after {}. Their descriptions are kept.",
            files(done.processed)
        );
    }
    let mut text = format!("Described {}", files(done.processed));
    if done.failed > 0 {
        text.push_str(&format!(" · {} couldn't be described", crate::shell::grouped(done.failed)));
    }
    text.push('.');
    text
}

pub(super) fn is_describe_error(kind: &str) -> bool {
    kind.starts_with("deep_analyze") || kind.starts_with("vlm") || kind.contains("llama")
}

pub(super) fn is_folder_error(kind: &str) -> bool {
    kind.contains("restructure") || kind == "db_unavailable"
}

pub(super) fn folder_error_text(kind: &str) -> &'static str {
    if kind == "restructure_busy" {
        "Another folder job is already running. Wait for it to finish, then try again."
    } else {
        "This didn't finish. Check Organize Folders to see what changed, then try again."
    }
}

pub(super) const RESTARTED: &str =
    "FileID's engine restarted before this finished. Check what changed, then try again.";

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::{ChatRename, RestructureConfidenceCounts};

    fn action(kind: &str) -> ChatAction {
        ChatAction {
            id: format!("a-{kind}"),
            kind: kind.into(),
            title: "Do it".into(),
            detail: "What happens".into(),
            requires_confirmation: needs_confirmation(kind),
            source: "parser".into(),
            ..ChatAction::default()
        }
    }

    fn mv(source: &str, destination: &str, confidence: &str) -> RestructureMove {
        RestructureMove {
            file_id: 1,
            source: source.into(),
            destination: destination.into(),
            category: "Photos".into(),
            confidence: confidence.into(),
            ..RestructureMove::default()
        }
    }

    fn plan(moves: Vec<RestructureMove>) -> RestructurePlan {
        RestructurePlan {
            library_root: "/lib".into(),
            plan_id: None,
            total_moves: None,
            truncated: false,
            moves,
            category_counts: vec![],
            confidence_counts: None,
            folder_classifications: None,
        }
    }

    #[test]
    fn confirmation_must_match_the_fixed_table() {
        let mut trash = action("moveDuplicatesToTrash");
        trash.file_ids = Some(vec![2, 3]);
        trash.keeper_file_ids = Some(vec![1, 1]);
        assert!(runnable(&trash));
        trash.requires_confirmation = false;
        assert!(!runnable(&trash), "a destructive kind that skips confirmation never runs");
        let mut open = action("openPage");
        open.page = Some("people".into());
        assert!(runnable(&open));
        open.requires_confirmation = true;
        assert!(!runnable(&open));
        assert!(!runnable(&action("formatDisk")), "unknown kinds are dropped");
        for kind in CHAT_ACTION_KINDS {
            assert_eq!(needs_confirmation(kind), CONFIRMING.contains(&kind));
        }
    }

    #[test]
    fn malformed_fields_are_dropped() {
        let mut trash = action("moveDuplicatesToTrash");
        trash.file_ids = Some(vec![1, 2]);
        trash.keeper_file_ids = Some(vec![1, 3]);
        assert!(!runnable(&trash), "a copy can't be its own keeper");
        trash.keeper_file_ids = Some(vec![3]);
        assert!(!runnable(&trash), "every copy needs a keeper");
        let mut rename = action("renameToSuggested");
        rename.renames = Some(vec![ChatRename { file_id: 1, new_name: "../x".into() }]);
        assert!(!runnable(&rename));
        rename.renames = Some(vec![ChatRename { file_id: 1, new_name: "Beach.jpg".into() }]);
        assert!(runnable(&rename));
        let mut merge = action("mergePeople");
        merge.person_id = Some(4);
        merge.person_ids = Some(vec![4]);
        assert!(!runnable(&merge));
        merge.person_ids = Some(vec![5, 6]);
        assert!(runnable(&merge));
        let mut open = action("openPage");
        open.page = Some("terminal".into());
        assert!(!runnable(&open));
        let mut scan = action("scanFolder");
        assert!(!runnable(&scan));
        scan.path = Some("/home/a/Pictures".into());
        assert!(runnable(&scan));
    }

    #[test]
    fn pages_map_both_ways() {
        for page in CHAT_PAGES {
            assert_eq!(context_page(page_id(page)), page);
        }
        assert_eq!(context_page("takes"), "search");
        assert_eq!(page_title("cleanup"), "Duplicates");
    }

    #[test]
    fn accents_and_labels_follow_the_port_note() {
        assert_eq!(accent(&action("moveDuplicatesToTrash")), Accent::Danger);
        assert_eq!(accent(&action("renameToSuggested")), Accent::Gold);
        assert_eq!(accent(&action("search")), Accent::Info);
        assert_eq!(section(&action("namePerson")), "Needs your OK");
        assert_eq!(section(&action("findMoments")), "Suggested");
    }

    #[test]
    fn facts_list_counts_three_examples_and_the_rest() {
        let mut trash = action("moveDuplicatesToTrash");
        trash.file_count = Some(1_204);
        trash.byte_count = Some(3_000_000);
        trash.examples = Some(vec![
            "/p/a.jpg".into(),
            "/p/b.jpg".into(),
            "c.jpg → Photos".into(),
            "/p/d.jpg".into(),
        ]);
        trash.undo = Some("Put them back from Trash.".into());
        let facts = facts(&trash, None);
        assert_eq!(facts.files.as_deref(), Some("1,204"));
        assert!(facts.size.is_some());
        assert_eq!(facts.examples, ["a.jpg", "b.jpg", "c.jpg → Photos"]);
        assert_eq!(facts.more.as_deref(), Some("and 1,201 more"));
        assert_eq!(facts.undo.as_deref(), Some("Undo: Put them back from Trash."));
        trash.file_count = Some(2);
        trash.examples = Some(vec!["/p/a.jpg".into(), "/p/b.jpg".into()]);
        assert_eq!(super::facts(&trash, None).more, None);
    }

    #[test]
    fn apply_uses_only_the_auto_tier_of_the_matching_plan() {
        let stored = plan(vec![
            mv("/lib/a.jpg", "/lib/Photos/2024/a.jpg", "auto"),
            mv("/lib/b.pdf", "/lib/b.pdf", "auto"),
            mv("/lib/c.jpg", "/lib/Photos/c.jpg", "review"),
        ]);
        let summary = plan_summary(&stored, Some("/lib")).unwrap();
        assert_eq!(summary.count, 2);
        assert_eq!(summary.moves.len(), 2);
        assert_eq!(summary.examples, ["a.jpg → Photos/2024", "b.pdf → top folder"]);
        assert!(plan_summary(&stored, Some("/other")).is_none());
        let mut apply = action("applyFolderLayout");
        apply.path = Some("/lib".into());
        assert_eq!(primary_title(&apply, Some(&summary)), "Move 2 files");
        assert!(card_detail(&apply, None).starts_with("There's no suggested layout"));
        assert_eq!(unavailable(&apply, None, true), Some("Ask Chat to organize this folder first."));
        assert_eq!(unavailable(&apply, Some(&summary), true), None);
        let review_only = plan(vec![mv("/lib/c.jpg", "/lib/Photos/c.jpg", "review")]);
        assert!(plan_summary(&review_only, None).is_none(), "nothing in the Auto tier means nothing to apply");
        let mut paged = plan(vec![mv("/lib/a.jpg", "/lib/Photos/a.jpg", "auto")]);
        paged.plan_id = Some("p1".into());
        paged.confidence_counts = Some(RestructureConfidenceCounts { auto: 40, review: 2, ask: 0, unknown: 0 });
        let summary = plan_summary(&paged, None).unwrap();
        assert_eq!((summary.count, summary.moves.len(), summary.plan_id.as_deref()), (40, 0, Some("p1")));
    }

    #[test]
    fn names_split_first_then_the_rest() {
        assert_eq!(split_name("Ada"), (Some("Ada".into()), None));
        assert_eq!(split_name("  Ada  Lovelace King "), (Some("Ada".into()), Some("Lovelace King".into())));
        assert_eq!(split_name(" "), (None, None));
    }

    #[test]
    fn result_lines_read_like_macos() {
        let freed = fileid_engine::util::display::file_size_i64(0);
        assert_eq!(trash_summary(1, 0, 0, 0), format!("Moved 1 copy to Trash · freed {freed}."));
        let line = trash_summary(3, 1_000, 1, 2);
        assert!(line.starts_with("Moved 3 copies to Trash · freed "));
        assert!(line.ends_with(" · 1 skipped because the file changed since the scan · 2 couldn't be moved."));
        assert_eq!(rename_summary(4, 1, 2), "Renamed 4 files · 1 failed · 2 skipped because their suggestion changed.");
        assert_eq!(rename_summary(1, 0, 0), "Renamed 1 file.");
        let result = BulkActionResult { action: "trashFiles:batch-9".into(), succeeded: 1, failed: 0, messages: vec![] };
        assert_eq!(trash_batch(&result).as_deref(), Some("batch-9"));
        assert!(is_trash_result(&result));
        let plain = BulkActionResult { action: "trashFiles".into(), ..result };
        assert_eq!(trash_batch(&plain), None);
        let applied = RestructureApplyResult { applied: 5, failed: 1, privilege_error: None, cancelled: false, planned: None, remaining: None, shortcut_undo_token: None };
        assert_eq!(apply_summary(&applied), "Moved 5 files · 1 couldn't move.");
        assert_eq!(undo_move_summary(&applied), "Moved 5 files back · 1 couldn't move back.");
        let done = DeepAnalyzeComplete { processed: 12, failed: 0, total_seconds: 3.0, model_kind: "m".into(), cancelled: false };
        assert_eq!(describe_finished(&done), "Described 12 files.");
        assert_eq!(scan_finished(ScanPhase::Completed, None, Some(1_500)), "Scan finished · 1,500 files.");
        assert_eq!(scan_finished(ScanPhase::Cancelled, None, None), "Scan stopped. Files found so far stay in the library.");
    }

    #[test]
    fn suggestions_must_still_match_the_card() {
        assert!(suggestion_matches(Some(" Beach day "), "/p/IMG_1.JPG", "Beach day.JPG"));
        assert!(suggestion_matches(Some("Notes"), "/p/README", "Notes"));
        assert!(!suggestion_matches(Some("Sunset"), "/p/IMG_1.JPG", "Beach day.JPG"));
        assert!(!suggestion_matches(None, "/p/IMG_1.JPG", "Beach day.JPG"));
    }

    #[test]
    fn unavailable_settings_and_runtime_disable_the_button() {
        let mut setting = action("changeSetting");
        setting.setting = Some("backgroundMotion".into());
        setting.value = Some(false);
        assert!(runnable(&setting));
        assert_eq!(unavailable(&setting, None, true), Some(SETTING_UNAVAILABLE));
        assert!(unavailable(&action("describeFiles"), None, false).is_some());
        assert_eq!(unavailable(&action("describeFiles"), None, true), None);
    }
}
