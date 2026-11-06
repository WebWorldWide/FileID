// What the pages share with Chat (macOS `ChatHandoff`): the Library
// selection and the person open or people checked in People travel with each
// send as ChatContext; File Tools receives the files an export card hands it.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

pub(super) const MAX_FILE_IDS: usize = 10_000;
pub(super) const MAX_PERSON_IDS: usize = 50;

type Receiver = Weak<dyn Fn(Vec<i64>)>;

#[derive(Default)]
struct Shared {
    files: Vec<i64>,
    people: Vec<i64>,
    tools: Option<Receiver>,
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
