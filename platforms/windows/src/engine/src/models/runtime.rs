// Inference runs on tract (CPU) through the seam in `inference.rs`, the same on
// every platform. This module holds what is left around it: the GPU adapter
// probe the Hardware card shows, the device-removed marker the llama.cpp path
// and the pipeline consult, and the user's "force CPU" setting.

use super::inference::TRACT_CPU;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Qualcomm,
    Other(&'static str),
    None,
}

#[derive(Debug, Clone)]
pub struct RuntimeProbe {
    pub vendor: GpuVendor,
    pub adapter_name: Option<String>,
}

impl RuntimeProbe {
    pub fn detect() -> Self {
        let (vendor, adapter_name) = probe_gpu_vendor();
        Self { vendor, adapter_name }
    }
}

/// Marker substring attached via `.context()` to an error that means the GPU
/// device was removed. The pipeline layer greps for it in the error chain to
/// cancel the scan rather than skip the file.
pub const GPU_DEVICE_REMOVED_MARKER: &str = "[FILEID_GPU_DEVICE_REMOVED]";

pub fn ensure_gpu_inference_alive() -> anyhow::Result<()> {
    if crate::coordinator::process_gpu_device_removed() {
        anyhow::bail!(GPU_DEVICE_REMOVED_MARKER);
    }
    Ok(())
}

pub fn error_has_device_removed_marker(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains(GPU_DEVICE_REMOVED_MARKER)
}

pub fn inference_backend() -> &'static str {
    TRACT_CPU
}

/// True when the user pinned the GPU execution provider to CPU via
/// `gpuExecutionProviderOverride` in the app's `app-settings.json`. The VLM
/// (llama.cpp) path consults this so the documented GPU-TDR recovery also
/// evacuates Deep Analyze.
pub fn user_forced_cpu() -> bool {
    let Ok(path) = crate::paths::app_settings_path() else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    value
        .get("gpuExecutionProviderOverride")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|choice| choice.eq_ignore_ascii_case("cpu"))
}

#[cfg(windows)]
fn probe_gpu_vendor() -> (GpuVendor, Option<String>) {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG,
        DXGI_ADAPTER_FLAG_SOFTWARE,
    };

    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(f) => f,
        Err(err) => {
            tracing::warn!(?err, "CreateDXGIFactory1 failed; skipping GPU probe");
            return (GpuVendor::None, None);
        }
    };

    let mut idx: u32 = 0;
    let mut best: Option<(GpuVendor, String, u64)> = None;
    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(idx) } {
            Ok(a) => a,
            Err(_) => break,
        };
        let desc = match unsafe { adapter.GetDesc1() } {
            Ok(d) => d,
            Err(_) => {
                idx += 1;
                continue;
            }
        };
        let flags = DXGI_ADAPTER_FLAG(desc.Flags as i32);
        let is_software = (flags.0 & DXGI_ADAPTER_FLAG_SOFTWARE.0) != 0;
        if is_software {
            idx += 1;
            continue;
        }
        let name_chars: Vec<u16> = desc.Description.iter().take_while(|&&c| c != 0).copied().collect();
        let name = String::from_utf16_lossy(&name_chars);
        let vendor = match desc.VendorId {
            0x10DE => GpuVendor::Nvidia,
            0x1002 | 0x1022 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            0x5143 | 0x4D4F4351 => GpuVendor::Qualcomm,
            _ => GpuVendor::Other("unknown"),
        };
        let vram = desc.DedicatedVideoMemory as u64;
        match best {
            Some((_, _, best_vram)) if best_vram >= vram => {}
            _ => best = Some((vendor, name, vram)),
        }
        idx += 1;
    }

    match best {
        Some((vendor, name, _)) => (vendor, Some(name)),
        None => (GpuVendor::None, None),
    }
}

#[cfg(not(windows))]
fn probe_gpu_vendor() -> (GpuVendor, Option<String>) {
    (GpuVendor::None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_runs_on_tract_cpu() {
        assert_eq!(inference_backend(), "tract-cpu");
    }

    #[test]
    fn the_device_removed_marker_is_found_in_a_context_chain() {
        let plain = anyhow::anyhow!("ordinary model error");
        assert!(!error_has_device_removed_marker(&plain));
        let marked = anyhow::anyhow!("llama-server exited").context(GPU_DEVICE_REMOVED_MARKER);
        assert!(error_has_device_removed_marker(&marked));
    }
}
