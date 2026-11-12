// Chat action cards (macOS `ChatActionCard` / `ChatActionRunner`): one card per
// proposed action under the answer. Nothing runs until the card's button is
// clicked; confirming kinds show the counts, examples and undo story first.
// Every kind goes through a command a page already sends.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use fileid_engine::ipc::*;
use gtk::glib;

use super::chat_actions::{self as rules, PlanSummary};
use super::workbench_state::catalog_request;
use super::workbenches::Engine;
use crate::engine_client::{read_db, EngineState};
use crate::v3;

const NOT_READY: &str = "FileID is still getting ready. Try again in a moment.";

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Proposed,
    Running(String),
    Done(String),
    Failed(String),
    Cancelled,
}

impl Phase {
    fn same_shape(&self, other: &Phase) -> bool {
        match (self, other) {
            (Phase::Running(_), Phase::Running(_)) => true,
            _ => self == other,
        }
    }
}

type UndoFn = Rc<dyn Fn(&Rc<Runner>, &str)>;

#[derive(Clone, Default)]
struct PersonName {
    title: Option<String>,
    first: Option<String>,
    middle: Option<String>,
    last: Option<String>,
    suffix: Option<String>,
    name: Option<String>,
    unknown: bool,
}

enum Wait {
    Scan,
    Describe,
    Plan { root: String },
    Move { undo_root: Option<String> },
    Trash { ids: HashSet<i64>, bytes: HashMap<i64, i64>, changed: u64 },
    Restore,
    Rename { ids: HashSet<i64>, old: HashMap<i64, String>, skipped: u64, undoing: bool },
    Person { person: i64, before: Option<PersonName> },
    Unknown,
    Merge { outstanding: usize, failed: bool },
    MomentsJobs { request_id: String },
}

struct Pending {
    action: String,
    wait: Wait,
}

#[derive(Default)]
struct FileFacts {
    path: String,
    size: i64,
    proposed: Option<String>,
}

fn file_facts(conn: &rusqlite::Connection, ids: &[i64]) -> HashMap<i64, FileFacts> {
    let Ok(mut statement) = conn.prepare(
        "SELECT path_text, size_bytes, vlm_proposed_name FROM files WHERE id = ?1 AND failed = 0",
    ) else {
        return HashMap::new();
    };
    ids.iter()
        .filter_map(|id| {
            statement
                .query_row([id], |row| {
                    Ok(FileFacts { path: row.get(0)?, size: row.get(1)?, proposed: row.get(2)? })
                })
                .ok()
                .map(|facts| (*id, facts))
        })
        .collect()
}

fn person_name(conn: &rusqlite::Connection, id: i64) -> Option<PersonName> {
    conn.query_row(
        "SELECT title, first_name, middle_name, last_name, suffix, name, COALESCE(is_unknown, 0) FROM persons WHERE id = ?1",
        [id],
        |row| {
            Ok(PersonName {
                title: row.get(0)?,
                first: row.get(1)?,
                middle: row.get(2)?,
                last: row.get(3)?,
                suffix: row.get(4)?,
                name: row.get(5)?,
                unknown: row.get::<_, i64>(6)? != 0,
            })
        },
    )
    .ok()
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Re-hashes every copy and its keeper; a pair is kept only when both still
/// have the same size and full-file SHA-256.
fn verify_copies(
    victims: &[i64],
    keepers: &[i64],
    facts: &HashMap<i64, FileFacts>,
) -> (Vec<ExactTrashIdentity>, u64) {
    let mut hashes: HashMap<i64, Option<[u8; 32]>> = HashMap::new();
    let mut hash = |id: i64| -> Option<[u8; 32]> {
        *hashes.entry(id).or_insert_with(|| {
            let file = facts.get(&id)?;
            fileid_engine::util::content_hash::exact_file_sha256(
                std::path::Path::new(&file.path),
                u64::try_from(file.size).ok()?,
            )
            .ok()
        })
    };
    let mut valid = Vec::new();
    let mut changed = 0;
    for (victim, keeper) in victims.iter().zip(keepers) {
        let (Some(copy), Some(kept)) = (facts.get(victim), facts.get(keeper)) else {
            changed += 1;
            continue;
        };
        match (hash(*keeper), hash(*victim)) {
            (Some(kept_hash), Some(copy_hash)) if kept_hash == copy_hash && copy.size == kept.size => {
                valid.push(ExactTrashIdentity {
                    file_id: *victim,
                    path: copy.path.clone(),
                    size_bytes: copy.size,
                    sha256_hex: hex(&copy_hash),
                    keeper_path: kept.path.clone(),
                    keeper_size_bytes: kept.size,
                    keeper_sha256_hex: hex(&kept_hash),
                });
            }
            _ => changed += 1,
        }
    }
    (valid, changed)
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into_owned())
}

pub(super) struct Runner {
    engine: Engine,
    phases: RefCell<HashMap<String, Phase>>,
    undos: RefCell<HashMap<String, (String, UndoFn)>>,
    pending: RefCell<Vec<Pending>>,
    plan: RefCell<Option<RestructurePlan>>,
    scan: RefCell<Option<ScanProgress>>,
    overlays: RefCell<HashMap<String, glib::WeakRef<adw::ToastOverlay>>>,
    listeners: RefCell<Vec<Weak<Cards>>>,
}

thread_local! {
    static RUNNER: RefCell<Option<Rc<Runner>>> = const { RefCell::new(None) };
}

