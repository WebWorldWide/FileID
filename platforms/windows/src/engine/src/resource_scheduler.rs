use anyhow::{bail, Result};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Demand {
    pub memory_mb: u64,
    pub cpu: usize,
    pub io: usize,
}
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Priority {
    Background,
    Interactive,
}
struct State {
    next: u64,
    active: BTreeMap<u64, (Demand, Priority)>,
    waiting: BTreeMap<u64, (Demand, Priority)>,
}
pub struct ResourceScheduler {
    state: Mutex<State>,
    changed: Condvar,
    memory: Box<dyn Fn() -> (u64, u64) + Send + Sync>,
    cpu: usize,
    io: usize,
}
pub struct Lease {
    scheduler: Arc<ResourceScheduler>,
    id: u64,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.scheduler.state.lock().active.remove(&self.id);
        self.scheduler.changed.notify_all();
    }
}
impl Lease {
    pub fn retain_memory(&self) {
        if let Some((demand, _)) = self.scheduler.state.lock().active.get_mut(&self.id) {
            demand.cpu = 0;
            demand.io = 0;
        }
        self.scheduler.changed.notify_all();
    }
}
impl ResourceScheduler {
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<ResourceScheduler>> = OnceLock::new();
        SHARED
            .get_or_init(|| {
                Self::new(
                    || {
                        (
                            (crate::platform::physical_memory_gb() * 1024.0) as u64,
                            crate::platform::available_memory_mb(),
                        )
                    },
                    num_cpus::get().saturating_sub(2).max(1),
                    2,
                )
            })
            .clone()
    }
    pub fn new(
        memory: impl Fn() -> (u64, u64) + Send + Sync + 'static,
        cpu: usize,
        io: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                next: 0,
                active: BTreeMap::new(),
                waiting: BTreeMap::new(),
            }),
            changed: Condvar::new(),
            memory: Box::new(memory),
            cpu: cpu.max(1),
            io: io.max(1),
        })
    }
    pub fn reserve(
        self: &Arc<Self>,
        demand: Demand,
        priority: Priority,
        cancelled: impl Fn() -> bool,
    ) -> Result<Lease> {
        let (total, available) = (self.memory)();
        if demand.memory_mb == 0 && demand.cpu == 0 && demand.io == 0 {
            bail!("Invalid resource estimate")
        }
        if demand.cpu > self.cpu || demand.io > self.io {
            bail!("Resource request exceeds local capacity")
        }
        if demand.memory_mb > 0 {
            if let Some(reason) =
                crate::util::model_memory::rejection(total, available, demand.memory_mb)
            {
                bail!("{reason}")
            }
        }
        let mut state = self.state.lock();
        state.next += 1;
        let id = state.next;
        state.waiting.insert(id, (demand, priority));
        loop {
            if cancelled() {
                state.waiting.remove(&id);
                self.changed.notify_all();
                bail!("Resource admission cancelled")
            }
            let first = state
                .waiting
                .iter()
                .max_by_key(|(id, (_, priority))| (*priority, std::cmp::Reverse(**id)))
                .map(|(id, _)| *id);
            let used = state
                .active
                .values()
                .fold(Demand::default(), |sum, (d, _)| Demand {
                    memory_mb: sum.memory_mb.saturating_add(d.memory_mb),
                    cpu: sum.cpu.saturating_add(d.cpu),
                    io: sum.io.saturating_add(d.io),
                });
            let (total, available) = (self.memory)();
            let memory_ok = demand.memory_mb == 0
                || (crate::util::model_memory::rejection(total, available, demand.memory_mb)
                    .is_none()
                    && demand.memory_mb.saturating_add(used.memory_mb)
                        <= total.saturating_sub((total / 4).clamp(2048, 8192)));
            if first == Some(id)
                && memory_ok
                && used.cpu + demand.cpu <= self.cpu
                && used.io + demand.io <= self.io
            {
                state.waiting.remove(&id);
                state.active.insert(id, (demand, priority));
                return Ok(Lease {
                    scheduler: self.clone(),
                    id,
                });
            }
            let resident_memory = state
                .active
                .values()
                .filter(|(d, _)| d.cpu == 0 && d.io == 0)
                .fold(0u64, |sum, (d, _)| sum.saturating_add(d.memory_mb));
            let resident_blocks_budget = demand.memory_mb > 0
                && demand.memory_mb.saturating_add(resident_memory)
                    > total.saturating_sub((total / 4).clamp(2048, 8192));
            if resident_blocks_budget
                || (!memory_ok && state.active.values().all(|(d, _)| d.cpu == 0 && d.io == 0))
            {
                state.waiting.remove(&id);
                self.changed.notify_all();
                bail!("Resource admission deferred: resident memory cannot release during this operation")
            }
            if state.active.is_empty()
                || (priority == Priority::Background
                    && state
                        .active
                        .values()
                        .any(|(_, p)| *p == Priority::Interactive))
            {
                state.waiting.remove(&id);
                self.changed.notify_all();
                bail!("Resource admission deferred: local capacity or available memory is busy")
            }
            self.changed.wait_for(&mut state, Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resident_budget_conflict_is_deferred_instead_of_waiting_on_itself() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let scheduler = ResourceScheduler::new(|| (16384, 12000), 1, 1);
        let resident = scheduler
            .reserve(
                Demand {
                    memory_mb: 7000,
                    cpu: 1,
                    io: 1,
                },
                Priority::Background,
                || false,
            )
            .unwrap();
        resident.retain_memory();
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (send, recv) = std::sync::mpsc::channel();
            let scheduler = &scheduler;
            let cancelled = &cancelled;
            let worker = scope.spawn(move || {
                let result = scheduler.reserve(
                    Demand {
                        memory_mb: 7000,
                        cpu: 1,
                        io: 1,
                    },
                    Priority::Background,
                    || cancelled.load(Ordering::Relaxed),
                );
                send.send(result.map(|_| ()).map_err(|error| error.to_string()))
                    .unwrap();
            });
            let result = recv.recv_timeout(Duration::from_secs(1));
            cancelled.store(true, Ordering::Relaxed);
            worker.join().unwrap();
            assert!(result.unwrap().unwrap_err().contains("resident memory"));
        });
        assert!(scheduler.state.lock().waiting.is_empty());
        drop(resident);
    }
    #[test]
    fn cancelled_pending_admission_does_not_leak_or_block_following_work() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let scheduler = ResourceScheduler::new(|| (16384, 12000), 1, 1);
        let demand = Demand {
            memory_mb: 0,
            cpu: 1,
            io: 1,
        };
        let lease = scheduler
            .reserve(demand, Priority::Background, || false)
            .unwrap();
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let pending = scope.spawn(|| {
                scheduler.reserve(demand, Priority::Interactive, || {
                    cancelled.load(Ordering::Relaxed)
                })
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while scheduler.state.lock().waiting.is_empty() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            cancelled.store(true, Ordering::Relaxed);
            assert!(pending.join().unwrap().is_err());
        });
        assert!(scheduler.state.lock().waiting.is_empty());
        drop(lease);
        let _next = scheduler
            .reserve(demand, Priority::Interactive, || false)
            .unwrap();
    }
    #[test]
    fn aggregate_budgets_cancellation_and_drop_release() {
        let scheduler = ResourceScheduler::new(|| (16384, 12000), 1, 1);
        let demand = Demand {
            memory_mb: 1024,
            cpu: 1,
            io: 1,
        };
        let lease = scheduler
            .reserve(demand, Priority::Background, || false)
            .unwrap();
        assert!(scheduler
            .reserve(demand, Priority::Interactive, || true)
            .is_err());
        lease.retain_memory();
        let second = scheduler
            .reserve(demand, Priority::Interactive, || false)
            .unwrap();
        assert!(scheduler
            .reserve(demand, Priority::Background, || false)
            .is_err());
        assert_eq!(scheduler.state.lock().active.len(), 2);
        drop(second);
        drop(lease);
        assert!(scheduler.state.lock().active.is_empty());
        assert!(scheduler.state.lock().waiting.is_empty());
        assert!(scheduler
            .reserve(
                Demand {
                    memory_mb: 16000,
                    cpu: 0,
                    io: 0
                },
                Priority::Background,
                || false
            )
            .is_err());
    }
    #[test]
    fn interactive_waiters_precede_background_and_cancel_cleanly() {
        let scheduler = ResourceScheduler::new(|| (16384, 12000), 1, 1);
        let demand = Demand {
            memory_mb: 0,
            cpu: 1,
            io: 0,
        };
        let lease = scheduler
            .reserve(demand, Priority::Background, || false)
            .unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            for (id, priority) in [(1, Priority::Background), (2, Priority::Interactive)] {
                let worker_scheduler = scheduler.clone();
                let order = order.clone();
                scope.spawn(move || {
                    if let Ok(_lease) = worker_scheduler.reserve(demand, priority, || false) {
                        order.lock().push(id);
                    }
                });
                while scheduler_waiting(&scheduler, id as usize) == 0 {
                    std::thread::yield_now();
                }
            }
            drop(lease);
        });
        assert_eq!(order.lock().first(), Some(&2));
        assert!(scheduler.state.lock().waiting.is_empty());
    }
    fn scheduler_waiting(scheduler: &ResourceScheduler, count: usize) -> usize {
        usize::from(scheduler.state.lock().waiting.len() >= count)
    }
}
