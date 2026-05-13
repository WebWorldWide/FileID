// ONNX Runtime execution-provider picker + GPU vendor probe.
//
// Mirrors the macOS side's hardware capability detection but targets
// the Windows EP matrix:
//   NVIDIA  → CUDA (if cuDNN+cudart present), else TensorRT, else DirectML
//   Intel   → OpenVINO (if present), else DirectML
//   Snapdragon WoA → QNN (if present), else DirectML on Adreno
//   AMD     → DirectML
//   CPU floor (AVX2/AVX-512 on x64; NEON on arm64)
//
// At launch we walk DXGI adapters once to decide vendor, then check the
// `Models/<pack>/` folders that Performance Pack downloads land in to
// decide which EPs are actually loadable. The `RuntimeProbe` struct
// is consumed by `emit_ready` (advertised back to the app) and by
// the EP-priority builder when an ORT session is created.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Qualcomm,
    /// Other discrete adapter we don't have a vendor-tuned EP for.
    Other(&'static str),
    /// No GPU at all (rare on consumer Windows).
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionProvider {
    Cuda,
    TensorRt,
    OpenVino,
    DirectMl,
    Qnn,
    Cpu,
}

impl ExecutionProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionProvider::Cuda => "cuda",
            ExecutionProvider::TensorRt => "tensorrt",
            ExecutionProvider::OpenVino => "openvino",
            ExecutionProvider::DirectMl => "directml",
            ExecutionProvider::Qnn => "qnn",
            ExecutionProvider::Cpu => "cpu",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeProbe {
    pub vendor: GpuVendor,
    pub adapter_name: Option<String>,
    pub provider: ExecutionProvider,
    pub cuda_pack_present: bool,
    pub openvino_pack_present: bool,
    pub qnn_pack_present: bool,
}

impl RuntimeProbe {
    /// Cheap one-shot probe at engine start. Walks DXGI adapters,
    /// checks pack DLL presence, picks the best loadable EP. Idempotent
    /// — `RuntimeProbe::detect()` is safe to call repeatedly.
    pub fn detect() -> Self {
        let (vendor, adapter_name) = probe_gpu_vendor();
        let cuda_pack_present = pack_present("cuda");
        let openvino_pack_present = pack_present("openvino");
        let qnn_pack_present = pack_present("qnn");
        let provider = pick_provider(
            vendor,
            cuda_pack_present,
            openvino_pack_present,
            qnn_pack_present,
        );
        Self {
            vendor,
            adapter_name,
            provider,
            cuda_pack_present,
            openvino_pack_present,
            qnn_pack_present,
        }
    }
}

/// Order the EPs we'd attempt for a given hardware tier. The first one
/// that successfully binds wins — `Session::builder()` falls through on
/// failure when we register multiple EPs in priority order.
pub fn priority_chain(vendor: GpuVendor) -> Vec<ExecutionProvider> {
    let user_override = read_user_ep_override();
    let mut chain: Vec<ExecutionProvider> = Vec::new();
    if let Some(ep) = user_override {
        chain.push(ep);
    }
    match vendor {
        GpuVendor::Nvidia => {
            push_unique(&mut chain, ExecutionProvider::Cuda);
            push_unique(&mut chain, ExecutionProvider::TensorRt);
            push_unique(&mut chain, ExecutionProvider::DirectMl);
        }
        GpuVendor::Intel => {
            push_unique(&mut chain, ExecutionProvider::OpenVino);
            push_unique(&mut chain, ExecutionProvider::DirectMl);
        }
        GpuVendor::Qualcomm => {
            push_unique(&mut chain, ExecutionProvider::Qnn);
            push_unique(&mut chain, ExecutionProvider::DirectMl);
        }
        GpuVendor::Amd | GpuVendor::Other(_) => {
            push_unique(&mut chain, ExecutionProvider::DirectMl);
        }
        GpuVendor::None => {}
    }
    push_unique(&mut chain, ExecutionProvider::Cpu);
    chain
}

fn push_unique(chain: &mut Vec<ExecutionProvider>, ep: ExecutionProvider) {
    if !chain.contains(&ep) {
        chain.push(ep);
    }
}

/// User-supplied EP override stored in the C# app's `app-settings.json`
/// under key `gpuExecutionProviderOverride`. Values: `"cuda"` |
/// `"tensorrt"` | `"openvino"` | `"directml"` | `"qnn"` | `"cpu"` |
/// `"auto"` | null. None / `"auto"` returns None so the auto-detected
/// chain wins.
fn read_user_ep_override() -> Option<ExecutionProvider> {
    let path = crate::paths::app_settings_path().ok()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let s = v.get("gpuExecutionProviderOverride")?.as_str()?;
    match s.to_ascii_lowercase().as_str() {
        "cuda" => Some(ExecutionProvider::Cuda),
        "tensorrt" => Some(ExecutionProvider::TensorRt),
        "openvino" => Some(ExecutionProvider::OpenVino),
        "directml" => Some(ExecutionProvider::DirectMl),
        "qnn" => Some(ExecutionProvider::Qnn),
        "cpu" => Some(ExecutionProvider::Cpu),
        _ => None,
    }
}

fn pick_provider(
    vendor: GpuVendor,
    cuda_pack: bool,
    ov_pack: bool,
    qnn_pack: bool,
) -> ExecutionProvider {
    match vendor {
        GpuVendor::Nvidia => {
            if cuda_pack {
                ExecutionProvider::Cuda
            } else {
                ExecutionProvider::DirectMl
            }
        }
        GpuVendor::Intel => {
            if ov_pack {
                ExecutionProvider::OpenVino
            } else {
                ExecutionProvider::DirectMl
            }
        }
        GpuVendor::Qualcomm => {
            if qnn_pack {
                ExecutionProvider::Qnn
            } else {
                ExecutionProvider::DirectMl
            }
        }
        GpuVendor::Amd | GpuVendor::Other(_) => ExecutionProvider::DirectMl,
        GpuVendor::None => ExecutionProvider::Cpu,
    }
}

fn pack_present(name: &str) -> bool {
    let Ok(root) = crate::paths::models_dir() else {
        return false;
    };
    let pack_dir: PathBuf = root.join("packs").join(name);
    if !pack_dir.exists() {
        return false;
    }
    has_any_dll(&pack_dir)
}

fn has_any_dll(dir: &PathBuf) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().any(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("dll"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

// ── DXGI vendor probe ──────────────────────────────────────────────

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
    // Non-Windows host (developer cross-compiling from macOS/Linux).
    // Returns None so the engine falls back to CPU EP — keeps the
    // engine buildable on dev hosts without affecting Windows runtime.
    (GpuVendor::None, None)
}

// ── System CUDA toolkit lookup ─────────────────────────────────────
//
// V14.9 (2.1): when an NVIDIA card is present we register the system
// CUDA toolkit's bin/ with AddDllDirectory so ORT's CUDA EP can find
// cudart64_*.dll + the cuDNN DLLs the user installed via the toolkit.
// Without this the EP silently falls back to DirectML.
//
// Order: env var (CUDA_PATH then CUDA_HOME) → standard install root.

/// Probe for the host-system CUDA toolkit's `bin/` directory. Returns
/// `None` if no toolkit is detected — caller treats that as "no system
/// CUDA available, ORT will use the bundled Performance Pack DLLs only."
pub fn system_cuda_toolkit_dir() -> Option<PathBuf> {
    if let Some(bin) = cuda_bin_from_env() {
        return Some(bin);
    }
    cuda_bin_from_default_install()
}

fn cuda_bin_from_env() -> Option<PathBuf> {
    for var in ["CUDA_PATH", "CUDA_HOME"] {
        if let Ok(raw) = std::env::var(var) {
            let root = PathBuf::from(raw);
            let bin = root.join("bin");
            if bin.is_dir() {
                return Some(bin);
            }
        }
    }
    None
}

#[cfg(windows)]
fn cuda_bin_from_default_install() -> Option<PathBuf> {
    let root = PathBuf::from(r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA");
    if !root.is_dir() {
        return None;
    }
    // Pick the highest "v<MAJOR.MINOR>" sibling — newest CUDA wins.
    let mut versions: Vec<(String, PathBuf)> = std::fs::read_dir(&root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('v') && entry.path().join("bin").is_dir() {
                Some((name, entry.path().join("bin")))
            } else {
                None
            }
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().next().map(|(_, bin)| bin)
}

#[cfg(not(windows))]
fn cuda_bin_from_default_install() -> Option<PathBuf> {
    None
}

// ── CUDA Performance Pack probe ────────────────────────────────────
//
// Re-runs the same checks as `RuntimeProbe::detect()` but specifically
// for the CUDA pack, and reports back a human-readable diagnostic when
// the pack is absent so Settings → Performance can tell the user
// exactly what's missing rather than just flashing a red ×.

#[derive(Debug, Clone)]
pub struct CudaPackProbe {
    /// `None` when the CUDA pack is present and at least one DLL was
    /// discovered; otherwise a non-PII explanation suitable for the
    /// `hardwareReprobed` IPC event's `diagnostics` field.
    pub diagnostics: Option<String>,
}

/// Probe the CUDA Performance Pack. Mirrors `pack_present("cuda")` but
/// returns *why* the probe came back negative so the Settings card can
/// surface a useful hint instead of a bare "✗".
pub fn probe_cuda_pack() -> CudaPackProbe {
    let Ok(root) = crate::paths::models_dir() else {
        return CudaPackProbe {
            diagnostics: Some(
                "Could not resolve %LOCALAPPDATA%\\FileID\\Models — \
                 install the CUDA Performance Pack from Settings → Performance."
                    .to_string(),
            ),
        };
    };
    let pack_dir = root.join("packs").join("cuda");
    if !pack_dir.exists() {
        return CudaPackProbe {
            diagnostics: Some(format!(
                "CUDA Performance Pack not installed (expected at {}). \
                 Install from Settings → Performance.",
                pack_dir.display()
            )),
        };
    }
    if !has_any_dll(&pack_dir) {
        return CudaPackProbe {
            diagnostics: Some(format!(
                "CUDA pack directory exists at {} but contains no DLLs. \
                 Try reinstalling from Settings → Performance.",
                pack_dir.display()
            )),
        };
    }
    CudaPackProbe { diagnostics: None }
}