impl Runner {
    fn shared(engine: &Engine) -> Rc<Runner> {
        if let Some(runner) = RUNNER.with_borrow(Clone::clone) {
            return runner;
        }
        let runner = Rc::new(Runner {
            engine: engine.clone(),
            phases: RefCell::default(),
            undos: RefCell::default(),
            pending: RefCell::default(),
            plan: RefCell::default(),
            scan: RefCell::default(),
            overlays: RefCell::default(),
            listeners: RefCell::default(),
        });
        let rx = engine.borrow_mut().subscribe();
        let weak = Rc::downgrade(&runner);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = rx.recv().await {
                let Some(runner) = weak.upgrade() else { break };
                runner.on_event(event);
            }
        });
        RUNNER.with_borrow_mut(|slot| *slot = Some(runner.clone()));
        runner
    }

    fn phase(&self, id: &str) -> Phase {
        self.phases.borrow().get(id).cloned().unwrap_or(Phase::Proposed)
    }

    fn running(&self, id: &str) -> bool {
        matches!(self.phase(id), Phase::Running(_))
    }

    fn notify(&self) {
        let listeners: Vec<Rc<Cards>> = {
            let mut listeners = self.listeners.borrow_mut();
            listeners.retain(|cards| cards.strong_count() > 0);
            listeners.iter().filter_map(Weak::upgrade).collect()
        };
        for cards in listeners {
            cards.refresh();
        }
    }

    fn set(&self, id: &str, phase: Phase) {
        self.phases.borrow_mut().insert(id.to_owned(), phase);
        self.notify();
    }

    fn progress(&self, id: &str, text: String) {
        if self.running(id) {
            self.set(id, Phase::Running(text));
        }
    }

    fn toast(self: &Rc<Self>, id: &str, text: &str) {
        let Some(overlay) = self.overlays.borrow().get(id).and_then(glib::WeakRef::upgrade) else {
            return;
        };
        let toast = adw::Toast::builder().title(text).use_markup(false).timeout(10).build();
        if let Some((label, _)) = self.undos.borrow().get(id) {
            toast.set_button_label(Some(label));
            let weak = Rc::downgrade(self);
            let id = id.to_owned();
            toast.connect_button_clicked(move |_| {
                if let Some(runner) = weak.upgrade() {
                    runner.undo(&id);
                }
            });
        }
        overlay.add_toast(toast);
    }

    fn finish(self: &Rc<Self>, id: &str, text: String, undo: Option<(&str, UndoFn)>) {
        match undo {
            Some((label, run)) => {
                self.undos.borrow_mut().insert(id.to_owned(), (label.to_owned(), run));
            }
            None => {
                self.undos.borrow_mut().remove(id);
            }
        }
        self.set(id, Phase::Done(text.clone()));
        self.toast(id, &text);
    }

    fn fail(self: &Rc<Self>, id: &str, text: &str) {
        self.undos.borrow_mut().remove(id);
        self.set(id, Phase::Failed(text.to_owned()));
        self.toast(id, text);
    }

    fn send(&self, payload: CommandPayload) -> bool {
        self.engine.borrow_mut().send(payload).is_ok()
    }

    fn wait(&self, id: &str, wait: Wait) {
        self.pending.borrow_mut().push(Pending { action: id.to_owned(), wait });
    }

    fn take(&self, matches: impl Fn(&Wait) -> bool) -> Option<Pending> {
        let mut pending = self.pending.borrow_mut();
        let index = pending.iter().position(|p| matches(&p.wait))?;
        Some(pending.remove(index))
    }

    fn has_undo(&self, id: &str) -> Option<String> {
        self.undos.borrow().get(id).map(|(label, _)| label.clone())
    }

    fn undo(self: &Rc<Self>, id: &str) {
        if !matches!(self.phase(id), Phase::Done(_)) {
            return;
        }
        let Some((_, run)) = self.undos.borrow_mut().remove(id) else { return };
        self.set(id, Phase::Running("Undoing…".to_owned()));
        run(self, id);
    }

    fn cancel(&self, action: &ChatAction) {
        if self.phase(&action.id) == Phase::Proposed {
            self.set(&action.id, Phase::Cancelled);
        }
    }

    fn stop(&self, action: &ChatAction) {
        let payload = match action.kind.as_str() {
            "scanFolder" | "rescan" => CommandPayload::CancelScan(Empty {}),
            "describeFiles" => CommandPayload::DeepAnalyzeCancel(Empty {}),
            _ => CommandPayload::CancelRestructure(Empty {}),
        };
        if !self.send(payload) {
            self.progress(&action.id, NOT_READY.to_owned());
        }
    }

    fn plan_summary(&self, action: &ChatAction) -> Option<PlanSummary> {
        let plan = self.plan.borrow();
        rules::plan_summary(plan.as_ref()?, action.path.as_deref())
    }

    fn run(self: &Rc<Self>, action: &ChatAction, anchor: &gtk::Button, overlay: &adw::ToastOverlay) {
        if !rules::runnable(action) || self.running(&action.id) || self.phase(&action.id) == Phase::Cancelled {
            return;
        }
        let id = action.id.as_str();
        self.overlays.borrow_mut().insert(id.to_owned(), overlay.downgrade());
        let navigate = |page: &str| {
            let _ = anchor.activate_action("win.navigate", Some(&rules::page_id(page).to_variant()));
        };
        match action.kind.as_str() {
            "scanFolder" | "rescan" => {
                let path = action.path.clone().unwrap_or_default();
                let started = if action.kind == "scanFolder" {
                    anchor.activate_action("win.scan-folder", Some(&path.to_variant())).is_ok()
                } else {
                    self.engine.borrow_mut().start_scan(&path).is_ok()
                };
                if !started {
                    self.fail(id, NOT_READY);
                    return;
                }
                self.wait(id, Wait::Scan);
                self.set(id, Phase::Running("Starting the scan".to_owned()));
            }
            "describeFiles" => {
                let kind = super::deep_analyze::host_recommended_vlm_kind();
                if !crate::model_license::ensure_or_prompt(anchor, kind) {
                    return;
                }
                let file_ids = action.file_ids.as_ref().map(|ids| ids.iter().take(100).copied().collect());
                let sent = self.send(CommandPayload::DeepAnalyzeAll(DeepAnalyzeAllPayload {
                    model_kind: kind.to_owned(),
                    skip_existing: action.file_ids.is_none(),
                    file_ids,
                    tags_only: false,
                    propose_renames: true,
                    excluded_folders: None,
                }));
                if !sent {
                    self.fail(id, "Describing didn't start. Check the AI model in Describe Files.");
                    return;
                }
                self.wait(id, Wait::Describe);
                self.set(id, Phase::Running("Starting".to_owned()));
            }
            "stopJob" => {
                let job = action.job.as_deref().unwrap_or("all");
                let all = job == "all";
                let mut sent = true;
                if all || job == "scan" {
                    sent &= self.send(CommandPayload::CancelScan(Empty {}));
                }
                if all || job == "describe" {
                    sent &= self.send(CommandPayload::DeepAnalyzeCancel(Empty {}));
                }
                if all || job == "organize" {
                    sent &= self.send(CommandPayload::CancelRestructure(Empty {}));
                }
                if all || job == "moments" {
                    let request = catalog_request("jobs");
                    let request_id = request.request_id.clone();
                    if self.send(CommandPayload::CatalogRequest(Box::new(CatalogRequestPayload { request }))) {
                        self.wait(id, Wait::MomentsJobs { request_id });
                    } else {
                        sent = false;
                    }
                }
                if sent {
                    self.finish(id, "Asked FileID to stop. Finished work is kept.".to_owned(), None);
                } else {
                    self.fail(id, NOT_READY);
                }
            }
            "findDuplicates" | "openPage" => {
                let page = action.page.as_deref().unwrap_or(if action.kind == "findDuplicates" { "cleanup" } else { "library" });
                navigate(page);
                self.finish(id, format!("Opened {}.", rules::page_title(page)), None);
            }
            "search" => {
                let query = action.query.clone().unwrap_or_default();
                let _ = anchor.activate_action("win.library-query", Some(&query.to_variant()));
                self.finish(id, "Opened Library with this search.".to_owned(), None);
            }
            "findMoments" => {
                match action.query.as_deref().filter(|query| !query.trim().is_empty()) {
                    Some(query) => {
                        let _ = anchor.activate_action("win.moments-query", Some(&query.to_variant()));
                    }
                    None => navigate("search"),
                }
                self.finish(id, "Opened Search & Moments with this search.".to_owned(), None);
            }
            "exportFiles" => {
                let ids = action.file_ids.clone().unwrap_or_default();
                navigate("tools");
                if super::chat_handoff::hand_to_tools(ids) {
                    self.finish(id, "Opened File Tools with Chat's files ticked. Pick a format there; nothing is written until you export.".to_owned(), None);
                } else {
                    self.fail(id, "File Tools isn't open in this window. Nothing was exported.");
                }
            }
            "changeSetting" if action.setting.as_deref() == Some("backgroundMotion") => {
                let Some(on) = action.value else { return };
                let previous = crate::app_settings::lava_lamp_on();
                let set = |on: bool| {
                    crate::app_settings::remember_lava_lamp(on);
                    crate::lavalamp::set_enabled(on);
                };
                set(on);
                let undo: UndoFn = Rc::new(move |runner, id| {
                    set(previous);
                    runner.finish(id, "Changed back.".to_owned(), None);
                });
                self.finish(id, format!("Turned {}.", if on { "on" } else { "off" }), Some(("Undo", undo)));
            }
            "changeSetting" => self.fail(id, rules::SETTING_UNAVAILABLE),
            "suggestFolderLayout" => {
                let root = action.path.clone().unwrap_or_default();
                if !self.send(CommandPayload::PlanRestructure(PlanRestructurePayload {
                    library_root: root.clone(),
                    supports_paged_plans: false,
                })) {
                    self.fail(id, NOT_READY);
                    return;
                }
                navigate("restructure");
                self.wait(id, Wait::Plan { root });
                self.set(id, Phase::Running("Planning a tidier layout".to_owned()));
            }
            "applyFolderLayout" => {
                let Some(summary) = self.plan_summary(action) else {
                    self.fail(id, "There's no suggested layout for this folder yet. Ask Chat to organize it first.");
                    return;
                };
                if !self.send(CommandPayload::ApplyRestructure(ApplyRestructurePayload {
                    library_root: summary.root.clone(),
                    plan_id: summary.plan_id.clone(),
                    moves: summary.moves.clone(),
                    use_symlinks: false,
                })) {
                    self.fail(id, NOT_READY);
                    return;
                }
                self.wait(id, Wait::Move { undo_root: Some(summary.root) });
                self.set(id, Phase::Running("Moving files".to_owned()));
            }
            "undoLastMove" => {
                let root = action.path.clone().unwrap_or_default();
                if !self.undo_move(id, root) {
                    return;
                }
                self.set(id, Phase::Running("Moving files back".to_owned()));
            }
            "moveDuplicatesToTrash" => self.trash(action),
            "renameToSuggested" => self.rename(action),
            "namePerson" => self.name_person(action),
            "mergePeople" => {
                let (Some(target), Some(sources)) = (action.person_id, action.person_ids.clone()) else {
                    return;
                };
                let mut outstanding = 0;
                for source in sources {
                    if self.send(CommandPayload::MergeClusters(MergeClustersPayload {
                        source_person_id: source,
                        destination_person_id: target,
                    })) {
                        outstanding += 1;
                    }
                }
                if outstanding == 0 {
                    self.fail(id, NOT_READY);
                    return;
                }
                self.wait(id, Wait::Merge { outstanding, failed: false });
                self.set(id, Phase::Running("Merging".to_owned()));
            }
            _ => self.fail(id, "This action isn't available."),
        }
    }

    fn undo_move(self: &Rc<Self>, id: &str, root: String) -> bool {
        if !self.send(CommandPayload::UndoRestructure(UndoRestructurePayload {
            library_root: root,
            shortcut_undo_token: None,
        })) {
            self.fail(id, NOT_READY);
            return false;
        }
        self.wait(id, Wait::Move { undo_root: None });
        true
    }

    fn trash(self: &Rc<Self>, action: &ChatAction) {
        let (Some(victims), Some(keepers)) = (action.file_ids.clone(), action.keeper_file_ids.clone()) else {
            return;
        };
        let id = action.id.clone();
        self.set(&id, Phase::Running("Checking copies".to_owned()));
        let ids: Vec<i64> = victims.iter().chain(&keepers).copied().collect::<HashSet<_>>().into_iter().collect();
        let weak = Rc::downgrade(self);
        let rx = read_db(move |conn| {
            let facts = file_facts(conn, &ids);
            let (valid, changed) = verify_copies(&victims, &keepers, &facts);
            let bytes = valid.iter().map(|identity| (identity.file_id, identity.size_bytes)).collect();
            (valid, changed, bytes)
        });
        glib::MainContext::default().spawn_local(async move {
            let Ok((valid, changed, bytes)) = rx.recv().await else { return };
            let Some(runner) = weak.upgrade() else { return };
            if !runner.running(&id) {
                return;
            }
            if valid.is_empty() {
                runner.fail(&id, "Nothing was moved to Trash. The copies changed since the scan; look again in Duplicates.");
                return;
            }
            let ids: HashSet<i64> = valid.iter().map(|identity| identity.file_id).collect();
            runner.progress(&id, format!("Moving {} to Trash", rules::copies(ids.len() as u64)));
            if !runner.send(CommandPayload::TrashFiles(TrashFilesPayload {
                file_ids: valid.iter().map(|identity| identity.file_id).collect(),
                exact_identities: Some(valid),
            })) {
                runner.fail(&id, NOT_READY);
                return;
            }
            runner.wait(&id, Wait::Trash { ids, bytes, changed });
        });
    }

    fn rename(self: &Rc<Self>, action: &ChatAction) {
        let Some(renames) = action.renames.clone() else { return };
        let id = action.id.clone();
        self.set(&id, Phase::Running(format!("Renaming {} files", crate::shell::grouped(renames.len() as u64))));
        let weak = Rc::downgrade(self);
        let rx = read_db(move |conn| {
            let ids: Vec<i64> = renames.iter().map(|rename| rename.file_id).collect();
            let facts = file_facts(conn, &ids);
            let mut kept = Vec::new();
            let mut old = HashMap::new();
            for rename in &renames {
                let Some(file) = facts.get(&rename.file_id) else { continue };
                if rules::suggestion_matches(file.proposed.as_deref(), &file.path, &rename.new_name)
                    && old.insert(rename.file_id, file_name(&file.path)).is_none()
                {
                    kept.push(RenameEntry { file_id: rename.file_id, new_name: rename.new_name.clone() });
                }
            }
            let skipped = (renames.len() - kept.len()) as u64;
            (kept, old, skipped)
        });
        glib::MainContext::default().spawn_local(async move {
            let Ok((kept, old, skipped)) = rx.recv().await else { return };
            let Some(runner) = weak.upgrade() else { return };
            if !runner.running(&id) {
                return;
            }
            if kept.is_empty() {
                runner.fail(&id, "Nothing was renamed. The suggestions changed; look again in Describe Files.");
                return;
            }
            let ids = kept.iter().map(|entry| entry.file_id).collect();
            if !runner.send(CommandPayload::RenameFiles(RenameFilesPayload { renames: kept })) {
                runner.fail(&id, NOT_READY);
                return;
            }
            runner.wait(&id, Wait::Rename { ids, old, skipped, undoing: false });
        });
    }

    fn name_person(self: &Rc<Self>, action: &ChatAction) {
        let (Some(person), Some(name)) = (action.person_id, action.name.clone()) else {
            return;
        };
        let id = action.id.clone();
        self.set(&id, Phase::Running("Saving the name".to_owned()));
        let weak = Rc::downgrade(self);
        let rx = read_db(move |conn| person_name(conn, person));
        glib::MainContext::default().spawn_local(async move {
            let Ok(before) = rx.recv().await else { return };
            let Some(runner) = weak.upgrade() else { return };
            if !runner.running(&id) {
                return;
            }
            let Some(before) = before else {
                runner.fail(&id, "That person is no longer in People.");
                return;
            };
            let (first_name, last_name) = rules::split_name(&name);
            if !runner.send(CommandPayload::RenamePerson(RenamePersonPayload {
                person_id: person,
                title: None,
                first_name,
                middle_name: None,
                last_name,
                suffix: None,
            })) {
                runner.fail(&id, NOT_READY);
                return;
            }
            runner.wait(&id, Wait::Person { person, before: Some(before) });
            runner.progress(&id, format!("Naming this person {name}"));
        });
    }

    fn on_event(self: &Rc<Self>, event: EngineState) {
        match event {
            EngineState::Progress(progress) => {
                let phase = progress.phase;
                let text = rules::scan_progress(&progress);
                self.scan.replace(Some(progress));
                match phase {
                    ScanPhase::Completed | ScanPhase::Cancelled | ScanPhase::Failed => self.scan_done(phase, None),
                    _ => {
                        if let Some(text) = text {
                            for id in self.waiting(|wait| matches!(wait, Wait::Scan)) {
                                self.progress(&id, text.clone());
                            }
                        }
                    }
                }
            }
            EngineState::ScanPhase(phase @ (ScanPhase::Cancelled | ScanPhase::Failed)) => self.scan_done(phase, None),
            EngineState::ScanComplete(processed) => self.scan_done(ScanPhase::Completed, Some(processed)),
            EngineState::DeepAnalyzeStarting(_) => {
                for id in self.waiting(|wait| matches!(wait, Wait::Describe)) {
                    self.progress(&id, "Loading the local AI model".to_owned());
                }
            }
            EngineState::DeepAnalyzeProgress(progress) => {
                let text = format!(
                    "Describing · {} of {} files",
                    crate::shell::grouped(progress.processed),
                    crate::shell::grouped(progress.total)
                );
                for id in self.waiting(|wait| matches!(wait, Wait::Describe)) {
                    self.progress(&id, text.clone());
                }
            }
            EngineState::DeepAnalyzeComplete(done) => {
                while let Some(pending) = self.take(|wait| matches!(wait, Wait::Describe)) {
                    self.finish(&pending.action, rules::describe_finished(&done), None);
                }
            }
            EngineState::RestructurePlan(plan) => {
                let root = plan.library_root.clone();
                self.plan.replace(Some(plan));
                let summary = self.plan.borrow().as_ref().and_then(|plan| rules::plan_summary(plan, None));
                while let Some(pending) = self.take(|wait| matches!(wait, Wait::Plan { root: wanted } if *wanted == root)) {
                    self.finish(&pending.action, rules::plan_ready(summary.as_ref()), None);
                }
                self.notify();
            }
            EngineState::RestructureApplyResult(result) => {
                let Some(pending) = self.take(|wait| matches!(wait, Wait::Move { .. })) else { return };
                let Wait::Move { undo_root } = pending.wait else { return };
                match undo_root {
                    Some(root) if result.applied > 0 && result.privilege_error.is_none() => {
                        let undo: UndoFn = Rc::new(move |runner, id| {
                            let _ = runner.undo_move(id, root.clone());
                        });
                        self.finish(&pending.action, rules::apply_summary(&result), Some(("Undo moves", undo)));
                    }
                    Some(_) => self.finish(&pending.action, rules::apply_summary(&result), None),
                    None => self.finish(&pending.action, rules::undo_move_summary(&result), None),
                }
            }
            EngineState::BulkActionResult(result) => self.on_bulk(result),
            EngineState::CatalogResponse(response) => {
                let request_id = response.request_id.clone();
                if self.take(|wait| matches!(wait, Wait::MomentsJobs { request_id: id } if *id == request_id)).is_none() {
                    return;
                }
                for job in response.jobs.iter().filter(|job| matches!(job.state.as_str(), "running" | "queued")) {
                    let mut request = catalog_request("cancelJob");
                    request.job_id = Some(job.id.clone());
                    let _ = self.send(CommandPayload::CatalogRequest(Box::new(CatalogRequestPayload { request })));
                }
            }
            EngineState::Error { kind, message, .. } => {
                if rules::is_describe_error(&kind) {
                    while let Some(pending) = self.take(|wait| matches!(wait, Wait::Describe)) {
                        self.fail(&pending.action, &message);
                    }
                }
                if rules::is_folder_error(&kind) {
                    while let Some(pending) = self.take(|wait| matches!(wait, Wait::Plan { .. } | Wait::Move { .. })) {
                        self.fail(&pending.action, rules::folder_error_text(&kind));
                    }
                }
            }
            EngineState::Exited | EngineState::Failed(_) => {
                let pending: Vec<Pending> = self.pending.borrow_mut().drain(..).collect();
                for pending in pending {
                    if self.running(&pending.action) {
                        self.fail(&pending.action, rules::RESTARTED);
                    }
                }
            }
            _ => {}
        }
    }

    fn waiting(&self, matches: impl Fn(&Wait) -> bool) -> Vec<String> {
        self.pending
            .borrow()
            .iter()
            .filter(|pending| matches(&pending.wait))
            .map(|pending| pending.action.clone())
            .collect()
    }

    fn scan_done(self: &Rc<Self>, phase: ScanPhase, processed: Option<u64>) {
        let text = rules::scan_finished(phase, self.scan.borrow().as_ref(), processed);
        while let Some(pending) = self.take(|wait| matches!(wait, Wait::Scan)) {
            self.finish(&pending.action, text.clone(), None);
        }
    }

    fn on_bulk(self: &Rc<Self>, result: BulkActionResult) {
        let reported: HashSet<i64> = result.messages.iter().filter_map(|item| item.file_id).collect();
        let first_error = || {
            result
                .messages
                .iter()
                .find_map(|item| (!item.ok).then(|| item.message.clone()).flatten())
        };
        if rules::is_trash_result(&result) {
            let Some(pending) = self.take(|wait| {
                matches!(wait, Wait::Trash { ids, .. } if reported.is_empty() || !reported.is_disjoint(ids))
            }) else {
                return;
            };
            let Wait::Trash { ids, bytes, changed } = pending.wait else { return };
            let trashed: Vec<i64> = result
                .messages
                .iter()
                .filter(|item| item.ok)
                .filter_map(|item| item.file_id)
                .filter(|id| ids.contains(id))
                .collect();
            let count = if reported.is_empty() { u64::from(result.succeeded) } else { trashed.len() as u64 };
            if count == 0 {
                let reason = first_error().unwrap_or_else(|| "Nothing was moved to Trash.".to_owned());
                self.fail(&pending.action, &reason);
                return;
            }
            let freed = trashed.iter().filter_map(|id| bytes.get(id)).sum();
            let text = rules::trash_summary(count, freed, changed, u64::from(result.failed));
            let undo = rules::trash_batch(&result).map(|batch_id| -> UndoFn {
                Rc::new(move |runner, id| {
                    if runner.send(CommandPayload::RestoreFromTrash(RestoreFromTrashPayload { batch_id: batch_id.clone() })) {
                        runner.wait(id, Wait::Restore);
                    } else {
                        runner.fail(id, NOT_READY);
                    }
                })
            });
            self.finish(&pending.action, text, undo.map(|undo| ("Put them back", undo)));
            return;
        }
        match result.action.as_str() {
            "restoreFromTrash" => {
                let Some(pending) = self.take(|wait| matches!(wait, Wait::Restore)) else { return };
                let text = if result.succeeded > 0 {
                    format!("Put back {}.", rules::copies(result.succeeded.into()))
                } else {
                    first_error().unwrap_or_else(|| "Nothing was put back. Check the Trash.".to_owned())
                };
                self.finish(&pending.action, text, None);
            }
            "renameFiles" => {
                let Some(pending) = self.take(|wait| {
                    matches!(wait, Wait::Rename { ids, .. } if reported.is_empty() || !reported.is_disjoint(ids))
                }) else {
                    return;
                };
                let Wait::Rename { ids, old, skipped, undoing } = pending.wait else { return };
                let renamed: Vec<i64> = result
                    .messages
                    .iter()
                    .filter(|item| item.ok)
                    .filter_map(|item| item.file_id)
                    .filter(|id| ids.contains(id))
                    .collect();
                if undoing {
                    self.finish(&pending.action, format!("Put back {} names.", crate::shell::grouped(renamed.len() as u64)), None);
                    return;
                }
                let text = rules::rename_summary(renamed.len() as u64, u64::from(result.failed), skipped);
                if renamed.is_empty() {
                    self.fail(&pending.action, &first_error().unwrap_or(text));
                    return;
                }
                let back: Vec<RenameEntry> = renamed
                    .iter()
                    .filter_map(|id| old.get(id).map(|name| RenameEntry { file_id: *id, new_name: name.clone() }))
                    .collect();
                let undo: UndoFn = Rc::new(move |runner, id| {
                    let ids = back.iter().map(|entry| entry.file_id).collect();
                    if runner.send(CommandPayload::RenameFiles(RenameFilesPayload { renames: back.clone() })) {
                        runner.wait(id, Wait::Rename { ids, old: HashMap::new(), skipped: 0, undoing: true });
                    } else {
                        runner.fail(id, NOT_READY);
                    }
                });
                self.finish(&pending.action, text, Some(("Undo renames", undo)));
            }
            "renamePerson" => {
                let Some(pending) = self.take(|wait| matches!(wait, Wait::Person { .. })) else { return };
                let Wait::Person { person, before } = pending.wait else { return };
                if result.failed > 0 || result.succeeded == 0 {
                    let reason = first_error().unwrap_or_else(|| "The name wasn't saved. Try again in People.".to_owned());
                    self.fail(&pending.action, &reason);
                    return;
                }
                match before {
                    Some(before) => {
                        let undo: UndoFn = Rc::new(move |runner, id| runner.restore_person(id, person, &before));
                        self.finish(&pending.action, "Name saved.".to_owned(), Some(("Undo", undo)));
                    }
                    None => self.finish(&pending.action, "Name put back.".to_owned(), None),
                }
            }
            "markPersonsAsUnknown" => {
                let Some(pending) = self.take(|wait| matches!(wait, Wait::Unknown)) else { return };
                self.finish(&pending.action, "Name put back.".to_owned(), None);
            }
            "mergeClusters" => {
                let (action, done, failed) = {
                    let mut pending = self.pending.borrow_mut();
                    let Some(entry) = pending.iter_mut().find(|p| matches!(p.wait, Wait::Merge { .. })) else { return };
                    let Wait::Merge { outstanding, failed } = &mut entry.wait else { return };
                    *outstanding = outstanding.saturating_sub(1);
                    *failed |= result.failed > 0 || result.succeeded == 0;
                    (entry.action.clone(), *outstanding == 0, *failed)
                };
                if !done {
                    return;
                }
                let _ = self.take(|wait| matches!(wait, Wait::Merge { outstanding: 0, .. }));
                if failed {
                    self.fail(&action, "The merge didn't fully go through. Check People.");
                } else {
                    self.finish(&action, "Merged. These people are now one card in People.".to_owned(), None);
                }
            }
            _ => {}
        }
    }

    fn restore_person(self: &Rc<Self>, id: &str, person_id: i64, before: &PersonName) {
        let (payload, wait) = if before.unknown {
            (
                CommandPayload::MarkPersonsAsUnknown(MarkPersonsAsUnknownPayload { person_ids: vec![person_id] }),
                Wait::Unknown,
            )
        } else {
            let structured = [&before.title, &before.first, &before.middle, &before.last, &before.suffix];
            let first_name = if structured.iter().all(|part| part.is_none()) {
                before.name.clone()
            } else {
                before.first.clone()
            };
            (
                CommandPayload::RenamePerson(RenamePersonPayload {
                    person_id,
                    title: before.title.clone(),
                    first_name,
                    middle_name: before.middle.clone(),
                    last_name: before.last.clone(),
                    suffix: before.suffix.clone(),
                }),
                Wait::Person { person: person_id, before: None },
            )
        };
        if self.send(payload) {
            self.wait(id, wait);
        } else {
            self.fail(id, NOT_READY);
        }
    }
}

