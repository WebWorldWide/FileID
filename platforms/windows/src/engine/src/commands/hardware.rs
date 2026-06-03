//! Hardware-detection IPC handlers: `emit_ready` (engine startup handshake)
//! and `verifyCudaPack` (Settings → Performance "Verify install" button).
//! Both surface the same `HardwareInfo` shape so the Ready event and the
//! HardwareReprobed event agree on adapter / EP / pack-present state.

use crate::ipc::{
    sink::Sink, EngineInfo, EventPayload, HardwareInfo, HardwareReprobed, IpcEvent, Wrap,
};
use crate::models::runtime::{ExecutionProvider, GpuVendor, RuntimeProbe};
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

    // Recommendation copy. Only suggest packs that would actually help.
    let recommendation = match (
        probe.vendor,
        probe.provider,
        probe.cuda_pack_present,
        probe.openvino_pack_present,
        probe.qnn_pack_present,
    ) {
        (GpuVendor::Nvidia, ExecutionProvider::DirectMl, false, _, _) =>
            "NVIDIA detected. Install the CUDA Pack in Settings → Performance for ~30% faster ML inference.".to_string(),
        (GpuVendor::Intel, ExecutionProvider::DirectMl, _, false, _) =>
            "Intel iGPU/Arc detected. Install the OpenVINO Pack in Settings → Performance for vendor-tuned inference.".to_string(),
        (GpuVendor::Qualcomm, ExecutionProvider::DirectMl, _, _, false) =>
            "Snapdragon NPU detected. Install the QNN Pack in Settings → Performance to use the Hexagon NPU.".to_string(),
        (GpuVendor::None, _, _, _, _) =>
            "No GPU detected. Falling back to CPU inference.".to_string(),
        _ => String::new(),
    };

    let topo = platform::cpu_topology();
    let mem_tier = platform::memory_tier();
    let ram_avail_mb = platform::available_memory_mb();
    let ram_total_mb = (platform::physical_memory_gb() * 1024.0) as u64;
    let vram_mb = platform::dedicated_vram_mb().unwrap_or(0);
    let (power_src, battery_pct) = platform::power_status();
    // First-pass NPU detection — QNN pack presence is our existing proxy
    // for "Hexagon NPU usable". Intel AI Boost / AMD XDNA detection is
    // a NEXT.md follow-up (need OpenVINO NPU plugin / VitisAI EP probes).
    let npu_present = matches!(probe.vendor, GpuVendor::Qualcomm) && probe.qnn_pack_present;
    HardwareInfo {
        gpu_vendor: vendor_str.into(),
        adapter_name: probe.adapter_name.clone(),
        execution_provider: probe.provider.as_str().into(),
        physical_cpu_cores: num_cpus::get_physical().max(1) as u32,
        cuda_pack_present: probe.cuda_pack_present,
        openvino_pack_present: probe.openvino_pack_present,
        qnn_pack_present: probe.qnn_pack_present,
        recommendation,
        p_cores: topo.p_cores,
        e_cores: topo.e_cores,
        logical_cpu_cores: topo.logical,
        worker_cap: topo.worker_cap(),
        ram_total_mb,
        ram_available_mb: ram_avail_mb,
        memory_tier: mem_tier.as_str().into(),
        vram_mb,
        npu_present,
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
        hardware: Some(hardware),
    };
    sink.send(IpcEvent::now(EventPayload::Ready(Wrap::new(info))))
        .await;
}

/// Handle `verifyCudaPack`. Re-runs the CUDA + cuDNN probe and emits a
/// `HardwareReprobed` event with fresh `HardwareInfo` plus a diagnostics
/// string when the pack is absent. Lets the Settings → Performance card
/// flip to ✓ without an engine restart after the user installs cuDNN.
pub(crate) async fn handle_verify_cuda_pack(sink: &Sink) {
    // Verifying is the user's "try the GPU EP again" action — clear the CUDA
    // crash-disable ONLY so the next engine launch re-attempts the CUDA bind.
    // (Per-EP: must not also re-arm a separately crash-disabled OpenVINO pack.)
    crate::models::ep_guard::reenable_ep("cuda");
    let hardware = build_hardware_info();
    let diagnostics = crate::models::runtime::probe_cuda_pack().diagnostics;
    tracing::info!(
        cuda_pack_present = hardware.cuda_pack_present,
        execution_provider = %hardware.execution_provider,
        "[VERIFY] hardware reprobed"
    );
    sink.send(IpcEvent::now(EventPayload::HardwareReprobed(Wrap::new(
        HardwareReprobed {
            hardware,
            diagnostics,
        },
    ))))
    .await;
}
