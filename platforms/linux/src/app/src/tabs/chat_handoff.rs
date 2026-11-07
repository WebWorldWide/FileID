// What the pages share with Chat (macOS `ChatHandoff`): the Library
// selection and the person open or people checked in People travel with each
// send as ChatContext; File Tools receives the files an export card hands it.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

pub(super) const MAX_FILE_IDS: usize = 10_000;
pub(super) const MAX_PERSON_IDS: usize = 50;

pub(super) const MAX_SCOPE: usize = 2000;

type Receiver = Weak<dyn Fn(Vec<i64>)>;

/// Files attached to the next chat question ("12 selected photos").
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChatScope {
    pub file_ids: Vec<i64>,
    pub label: String,
}

pub(super) fn noun(kinds: &[&str], count: usize) -> String {
    let base = if kinds.iter().all(|kind| *kind == "image") {
        "photo"
    } else if kinds.iter().all(|kind| *kind == "video") {
        "video"
    } else {
        "file"
    };
    if count == 1 { base.to_owned() } else { format!("{base}s") }
}

impl ChatScope {
    pub(super) fn selected(ids: &[i64], kinds: &[&str]) -> Option<Self> {
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<i64> = ids.iter().copied().filter(|id| *id > 0 && seen.insert(*id)).take(MAX_SCOPE).collect();
        (!unique.is_empty()).then(|| Self { label: format!("{} selected {}", unique.len(), noun(kinds, unique.len())), file_ids: unique })
    }

    /// The matches of a scoped answer, which become the next question's scope.
    pub(super) fn narrowed(hits: &[fileid_engine::ipc::CatalogHit]) -> Option<Self> {
        let yes: Vec<i64> = hits.iter().filter(|hit| hit.selection_match.as_deref() == Some("yes")).map(|hit| hit.file_id).collect();
        let count = yes.len();
        (count > 0).then(|| Self { file_ids: yes, label: format!("{count} {} from the last answer", if count == 1 { "match" } else { "matches" }) })
    }
}

#[derive(Default)]
struct Shared {
    files: Vec<i64>,
    people: Vec<i64>,
    tools: Option<Receiver>,
    scope: Option<ChatScope>,
    scope_watchers: Vec<Weak<dyn Fn()>>,
}

pub(super) fn scope() -> Option<ChatScope> {
    SHARED.with_borrow(|shared| shared.scope.clone())
}

/// Replaces the scope and tells every open Chat view.
pub(super) fn set_scope(scope: Option<ChatScope>) {
    let watchers: Vec<Rc<dyn Fn()>> = SHARED.with_borrow_mut(|shared| {
        shared.scope = scope;
        shared.scope_watchers.retain(|watcher| watcher.strong_count() > 0);
        shared.scope_watchers.iter().filter_map(Weak::upgrade).collect()
    });
    for watcher in watchers {
        watcher();
    }
}

pub(super) fn watch_scope(watcher: &Rc<dyn Fn()>) {
    SHARED.with_borrow_mut(|shared| shared.scope_watchers.push(Rc::downgrade(watcher)));
}

/// Attaches the files to the next question and opens Chat.
pub(super) fn ask_about(anchor: &impl gtk::prelude::IsA<gtk::Widget>, ids: &[i64], kinds: &[&str]) {
    use gtk::prelude::*;
    if let Some(scope) = ChatScope::selected(ids, kinds) {
        set_scope(Some(scope));
        let _ = anchor.as_ref().activate_action("win.navigate", Some(&"chat".to_variant()));
    }
}

thread_local! {
    static SHARED: RefCell<Shared> = RefCell::default();
}

pub(super) fn set_files(ids: Vec<i64>) {
    SHARED.with_borrow_mut(|shared| shared.files = ids);
}

pub(super) fn selected_files() -> Vec<i64> {
    SHARED.with_borrow(|shared| shared.files.clone())
}

pub(super) fn set_people(ids: Vec<i64>) {
    SHARED.with_borrow_mut(|shared| shared.people = ids);
}

fn capped(ids: &[i64], max: usize) -> Option<Vec<i64>> {
    (!ids.is_empty()).then(|| ids.iter().copied().filter(|id| *id > 0).take(max).collect())
}