struct Card {
    action: ChatAction,
    detail: gtk::Label,
    well: gtk::Box,
    status: gtk::Box,
    buttons: gtk::Box,
    shown: RefCell<Option<Phase>>,
    progress: RefCell<Option<gtk::Label>>,
    plan: RefCell<Option<PlanSummary>>,
}

pub(super) struct Cards {
    runner: Rc<Runner>,
    pub(super) root: gtk::Box,
    overlay: adw::ToastOverlay,
    cards: RefCell<Vec<Rc<Card>>>,
}

impl Cards {
    pub(super) fn new(engine: &Engine, overlay: &adw::ToastOverlay) -> Rc<Self> {
        let root = v3::vbox(10);
        root.add_css_class("v3-action-cards");
        root.set_visible(false);
        let cards = Rc::new(Self {
            runner: Runner::shared(engine),
            root,
            overlay: overlay.clone(),
            cards: RefCell::default(),
        });
        cards.runner.listeners.borrow_mut().push(Rc::downgrade(&cards));
        cards
    }

    pub(super) fn show(self: &Rc<Self>, actions: Vec<ChatAction>) {
        v3::clear(&self.root);
        let cards: Vec<Rc<Card>> = actions
            .into_iter()
            .filter(rules::runnable)
            .map(|action| {
                let (widget, card) = self.card(action);
                self.root.append(&widget);
                card
            })
            .collect();
        self.root.set_visible(!cards.is_empty());
        self.cards.replace(cards);
        self.refresh();
    }

