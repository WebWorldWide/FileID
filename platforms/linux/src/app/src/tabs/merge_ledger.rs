// Which page asked for each `mergeClusters`. Every sender files its merge
// here with the id of the command it sent, and each listener asks who a
// result belongs to; a Chat merge can then never be filed against a People
// undo record (or the other way round). A result matches by the command id it
// echoes (schema 1.28.0); one from a helper that does not echo it is matched
// by the two people its sentence names.

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct Waiting {
    command_id: String,
    entry: Entry,
}

#[derive(Default)]
struct Ledger {
    pending: VecDeque<Waiting>,
    /// The last result resolved, kept alive so every listener of that one
    /// event gets the same answer.
    last: Option<(Rc<EngineEvent>, Option<Entry>)>,
}

thread_local! {
    static LEDGER: RefCell<Ledger> = RefCell::default();
}

pub(super) fn sent(owner: Owner, command_id: String, source: i64, destination: i64) {
    LEDGER.with_borrow_mut(|ledger| {
        ledger.pending.push_back(Waiting {
            command_id,
            entry: Entry {
                owner,
                source,
                destination,
            },
        });
    });
}

/// The merge this `mergeClusters` result answers, if one of ours.
pub(super) fn entry_for(event: &Rc<EngineEvent>) -> Option<Entry> {
    let EngineEvent::BulkActionResult(result) = &**event else {
        return None;
    };
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

/// A result that echoes a command id is matched by it, and belongs to no one
/// here when we sent no such command. One without an id that names its two
/// people is matched by them; one that names none (a failure) answers the
/// oldest merge still waiting.
fn resolve(pending: &mut VecDeque<Waiting>, result: &BulkActionResult) -> Option<Entry> {
    let index = if let Some(id) = result.command_id.as_deref() {
        pending
            .iter()
            .position(|waiting| waiting.command_id == id)?
    } else {
        let named = result
            .messages
            .iter()
            .find_map(|item| item.message.as_deref().and_then(named_pair));
        match named {
            Some((source, destination)) => pending.iter().position(|waiting| {
                waiting.entry.source == source && waiting.entry.destination == destination
            })?,
            None if pending.is_empty() => return None,
            None => 0,
        }
    };
    pending.remove(index).map(|waiting| waiting.entry)
}

fn named_pair(message: &str) -> Option<(i64, i64)> {
    if let Some((_, rest)) = message.split_once("from #") {
        let (source, destination) = rest.split_once(" into #")?;
        return Some((source.trim().parse().ok()?, leading_id(destination)?));
    }
    let same = leading_id(message.strip_prefix('#')?)?;
    message
        .contains("already one cluster")
        .then_some((same, same))
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
            command_id: None,
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
        Entry {
            owner,
            source,
            destination,
        }
    }

    fn waiting(command_id: &str, owner: Owner, source: i64, destination: i64) -> Waiting {
        Waiting {
            command_id: command_id.to_owned(),
            entry: entry(owner, source, destination),
        }
    }

    fn echoed(command_id: &str, ok: bool, message: &str) -> BulkActionResult {
        BulkActionResult {
            command_id: Some(command_id.to_owned()),
            ..result(ok, message)
        }
    }

    #[test]
    fn a_chat_merge_is_never_filed_against_a_people_record() {
        let mut pending = VecDeque::from([
            waiting("a", Owner::People, 4, 9),
            waiting("b", Owner::Chat, 7, 2),
        ]);
        let chat = resolve(
            &mut pending,
            &result(true, "moved 12 face prints from #7 into #2"),
        );
        assert_eq!(chat, Some(entry(Owner::Chat, 7, 2)));
        assert_eq!(pending, [waiting("a", Owner::People, 4, 9)]);
        let people = resolve(
            &mut pending,
            &result(true, "moved 3 face prints from #4 into #9"),
        );
        assert_eq!(people, Some(entry(Owner::People, 4, 9)));
        assert!(pending.is_empty());
    }

    #[test]
    fn a_merge_nobody_here_sent_belongs_to_no_one() {
        let mut pending = VecDeque::from([waiting("a", Owner::People, 4, 9)]);
        assert_eq!(
            resolve(
                &mut pending,
                &result(true, "moved 1 face prints from #30 into #31")
            ),
            None
        );
        assert_eq!(pending.len(), 1);
        assert_eq!(resolve(&mut VecDeque::new(), &result(false, "busy")), None);
    }

    #[test]
    fn a_failure_without_ids_answers_the_oldest_waiting_merge() {
        let mut pending = VecDeque::from([
            waiting("a", Owner::Chat, 1, 2),
            waiting("b", Owner::People, 3, 4),
        ]);
        let failed = result(
            false,
            "One or both people no longer exist; refresh People and try again.",
        );
        assert_eq!(
            resolve(&mut pending, &failed),
            Some(entry(Owner::Chat, 1, 2))
        );
        assert_eq!(pending, [waiting("b", Owner::People, 3, 4)]);
    }

    #[test]
    fn a_result_is_matched_by_its_command_id_whatever_its_sentence_says() {
        let mut pending = VecDeque::from([
            waiting("a", Owner::People, 4, 9),
            waiting("b", Owner::Chat, 7, 2),
        ]);
        let reworded = echoed("b", true, "Merged the two people.");
        assert_eq!(
            resolve(&mut pending, &reworded),
            Some(entry(Owner::Chat, 7, 2))
        );
        assert_eq!(pending, [waiting("a", Owner::People, 4, 9)]);
        let failed = echoed("a", false, "busy");
        assert_eq!(
            resolve(&mut pending, &failed),
            Some(entry(Owner::People, 4, 9))
        );
    }

    #[test]
    fn the_command_id_wins_over_a_sentence_naming_other_people() {
        let mut pending = VecDeque::from([
            waiting("a", Owner::People, 4, 9),
            waiting("b", Owner::Chat, 4, 9),
        ]);
        let second = echoed("b", true, "moved 3 face prints from #4 into #9");
        assert_eq!(
            resolve(&mut pending, &second),
            Some(entry(Owner::Chat, 4, 9))
        );
        assert_eq!(pending, [waiting("a", Owner::People, 4, 9)]);
    }

    #[test]
    fn a_result_with_an_id_we_never_sent_belongs_to_no_one() {
        let mut pending = VecDeque::from([waiting("a", Owner::People, 4, 9)]);
        assert_eq!(
            resolve(
                &mut pending,
                &echoed("other", true, "moved 3 face prints from #4 into #9")
            ),
            None
        );
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn two_pages_merging_the_same_pair_each_get_their_own_result() {
        let mut pending = VecDeque::from([
            waiting("people", Owner::People, 4, 9),
            waiting("chat", Owner::Chat, 4, 9),
        ]);
        let chat = echoed("chat", true, "moved 3 face prints from #4 into #9");
        assert_eq!(resolve(&mut pending, &chat), Some(entry(Owner::Chat, 4, 9)));
        let people = echoed(
            "people",
            false,
            "#4 is already one cluster; nothing to merge",
        );
        assert_eq!(
            resolve(&mut pending, &people),
            Some(entry(Owner::People, 4, 9))
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn ids_are_read_from_the_helper_sentences() {
        assert_eq!(
            named_pair("moved 12 face prints from #7 into #2"),
            Some((7, 2))
        );
        assert_eq!(
            named_pair("#5 is already one cluster; nothing to merge"),
            Some((5, 5))
        );
        assert_eq!(named_pair("One or both people no longer exist"), None);
        assert_eq!(named_pair("from #x into #2"), None);
    }

    #[test]
    fn every_listener_of_one_event_gets_the_same_answer() {
        clear();
        sent(Owner::People, "a".into(), 4, 9);
        sent(Owner::People, "b".into(), 5, 9);
        let event = Rc::new(EngineEvent::BulkActionResult(result(
            true,
            "moved 3 face prints from #4 into #9",
        )));
        assert_eq!(entry_for(&event), Some(entry(Owner::People, 4, 9)));
        assert_eq!(entry_for(&event), Some(entry(Owner::People, 4, 9)));
        let next = Rc::new(EngineEvent::BulkActionResult(result(
            true,
            "moved 1 face prints from #5 into #9",
        )));
        assert_eq!(entry_for(&next), Some(entry(Owner::People, 5, 9)));
        assert_eq!(entry_for(&Rc::new(EngineEvent::Ready)), None);
        clear();
    }
}
