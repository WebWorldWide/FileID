// Session change log: what this session changed and what can still be put
// back (Windows `ChangeLog` / `UndoStack`). Session-only; nothing is written
// to disk. GTK main thread only.

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

/// Starts putting the change back; false when it could not be started.
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
    listeners: Vec<Rc<dyn Fn()>>,
}

thread_local! {
    static LOG: RefCell<Log> = RefCell::new(Log::default());
}

fn changed() {
    let listeners = LOG.with_borrow(|log| log.listeners.clone());
    for listener in listeners {
        listener();
    }
}

pub fn subscribe(listener: Rc<dyn Fn()>) {
    LOG.with_borrow_mut(|log| log.listeners.push(listener));
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
