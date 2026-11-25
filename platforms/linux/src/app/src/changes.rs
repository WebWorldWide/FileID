// Session-only change log with undo. Nothing is written to disk; GTK main thread only.

use std::cell::RefCell;
use std::rc::Rc;

const CAPACITY: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Undoable,
    Undoing,
    Undone,
    Failed(String),
    NotUndoable(String),
}

pub type Reverse = Rc<dyn Fn() -> bool>;

#[derive(Clone)]
pub struct Entry {
    pub id: u64,
    pub label: String,
    pub at: std::time::SystemTime,
    pub status: Status,
    reverse: Option<Reverse>,
}

#[derive(Default)]
struct Log {
    next: u64,
    entries: Vec<Entry>,
    next_listener: u64,
    listeners: Vec<(u64, Rc<dyn Fn()>)>,
}

thread_local! {
    static LOG: RefCell<Log> = RefCell::new(Log::default());
}

fn changed() {
    let listeners = LOG.with_borrow(|log| log.listeners.clone());
    for (_, listener) in listeners {
        listener();
    }
}

pub fn subscribe(listener: Rc<dyn Fn()>) -> u64 {
    LOG.with_borrow_mut(|log| {
        log.next_listener += 1;
        let id = log.next_listener;
        log.listeners.push((id, listener));
        id
    })
}

pub fn unsubscribe(id: u64) {
    LOG.with_borrow_mut(|log| log.listeners.retain(|(listener, _)| *listener != id));
}


/// Records a change; `reverse` puts it back. Returns the entry id.
pub fn push(label: &str, reverse: Option<Reverse>) -> u64 {
    let id = LOG.with_borrow_mut(|log| {
        log.next += 1;
        let id = log.next;
        let status = if reverse.is_some() {
            Status::Undoable
        } else {
            Status::NotUndoable("Nothing to put back".to_owned())
        };
        log.entries.push(Entry {
            id,
            label: label.to_owned(),
            at: std::time::SystemTime::now(),
            status,
            reverse,
        });
        if log.entries.len() > CAPACITY {
            let overflow = log.entries.len() - CAPACITY;
            log.entries.drain(..overflow);
        }
        id
    });
    changed();
    id
}

pub fn set_status(id: u64, status: Status) {
    LOG.with_borrow_mut(|log| {
        if let Some(entry) = log.entries.iter_mut().find(|entry| entry.id == id) {
            if matches!(status, Status::Undone | Status::NotUndoable(_)) {
                entry.reverse = None;
            }
            entry.status = status;
        }
    });
    changed();
}

/// Newest first.
pub fn entries() -> Vec<Entry> {
    LOG.with_borrow(|log| log.entries.iter().rev().cloned().collect())
}

pub fn count() -> usize {
    LOG.with_borrow(|log| log.entries.len())
}

pub fn pending_count() -> usize {
    LOG.with_borrow(|log| {
        log.entries
            .iter()
            .filter(|entry| entry.status == Status::Undoable)
            .count()
    })
}

pub fn most_recent_undoable() -> Option<u64> {
    LOG.with_borrow(|log| {
        log.entries
            .iter()
            .rev()
            .find(|entry| entry.status == Status::Undoable)
            .map(|entry| entry.id)
    })
}

/// Undoes one entry. Returns false when it is not undoable right now.
pub fn undo(id: u64) -> bool {
    let reverse = LOG.with_borrow_mut(|log| {
        let entry = log.entries.iter_mut().find(|entry| entry.id == id)?;
        if !matches!(entry.status, Status::Undoable | Status::Failed(_)) {
            return None;
        }
        let reverse = entry.reverse.clone()?;
        entry.status = Status::Undoing;
        Some(reverse)
    });
    let Some(reverse) = reverse else { return false };
    changed();
    if !reverse() {
        set_status(
            id,
            Status::Failed("FileID couldn't start putting this back.".to_owned()),
        );
        return false;
    }
    true
}

pub fn undo_latest() -> bool {
    most_recent_undoable().is_some_and(undo)
}

/// Records a change only when something actually changed and there is a way
/// to put it back; a result with no successes, or an engine that returned no
/// undo handle, leaves the list alone.
pub fn record(changed: u64, label: impl FnOnce() -> String, reverse: Option<Reverse>) -> Option<u64> {
    if changed == 0 {
        return None;
    }
    reverse.map(|reverse| push(&label(), Some(reverse)))
}

/// Settles an entry once the engine answers its undo: `failed` things that
/// could not be put back leave it retryable.
pub fn settle(entry: Option<u64>, failed: u64, what: &str) {
    let Some(id) = entry else { return };
    set_status(
        id,
        if failed == 0 {
            Status::Undone
        } else {
            Status::Failed(format!("{} couldn't be put back", counted(failed, what)))
        },
    );
}

/// Retires an entry that can no longer be put back (a newer change replaced
/// the only record that could undo it). A change already undone stays as is.
pub fn supersede(entry: Option<u64>, reason: &str) {
    let Some(id) = entry else { return };
    let open = LOG.with_borrow(|log| {
        log.entries
            .iter()
            .any(|e| e.id == id && matches!(e.status, Status::Undoable | Status::Failed(_)))
    });
    if open {
        set_status(id, Status::NotUndoable(reason.to_owned()));
    }
}

