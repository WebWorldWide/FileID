// Per-subscriber engine event queues. The pump pushes and returns at once, so
// one slow listener can never stall delivery to the others or back up the
// engine's stdout. Events are shared (`Rc`), and a progress-type event
// replaces an unread one of the same kind; replies and terminal events are
// never dropped or reordered.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

pub trait Coalesce {
    /// False for replies and terminal events, which always keep their place.
    fn is_progress(&self) -> bool;
    /// Whether this progress event makes the unread `older` one redundant.
    fn supersedes(&self, older: &Self) -> bool;
}

#[derive(Debug, PartialEq, Eq)]
pub struct Closed;

struct Shared<T> {
    items: VecDeque<Rc<T>>,
    waker: Option<Waker>,
    sender_gone: bool,
    receiver_gone: bool,
}

pub struct EventSender<T: Coalesce> {
    shared: Rc<RefCell<Shared<T>>>,
}

pub struct EventReceiver<T: Coalesce> {
    shared: Rc<RefCell<Shared<T>>>,
}

pub fn channel<T: Coalesce>() -> (EventSender<T>, EventReceiver<T>) {
    let shared = Rc::new(RefCell::new(Shared {
        items: VecDeque::new(),
        waker: None,
        sender_gone: false,
        receiver_gone: false,
    }));
    (EventSender { shared: shared.clone() }, EventReceiver { shared })
}

impl<T: Coalesce> EventSender<T> {
    /// Never blocks. A coalescable event replaces an unread one with the same
    /// key only when nothing that must keep its place was queued after it.
    pub fn push(&self, event: &Rc<T>) {
        let waker = {
            let mut shared = self.shared.borrow_mut();
            if shared.receiver_gone {
                return;
            }
            let superseded = if event.is_progress() {
                shared
                    .items
                    .iter()
                    .rposition(|queued| !queued.is_progress() || event.supersedes(queued))
                    .filter(|index| shared.items[*index].is_progress())
            } else {
                None
            };
            match superseded {
                Some(index) => {
                    shared.items.remove(index);
                    shared.items.push_back(event.clone());
                }
                None => shared.items.push_back(event.clone()),
            }
            shared.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.shared.borrow().receiver_gone
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.shared.borrow().items.len()
    }
}

impl<T: Coalesce> Drop for EventSender<T> {
    fn drop(&mut self) {
        let waker = {
            let mut shared = self.shared.borrow_mut();
            shared.sender_gone = true;
            shared.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// Ends a receiver's wait from elsewhere (a timeout, a closed dialog).
pub struct Closer<T> {
    shared: Weak<RefCell<Shared<T>>>,
}

impl<T> Closer<T> {
    pub fn close(&self) {
        if let Some(shared) = self.shared.upgrade() {
            close(&shared);
        }
    }
}

/// A one-shot listener's time limit. Dropping it (the answer arrived, the
/// task ended) cancels the timer; a timer that already fired is left alone,
/// because removing a finished GLib source is an error.
pub struct Deadline {
    source: Rc<Cell<Option<glib::SourceId>>>,
    fired: Rc<Cell<bool>>,
}

impl Deadline {
    pub fn fired(&self) -> bool {
        self.fired.get()
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            source.remove();
        }
    }
}

fn close<T>(shared: &RefCell<Shared<T>>) {
    let waker = {
        let mut shared = shared.borrow_mut();
        shared.receiver_gone = true;
        shared.items.clear();
        shared.waker.take()
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}

impl<T: Coalesce> EventReceiver<T> {
    /// The next event. `Err(Closed)` after `close`, or once the client has
    /// shut down and everything already queued has been read.
    pub fn recv(&self) -> Recv<'_, T> {
        Recv { receiver: self }
    }

    /// Stops listening: unread events are discarded and a pending `recv`
    /// ends with `Err(Closed)`.
    pub fn close(&self) {
        close(&self.shared);
    }

    pub fn closer(&self) -> Closer<T> {
        Closer { shared: Rc::downgrade(&self.shared) }
    }

    /// Closes this receiver after `timeout` unless the returned guard is
    /// dropped first, so a listener waiting for one answer cannot wait forever.
    pub fn deadline(&self, timeout: Duration) -> Deadline
    where
        T: 'static,
    {
        let source = Rc::new(Cell::new(None));
        let fired = Rc::new(Cell::new(false));
        let closer = self.closer();
        let id = glib::timeout_add_local_once(timeout, {
            let (source, fired) = (source.clone(), fired.clone());
            move || {
                source.set(None);
                fired.set(true);
                closer.close();
            }
        });
        source.set(Some(id));
        Deadline { source, fired }
    }

    #[cfg(test)]
    pub fn try_recv(&self) -> Option<Rc<T>> {
        self.shared.borrow_mut().items.pop_front()
    }
}

impl<T: Coalesce> Drop for EventReceiver<T> {
    fn drop(&mut self) {
        close(&self.shared);
    }
}

pub struct Recv<'a, T: Coalesce> {
    receiver: &'a EventReceiver<T>,
}

impl<T: Coalesce> Future for Recv<'_, T> {
    type Output = Result<Rc<T>, Closed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut shared = self.receiver.shared.borrow_mut();
        if let Some(event) = shared.items.pop_front() {
            return Poll::Ready(Ok(event));
        }
        if shared.sender_gone || shared.receiver_gone {
            return Poll::Ready(Err(Closed));
        }
        shared.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    #[derive(Debug, PartialEq)]
    enum Ev {
        Progress(u32),
        Download(&'static str, u32),
        Reply(u32),
        Done,
    }

