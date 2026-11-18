//! Hardware-detection IPC handlers: `emit_ready` (engine startup handshake)
//! and `verifyCudaPack` (kept in the contract; answers with the same
//! `HardwareInfo` shape as the Ready event).

use crate::ipc::{
    sink::Sink, EngineInfo, EventPayload, HardwareInfo, HardwareReprobed, IpcEvent, Wrap,
};
use crate::models::runtime::{GpuVendor, RuntimeProbe};
use crate::platform;

const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build a fresh `HardwareInfo` snapshot by re-running the detection probe.
/// Shared by `emit_ready` (engine startup) and the `verifyCudaPack` handler
/// so both surfaces see the same authoritative shape.
pub(crate) fn build_hardware_info() -> HardwareInfo {
    let probe = RuntimeProbe::detect();
    let vendor_str = match probe.vendor {
        GpuVendor::Nvidia => "nvidia",
        GpuVendor::Amd => "amd",
        GpuVendor::Intel => "intel",
        GpuVendor::Qualcomm => "qualcomm",
        GpuVendor::Other(_) => "other",
        GpuVendor::None => "none",
    };

    let topo = platform::cpu_topology();
    let mem_tier = platform::memory_tier();
    let ram_avail_mb = platform::available_memory_mb();
    let ram_total_mb = (platform::physical_memory_gb() * 1024.0) as u64;
    let vram_mb = platform::dedicated_vram_mb().unwrap_or(0);
    let (power_src, battery_pct) = platform::power_status();
    HardwareInfo {
        gpu_vendor: vendor_str.into(),
        adapter_name: probe.adapter_name.clone(),
        execution_provider: crate::models::runtime::inference_backend().into(),
        physical_cpu_cores: num_cpus::get_physical().max(1) as u32,
        cuda_pack_present: false,
        openvino_pack_present: false,
        qnn_pack_present: false,
        recommendation: String::new(),
        p_cores: topo.p_cores,
        e_cores: topo.e_cores,
        logical_cpu_cores: topo.logical,
        worker_cap: topo.worker_cap(),
        ram_total_mb,
        ram_available_mb: ram_avail_mb,
        memory_tier: mem_tier.as_str().into(),
        vram_mb,
        npu_present: false,
        power_source: power_src.as_str().into(),
        battery_percent: battery_pct,
        // Phase-1 ships "auto" only; the Eco/Performance selector in
        // Settings is shown but grayed pending the throttling/release
        // valve work tracked in NEXT.md.
        active_profile: "auto".into(),
    }
}

/// Engine startup handshake. Emits a `ready` event containing version,
/// PID, worker cap, RAM, and the fresh hardware probe so the app's sidebar
/// can transition out of `.starting`.
pub(crate) async fn emit_ready(sink: &Sink) {
    let hardware = build_hardware_info();
    let info = EngineInfo {
        version: ENGINE_VERSION.into(),
        pid: std::process::id() as i32,
        worker_cap: platform::default_worker_cap(),
        physical_memory_gb: platform::physical_memory_gb(),
        engine: Some("rust".into()),
        hardware: Some(hardware),
    };
    sink.send(IpcEvent::now(EventPayload::Ready(Wrap::new(info))))
        .await;
}

/// Handle `verifyCudaPack`. No GPU pack exists any more (every model runs on
/// the CPU), so this re-emits the hardware snapshot with a diagnostics string
/// that says so; the command stays in the IPC contract for older apps.
pub(crate) async fn handle_verify_cuda_pack(sink: &Sink) {
    let hardware = build_hardware_info();
    tracing::info!(execution_provider = %hardware.execution_provider, "[VERIFY] hardware reprobed");
    sink.send(IpcEvent::now(EventPayload::HardwareReprobed(Wrap::new(
        HardwareReprobed {
            hardware,
            diagnostics: Some("FileID runs its AI models on the CPU on every platform; there is no GPU pack to install.".to_string()),
        },
    ))))
    .await;
}
