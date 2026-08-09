// ONNX Runtime execution-provider picker + GPU vendor probe.
//
// Windows EP matrix:
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

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
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
    /// DXGI adapter index of the chosen (highest-VRAM, non-software) adapter.
    /// Passed to the DirectML EP as `device_id` so inference lands on the
    /// discrete GPU on hybrid iGPU+dGPU systems instead of DXGI adapter 0
    /// (often the integrated GPU).
    pub adapter_index: Option<u32>,
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
        let (vendor, adapter_name, adapter_index) = probe_gpu_vendor();
        // A pack EP that crashed during bind on the prior run is treated as
        // absent until the user re-enables it (ep_guard), so we transparently
        // fall through to DirectML instead of crash-looping.
        // Order matters: the stack preload only runs when the provider DLL is
        // on disk and not crash-disabled, and its verdict is part of "present"
        // — a pack whose native closure can't load must NOT advertise CUDA
        // (the chain would silently land on CPU; see cuda_stack_ready).
        let cuda_pack_present = cuda_provider_present()
            && !crate::models::ep_guard::is_disabled("cuda")
            && cuda_stack_ready();
        let openvino_pack_present = openvino_provider_present() && !crate::models::ep_guard::is_disabled("openvino");
        let qnn_pack_present = pack_present("qnn");
        let provider = pick_provider(
            vendor,
            cuda_pack_present,
            openvino_pack_present,
            qnn_pack_present,
        );
        // One-time perf hint: an NVIDIA card without the CUDA Performance
        // Pack falls through to DirectML, which is ~3-5× slower for ML
        // inference. Surface a single info line so users see the upgrade
        // path in the engine log. No auto-install, no UI prompt — install
        // is gated behind Settings → Performance.
        if vendor == GpuVendor::Nvidia && provider == ExecutionProvider::DirectMl {
            static EMITTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            EMITTED.get_or_init(|| {
                tracing::info!(
                    "perf: NVIDIA GPU detected but the CUDA Performance Pack is \
                     not installed (or its runtime DLLs failed to load — see any \
                     [EP] CUDA stack lines above) — ML inference is on DirectML \
                     (~3-5x slower). Install/reinstall the CUDA pack via Settings \
                     → Models → GPU acceleration; the pack supplies the full CUDA \
                     runtime (cudart/cublas/cuFFT) and cuDNN auto-installs."
                );
            });
        }
        Self {
            vendor,
            adapter_name,
            adapter_index,
            provider,
            cuda_pack_present,
            openvino_pack_present,
            qnn_pack_present,
        }
    }

    /// Process-lifetime memoized probe. `detect()` is idempotent but walks DXGI
    /// and probes the filesystem each call, so every model wrapper's `load()`
    /// re-walked the adapters + pack dirs 7-15× at startup. Cache the first
    /// `detect()` for the life of the process (same pattern as
    /// [`active_provider`]). The wrappers read `vendor` (Copy) and
    /// `adapter_index` (Copy) off the shared probe, so a shared `&'static`
    /// borrow is sufficient.
    pub fn shared() -> &'static RuntimeProbe {
        static CELL: std::sync::OnceLock<RuntimeProbe> = std::sync::OnceLock::new();
        CELL.get_or_init(RuntimeProbe::detect)
    }
}

