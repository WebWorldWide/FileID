use anyhow::{bail, Result};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
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
#[derive(Clone, Copy, Debug, Default)]
pub struct AcceleratorDemand {
    pub gpu_mb: u64,
    pub npu_mb: u64,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct AcceleratorCapacity {
    pub gpu_mb: Option<u64>,
    pub npu_mb: Option<u64>,
}
#[derive(Clone, Copy, Debug, Default)]
#[allow(dead_code)]
pub struct Snapshot {
    pub memory_mb: u64,
    pub gpu_mb: u64,
    pub npu_mb: u64,
    pub active: usize,
    pub waiting: usize,
}
struct State {
    next: u64,
    active: BTreeMap<u64, (Demand, Priority)>,
    accelerators: BTreeMap<u64, AcceleratorDemand>,
    queued_at: BTreeMap<u64, Instant>,
    waiting: BTreeMap<u64, (Demand, Priority)>,
}
pub struct ResourceScheduler {
    state: Mutex<State>,
    changed: Condvar,
    memory: Box<dyn Fn() -> (u64, u64) + Send + Sync>,
    cpu: usize,
    io: usize,
    accelerator_capacity: AcceleratorCapacity,
}
pub struct Lease {
    scheduler: Arc<ResourceScheduler>,
    id: u64,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.scheduler.state.lock();
        state.active.remove(&self.id);
        state.accelerators.remove(&self.id);
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
                Self::new_with_accelerators(
                    || {
                        (
                            (crate::platform::physical_memory_gb() * 1024.0) as u64,
                            crate::platform::available_memory_mb(),
                        )
                    },
                    num_cpus::get().saturating_sub(2).max(1),
                    2,
                    AcceleratorCapacity { gpu_mb: crate::models::runtime::verified_gpu_capacity_mb(), npu_mb: None },
                )
            })
            .clone()
    }
    pub fn new(
        memory: impl Fn() -> (u64, u64) + Send + Sync + 'static,
        cpu: usize,
        io: usize,
    ) -> Arc<Self> {
        Self::new_with_accelerators(memory, cpu, io, AcceleratorCapacity::default())
    }
    pub fn new_with_accelerators(
        memory: impl Fn() -> (u64, u64) + Send + Sync + 'static,
        cpu: usize,
        io: usize,
        accelerator_capacity: AcceleratorCapacity,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                next: 0,
                active: BTreeMap::new(),
                accelerators: BTreeMap::new(),
                queued_at: BTreeMap::new(),
                waiting: BTreeMap::new(),
            }),
            changed: Condvar::new(),
            memory: Box::new(memory),
            cpu: cpu.max(1),
            io: io.max(1),
            accelerator_capacity,
        })
    }
    pub fn reserve(
        self: &Arc<Self>,
        demand: Demand,
        priority: Priority,
        cancelled: impl Fn() -> bool,
    ) -> Result<Lease> {
        self.reserve_with_accelerators(demand, AcceleratorDemand::default(), priority, cancelled)
    }
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock();
        Snapshot {
            memory_mb: state.active.values().map(|(d, _)| d.memory_mb).sum(),
            gpu_mb: state.accelerators.values().map(|d| d.gpu_mb).sum(),
            npu_mb: state.accelerators.values().map(|d| d.npu_mb).sum(),
            active: state.active.len(), waiting: state.waiting.len(),
        }
    }
    pub fn under_pressure(&self) -> bool {
        let (total, available) = (self.memory)();
        total == 0 || available < (total / 4).clamp(2048, 8192)
    }
    pub fn reserve_with_accelerators(
        self: &Arc<Self>, demand: Demand, accelerators: AcceleratorDemand,
        priority: Priority, cancelled: impl Fn() -> bool,
    ) -> Result<Lease> {
        self.reserve_internal(demand, accelerators, priority, cancelled, false)
    }
    pub fn reserve_queued(
        self: &Arc<Self>, demand: Demand, accelerators: AcceleratorDemand,
        priority: Priority, cancelled: impl Fn() -> bool,
    ) -> Result<Lease> {
        self.reserve_internal(demand, accelerators, priority, cancelled, true)
    }
    fn reserve_internal(
        self: &Arc<Self>, demand: Demand, accelerators: AcceleratorDemand,
        priority: Priority, cancelled: impl Fn() -> bool, wait_background: bool,
    ) -> Result<Lease> {
        for (requested, capacity) in [(accelerators.gpu_mb, self.accelerator_capacity.gpu_mb), (accelerators.npu_mb, self.accelerator_capacity.npu_mb)] {
            if requested > 0 && capacity.is_none_or(|capacity| requested > capacity) {
                bail!("Accelerator capacity unknown or exceeded")
            }
        }
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
        state.queued_at.insert(id, Instant::now());
        loop {
            if cancelled() {
                state.waiting.remove(&id);
                state.queued_at.remove(&id);
                self.changed.notify_all();
                bail!("Resource admission cancelled")
            }
            let first = state
                .waiting
                .iter()
                .max_by_key(|(id, (_, priority))| (
                    if state.queued_at[id].elapsed() >= Duration::from_secs(2) { Priority::Interactive } else { *priority },
                    std::cmp::Reverse(**id)))
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
            let used_gpu: u64 = state.accelerators.values().map(|d| d.gpu_mb).sum();
            let used_npu: u64 = state.accelerators.values().map(|d| d.npu_mb).sum();
            let accelerator_ok = (accelerators.gpu_mb == 0 || self.accelerator_capacity.gpu_mb.is_some_and(|cap| used_gpu.saturating_add(accelerators.gpu_mb) <= cap))
                && (accelerators.npu_mb == 0 || self.accelerator_capacity.npu_mb.is_some_and(|cap| used_npu.saturating_add(accelerators.npu_mb) <= cap));
            if first == Some(id)
                && accelerator_ok
                && memory_ok
                && used.cpu + demand.cpu <= self.cpu
                && used.io + demand.io <= self.io
            {
                state.waiting.remove(&id);
                state.queued_at.remove(&id);
                state.active.insert(id, (demand, priority));
                state.accelerators.insert(id, accelerators);
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
            if (!accelerator_ok && state.active.values().all(|(d, _)| d.cpu == 0 && d.io == 0)) || resident_blocks_budget
                || (!memory_ok && state.active.values().all(|(d, _)| d.cpu == 0 && d.io == 0))
            {
                state.waiting.remove(&id);
                state.queued_at.remove(&id);
                self.changed.notify_all();
                bail!("Resource admission deferred: resident memory cannot release during this operation")
            }
            if state.active.is_empty()
                || (!wait_background && priority == Priority::Background
                    && state
                        .active
                        .values()
                        .any(|(_, p)| *p == Priority::Interactive))
            {
                state.waiting.remove(&id);
                state.queued_at.remove(&id);
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

#[cfg(test)]
mod accelerator_tests {
    use super::*;
    #[test]
    fn unknown_capacity_is_not_invented_and_resident_accelerators_release() {
        let demand = Demand { memory_mb: 100, cpu: 0, io: 0 };
        let unknown = ResourceScheduler::new(|| (16384, 12000), 1, 1);
        assert!(unknown.reserve_with_accelerators(demand, AcceleratorDemand { gpu_mb: 10, npu_mb: 0 }, Priority::Interactive, || false).is_err());
        let known = ResourceScheduler::new_with_accelerators(|| (16384, 12000), 1, 1, AcceleratorCapacity { gpu_mb: Some(1024), npu_mb: None });
        assert!(known.reserve_with_accelerators(demand, AcceleratorDemand { gpu_mb: 0, npu_mb: 1 }, Priority::Interactive, || false).is_err());
        let lease = known.reserve_with_accelerators(demand, AcceleratorDemand { gpu_mb: 700, npu_mb: 0 }, Priority::Background, || false).unwrap();
        lease.retain_memory();
        assert_eq!(known.snapshot().gpu_mb, 700);
        assert!(known.reserve_with_accelerators(demand, AcceleratorDemand { gpu_mb: 700, npu_mb: 0 }, Priority::Interactive, || false).is_err());
        drop(lease);
        assert_eq!(known.snapshot().gpu_mb, 0);
        assert_eq!(known.snapshot().waiting, 0);
    }
    #[test]
    fn aged_background_waiter_precedes_new_interactive_work() {
        let scheduler = ResourceScheduler::new(|| (16384, 12000), 1, 1);
        let demand = Demand { cpu: 1, ..Demand::default() };
        let lease = scheduler.reserve(demand, Priority::Background, || false).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            let s = scheduler.clone(); let order1 = order.clone();
            scope.spawn(move || { let _l = s.reserve(demand, Priority::Background, || false).unwrap(); order1.lock().push(1); });
            while scheduler.snapshot().waiting == 0 { std::thread::yield_now(); }
            { let mut state = scheduler.state.lock(); for time in state.queued_at.values_mut() { *time -= Duration::from_secs(3); } }
            let s = scheduler.clone(); let order2 = order.clone();
            scope.spawn(move || { let _l = s.reserve(demand, Priority::Interactive, || false).unwrap(); order2.lock().push(2); });
            while scheduler.snapshot().waiting < 2 { std::thread::yield_now(); }
            drop(lease);
        });
        assert_eq!(*order.lock(), [1, 2]);
    }
}

#[cfg(test)]
mod queued_tests {
    use super::*;
    #[test]
    fn explicit_queued_background_intent_waits_for_interactive_cpu() {
        let scheduler = ResourceScheduler::new(|| (16384, 12000), 1, 1);
        let demand = Demand { cpu: 1, io: 1, memory_mb: 0 };
        let active = scheduler.reserve(demand, Priority::Interactive, || false).unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| scheduler.reserve_queued(demand, AcceleratorDemand::default(), Priority::Background, || false));
            while scheduler.snapshot().waiting == 0 { std::thread::yield_now(); }
            drop(active);
            assert!(worker.join().unwrap().is_ok());
        });
        assert_eq!(scheduler.snapshot().active, 0);
        assert_eq!(scheduler.snapshot().waiting, 0);
    }
}