    pub(super) fn clear(self: &Rc<Self>) {
        self.show(Vec::new());
    }

    /// Cards wait under the answer; they hide while a new answer is coming.
    pub(super) fn set_hidden(&self, hidden: bool) {
        self.root.set_visible(!hidden && !self.cards.borrow().is_empty());
    }

    fn refresh(self: &Rc<Self>) {
        let cards = self.cards.borrow().clone();
        for card in cards {
            self.update(&card);
        }
    }

    fn card(self: &Rc<Self>, action: ChatAction) -> (gtk::Box, Rc<Card>) {
        let accent = rules::accent(&action);
        let root = v3::vbox(10);
        root.add_css_class("v3-action-card");
        root.add_css_class(match accent {
            rules::Accent::Danger => "accent-danger",
            rules::Accent::Gold => "accent-gold",
            rules::Accent::Info => "accent-info",
        });
        let header = v3::hbox(8);
        let section = v3::section(rules::section(&action));
        section.add_css_class(accent.css());
        section.set_hexpand(true);
        header.append(&section);
        if action.source == "model" {
            header.append(&v3::label("Suggested by local AI", &["v3-meta", "tone-ai"]));
        }
        root.append(&header);
        let detail = v3::wrapped("", &["v3-text-13"]);
        root.append(&detail);
        let well = v3::vbox(6);
        well.add_css_class("v3-action-well");
        root.append(&well);
        let status = v3::hbox(10);
        root.append(&status);
        let buttons = v3::hbox(8);
        root.append(&buttons);
        root.update_property(&[gtk::accessible::Property::Label(&format!(
            "{}: {}",
            if action.requires_confirmation { "Needs your OK" } else { "Suggested action" },
            action.title
        ))]);
        let card = Rc::new(Card {
            action,
            detail,
            well,
            status,
            buttons,
            shown: RefCell::new(None),
            progress: RefCell::new(None),
            plan: RefCell::new(None),
        });
        (root, card)
    }