/// Order the EPs we'd attempt for a given hardware tier. The first one
/// that successfully binds wins — `Session::builder()` falls through on
/// failure when we register multiple EPs in priority order.
pub fn priority_chain(vendor: GpuVendor) -> Vec<ExecutionProvider> {
    let user_override = read_user_ep_override();
    // An explicit CPU override is authoritative: bind CPU ONLY. Previously CPU
    // was merely pushed first and the vendor's GPU EPs were still appended, so
    // a GPU EP bound and ran anyway — ignoring a user who forced CPU (e.g. to
    // work around a flaky GPU driver / TDR).
    if matches!(user_override, Some(ExecutionProvider::Cpu)) {
        return vec![ExecutionProvider::Cpu];
    }
    let mut chain: Vec<ExecutionProvider> = Vec::new();
    if let Some(ep) = user_override {
        // A forced "cpu" override must be EXCLUSIVE — the vendor GPU EPs cannot
        // be appended after it. `execution_providers_for_chain` emits NO dispatch
        // for Cpu (it's ORT's implicit fallback), so a chain of [Cpu, Cuda, ...]
        // would silently bind the GPU EP first and discard the user's CPU choice.
        // That defeated the documented GPU-TDR recovery path ("switch to CPU EP
        // via gpuExecutionProviderOverride"), leaving the user stuck in a
        // device-removed crash-loop. For CPU, return CPU-only. Other overrides
        // emit a real dispatch that binds first, so prepend-then-fall-through is
        // correct for them (preserves graceful GPU→DirectML→CPU degradation).
        if ep == ExecutionProvider::Cpu {
            return vec![ExecutionProvider::Cpu];
        }
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

fn bind_chain(probe: &RuntimeProbe) -> Vec<ExecutionProvider> {
    bind_chain_with_availability(probe, tensorrt_provider_present())
}

fn bind_chain_with_availability(
    probe: &RuntimeProbe,
    tensorrt_provider_present: bool,
) -> Vec<ExecutionProvider> {
    let directml_override = matches!(read_user_ep_override(), Some(ExecutionProvider::DirectMl));
    priority_chain(probe.vendor)
        .into_iter()
        .filter(|ep| match ep {
            ExecutionProvider::Cuda => probe.cuda_pack_present,
            ExecutionProvider::TensorRt => tensorrt_provider_present,
            ExecutionProvider::OpenVino => probe.openvino_pack_present,
            ExecutionProvider::Qnn => probe.qnn_pack_present,
            // The matched CUDA ORT runtime does not export DirectML. Keep the
            // fallback only on the base runtime, or when the user explicitly
            // selected DirectML (which also suppresses the CUDA runtime pin).
            ExecutionProvider::DirectMl => !probe.cuda_pack_present || directml_override,
            ExecutionProvider::Cpu => true,
        })
        .collect()
}

fn push_unique(chain: &mut Vec<ExecutionProvider>, ep: ExecutionProvider) {
    if !chain.contains(&ep) {
        chain.push(ep);
    }
}

/// CRITICAL SAFETY: detects `DXGI_ERROR_DEVICE_REMOVED` (HRESULT 0x887A0005)
/// in any ORT/DirectML error string.
///
/// When Windows TDR kills the GPU because an op exceeded the 2-second
/// deadline, every subsequent `session.run()` returns this error. Without
/// explicit detection, the engine logs ~100 identical per-file warnings
/// per second, hammering the dying driver and preventing DWM recovery —
/// user sees a black screen + maxed fans + must hard reboot.
///
/// Callers MUST treat a true result as fatal: cancel the scan, emit
/// `EngineError { kind: "gpu_device_removed" }`, and stop submitting GPU
/// work for the rest of the process lifetime. The EP's GPU device handle
/// is permanently invalid; only a full engine restart recovers.
///
/// Match strings cover the bare HRESULT hex, DirectML EP wording, and
/// generic "device removed" phrases from CUDA / OpenVINO. Substring (not
/// regex) keeps the hot path fast.
/// Marker substring attached via `.context()` to any session.run() error
/// classified as device-removed. The pipeline layer greps for this in the
/// error chain to know it should cancel the scan rather than skip the file.
pub const GPU_DEVICE_REMOVED_MARKER: &str = "[FILEID_GPU_DEVICE_REMOVED]";

pub fn ensure_gpu_inference_alive() -> anyhow::Result<()> {
    if crate::coordinator::process_gpu_device_removed() {
        anyhow::bail!(GPU_DEVICE_REMOVED_MARKER);
    }
    Ok(())
}

/// Detects whether an error carries the marker added by the model
/// wrappers when they classify a session.run failure as device-removed.
/// Cheap substring check on the formatted error chain.
pub fn error_has_device_removed_marker(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains(GPU_DEVICE_REMOVED_MARKER)
}

/// Helper used by every model wrapper after `session.run()` returns
/// Err. Inspects the error and, if it looks like GPU device-removed,
/// attaches the marker so the pipeline layer can detect + cancel the
/// whole scan. Non-device errors pass through unchanged (single-file
/// non-fatal failures the pipeline can skip).
pub fn classify_inference_error(err: anyhow::Error) -> anyhow::Error {
    if is_device_removed_error(&err) {
        crate::coordinator::latch_process_gpu_device_removed();
        err.context(GPU_DEVICE_REMOVED_MARKER)
    } else {
        err
    }
}

pub fn is_device_removed_error(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}");
    // HRESULT in hex (both upper and lower-case forms).
    if s.contains("887A0005") || s.contains("887a0005") {
        return true;
    }
    // DirectML EP wording.
    if s.contains("DEVICE_REMOVED")
        || s.contains("device removed")
        || s.contains("device instance has been suspended")
    {
        return true;
    }
    // CUDA / generic.
    if s.contains("CUDA_ERROR_NOT_INITIALIZED")
        || s.contains("CUDA_ERROR_INVALID_CONTEXT")
        || s.contains("DXGI_ERROR_DEVICE_HUNG")
        || s.contains("DXGI_ERROR_DEVICE_RESET")
    {
        return true;
    }
    false
}

/// Resolve a priority chain of our internal `ExecutionProvider` variants
/// into the `ExecutionProviderDispatch` values ORT's `SessionBuilder::
/// with_execution_providers` consumes.
///
/// CPU is the implicit fallback (ORT always has the CPU EP), so we don't
/// emit an explicit dispatch for it — letting it fall through means a
/// failed-to-bind GPU EP cleanly degrades to CPU instead of failing
/// session creation outright.
pub fn execution_providers_for_chain(
    chain: &[ExecutionProvider],
    adapter_index: Option<u32>,
) -> Vec<ort::execution_providers::ExecutionProviderDispatch> {
    use ort::execution_providers::{
        cuda::CUDAExecutionProvider,
        directml::DirectMLExecutionProvider,
        openvino::OpenVINOExecutionProvider,
        qnn::QNNExecutionProvider,
        tensorrt::TensorRTExecutionProvider,
    };
    let mut out = Vec::with_capacity(chain.len());
    for ep in chain {
        match ep {
            ExecutionProvider::Cuda => out.push(CUDAExecutionProvider::default().build()),
            ExecutionProvider::TensorRt => out.push(TensorRTExecutionProvider::default().build()),
            // P4: pin the OpenVINO device to AUTO:GPU,CPU so Intel boxes target
            // the GPU (or NPU once detected) and only fall back to the OpenVINO
            // CPU plugin when no accelerator binds — instead of OpenVINO's
            // default possibly landing on CPU silently. AUTO keeps a safe
            // fallback if the GPU plugin is unavailable. (Explicit NPU hint +
            // INT8 variants land in Phase 6 alongside NPU detection.)
            ExecutionProvider::OpenVino => out.push(
                OpenVINOExecutionProvider::default()
                    .with_device_type("AUTO:GPU,CPU")
                    .build(),
            ),
            ExecutionProvider::DirectMl => {
                // Pin DirectML to the discrete adapter on hybrid iGPU+dGPU
                // boxes. DirectML's `device_id` is the DXGI adapter index —
                // the same enumeration `probe_gpu_vendor` walks — so the
                // highest-VRAM adapter we found is the one we select. Without
                // this DirectML defaults to adapter 0, which is often the iGPU.
                // (CUDA/TensorRT use the CUDA device ordinal, not the DXGI
                // index, and the iGPU isn't CUDA-visible, so they stay default.)
                let mut dml = DirectMLExecutionProvider::default();
                if let Some(idx) = adapter_index {
                    dml = dml.with_device_id(idx as i32);
                }
                out.push(dml.build());
            }
            ExecutionProvider::Qnn => {
                // Snapdragon: bind the Hexagon NPU (HTP) backend explicitly.
                // `QnnHtp.dll` ships in the QNN Performance Pack and is on the
                // process DLL search path (`register_dll_dirs_under`). If it's
                // absent the EP fails to load and we fall through to DirectML
                // on the Adreno GPU.
                out.push(
                    QNNExecutionProvider::default()
                        .with_backend_path("QnnHtp.dll")
                        .build(),
                );
            }
            ExecutionProvider::Cpu => { /* CPU is the implicit fallback */ }
        }
    }
    out
}

/// The execution provider this process will actually bind, cached. Single
/// source of truth for per-EP decisions: model-variant selection
/// (`models::variants`) and session tuning (`configure_session_builder`).
/// `RuntimeProbe::detect()` is idempotent but walks DXGI + probes the
/// filesystem, so memoize it for the life of the process.
pub fn active_provider() -> ExecutionProvider {
    static CELL: std::sync::OnceLock<ExecutionProvider> = std::sync::OnceLock::new();
    *CELL.get_or_init(|| RuntimeProbe::detect().provider)
}

/// The execution provider whose first native session-bind the EP crash gate
/// (`ep_guard`) must protect. Unlike [`active_provider`] (which is derived from
/// `pick_provider` and ignores the user override), this honors
/// `gpuExecutionProviderOverride` by walking the SAME override-aware
/// `priority_chain` the model wrappers bind from, and returns the first
/// *guarded* EP (`cuda`/`openvino`) that is actually reachable for a real
/// native bind (pack present and not crash-disabled — the same predicate
/// `RuntimeProbe::detect` uses, since a disable un-pins `ORT_DYLIB_PATH`). When
/// no guarded EP will bind it returns `active_provider()` so `ep_guard::arm`
/// no-ops on DirectML/CPU. Without this, a user who forces `cuda`/`openvino` on
/// a box whose auto-detected provider is DirectML armed the wrong (unguarded)
/// EP, so a crash during that forced GPU bind left no breadcrumb and the engine
/// crash-looped instead of reverting to DirectML (B6).
pub fn armed_provider() -> ExecutionProvider {
    // Only reads the Copy `vendor` field; the loop re-queries pack/disable state
    // live, so the memoized probe is byte-identical and skips a DXGI re-walk.
    let probe = RuntimeProbe::shared();
    for ep in bind_chain(probe) {
        match ep {
            ExecutionProvider::Cuda
                if cuda_provider_present() && !crate::models::ep_guard::is_disabled("cuda") =>
            {
                return ExecutionProvider::Cuda;
            }
            ExecutionProvider::OpenVino
                if openvino_provider_present() && !crate::models::ep_guard::is_disabled("openvino") =>
            {
                return ExecutionProvider::OpenVino;
            }
            _ => {}
        }
    }
    // R4-09: no guarded EP is reachable in the override-aware bind chain, so the
    // real session bind registers no cuda/openvino dispatch — nothing guarded will
    // bind. Return an UNGUARDED EP so `ep_guard::arm` no-ops. Must NOT fall back to
    // `active_provider()`: it ignores the user override, so on a forced-"cpu"
    // override (the GPU-TDR recovery path) it returns Cuda on an NVIDIA+pack box,
    // arming a CUDA breadcrumb around a CPU-only bind — an unrelated native crash
    // then false-poisons a healthy CUDA pack. (B6 follow-up)
    ExecutionProvider::Cpu
}

/// Apply execution-provider-specific session tuning that would otherwise be
/// copy-pasted into every model wrapper. Call right after `Session::builder()`
/// and before `with_execution_providers` / `commit_from_file`; it replaces the
/// bare `.with_intra_threads(1)` the wrappers used to hardcode.
///
/// - **Graph optimization**: Level3 (all) everywhere except QNN, where the
///   Hexagon (HTP) backend's own partitioner wants Basic (Level1) — ORT's
///   aggressive op fusion otherwise emits nodes QNN can't consume, silently
///   forcing a CPU fallback. (Level3 is ORT's default; we set it explicitly
///   so the intent is on the page.)
/// - **Intra-op threads**: on the CPU EP, use all performance cores so
///   CPU-only boxes get multi-threaded inference — the wrappers previously
///   pinned 1, leaving CPU users single-threaded and badly underutilized. On
///   GPU/NPU EPs keep 1: the accelerator does the parallelism and extra host
///   threads only contend with it.
pub fn configure_session_builder(
    builder: ort::session::builder::SessionBuilder,
) -> ort::Result<ort::session::builder::SessionBuilder> {
    use ort::logging::LogLevel;
    use ort::session::builder::GraphOptimizationLevel;
    // Use the EP that will ACTUALLY bind — the first in the override-aware
    // priority chain — not active_provider() (which is derived from pick_provider
    // and ignores gpuExecutionProviderOverride, see armed_provider's doc). Else a
    // forced "cpu" override (now a CPU-exclusive chain, the GPU-TDR recovery path)
    // would be tuned with the GPU default of 1 intra-op thread → single-threaded
    // CPU inference on a multi-core box. priority_chain always ends with Cpu, so
    // next() is always Some.
    let ep = bind_chain(RuntimeProbe::shared())
        .into_iter()
        .next()
        .unwrap_or(ExecutionProvider::Cpu);
    let opt = if ep == ExecutionProvider::Qnn {
        GraphOptimizationLevel::Level1
    } else {
        GraphOptimizationLevel::Level3
    };
    let intra: usize = if ep == ExecutionProvider::Cpu {
        crate::platform::cpu_topology().p_cores.max(1) as usize
    } else {
        1
    };
    builder
        .with_log_level(LogLevel::Error)?
        .with_optimization_level(opt)?
        .with_intra_threads(intra)
}

/// Build -> configure -> register-EPs -> commit an ORT session from `onnx_path`
/// over this process's EP priority chain, returning the session and its first
/// input tensor name. Folds the identical per-wrapper session-bind boilerplate
/// (RAM++, SFace, MobileCLIP, CLIP-text, YuNet, SCRFD) into one place; `label`
/// flows into the `.context()` chain and the EP-chain log line. BGE binds the
/// CPU EP with a thread override + multi-input binding, so it keeps its own path.
pub fn commit_chain_session(
    label: &str,
    onnx_path: &Path,
) -> anyhow::Result<(ort::session::Session, String)> {
    use anyhow::Context;
    ensure_gpu_inference_alive()?;
    let probe = RuntimeProbe::shared();
    let chain = bind_chain(probe);
    let chain_labels: Vec<&'static str> = chain.iter().map(|e| e.as_str()).collect();
    let builder = ort::session::Session::builder().context("ORT session builder")?;
    let mut builder =
        configure_session_builder(builder).with_context(|| format!("configure session ({label})"))?;
    let providers = execution_providers_for_chain(&chain, probe.adapter_index);
    if !providers.is_empty() {
        builder = builder
            .with_execution_providers(providers)
            .with_context(|| format!("register execution providers ({label})"))
            .map_err(classify_inference_error)?;
    }
    tracing::info!(model = label, chain = ?chain_labels, "EP priority chain registered");
    let session = builder
        .commit_from_file(onnx_path)
        .with_context(|| format!("ORT session commit ({label})"))
        .map_err(classify_inference_error)?;
    let input_name = session
        .inputs
        .first()
        .ok_or_else(|| anyhow::anyhow!("{label} ONNX has no inputs"))?
        .name
        .clone();
    Ok((session, input_name))
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

/// True when the user pinned the execution provider to CPU via
/// `gpuExecutionProviderOverride`. The VLM (llama.cpp) path consults this so the
/// documented GPU-TDR recovery — evacuate the GPU — also evacuates Deep Analyze,
/// not just the ORT scan path that already honors the override. (F-C1-006)
pub fn user_forced_cpu() -> bool {
    matches!(read_user_ep_override(), Some(ExecutionProvider::Cpu))
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

/// CUDA is usable only if ORT's own CUDA provider DLL is on disk. pyke's
/// `download-binaries` ships the base `onnxruntime.dll` + `providers_shared`
/// but NOT `onnxruntime_providers_cuda.dll`, so it must come from an installed
/// CUDA Performance Pack (the pack extracts into a versioned subdir, hence the
/// walk). This is stricter than `pack_present("cuda")` — a stray unrelated DLL
/// in the pack dir must not make us advertise CUDA and skip DirectML's
/// discrete-adapter `device_id` pinning.
fn cuda_provider_present() -> bool {
    let Ok(root) = crate::paths::models_dir() else {
        return false;
    };
    crate::platform::find_file_under(
        &root.join("packs").join("cuda"),
        "onnxruntime_providers_cuda.dll",
        4,
    )
    .is_some()
}

fn tensorrt_provider_present() -> bool {
    let Ok(root) = crate::paths::models_dir() else {
        return false;
    };
    let provider = crate::platform::find_file_under(
        &root.join("packs").join("cuda"),
        "onnxruntime_providers_tensorrt.dll",
        4,
    );
    // The ORT TensorRT provider is only loadable when NVIDIA's TensorRT
    // runtime is installed as well. The CUDA performance pack intentionally
    // does not ship that separately licensed dependency.
    provider.is_some()
        && crate::platform::find_file_under(
            &root.join("packs").join("cuda"),
            "nvinfer_10.dll",
            4,
        )
        .is_some()
}

/// The CUDA EP's native dependency closure. `onnxruntime_providers_cuda.dll`
/// (ORT 1.22 win-x64-gpu) hard-imports every one of these — a single missing
/// name makes its LoadLibrary fail with ERROR_MOD_NOT_FOUND at session build,
/// ORT falls through the chain, and (because the pinned gpu runtime carries no
/// DirectML EP) the session lands on CPU with zero Rust-visible error. That
/// exact shape burned a full overnight scan on 2026-07-20: pack "present",
/// ep="cuda" everywhere, cufft64_11.dll absent, Swin-L on one CPU thread.
#[cfg(windows)]
const CUDA_STACK_REQUIRED: &[&str] = &[
    "cudart64_12.dll",
    "cublasLt64_12.dll",
    "cublas64_12.dll",
    "cufft64_11.dll",
    "cudnn64_9.dll",
];

/// Loaded when present; their absence degrades specific paths (cuDNN
/// runtime-compiled engines) rather than blocking the EP outright.
#[cfg(windows)]
const CUDA_STACK_OPTIONAL: &[&str] = &["nvrtc64_120_0.dll"];

#[cfg(windows)]
static CUDA_STACK_STATE: parking_lot::Mutex<Option<bool>> = parking_lot::Mutex::new(None);

/// True once the CUDA math stack has been deterministically pre-loaded.
/// Memoized (module identity can't change after first resolution) but
/// invalidatable — a mid-session pack install must be able to flip
/// absent → ready for the Settings "Verify install" flow without an engine
/// restart. This is the loadability half of "is CUDA actually usable";
/// `cuda_provider_present` is only the on-disk half.
#[cfg(windows)]
pub fn cuda_stack_ready() -> bool {
    let mut state = CUDA_STACK_STATE.lock();
    if let Some(ready) = *state {
        return ready;
    }
    let ready = preload_cuda_math_stack();
    *state = Some(ready);
    ready
}

#[cfg(not(windows))]
pub fn cuda_stack_ready() -> bool {
    // Non-Windows builds never pin the CUDA gpu runtime; presence checks
    // alone keep their existing meaning.
    true
}

/// Re-run the stack preload on the next `cuda_stack_ready` query — called
/// after a CUDA-related pack (re)install or EP re-enable. DLLs already loaded
/// stay loaded for the process lifetime, so a re-run can only flip a previous
/// "missing" verdict to ready, never unload a live stack.
#[cfg(windows)]
pub fn invalidate_cuda_stack_probe() {
    *CUDA_STACK_STATE.lock() = None;
}

#[cfg(not(windows))]
pub fn invalidate_cuda_stack_probe() {}

/// Pre-load the CUDA EP's math DLLs by full path, in dependency order, from a
/// deterministic per-DLL location preference: the CUDA Performance Pack →
/// a system CUDA toolkit (`CUDA_PATH`/default install) → the llama.cpp CUDA
/// runtime → the newest installed cuDNN drop. Two birds: (a) a broken/partial
/// pack is detected here, BEFORE we advertise ep="cuda" or pin ORT_DYLIB_PATH
/// to the DirectML-less gpu runtime; (b) when the same DLL name exists in
/// several registered directories (llama.cpp-cuda ships its own cudart/cublas
/// line), the copy the CUDA EP binds is the one chosen here — AddDllDirectory
/// search order is explicitly unspecified.
#[cfg(windows)]
fn preload_cuda_math_stack() -> bool {
    let mut ok = true;
    for name in CUDA_STACK_REQUIRED {
        match locate_stack_dll(name) {
            Some(path) => match crate::platform::preload_dll(&path) {
                Ok(()) => {
                    tracing::info!(
                        dll = name,
                        path = %crate::platform::redact_path_for_log(&path),
                        "[EP] CUDA stack preloaded"
                    );
                }
                Err(code) => {
                    tracing::error!(
                        dll = name,
                        path = %crate::platform::redact_path_for_log(&path),
                        win32 = code,
                        "[EP] CUDA stack DLL failed to load — CUDA EP unusable, falling back to DirectML"
                    );
                    ok = false;
                }
            },
            None => {
                tracing::error!(
                    dll = name,
                    "[EP] CUDA stack DLL missing from every known location — CUDA EP unusable, falling back to DirectML. Reinstall the CUDA Performance Pack (Settings → Models → GPU acceleration)."
                );
                ok = false;
            }
        }
    }
    for name in CUDA_STACK_OPTIONAL {
        match locate_stack_dll(name) {
            Some(path) => {
                let _ = crate::platform::preload_dll(&path);
            }
            None => {
                tracing::info!(dll = name, "[EP] optional CUDA stack DLL not present");
            }
        }
    }
    // Pin the cuDNN sub-library identities to the same drop as the dispatch
    // shim we just loaded — the shim LoadLibrary's them lazily by bare name,
    // which would otherwise re-enter the unordered search set and could mix
    // cuDNN versions across directories.
    if ok {
        if let Some(shim) = locate_stack_dll("cudnn64_9.dll") {
            if let Some(dir) = shim.parent() {
                if let Ok(rd) = std::fs::read_dir(dir) {
                    for entry in rd.flatten() {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("cudnn_") && name.ends_with("64_9.dll") {
                            let _ = crate::platform::preload_dll(&entry.path());
                        }
                    }
                }
            }
        }
    }
    ok
}

/// Deterministic per-DLL location preference for the CUDA math stack.
#[cfg(windows)]
fn locate_stack_dll(name: &str) -> Option<PathBuf> {
    let root = crate::paths::models_dir().ok();
    if let Some(root) = &root {
        if let Some(p) =
            crate::platform::find_file_under(&root.join("packs").join("cuda"), name, 4)
        {
            return Some(p);
        }
    }
    if let Some(bin) = system_cuda_toolkit_dir() {
        let p = bin.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(root) = &root {
        let llama = root.join("llama.cpp-cuda").join(name);
        if llama.is_file() {
            return Some(llama);
        }
        if let Some(p) = newest_cudnn_dll(root, name) {
            return Some(p);
        }
    }
    None
}

/// Find `name` under the NEWEST versioned cuDNN drop in `Models/cudnn/`.
/// A pin bump extracts a second `cudnn-windows-x86_64-<ver>_cudaN-archive`
/// sibling next to the old one; a plain directory walk finds whichever
/// `read_dir` yields first (lexicographic on NTFS — "9.5.1" sorts before
/// "9.8.0", and would sort AFTER "9.10.0"). Parse the version numerically.
#[cfg(windows)]
fn newest_cudnn_dll(models_root: &std::path::Path, name: &str) -> Option<PathBuf> {
    let cudnn_root = models_root.join("cudnn");
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for entry in std::fs::read_dir(&cudnn_root).ok()?.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let dir_name = entry.file_name();
        let dir_name = dir_name.to_string_lossy().to_string();
        let version: Vec<u32> = dir_name
            .strip_prefix("cudnn-windows-x86_64-")
            .and_then(|rest| rest.split('_').next())
            .map(|v| v.split('.').filter_map(|c| c.parse().ok()).collect())
            .unwrap_or_default();
        let candidate = entry.path().join("bin").join(name);
        if candidate.is_file() && best.as_ref().is_none_or(|(v, _)| version > *v) {
            best = Some((version, candidate));
        }
    }
    best.map(|(_, p)| p)
}

/// OpenVINO mirrors the CUDA gate: usable only when the pack's own provider
/// DLL is on disk. The shallow `pack_present` (any top-level DLL) both
/// under-reported a pack whose zip carries a top-level directory and
/// over-reported on a stray unrelated DLL — the same two failure modes the
/// CUDA gate was hardened against.
fn openvino_provider_present() -> bool {
    let Ok(root) = crate::paths::models_dir() else {
        return false;
    };
    crate::platform::find_file_under(
        &root.join("packs").join("openvino"),
        "onnxruntime_providers_openvino.dll",
        4,
    )
    .is_some()
}

/// The accelerator pack directory for the detected GPU vendor, if that vendor
/// uses a pack-backed ORT execution provider: NVIDIA → `packs/cuda`,
/// Intel → `packs/openvino`. Returned as `(ep_name, dir)` where `ep_name` is
/// also the `ep_guard` key. `main.rs` pins `ORT_DYLIB_PATH` to this pack's
/// `onnxruntime.dll` so the vendor's provider DLL binds against the *same* ORT
/// build pyke's base lacks. AMD/Qualcomm/None use DirectML/CPU — no pinned
/// runtime, so this returns None.
pub fn active_pack_dir() -> Option<(&'static str, PathBuf)> {
    if matches!(
        read_user_ep_override(),
        Some(ExecutionProvider::DirectMl | ExecutionProvider::Cpu)
    ) {
        return None;
    }
    let root = crate::paths::models_dir().ok()?;
    let (vendor, _, _) = probe_gpu_vendor();
    let ep = match vendor {
        GpuVendor::Nvidia => "cuda",
        GpuVendor::Intel => "openvino",
        _ => return None,
    };
    Some((ep, root.join("packs").join(ep)))
}

fn has_any_dll(dir: &PathBuf) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().any(|entry| {
                entry.path().extension().is_some_and(|s| s.eq_ignore_ascii_case("dll"))
            })
        })
        .unwrap_or(false)
}