/// Settles an entry whose undo never got an answer (the helper restarted).
pub fn settle_failed(entry: Option<u64>, reason: &str) {
    if let Some(id) = entry {
        set_status(id, Status::Failed(reason.to_owned()));
    }
}

fn counted(count: u64, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

pub fn count_label(verb: &str, count: u64, noun: &str) -> String {
    format!("{verb} {}", counted(count, noun))
}

pub fn status_text(status: &Status) -> String {
    match status {
        Status::Undoable => String::new(),
        Status::Undoing => "Putting back…".to_owned(),
        Status::Undone => "Put back".to_owned(),
        Status::Failed(reason) | Status::NotUndoable(reason) => reason.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn reset() {
        LOG.with_borrow_mut(|log| *log = Log::default());
    }

    #[test]
    fn an_unsubscribed_listener_is_no_longer_called() {
        reset();
        let calls = Rc::new(Cell::new(0));
        let id = subscribe(Rc::new({
            let calls = calls.clone();
            move || calls.set(calls.get() + 1)
        }));
        push("one", None);
        unsubscribe(id);
        push("two", None);
        assert_eq!(calls.get(), 1);
        assert!(LOG.with_borrow(|log| log.listeners.is_empty()));
    }

    #[test]
    fn newest_undoable_is_undone_first_and_stays_listed() {
        reset();
        let calls = Rc::new(Cell::new(0));
        let make = |calls: &Rc<Cell<i32>>| -> Reverse {
            let calls = calls.clone();
            Rc::new(move || {
                calls.set(calls.get() + 1);
                true
            })
        };
        let first = push("first", Some(make(&calls)));
        let second = push("second", Some(make(&calls)));
        assert_eq!(pending_count(), 2);
        assert_eq!(most_recent_undoable(), Some(second));
        assert!(undo_latest());
        assert_eq!(calls.get(), 1);
        assert_eq!(most_recent_undoable(), Some(first));
        set_status(second, Status::Undone);
        assert_eq!(pending_count(), 1);
        assert_eq!(count(), 2);
        assert!(!undo(second));
        assert_eq!(entries()[0].label, "second");
    }

    #[test]
    fn a_reverse_that_cannot_start_marks_the_entry_failed() {
        reset();
        let id = push("move", Some(Rc::new(|| false)));
        assert!(!undo(id));
        assert!(matches!(entries()[0].status, Status::Failed(_)));
        assert!(!undo(id));
        assert_eq!(most_recent_undoable(), None);
    }

    #[test]
    fn history_is_capped_and_entries_without_a_reverse_are_not_undoable() {
        reset();
        for n in 0..CAPACITY + 5 {
            push(&format!("change {n}"), None);
        }
        assert_eq!(count(), CAPACITY);
        assert_eq!(pending_count(), 0);
        assert!(!undo_latest());
    }

    #[test]
    fn only_a_real_change_with_an_undo_is_recorded() {
        reset();
        let undo = || -> Option<Reverse> { Some(Rc::new(|| true)) };
        assert_eq!(record(0, || "none".to_owned(), undo()), None);
        assert_eq!(record(3, || "no undo".to_owned(), None), None);
        assert_eq!(count(), 0);
        let id = record(2, || count_label("Renamed", 2, "file"), undo()).unwrap();
        assert_eq!(entries()[0].label, "Renamed 2 files");
        assert_eq!(most_recent_undoable(), Some(id));
    }

    #[test]
    fn labels_count_in_the_singular_and_the_plural() {
        assert_eq!(count_label("Moved", 1, "file"), "Moved 1 file");
        assert_eq!(count_label("Moved", 12, "file"), "Moved 12 files");
    }

    #[test]
    fn an_undo_settles_its_entry_and_a_partial_one_can_be_retried() {
        reset();
        let calls = Rc::new(std::cell::Cell::new(0));
        let counter = calls.clone();
        let id = push("merge", Some(Rc::new(move || {
            counter.set(counter.get() + 1);
            true
        })));
        assert!(undo(id));
        settle(Some(id), 2, "face");
        assert_eq!(entries()[0].status, Status::Failed("2 faces couldn't be put back".to_owned()));
        assert!(undo(id));
        assert_eq!(calls.get(), 2);
        settle(Some(id), 0, "face");
        assert_eq!(entries()[0].status, Status::Undone);
        assert!(!undo(id));
        settle(None, 0, "face");
        settle_failed(None, "gone");
    }

    #[test]
    fn a_newer_change_retires_the_older_one_it_replaced_but_not_an_undone_one() {
        reset();
        let old = push("old", Some(Rc::new(|| true)));
        let done = push("done", Some(Rc::new(|| true)));
        set_status(done, Status::Undone);
        supersede(Some(old), "A newer rename replaced it");
        supersede(Some(done), "A newer rename replaced it");
        supersede(None, "nothing");
        let listed = entries();
        assert_eq!(listed[1].status, Status::NotUndoable("A newer rename replaced it".to_owned()));
        assert_eq!(listed[0].status, Status::Undone);
        assert_eq!(most_recent_undoable(), None);
        assert!(!undo(old));
    }

    #[test]
    fn listeners_hear_every_change() {
        reset();
        let heard = Rc::new(Cell::new(0));
        let counter = heard.clone();
        subscribe(Rc::new(move || counter.set(counter.get() + 1)));
        let id = push("x", None);
        set_status(id, Status::Undone);
        assert_eq!(heard.get(), 2);
    }
}
