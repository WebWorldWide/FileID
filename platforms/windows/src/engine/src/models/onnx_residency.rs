use crate::resource_scheduler::{AcceleratorDemand, Demand, Lease, Priority, ResourceScheduler};
use anyhow::Result;
use std::path::Path;

pub(crate) fn reserve(path: &Path, cpu_only: bool) -> Result<Lease> {
    let mut bytes = std::fs::metadata(path)?.len();
    for external in [path.with_extension("onnx.data"), path.with_file_name("model.onnx_data")] {
        if external != path && external.is_file() { bytes = bytes.saturating_add(std::fs::metadata(external)?.len()); }
    }
    let memory_mb = bytes.div_ceil(1_048_576).saturating_mul(2).saturating_add(64);
    let gpu_mb = if !cpu_only && !super::runtime::user_forced_cpu() && super::runtime::verified_gpu_capacity_mb().is_some() {
        memory_mb
    } else { 0 };
    ResourceScheduler::shared().reserve_with_accelerators(
        Demand { memory_mb, cpu: 0, io: 0 }, AcceleratorDemand { gpu_mb, npu_mb: 0 },
        Priority::Background, || crate::coordinator::process_gpu_device_removed(),
    )
}