// ── DXGI vendor probe ──────────────────────────────────────────────

#[cfg(windows)]
fn probe_gpu_vendor() -> (GpuVendor, Option<String>, Option<u32>) {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG,
        DXGI_ADAPTER_FLAG_SOFTWARE,
    };

    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(f) => f,
        Err(err) => {
            tracing::warn!(?err, "CreateDXGIFactory1 failed; skipping GPU probe");
            return (GpuVendor::None, None, None);
        }
    };

    let mut idx: u32 = 0;
    let mut best: Option<(GpuVendor, String, u64, u32)> = None;
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
            Some((_, _, best_vram, _)) if best_vram >= vram => {}
            _ => best = Some((vendor, name, vram, idx)),
        }
        idx += 1;
    }

    match best {
        Some((vendor, name, _, adapter_idx)) => (vendor, Some(name), Some(adapter_idx)),
        None => (GpuVendor::None, None, None),
    }
}

#[cfg(not(windows))]
fn probe_gpu_vendor() -> (GpuVendor, Option<String>, Option<u32>) {
    // Non-Windows host (developer cross-compiling from macOS/Linux).
    // Returns None so the engine falls back to CPU EP — keeps the
    // engine buildable on dev hosts without affecting Windows runtime.
    (GpuVendor::None, None, None)
}

