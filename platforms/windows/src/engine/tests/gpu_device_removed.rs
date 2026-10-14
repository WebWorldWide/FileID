use fileid_engine::coordinator::{process_gpu_device_removed, ScanCoordinator};
use fileid_engine::models::runtime::ensure_gpu_inference_alive;

// Lives in its own test binary: the latch is process-global and would leak
// into every other test sharing the process.
#[test]
fn device_removal_outlives_the_scan_that_saw_it() {
    assert!(!process_gpu_device_removed());
    assert!(ensure_gpu_inference_alive().is_ok());

    let scan = ScanCoordinator::new();
    assert!(scan.mark_gpu_dead());
    assert!(!scan.mark_gpu_dead());

    assert!(process_gpu_device_removed());
    assert!(ensure_gpu_inference_alive().is_err());
    assert!(ScanCoordinator::new().is_gpu_dead());
}
