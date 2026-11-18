//! Byte-identical images share one analysis: the first copy in a scan is decoded and analyzed, and others wait for it or copy its rows.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

pub type ContentHash = [u8; 32];

/// Rows whose analysis a byte-identical copy may take over: an image that finished its scan with at least one tag or embedding. The same rule seeds the registry at scan start and picks the twin row in the writer.
pub const COMPLETE_IMAGE_SQL: &str = "kind = 'image' AND failed = 0 AND content_hash IS NOT NULL \
     AND length(content_hash) = 32 \
     AND (EXISTS (SELECT 1 FROM tags t WHERE t.file_id = files.id AND t.source = 'auto') \
          OR EXISTS (SELECT 1 FROM clip_embeddings c WHERE c.file_id = files.id))";

/// Hashes of every image row a copy may take over. When the scan can embed (`needs_vector`), a row without a vector is not complete (scanned before CLIP was installed or lost its vector); treating it as done would skip the decode that gives it one, on every later scan.
pub fn known_image_hashes(conn: &rusqlite::Connection, needs_vector: bool) -> rusqlite::Result<Vec<ContentHash>> {
    let vector = if needs_vector { " AND EXISTS (SELECT 1 FROM clip_embeddings c WHERE c.file_id = files.id)" } else { "" };
    let mut stmt = conn.prepare(&format!("SELECT content_hash FROM files WHERE {COMPLETE_IMAGE_SQL}{vector}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
    Ok(rows
        .filter_map(|row| row.ok().and_then(|bytes| <ContentHash>::try_from(bytes.as_slice()).ok()))
        .collect())
}

enum State {
    InFlight(Arc<Notify>),
    Done(bool),
}

pub enum TwinClaim {
    None,
    Owner,
    Ready,
    Pending(Arc<Notify>),
}

/// `T` is what a decoder parks for a copy until its owner is queued (see [`TwinRegistry::hold`]).
pub struct TwinRegistry<T = ()> {
    map: Mutex<HashMap<ContentHash, State>>,
    /// Copies held back while their owner is still decoding; an entry exists only between the owner's claim and its hand-over to the workers.
    held: Mutex<HashMap<ContentHash, Vec<T>>>,
}

impl<T> Default for TwinRegistry<T> {
    fn default() -> Self {
        Self { map: Mutex::new(HashMap::new()), held: Mutex::new(HashMap::new()) }
    }
}

impl<T> TwinRegistry<T> {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// [`claim`](Self::claim) for a stage that queues results for workers (the decoders). A copy only hashes, so it would reach the workers ahead of its still-decoding owner, and with every worker waiting on such copies nothing would pick the owner up. An owner claimed here therefore collects its copies ([`hold`](Self::hold)) until it is queued ([`owner_queued`](Self::owner_queued)).
    pub fn claim_queued(&self, hash: ContentHash) -> TwinClaim {
        let mut held = self.held.lock();
        let claim = self.claim(hash);
        if matches!(claim, TwinClaim::Owner) {
            held.entry(hash).or_default();
        }
        claim
    }

    pub fn hold(&self, hash: &ContentHash, item: T) -> Option<T> {
        match self.held.lock().get_mut(hash) {
            Some(waiting) => {
                waiting.push(item);
                None
            }
            None => Some(item),
        }
    }

    pub fn owner_queued(&self, hash: &ContentHash) -> Vec<T> {
        self.held.lock().remove(hash).unwrap_or_default()
    }

    pub fn seed(&self, hashes: impl IntoIterator<Item = ContentHash>) -> usize {
        let mut map = self.map.lock();
        let before = map.len();
        for hash in hashes {
            map.entry(hash).or_insert(State::Done(true));
        }
        map.len() - before
    }

    pub fn claim(&self, hash: ContentHash) -> TwinClaim {
        let mut map = self.map.lock();
        match map.get(&hash) {
            Some(State::Done(true)) => TwinClaim::Ready,
            Some(State::Done(false)) => {
                map.insert(hash, State::InFlight(Arc::new(Notify::new())));
                TwinClaim::Owner
            }
            Some(State::InFlight(notify)) => TwinClaim::Pending(notify.clone()),
            None => {
                map.insert(hash, State::InFlight(Arc::new(Notify::new())));
                TwinClaim::Owner
            }
        }
    }

    pub fn publish(&self, hash: ContentHash, ok: bool) {
        let previous = self.map.lock().insert(hash, State::Done(ok));
        if let Some(State::InFlight(notify)) = previous {
            notify.notify_waiters();
        }
    }

    /// `notify` is the waiter's handle on its owner. Another owner in flight means that one failed and a later copy took over: the old handle will never fire, so the wait is over.
    pub fn settled(&self, hash: &ContentHash, notify: &Notify) -> Option<bool> {
        match self.map.lock().get(hash) {
            Some(State::Done(ok)) => Some(*ok),
            Some(State::InFlight(current)) if std::ptr::eq(Arc::as_ptr(current), notify) => None,
            Some(State::InFlight(_)) | None => Some(false),
        }
    }

    /// Waits until the owner of `hash` publishes; true when its analysis can be copied. False also when that owner failed and another copy has since claimed the image: the caller claims again.
    pub async fn wait(&self, hash: &ContentHash, notify: &Notify) -> bool {
        loop {
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(ok) = self.settled(hash, notify) {
                return ok;
            }
            notified.await;
        }
    }

    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type TwinRegistry = super::TwinRegistry<&'static str>;

    #[test]
    fn a_new_owner_keeps_the_copies_a_failed_owner_still_holds() {
        let registry = TwinRegistry::new();
        let hash = [7u8; 32];
        assert!(matches!(registry.claim_queued(hash), TwinClaim::Owner));
        assert_eq!(registry.hold(&hash, "copy 1"), None);
        registry.publish(hash, false);
        assert!(matches!(registry.claim_queued(hash), TwinClaim::Owner));
        assert_eq!(registry.owner_queued(&hash), vec!["copy 1"]);
    }

    #[test]
    fn copies_are_held_until_their_owner_is_queued() {
        let registry = TwinRegistry::new();
        let hash = [9u8; 32];
        assert!(matches!(registry.claim_queued(hash), TwinClaim::Owner));
        assert!(matches!(registry.claim_queued(hash), TwinClaim::Pending(_)));
        assert_eq!(registry.hold(&hash, "copy 1"), None);
        assert_eq!(registry.hold(&hash, "copy 2"), None);
        assert_eq!(registry.owner_queued(&hash), vec!["copy 1", "copy 2"]);
        assert_eq!(registry.hold(&hash, "copy 3"), Some("copy 3"), "the owner is with the workers: follow at once");
        assert!(registry.owner_queued(&hash).is_empty());

        let other = [10u8; 32];
        assert!(matches!(registry.claim(other), TwinClaim::Owner));
        assert_eq!(registry.hold(&other, "copy"), Some("copy"));

        registry.publish(hash, false);
        assert!(matches!(registry.claim_queued(hash), TwinClaim::Owner));
        assert_eq!(registry.hold(&hash, "copy 4"), None);
        assert_eq!(registry.owner_queued(&hash), vec!["copy 4"]);
    }

    #[tokio::test]
    async fn a_waiter_of_a_failed_owner_is_released_when_another_copy_takes_over() {
        let registry = TwinRegistry::new();
        let hash = [8u8; 32];
        assert!(matches!(registry.claim(hash), TwinClaim::Owner));
        let TwinClaim::Pending(first_owner) = registry.claim(hash) else { panic!("second claim must wait") };
        registry.publish(hash, false);
        assert!(matches!(registry.claim(hash), TwinClaim::Owner));
        let waited = tokio::time::timeout(std::time::Duration::from_secs(2), registry.wait(&hash, &first_owner)).await;
        assert_eq!(waited, Ok(false), "the old owner's handle is never notified again");
        let TwinClaim::Pending(second_owner) = registry.claim(hash) else { panic!("the new owner is in flight") };
        registry.publish(hash, true);
        assert!(registry.wait(&hash, &second_owner).await);
    }

    #[test]
    fn first_claim_owns_and_later_claims_follow_the_outcome() {
        let registry = TwinRegistry::new();
        let hash = [1u8; 32];
        assert!(matches!(registry.claim(hash), TwinClaim::Owner));
        assert!(matches!(registry.claim(hash), TwinClaim::Pending(_)));
        registry.publish(hash, true);
        assert!(matches!(registry.claim(hash), TwinClaim::Ready));

        let failed = [2u8; 32];
        assert!(matches!(registry.claim(failed), TwinClaim::Owner));
        registry.publish(failed, false);
        assert!(matches!(registry.claim(failed), TwinClaim::Owner), "a failed twin is retried by the next copy");
    }

    #[test]
    fn seeded_hashes_are_ready_without_an_owner() {
        let registry = TwinRegistry::new();
        assert_eq!(registry.seed([[3u8; 32], [3u8; 32], [4u8; 32]]), 2);
        assert!(matches!(registry.claim([3u8; 32]), TwinClaim::Ready));
        assert!(matches!(registry.claim([5u8; 32]), TwinClaim::Owner));
    }

    #[tokio::test]
    async fn waiters_wake_on_publish_even_when_it_lands_first() {
        let registry = TwinRegistry::new();
        let hash = [6u8; 32];
        assert!(matches!(registry.claim(hash), TwinClaim::Owner));
        let TwinClaim::Pending(notify) = registry.claim(hash) else { panic!("second claim must wait") };

        let waiter = {
            let registry = registry.clone();
            let notify = notify.clone();
            tokio::spawn(async move { registry.wait(&hash, &notify).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        registry.publish(hash, true);
        assert!(waiter.await.unwrap());

        let late = [7u8; 32];
        assert!(matches!(registry.claim(late), TwinClaim::Owner));
        let TwinClaim::Pending(late_notify) = registry.claim(late) else { panic!() };
        registry.publish(late, false);
        assert!(!registry.wait(&late, &late_notify).await, "a publish before the wait is still observed");
    }
}