// ── System CUDA toolkit lookup ─────────────────────────────────────
//
// When an NVIDIA card is present, register the system CUDA toolkit's bin/
// with AddDllDirectory so ORT's CUDA EP can find cudart64_*.dll + cuDNN.
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
    // R4-12: parse the version NUMERICALLY. A lexicographic string sort ranks
    // "v9.0" above "v12.0" (and "v12.4" above "v12.10"), registering an older
    // toolkit's bin/ over a newer one. (u32,u32) tuple Ord compares major then
    // minor; non-parsing entries (e.g. "vlatest", bare "v12") are dropped rather
    // than winning a string compare.
    std::fs::read_dir(&root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let ver = name.strip_prefix('v')?;
            let (major, minor) = ver.split_once('.')?;
            let version = (major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?);
            let bin = entry.path().join("bin");
            bin.is_dir().then_some((version, bin))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, bin)| bin)
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
    // Match the detection gate (`cuda_provider_present`): the pack extracts
    // the provider into a versioned subdir, so walk for the specific DLL rather
    // than a non-recursive "any dll at the top level" check (which would
    // mis-report a correctly-installed pack).
    if crate::platform::find_file_under(&pack_dir, "onnxruntime_providers_cuda.dll", 4).is_none() {
        return CudaPackProbe {
            diagnostics: Some(format!(
                "CUDA pack directory exists at {} but onnxruntime_providers_cuda.dll \
                 wasn't found. Try reinstalling from Settings → Performance.",
                pack_dir.display()
            )),
        };
    }
    CudaPackProbe { diagnostics: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard: every vendor's requested EP chain must end at CPU
    /// (the always-present floor). Availability filtering below removes
    /// providers whose matched native runtime cannot load them.
    fn assert_chain_terminates_at_cpu_with_directml(vendor: GpuVendor, chain: &[ExecutionProvider]) {
        assert_eq!(
            chain.last(),
            Some(&ExecutionProvider::Cpu),
            "{vendor:?} chain must end at CPU, got {chain:?}"
        );
        if !matches!(vendor, GpuVendor::None) {
            assert!(
                chain.contains(&ExecutionProvider::DirectMl),
                "{vendor:?} chain must include DirectML, got {chain:?}"
            );
        }
    }

    #[test]
    fn nvidia_chain_starts_with_cuda_then_tensorrt_then_directml_then_cpu() {
        // Unset the user override so the test isn't poisoned by a developer
        // box's preferred EP.
        unsafe { std::env::remove_var("FILEID_GPU_EP_OVERRIDE"); }
        let chain = priority_chain(GpuVendor::Nvidia);
        assert_eq!(
            chain,
            vec![
                ExecutionProvider::Cuda,
                ExecutionProvider::TensorRt,
                ExecutionProvider::DirectMl,
                ExecutionProvider::Cpu,
            ]
        );
    }

    #[test]
    fn nvidia_bind_chain_skips_uninstalled_pack_providers() {
        unsafe { std::env::remove_var("FILEID_GPU_EP_OVERRIDE"); }
        let probe = RuntimeProbe {
            vendor: GpuVendor::Nvidia,
            adapter_name: None,
            adapter_index: Some(0),
            provider: ExecutionProvider::DirectMl,
            cuda_pack_present: false,
            openvino_pack_present: false,
            qnn_pack_present: false,
        };
        assert_eq!(
            bind_chain_with_availability(&probe, false),
            vec![ExecutionProvider::DirectMl, ExecutionProvider::Cpu]
        );
    }

    #[test]
    fn nvidia_bind_chain_omits_directml_from_cuda_runtime() {
        unsafe { std::env::remove_var("FILEID_GPU_EP_OVERRIDE"); }
        let probe = RuntimeProbe {
            vendor: GpuVendor::Nvidia,
            adapter_name: None,
            adapter_index: Some(0),
            provider: ExecutionProvider::Cuda,
            cuda_pack_present: true,
            openvino_pack_present: false,
            qnn_pack_present: false,
        };
        assert_eq!(
            bind_chain_with_availability(&probe, false),
            vec![ExecutionProvider::Cuda, ExecutionProvider::Cpu]
        );
    }

    #[test]
    fn amd_chain_is_directml_then_cpu() {
        unsafe { std::env::remove_var("FILEID_GPU_EP_OVERRIDE"); }
        let chain = priority_chain(GpuVendor::Amd);
        assert_eq!(chain, vec![ExecutionProvider::DirectMl, ExecutionProvider::Cpu]);
    }

    #[test]
    fn intel_chain_starts_with_openvino_then_directml_then_cpu() {
        unsafe { std::env::remove_var("FILEID_GPU_EP_OVERRIDE"); }
        let chain = priority_chain(GpuVendor::Intel);
        assert_eq!(
            chain,
            vec![
                ExecutionProvider::OpenVino,
                ExecutionProvider::DirectMl,
                ExecutionProvider::Cpu,
            ]
        );
    }

    #[test]
    fn qualcomm_chain_starts_with_qnn_then_directml_then_cpu() {
        unsafe { std::env::remove_var("FILEID_GPU_EP_OVERRIDE"); }
        let chain = priority_chain(GpuVendor::Qualcomm);
        assert_eq!(
            chain,
            vec![
                ExecutionProvider::Qnn,
                ExecutionProvider::DirectMl,
                ExecutionProvider::Cpu,
            ]
        );
    }

    #[test]
    fn other_vendor_chain_is_directml_then_cpu() {
        unsafe { std::env::remove_var("FILEID_GPU_EP_OVERRIDE"); }
        let chain = priority_chain(GpuVendor::Other("S3"));
        assert_eq!(chain, vec![ExecutionProvider::DirectMl, ExecutionProvider::Cpu]);
    }

    #[test]
    fn none_vendor_chain_is_cpu_only() {
        unsafe { std::env::remove_var("FILEID_GPU_EP_OVERRIDE"); }
        let chain = priority_chain(GpuVendor::None);
        assert_eq!(chain, vec![ExecutionProvider::Cpu]);
    }

    #[test]
    fn every_vendor_chain_invariants_hold() {
        unsafe { std::env::remove_var("FILEID_GPU_EP_OVERRIDE"); }
        for vendor in [
            GpuVendor::Nvidia,
            GpuVendor::Amd,
            GpuVendor::Intel,
            GpuVendor::Qualcomm,
            GpuVendor::Other("test"),
            GpuVendor::None,
        ] {
            let chain = priority_chain(vendor);
            assert_chain_terminates_at_cpu_with_directml(vendor, &chain);
        }
    }

    #[test]
    fn device_removed_classification_latches_only_in_an_isolated_process() {
        const CHILD: &str = "FILEID_GPU_LATCH_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert!(!crate::coordinator::process_gpu_device_removed());
            let ordinary = classify_inference_error(anyhow::anyhow!("ordinary model error"));
            assert!(!error_has_device_removed_marker(&ordinary));
            assert!(!crate::coordinator::process_gpu_device_removed());

            let removed = classify_inference_error(
                anyhow::anyhow!("DXGI_ERROR_DEVICE_REMOVED 0x887A0005")
                    .context("ORT session commit (test model)"),
            );
            assert!(error_has_device_removed_marker(&removed));
            assert!(crate::coordinator::process_gpu_device_removed());
            let blocked = ensure_gpu_inference_alive().expect_err("GPU work must stay blocked");
            assert!(error_has_device_removed_marker(&blocked));
            return;
        }

        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("models::runtime::tests::device_removed_classification_latches_only_in_an_isolated_process")
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD, "1")
            .status()
            .expect("launch isolated GPU-latch test");
        assert!(status.success());
    }
}
