//! Byte-identical images share one analysis. The decoder already hashes the
//! file before decoding, so the first copy of a photo is the only one that
//! is decoded and run through faces, CLIP, RAM++ and OCR; later copies in the
//! same scan wait for it, and copies of photos analyzed in an earlier scan
//! skip straight to the writer, which copies the twin's rows. Model work then
//! grows with unique content, not with the number of files.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

pub type ContentHash = [u8; 32];

/// Rows whose analysis a byte-identical copy may take over: an image that
/// finished its scan and kept at least one tag or embedding. The same rule
/// seeds the registry at scan start and picks the twin row in the writer.
pub const COMPLETE_IMAGE_SQL: &str = "kind = 'image' AND failed = 0 AND content_hash IS NOT NULL \
     AND length(content_hash) = 32 \
     AND (EXISTS (SELECT 1 FROM tags t WHERE t.file_id = files.id AND t.source = 'auto') \
          OR EXISTS (SELECT 1 FROM clip_embeddings c WHERE c.file_id = files.id))";

/// Hashes of every image row a copy may take over, for seeding a scan.
pub fn known_image_hashes(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<ContentHash>> {
    let mut stmt = conn.prepare(&format!("SELECT content_hash FROM files WHERE {COMPLETE_IMAGE_SQL}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
    Ok(rows
        .filter_map(|row| row.ok().and_then(|bytes| <ContentHash>::try_from(bytes.as_slice()).ok()))
        .collect())
}

enum State {
    InFlight(Arc<Notify>),
    Done(bool),
}

/// What the decoder learned about a file's content hash before decoding.
pub enum TwinClaim {
    /// Not eligible (no full hash, not an image, cloud placeholder).
    None,
    /// First copy seen this scan: decode and analyze, then publish.
    Owner,
    /// A twin's analysis is already stored: skip decode and inference.
    Ready,
    /// A twin is being analyzed right now: decode, then wait for it.
    Pending(Arc<Notify>),
}

#[derive(Default)]
pub struct TwinRegistry {
    map: Mutex<HashMap<ContentHash, State>>,
}

impl TwinRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Hashes of images already analyzed in an earlier scan.
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

    /// Record the owner's outcome and wake every copy waiting on it.
    pub fn publish(&self, hash: ContentHash, ok: bool) {
        let previous = self.map.lock().insert(hash, State::Done(ok));
        if let Some(State::InFlight(notify)) = previous {
            notify.notify_waiters();
        }
    }

    /// `notify` is the waiter's handle on the owner it is waiting for. Another
    /// owner in flight means that one failed and a later copy took over: the
    /// old handle will never be notified again, so the wait is over.
    fn done(&self, hash: &ContentHash, notify: &Notify) -> Option<bool> {
        match self.map.lock().get(hash) {
            Some(State::Done(ok)) => Some(*ok),
            Some(State::InFlight(current)) if std::ptr::eq(Arc::as_ptr(current), notify) => None,
            Some(State::InFlight(_)) | None => Some(false),
        }
    }

    /// Wait until the owner of `hash` publishes; true when its analysis can be
    /// copied. `notify` is the handle returned by [`TwinClaim::Pending`]. False
    /// also when that owner failed and another copy has since claimed the
    /// image: the caller claims again to wait on the new owner.
    pub async fn wait(&self, hash: &ContentHash, notify: &Notify) -> bool {
        loop {
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(ok) = self.done(hash, notify) {
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

    #[tokio::test]
    async fn a_waiter_of_a_failed_owner_is_released_when_another_copy_takes_over() {
        let registry = TwinRegistry::new();
        let hash = [8u8; 32];
        assert!(matches!(registry.claim(hash), TwinClaim::Owner));
        let TwinClaim::Pending(first_owner) = registry.claim(hash) else { panic!("second claim must wait") };
        // The owner fails and a third copy claims the image before the waiter looks.
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