    fn update(self: &Rc<Self>, card: &Rc<Card>) {
        let action = &card.action;
        let phase = self.runner.phase(&action.id);
        let plan = (action.kind == "applyFolderLayout").then(|| self.runner.plan_summary(action)).flatten();
        let plan_changed = match (card.plan.borrow().as_ref(), plan.as_ref()) {
            (Some(shown), Some(next)) => !shown.same(next),
            (None, None) => false,
            _ => true,
        };
        let unchanged = card.shown.borrow().as_ref().is_some_and(|shown| shown.same_shape(&phase));
        if unchanged && !plan_changed {
            if let (Phase::Running(text), Some(label)) = (&phase, card.progress.borrow().as_ref()) {
                label.set_text(text);
            }
            return;
        }
        card.plan.replace(plan.clone());
        card.shown.replace(Some(phase.clone()));
        card.detail.set_text(&rules::card_detail(action, plan.as_ref()));
        v3::clear(&card.well);
        card.well.set_visible(action.requires_confirmation && phase == Phase::Proposed);
        if card.well.is_visible() {
            fill_well(&card.well, action, plan.as_ref());
        }
        v3::clear(&card.status);
        card.progress.replace(None);
        match &phase {
            Phase::Proposed => {}
            Phase::Running(text) => {
                let bar = v3::busy_bar(60);
                bar.set_visible(true);
                card.status.append(&bar);
                let label = v3::wrapped(text, &["v3-meta", "tone-ai"]);
                card.status.append(&label);
                card.progress.replace(Some(label));
            }
            Phase::Done(text) => {
                let icon = gtk::Image::from_icon_name("object-select-symbolic");
                icon.add_css_class("tone-done");
                icon.set_valign(gtk::Align::Start);
                card.status.append(&icon);
                card.status.append(&v3::wrapped(text, &["v3-meta", "tone-text2"]));
            }
            Phase::Failed(text) => {
                let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
                icon.add_css_class("tone-danger");
                icon.set_valign(gtk::Align::Start);
                card.status.append(&icon);
                card.status.append(&v3::wrapped(text, &["v3-meta", "tone-danger"]));
            }
            Phase::Cancelled => card.status.append(&v3::meta("Cancelled. Nothing changed.")),
        }
        card.status.set_visible(phase != Phase::Proposed);
        if card.status.is_visible() {
            let status = card.status.clone();
            let _ = crate::spring::animate(&card.status, 0.0, 1.0, move |value| {
                status.set_opacity(value.clamp(0.0, 1.0));
            });
        }
        self.fill_buttons(card, &phase, plan.as_ref());
    }

