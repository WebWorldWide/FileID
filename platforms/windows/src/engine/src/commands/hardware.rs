//! `emit_ready` (engine startup handshake) and `verifyCudaPack` (kept in the contract; answers with the same
//! `HardwareInfo` shape).

use crate::ipc::{
    sink::Sink, EngineInfo, EventPayload, HardwareInfo, HardwareReprobed, IpcEvent, Wrap,
};
use crate::models::runtime::{GpuVendor, RuntimeProbe};
use crate::platform;

const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The Mac-hosted engine runs picture search and tagging on the graphics chip through MLX (Metal), with the tract CPU copies as the
/// fallback, so reporting "no GPU, tract-cpu" would tell diagnostics the opposite.
fn mac_graphics() -> Option<(&'static str, &'static str, &'static str)> {
    (crate::paths::APP_HOSTED && crate::pipeline::tagging::apple_scan_enabled())
        .then_some(("apple", "Apple silicon GPU (Metal)", "apple-metal (MLX), tract-cpu fallback"))
}

pub(crate) fn build_hardware_info() -> HardwareInfo {
    let probe = RuntimeProbe::detect();
    let mac = mac_graphics();
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
        gpu_vendor: mac.map_or(vendor_str, |(vendor, ..)| vendor).into(),
        adapter_name: mac.map(|(_, adapter, _)| adapter.to_owned()).or_else(|| probe.adapter_name.clone()),
        execution_provider: mac.map_or(crate::models::runtime::inference_backend(), |(.., provider)| provider).into(),
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
        // Only "auto" ships; the Eco/Performance selector in Settings is grayed pending the throttling work in NEXT.md.
        active_profile: "auto".into(),
    }
}

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

/// No GPU pack exists (every model runs on the CPU), so this re-emits the hardware snapshot with a diagnostics string
/// saying so; kept in the IPC contract for older apps.
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

#[cfg(all(test, target_os = "macos", feature = "apple-adapters"))]
mod tests {
    #[test]
    fn the_mac_hosted_engine_reports_the_graphics_chip_it_runs_on() {
        let hardware = super::build_hardware_info();
        assert_eq!(hardware.gpu_vendor, "apple");
        assert!(hardware.execution_provider.starts_with("apple-metal"));
    }
}
