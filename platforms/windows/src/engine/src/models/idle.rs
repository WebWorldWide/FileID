//! Process-wide model slots that free their contents after a quiet period and
//! load again on the next use, like the Mac parks Deep Analyze models.

use anyhow::Result;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Matches the Mac's five-minute park for Deep Analyze models.
pub const PARK_AFTER: Duration = Duration::from_secs(300);
const SWEEP_EVERY: Duration = Duration::from_secs(30);

pub trait Park: Sync {
    fn park_idle(&self, now: Instant) -> bool;
}

static SLOTS: Mutex<Vec<&'static dyn Park>> = Mutex::new(Vec::new());
static SWEEPER: AtomicBool = AtomicBool::new(false);

pub struct IdleSlot<T> {
    name: &'static str,
    idle: Duration,
    registered: AtomicBool,
    state: Mutex<Option<(T, Instant)>>,
}

impl<T: Send> IdleSlot<T> {
    pub const fn new(name: &'static str, idle: Duration) -> Self {
        Self { name, idle, registered: AtomicBool::new(false), state: Mutex::new(None) }
    }

    /// Runs `work` on the held value, loading it first when parked. A failed
    /// load leaves the slot empty so the next use retries.
    pub fn with<R>(&'static self, load: impl FnOnce() -> Result<T>, work: impl FnOnce(&mut T) -> R) -> Result<R> {
        self.register();
        let mut state = self.state.lock();
        if state.is_none() {
            *state = Some((load()?, Instant::now()));
            tracing::debug!(model = self.name, "model loaded");
        }
        let (value, _) = state.as_mut().expect("loaded above");
        let result = work(value);
        if let Some((_, used)) = state.as_mut() {
            *used = Instant::now();
        }
        Ok(result)
    }

    pub fn is_loaded(&self) -> bool {
        self.state.lock().is_some()
    }

    fn register(&'static self) {
        if self.registered.swap(true, Ordering::AcqRel) {
            return;
        }
        SLOTS.lock().push(self);
        if !SWEEPER.swap(true, Ordering::AcqRel) {
            let started = std::thread::Builder::new().name("fileid-model-park".into()).spawn(|| loop {
                std::thread::sleep(SWEEP_EVERY);
                sweep(Instant::now());
            });
            if let Err(error) = started {
                SWEEPER.store(false, Ordering::Release);
                tracing::warn!(?error, "model park sweeper did not start; idle models stay loaded");
            }
        }
    }
}

impl<T: Send> Park for IdleSlot<T> {
    /// Never waits: a slot in use is not idle.
    fn park_idle(&self, now: Instant) -> bool {
        let Some(mut state) = self.state.try_lock() else { return false };
        let idle = state.as_ref().is_some_and(|(_, used)| now.saturating_duration_since(*used) >= self.idle);
        let parked = if idle { state.take() } else { None };
        drop(state);
        let freed = parked.is_some();
        drop(parked);
        if freed {
            tracing::info!(model = self.name, "idle model parked");
        }
        freed
    }
}

/// Parks every registered slot idle at `now`; returns how many were freed.
pub fn sweep(now: Instant) -> usize {
    let slots: Vec<&'static dyn Park> = SLOTS.lock().clone();
    slots.into_iter().filter(|slot| slot.park_idle(now)).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Model(&'static AtomicUsize);
    impl Drop for Model {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn parks_after_the_idle_period_and_reloads_on_next_use() {
        static LOADS: AtomicUsize = AtomicUsize::new(0);
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        static SLOT: IdleSlot<Model> = IdleSlot::new("test-model", Duration::from_secs(60));
        let load = || {
            LOADS.fetch_add(1, Ordering::SeqCst);
            Ok(Model(&DROPS))
        };
        assert_eq!(SLOT.with(load, |_| 7).unwrap(), 7);
        SLOT.with(load, |_| ()).unwrap();
        assert_eq!(LOADS.load(Ordering::SeqCst), 1, "a loaded model is reused");
        let start = Instant::now();
        assert!(!SLOT.park_idle(start + Duration::from_secs(59)));
        assert!(SLOT.is_loaded());
        assert!(SLOT.park_idle(start + Duration::from_secs(61)));
        assert!(!SLOT.is_loaded());
        assert_eq!(DROPS.load(Ordering::SeqCst), 1, "parking frees the model");
        assert!(!SLOT.park_idle(start + Duration::from_secs(600)), "an empty slot has nothing to park");
        SLOT.with(load, |_| ()).unwrap();
        assert_eq!(LOADS.load(Ordering::SeqCst), 2, "next use reloads");
        assert!(sweep(Instant::now() + Duration::from_secs(61)) >= 1);
        assert!(!SLOT.is_loaded());
    }

    #[test]
    fn a_model_in_use_is_never_parked_and_a_failed_load_retries() {
        static SLOT: IdleSlot<u32> = IdleSlot::new("test-busy", Duration::ZERO);
        assert!(SLOT.with(|| anyhow::bail!("weights missing"), |_| ()).is_err());
        assert!(!SLOT.is_loaded());
        SLOT.with(
            || Ok(1),
            |_| assert!(!SLOT.park_idle(Instant::now() + Duration::from_secs(3600))),
        )
        .unwrap();
        assert!(SLOT.park_idle(Instant::now()));
    }
}