/// `(fileIDs, personIDs)` for the next send, omitted when empty.
pub(super) fn context_ids() -> (Option<Vec<i64>>, Option<Vec<i64>>) {
    SHARED.with_borrow(|shared| {
        (
            capped(&shared.files, MAX_FILE_IDS),
            capped(&shared.people, MAX_PERSON_IDS),
        )
    })
}

pub(super) fn receive_in_tools(receiver: &Rc<dyn Fn(Vec<i64>)>) {
    SHARED.with_borrow_mut(|shared| shared.tools = Some(Rc::downgrade(receiver)));
}

/// False when File Tools isn't built, so the caller can say so.
pub(super) fn hand_to_tools(ids: Vec<i64>) -> bool {
    let receiver = SHARED.with_borrow(|shared| shared.tools.as_ref().and_then(Weak::upgrade));
    match receiver {
        Some(receive) => {
            receive(ids);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selections_are_capped_like_macos_and_omitted_when_empty() {
        set_files(Vec::new());
        set_people(Vec::new());
        assert_eq!(context_ids(), (None, None));
        set_files((1..=12_000).collect());
        set_people((1..=60).collect());
        let (files, people) = context_ids();
        assert_eq!(files.map(|ids| ids.len()), Some(MAX_FILE_IDS));
        assert_eq!(people.as_deref().map(<[i64]>::len), Some(MAX_PERSON_IDS));
        set_people(vec![7]);
        assert_eq!(context_ids().1, Some(vec![7]));
    }

    #[test]
    fn a_scope_is_deduplicated_capped_and_labelled_by_kind() {
        let scope = ChatScope::selected(&[3, 3, 0, 4], &["image", "image"]).unwrap();
        assert_eq!(scope, ChatScope { file_ids: vec![3, 4], label: "2 selected photos".into() });
        assert_eq!(ChatScope::selected(&[5], &["video"]).unwrap().label, "1 selected video");
        assert_eq!(ChatScope::selected(&[5, 6], &["video", "image"]).unwrap().label, "2 selected files");
        assert!(ChatScope::selected(&[], &[]).is_none());
        assert_eq!(ChatScope::selected(&(1..=2500).collect::<Vec<_>>(), &[]).unwrap().file_ids.len(), MAX_SCOPE);
    }

    #[test]
    fn only_matches_narrow_the_next_question_and_watchers_hear_changes() {
        let hit = |id: i64, verdict: &str| fileid_engine::ipc::CatalogHit {
            file_id: id,
            path: String::new(),
            kind: "selection".into(),
            text: String::new(),
            evidence_id: None,
            start_seconds: None,
            page: None,
            selection_match: Some(verdict.into()),
        };
        assert_eq!(ChatScope::narrowed(&[hit(1, "unsure"), hit(2, "yes")]).unwrap(), ChatScope { file_ids: vec![2], label: "1 match from the last answer".into() });
        assert!(ChatScope::narrowed(&[hit(1, "unsure")]).is_none());
        let heard = Rc::new(std::cell::Cell::new(0));
        let count = heard.clone();
        let watcher: Rc<dyn Fn()> = Rc::new(move || count.set(count.get() + 1));
        watch_scope(&watcher);
        set_scope(ChatScope::selected(&[9], &["image"]));
        assert_eq!(scope().unwrap().file_ids, vec![9]);
        set_scope(None);
        assert_eq!(heard.get(), 2);
        drop(watcher);
        set_scope(None);
        assert_eq!(heard.get(), 2);
    }

    #[test]
    fn tools_receive_the_handed_files_while_they_exist() {
        let got = Rc::new(RefCell::new(Vec::new()));
        let sink = got.clone();
        let receiver: Rc<dyn Fn(Vec<i64>)> = Rc::new(move |ids| {
            sink.replace(ids);
        });
        receive_in_tools(&receiver);
        assert!(hand_to_tools(vec![3, 4]));
        assert_eq!(*got.borrow(), vec![3, 4]);
        drop(receiver);
        assert!(!hand_to_tools(vec![5]));
    }
}
