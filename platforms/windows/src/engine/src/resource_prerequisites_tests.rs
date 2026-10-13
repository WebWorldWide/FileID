use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[test]
fn prerequisites_unknown_accelerators_are_not_invented_or_registered() {
    let s = ResourceScheduler::new(|| (16384, 12000), 1, 1);
    let demand = Demand { memory_mb: 64, cpu: 1, io: 1 };
    for accelerator in [AcceleratorDemand { gpu_mb: 1, npu_mb: 0 }, AcceleratorDemand { gpu_mb: 0, npu_mb: 1 }] {
        assert!(s.reserve_with_accelerators(demand, accelerator, Priority::Interactive, || false).is_err());
    }
    let snap = s.snapshot();
    assert_eq!((snap.memory_mb, snap.gpu_mb, snap.npu_mb, snap.active, snap.waiting), (0, 0, 0, 0, 0));
}

#[test]
fn prerequisites_accelerator_charge_survives_cpu_io_retirement_until_lease_drop() {
    let s = ResourceScheduler::new_with_accelerators(|| (16384, 12000), 1, 1, AcceleratorCapacity { gpu_mb: Some(128), npu_mb: Some(64) });
    let resident = s.reserve_with_accelerators(Demand { memory_mb: 256, cpu: 1, io: 1 }, AcceleratorDemand { gpu_mb: 80, npu_mb: 40 }, Priority::Interactive, || false).unwrap();
    resident.retain_memory();
    let snap = s.snapshot();
    assert_eq!((snap.memory_mb, snap.cpu, snap.io, snap.gpu_mb, snap.npu_mb, snap.active), (256, 0, 0, 80, 40, 1));
    assert!(s.reserve_with_accelerators(Demand { memory_mb: 64, cpu: 1, io: 1 }, AcceleratorDemand { gpu_mb: 60, npu_mb: 0 }, Priority::Interactive, || false).is_err());
    assert_eq!((s.snapshot().active, s.snapshot().waiting), (1, 0));
    let work = s.reserve(Demand { memory_mb: 64, cpu: 1, io: 1 }, Priority::Interactive, || false).unwrap();
    assert_eq!((s.snapshot().memory_mb, s.snapshot().gpu_mb, s.snapshot().active), (320, 80, 2));
    drop(work);
    drop(resident);
    let snap = s.snapshot();
    assert_eq!((snap.memory_mb, snap.cpu, snap.io, snap.gpu_mb, snap.npu_mb, snap.active, snap.waiting), (0, 0, 0, 0, 0, 0, 0));
}

#[test]
fn prerequisites_queued_background_is_opt_in_legacy_background_still_defers() {
    let s = ResourceScheduler::new(|| (16384, 12000), 1, 1);
    let demand = Demand { memory_mb: 0, cpu: 1, io: 1 };
    let holder = s.reserve(demand, Priority::Interactive, || false).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| s.reserve_queued(demand, AcceleratorDemand::default(), Priority::Background, || false));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while s.snapshot().waiting != 1 { assert!(std::time::Instant::now() < deadline); std::thread::yield_now(); }
        assert!(s.reserve(demand, Priority::Background, || false).is_err());
        assert_eq!((s.snapshot().active, s.snapshot().waiting), (1, 1));
        drop(holder);
        let lease = worker.join().unwrap().unwrap();
        assert_eq!((s.snapshot().active, s.snapshot().waiting), (1, 0));
        drop(lease);
    });
    assert_eq!(s.snapshot().active, 0);
}

#[test]
fn prerequisites_cancelled_accelerator_waiter_keeps_only_existing_owner() {
    let s = ResourceScheduler::new_with_accelerators(|| (16384, 12000), 2, 2, AcceleratorCapacity { gpu_mb: Some(128), npu_mb: None });
    let demand = Demand { memory_mb: 64, cpu: 1, io: 1 };
    let gpu = AcceleratorDemand { gpu_mb: 100, npu_mb: 0 };
    let holder = s.reserve_with_accelerators(demand, gpu, Priority::Interactive, || false).unwrap();
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| s.reserve_queued(demand, gpu, Priority::Interactive, || cancelled.load(Ordering::Relaxed)));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while s.snapshot().waiting != 1 { assert!(std::time::Instant::now() < deadline); std::thread::yield_now(); }
        cancelled.store(true, Ordering::Relaxed);
        assert!(worker.join().unwrap().is_err());
    });
    assert_eq!((s.snapshot().memory_mb, s.snapshot().gpu_mb, s.snapshot().active, s.snapshot().waiting), (64, 100, 1, 0));
    drop(holder);
    assert_eq!(s.snapshot().gpu_mb, 0);
}

#[test]
fn prerequisites_pressure_reports_callback_not_available_accelerator_claims() {
    let available = Arc::new(AtomicU64::new(12000));
    let measure = available.clone();
    let s = ResourceScheduler::new(move || (16384, measure.load(Ordering::Relaxed)), 1, 1);
    assert!(!s.under_pressure());
    available.store(100, Ordering::Relaxed);
    assert!(s.under_pressure());
    assert_eq!((s.snapshot().memory_mb, s.snapshot().gpu_mb, s.snapshot().npu_mb), (0, 0, 0));
}