    impl Coalesce for Ev {
        fn is_progress(&self) -> bool {
            matches!(self, Ev::Progress(_) | Ev::Download(..))
        }

        fn supersedes(&self, older: &Self) -> bool {
            match (self, older) {
                (Ev::Progress(_), Ev::Progress(_)) => true,
                (Ev::Download(model, _), Ev::Download(other, _)) => model == other,
                _ => false,
            }
        }
    }

    fn drain(rx: &EventReceiver<Ev>) -> Vec<Rc<Ev>> {
        std::iter::from_fn(|| rx.try_recv()).collect()
    }

    fn values(rx: &EventReceiver<Ev>) -> Vec<String> {
        drain(rx).iter().map(|event| format!("{event:?}")).collect()
    }

    #[test]
    fn unread_progress_is_superseded_by_the_newest() {
        let (tx, rx) = channel();
        for step in 0..10_000 {
            tx.push(&Rc::new(Ev::Progress(step)));
        }
        assert_eq!(tx.len(), 1);
        assert_eq!(values(&rx), ["Progress(9999)"]);
    }

    #[test]
    fn replies_and_terminal_events_are_never_dropped_or_reordered() {
        let (tx, rx) = channel();
        for step in 0..500 {
            tx.push(&Rc::new(Ev::Reply(step)));
            tx.push(&Rc::new(Ev::Progress(step)));
        }
        tx.push(&Rc::new(Ev::Done));
        let seen = drain(&rx);
        let replies: Vec<u32> = seen
            .iter()
            .filter_map(|event| match **event {
                Ev::Reply(step) => Some(step),
                _ => None,
            })
            .collect();
        assert_eq!(replies, (0..500).collect::<Vec<_>>());
        assert_eq!(*seen[seen.len() - 1], Ev::Done);
    }

    #[test]
    fn progress_never_moves_ahead_of_a_terminal_event_queued_after_it() {
        let (tx, rx) = channel();
        tx.push(&Rc::new(Ev::Progress(1)));
        tx.push(&Rc::new(Ev::Done));
        tx.push(&Rc::new(Ev::Progress(2)));
        tx.push(&Rc::new(Ev::Progress(3)));
        assert_eq!(values(&rx), ["Progress(1)", "Done", "Progress(3)"]);
    }

    #[test]
    fn different_keys_coalesce_independently() {
        let (tx, rx) = channel();
        tx.push(&Rc::new(Ev::Download("a", 1)));
        tx.push(&Rc::new(Ev::Download("b", 1)));
        tx.push(&Rc::new(Ev::Progress(1)));
        tx.push(&Rc::new(Ev::Download("a", 2)));
        tx.push(&Rc::new(Ev::Progress(2)));
        assert_eq!(values(&rx), ["Download(\"b\", 1)", "Download(\"a\", 2)", "Progress(2)"]);
    }

    #[test]
    fn one_payload_is_shared_by_every_subscriber() {
        let event = Rc::new(Ev::Reply(7));
        let channels: Vec<_> = (0..30).map(|_| channel()).collect();
        for (tx, _) in &channels {
            tx.push(&event);
        }
        assert_eq!(Rc::strong_count(&event), 31);
        for (_, rx) in &channels {
            assert!(Rc::ptr_eq(&rx.try_recv().unwrap(), &event));
        }
    }

    #[test]
    fn a_dropped_receiver_closes_the_sender_and_frees_its_queue() {
        let (tx, rx) = channel();
        let event = Rc::new(Ev::Reply(1));
        tx.push(&event);
        assert!(!tx.is_closed());
        drop(rx);
        assert!(tx.is_closed());
        tx.push(&event);
        assert_eq!(Rc::strong_count(&event), 1);
    }

    #[test]
    fn closing_from_a_timeout_ends_the_wait_and_drops_unread_events() {
        let (tx, rx) = channel();
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(counter.clone());
        let mut cx = Context::from_waker(&waker);
        let closer = rx.closer();
        let mut recv = rx.recv();
        assert!(Pin::new(&mut recv).poll(&mut cx).is_pending());
        closer.close();
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert_eq!(Pin::new(&mut recv).poll(&mut cx), Poll::Ready(Err(Closed)));
        assert!(tx.is_closed());
        tx.push(&Rc::new(Ev::Reply(1)));
        assert_eq!(tx.len(), 0);
        drop(rx);
        closer.close();
    }

    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_waiting_receiver_is_woken_by_a_push_and_by_shutdown() {
        let (tx, rx) = channel();
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(counter.clone());
        let mut cx = Context::from_waker(&waker);

        let mut recv = rx.recv();
        assert!(Pin::new(&mut recv).poll(&mut cx).is_pending());
        tx.push(&Rc::new(Ev::Reply(1)));
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert!(matches!(Pin::new(&mut recv).poll(&mut cx), Poll::Ready(Ok(event)) if *event == Ev::Reply(1)));

        let mut recv = rx.recv();
        assert!(Pin::new(&mut recv).poll(&mut cx).is_pending());
        tx.push(&Rc::new(Ev::Done));
        drop(tx);
        assert!(matches!(Pin::new(&mut recv).poll(&mut cx), Poll::Ready(Ok(event)) if *event == Ev::Done));
        let mut recv = rx.recv();
        assert_eq!(Pin::new(&mut recv).poll(&mut cx), Poll::Ready(Err(Closed)));
    }
}
