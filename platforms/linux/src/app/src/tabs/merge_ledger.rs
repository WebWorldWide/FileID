// Which page asked for each `mergeClusters`. The result carries no request
// id, so every sender files its merge here and each listener asks who a
// result belongs to; a Chat merge can then never be filed against a People
// undo record (or the other way round).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use fileid_engine::ipc::BulkActionResult;

use crate::engine_client::EngineEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Owner {
    People,
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Entry {
    pub owner: Owner,
    pub source: i64,
    pub destination: i64,
}

#[derive(Default)]
struct Ledger {
    pending: VecDeque<Entry>,
    /// The last result resolved, kept alive so every listener of that one
    /// event gets the same answer.
    last: Option<(Rc<EngineEvent>, Option<Entry>)>,
}

thread_local! {
    static LEDGER: RefCell<Ledger> = RefCell::default();
}

pub(super) fn sent(owner: Owner, source: i64, destination: i64) {
    LEDGER.with_borrow_mut(|ledger| ledger.pending.push_back(Entry { owner, source, destination }));
}

/// The merge this `mergeClusters` result answers, if one of ours.
pub(super) fn entry_for(event: &Rc<EngineEvent>) -> Option<Entry> {
    let EngineEvent::BulkActionResult(result) = &**event else { return None };
    if result.action != "mergeClusters" {
        return None;
    }
    LEDGER.with_borrow_mut(|ledger| {
        if let Some((seen, entry)) = &ledger.last
            && Rc::ptr_eq(seen, event)
        {
            return *entry;
        }
        let entry = resolve(&mut ledger.pending, result);
        ledger.last = Some((event.clone(), entry));
        entry
    })
}

/// The helper restarted: no result is coming for anything still waiting.
pub(super) fn clear() {
    LEDGER.with_borrow_mut(|ledger| ledger.pending.clear());
}

/// A result that names its two people is matched by them; one that names
/// none (a failure) answers the oldest merge still waiting.
fn resolve(pending: &mut VecDeque<Entry>, result: &BulkActionResult) -> Option<Entry> {
    let named = result.messages.iter().find_map(|item| item.message.as_deref().and_then(named_pair));
    let index = match named {
        Some((source, destination)) => {
            pending.iter().position(|entry| entry.source == source && entry.destination == destination)?
        }
        None if pending.is_empty() => return None,
        None => 0,
    };
    pending.remove(index)
}

fn named_pair(message: &str) -> Option<(i64, i64)> {
    if let Some((_, rest)) = message.split_once("from #") {
        let (source, destination) = rest.split_once(" into #")?;
        return Some((source.trim().parse().ok()?, leading_id(destination)?));
    }
    let same = leading_id(message.strip_prefix('#')?)?;
    message.contains("already one cluster").then_some((same, same))
}

fn leading_id(text: &str) -> Option<i64> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::BulkActionItem;

    fn result(ok: bool, message: &str) -> BulkActionResult {
        BulkActionResult {
            action: "mergeClusters".into(),
            succeeded: u32::from(ok),
            failed: u32::from(!ok),
            messages: vec![BulkActionItem {
                file_id: None,
                ok,
                message: Some(message.to_owned()),
                reason: None,
                operation_id: None,
            }],
        }
    }

    fn entry(owner: Owner, source: i64, destination: i64) -> Entry {
        Entry { owner, source, destination }
    }

    #[test]
    fn a_chat_merge_is_never_filed_against_a_people_record() {
        let mut pending = VecDeque::from([entry(Owner::People, 4, 9), entry(Owner::Chat, 7, 2)]);
        let chat = resolve(&mut pending, &result(true, "moved 12 face prints from #7 into #2"));
        assert_eq!(chat, Some(entry(Owner::Chat, 7, 2)));
        assert_eq!(pending, [entry(Owner::People, 4, 9)]);
        let people = resolve(&mut pending, &result(true, "moved 3 face prints from #4 into #9"));
        assert_eq!(people, Some(entry(Owner::People, 4, 9)));
        assert!(pending.is_empty());
    }

    #[test]
    fn a_merge_nobody_here_sent_belongs_to_no_one() {
        let mut pending = VecDeque::from([entry(Owner::People, 4, 9)]);
        assert_eq!(resolve(&mut pending, &result(true, "moved 1 face prints from #30 into #31")), None);
        assert_eq!(pending.len(), 1);
        assert_eq!(resolve(&mut VecDeque::new(), &result(false, "busy")), None);
    }

    #[test]
    fn a_failure_without_ids_answers_the_oldest_waiting_merge() {
        let mut pending = VecDeque::from([entry(Owner::Chat, 1, 2), entry(Owner::People, 3, 4)]);
        let failed = result(false, "One or both people no longer exist; refresh People and try again.");
        assert_eq!(resolve(&mut pending, &failed), Some(entry(Owner::Chat, 1, 2)));
        assert_eq!(pending, [entry(Owner::People, 3, 4)]);
    }

    #[test]
    fn ids_are_read_from_the_helper_sentences() {
        assert_eq!(named_pair("moved 12 face prints from #7 into #2"), Some((7, 2)));
        assert_eq!(named_pair("#5 is already one cluster; nothing to merge"), Some((5, 5)));
        assert_eq!(named_pair("One or both people no longer exist"), None);
        assert_eq!(named_pair("from #x into #2"), None);
    }

    #[test]
    fn every_listener_of_one_event_gets_the_same_answer() {
        clear();
        sent(Owner::People, 4, 9);
        sent(Owner::People, 5, 9);
        let event = Rc::new(EngineEvent::BulkActionResult(result(true, "moved 3 face prints from #4 into #9")));
        assert_eq!(entry_for(&event), Some(entry(Owner::People, 4, 9)));
        assert_eq!(entry_for(&event), Some(entry(Owner::People, 4, 9)));
        let next = Rc::new(EngineEvent::BulkActionResult(result(true, "moved 1 face prints from #5 into #9")));
        assert_eq!(entry_for(&next), Some(entry(Owner::People, 5, 9)));
        assert_eq!(entry_for(&Rc::new(EngineEvent::Ready)), None);
        clear();
    }
}
