use anyhow::{bail, Result};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

/// How `reserve_internal` treats a request it can't admit right now.
#[derive(Clone, Copy)]
enum Wait {
    /// Fail at once while an interactive lease is active or memory is tight.
    Immediate,
    /// Wait behind interactive leases; still fail when nothing can release.
    Queued,
    /// Wait through every transient condition until the deadline.
    Until(Instant),
}

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
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub memory_mb: u64,
    pub cpu: usize,
    pub io: usize,
    pub gpu_mb: u64,
    pub npu_mb: u64,
    pub active: usize,
    pub waiting: usize,
}
struct State {
    next: u64,
    active: BTreeMap<u64, (Demand, Priority)>,
    accelerators: BTreeMap<u64, AcceleratorDemand>,
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
    #[cfg(test)]
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock();
        let used = state.active.values().fold(Demand::default(), |sum, (d, _)| Demand {
            memory_mb: sum.memory_mb.saturating_add(d.memory_mb),
            cpu: sum.cpu.saturating_add(d.cpu),
            io: sum.io.saturating_add(d.io),
        });
        Snapshot {
            memory_mb: used.memory_mb, cpu: used.cpu, io: used.io,
            gpu_mb: state.accelerators.values().fold(0u64, |sum, d| sum.saturating_add(d.gpu_mb)),
            npu_mb: state.accelerators.values().fold(0u64, |sum, d| sum.saturating_add(d.npu_mb)),
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
        self.reserve_internal(demand, accelerators, priority, cancelled, Wait::Immediate)
    }
    /// Admission for work that must not fail just because the machine is busy
    /// (a scan file): waits, cancellably and for at most `patience`, for
    /// interactive leases, residents and low free memory to clear. Fails at
    /// once only when the demand could never fit on this machine.
    pub fn reserve_patient(
        self: &Arc<Self>, demand: Demand, priority: Priority,
        cancelled: impl Fn() -> bool, patience: Duration,
    ) -> Result<Lease> {
        self.reserve_internal(demand, AcceleratorDemand::default(), priority, cancelled, Wait::Until(Instant::now() + patience))
    }
    pub fn reserve_queued(
        self: &Arc<Self>, demand: Demand, accelerators: AcceleratorDemand,
        priority: Priority, cancelled: impl Fn() -> bool,
    ) -> Result<Lease> {
        self.reserve_internal(demand, accelerators, priority, cancelled, Wait::Queued)
    }
    fn reserve_internal(
        self: &Arc<Self>, demand: Demand, accelerators: AcceleratorDemand,
        priority: Priority, cancelled: impl Fn() -> bool, wait: Wait,
    ) -> Result<Lease> {
        let wait_background = !matches!(wait, Wait::Immediate);
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
                // Free memory comes and goes; only a demand larger than the
                // whole budget is permanent for a patient caller.
                let reserve = (total / 4).clamp(2048, 8192);
                let never_fits = total == 0 || total <= reserve || demand.memory_mb > total - reserve;
                if !matches!(wait, Wait::Until(_)) || never_fits {
                    bail!("{reason}")
                }
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
            let used_gpu = state.accelerators.values().fold(0u64, |sum, d| sum.saturating_add(d.gpu_mb));
            let used_npu = state.accelerators.values().fold(0u64, |sum, d| sum.saturating_add(d.npu_mb));
            let accelerator_ok = (accelerators.gpu_mb == 0 || self.accelerator_capacity.gpu_mb.is_some_and(|cap| used_gpu.saturating_add(accelerators.gpu_mb) <= cap))
                && (accelerators.npu_mb == 0 || self.accelerator_capacity.npu_mb.is_some_and(|cap| used_npu.saturating_add(accelerators.npu_mb) <= cap));
            if first == Some(id)
                && accelerator_ok
                && memory_ok
                && used.cpu + demand.cpu <= self.cpu
                && used.io + demand.io <= self.io
            {
                state.waiting.remove(&id);
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
            let stuck_on_residents = (!accelerator_ok && state.active.values().all(|(d, _)| d.cpu == 0 && d.io == 0))
                || resident_blocks_budget
                || (!memory_ok && state.active.values().all(|(d, _)| d.cpu == 0 && d.io == 0));
            let busy = state.active.is_empty()
                || (!wait_background && priority == Priority::Background
                    && state
                        .active
                        .values()
                        .any(|(_, p)| *p == Priority::Interactive));
            if stuck_on_residents || busy {
                // Residents retire when idle and free memory recovers, so a
                // patient caller keeps polling until its deadline.
                let reason = match wait {
                    Wait::Until(deadline) if Instant::now() < deadline => None,
                    Wait::Until(_) => Some("Resource admission timed out: the machine stayed too busy or low on memory"),
                    _ if stuck_on_residents => Some("Resource admission deferred: resident memory cannot release during this operation"),
                    _ => Some("Resource admission deferred: local capacity or available memory is busy"),
                };
                if let Some(reason) = reason {
                    state.waiting.remove(&id);
                    self.changed.notify_all();
                    bail!("{reason}")
                }
            }
            self.changed.wait_for(&mut state, Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
#[path = "resource_prerequisites_tests.rs"]
mod prerequisites_tests;

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
    #[test]
    fn patient_admission_waits_out_interactive_leases_and_low_memory() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let available = Arc::new(AtomicU64::new(12000));
        let reading = available.clone();
        let scheduler = ResourceScheduler::new(move || (16384, reading.load(Ordering::Relaxed)), 2, 2);
        let interactive = scheduler
            .reserve(Demand { memory_mb: 64, cpu: 1, io: 0 }, Priority::Interactive, || false)
            .unwrap();
        let busy = scheduler
            .reserve(Demand { memory_mb: 64, cpu: 1, io: 0 }, Priority::Background, || false)
            .unwrap();
        let demand = Demand { memory_mb: 32, cpu: 1, io: 0 };
        let deferred = scheduler.reserve(demand, Priority::Background, || false).err().expect("the immediate mode defers");
        assert!(deferred.to_string().contains("deferred"), "{deferred}");
        available.store(600, Ordering::Relaxed);
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(busy);
            std::thread::sleep(Duration::from_millis(100));
            available.store(12000, Ordering::Relaxed);
        });
        let started = Instant::now();
        let lease = scheduler
            .reserve_patient(demand, Priority::Background, || false, Duration::from_secs(10))
            .expect("waits for memory instead of failing");
        assert!(started.elapsed() >= Duration::from_millis(250), "admitted only after the CPU and memory freed");
        releaser.join().unwrap();
        drop(lease);
        drop(interactive);
        assert!(scheduler.state.lock().waiting.is_empty());
    }

    #[test]
    fn patient_admission_is_bounded_cancellable_and_refuses_impossible_demands() {
        let scheduler = ResourceScheduler::new(|| (16384, 100), 4, 2);
        let demand = Demand { memory_mb: 32, cpu: 1, io: 0 };
        let error = scheduler
            .reserve_patient(demand, Priority::Background, || false, Duration::from_millis(150))
            .err().expect("timed out");
        assert!(error.to_string().contains("timed out"), "{error}");
        let started = Instant::now();
        assert!(scheduler
            .reserve_patient(demand, Priority::Background, || started.elapsed() > Duration::from_millis(100), Duration::from_secs(30))
            .err().expect("cancelled")
            .to_string()
            .contains("cancelled"));
        let huge = Demand { memory_mb: 16_000, cpu: 1, io: 0 };
        let started = Instant::now();
        assert!(scheduler.reserve_patient(huge, Priority::Background, || false, Duration::from_secs(30)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1), "a demand that never fits fails at once");
        assert!(scheduler.state.lock().waiting.is_empty());
    }

    fn scheduler_waiting(scheduler: &ResourceScheduler, count: usize) -> usize {
        usize::from(scheduler.state.lock().waiting.len() >= count)
    }
}