    fn fill_buttons(self: &Rc<Self>, card: &Rc<Card>, phase: &Phase, plan: Option<&PlanSummary>) {
        v3::clear(&card.buttons);
        let action = &card.action;
        match phase {
            Phase::Proposed => {
                let title = rules::primary_title(action, plan);
                let primary = if action.kind == "moveDuplicatesToTrash" {
                    v3::danger(&title)
                } else {
                    v3::primary(&title, true)
                };
                let reason = rules::unavailable(action, plan, super::deep_analyze::vlm_runtime_available());
                primary.set_sensitive(reason.is_none());
                primary.set_tooltip_text(reason);
                let weak = Rc::downgrade(self);
                let target = card.clone();
                primary.connect_clicked(move |button| {
                    if let Some(cards) = weak.upgrade() {
                        cards.runner.clone().run(&target.action, button, &cards.overlay);
                    }
                });
                card.buttons.append(&primary);
                if action.requires_confirmation {
                    let cancel = v3::secondary("Cancel", false);
                    let weak = Rc::downgrade(self);
                    let target = card.clone();
                    cancel.connect_clicked(move |_| {
                        if let Some(cards) = weak.upgrade() {
                            cards.runner.cancel(&target.action);
                        }
                    });
                    card.buttons.append(&cancel);
                }
            }
            Phase::Running(_) if rules::can_stop(&action.kind) => {
                let stop = v3::secondary("Stop", false);
                let weak = Rc::downgrade(self);
                let target = card.clone();
                stop.connect_clicked(move |_| {
                    if let Some(cards) = weak.upgrade() {
                        cards.runner.stop(&target.action);
                    }
                });
                card.buttons.append(&stop);
            }
            Phase::Done(_) => {
                if let Some(label) = self.runner.has_undo(&action.id) {
                    let undo = v3::secondary(&label, false);
                    let weak = Rc::downgrade(self);
                    let id = action.id.clone();
                    undo.connect_clicked(move |_| {
                        if let Some(cards) = weak.upgrade() {
                            cards.runner.clone().undo(&id);
                        }
                    });
                    card.buttons.append(&undo);
                }
                if action.kind == "moveDuplicatesToTrash" {
                    let open = v3::secondary("Open Trash", false);
                    open.connect_clicked(|_| {
                        let _ = gtk::gio::AppInfo::launch_default_for_uri(
                            "trash:///",
                            gtk::gio::AppLaunchContext::NONE,
                        );
                    });
                    card.buttons.append(&open);
                }
            }
            _ => {}
        }
        card.buttons.set_visible(card.buttons.first_child().is_some());
    }
}

fn fill_well(well: &gtk::Box, action: &ChatAction, plan: Option<&PlanSummary>) {
    let facts = rules::facts(action, plan);
    let counts = v3::hbox(14);
    for (label, value) in [("Files", &facts.files), ("Size", &facts.size)] {
        if let Some(value) = value {
            let fact = v3::vbox(2);
            fact.append(&v3::section(label));
            let shown = v3::mono(value, "s125");
            shown.add_css_class("v3-semibold");
            fact.append(&shown);
            fact.update_property(&[gtk::accessible::Property::Label(&format!("{label}: {value}"))]);
            counts.append(&fact);
        }
    }
    if counts.first_child().is_some() {
        well.append(&counts);
    }
    if !facts.examples.is_empty() {
        let list = v3::vbox(3);
        for example in &facts.examples {
            let row = v3::mono(example, "s115");
            row.add_css_class("tone-text2");
            row.set_tooltip_text(Some(example));
            list.append(&row);
        }
        if let Some(more) = &facts.more {
            list.append(&v3::label(more, &["v3-meta-115"]));
        }
        well.append(&list);
    }
    if let Some(undo) = &facts.undo {
        well.append(&v3::wrapped(undo, &["v3-meta", "tone-text2"]));
    }
}
