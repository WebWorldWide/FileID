// Registry of downloadable model artifacts.
//
// Maps a `model_kind` string (the welcome-sheet / Settings buttons send
// these) to a fully-resolved `Model` describing the URLs to fetch, where
// to write them, expected SHA256s, and the sentinel file the engine
// drops once every artifact has landed. The downloader walks
// `model.files` in order; the welcome sheet polls the sentinel to flip
// the row's status to "Installed".
//
// Adding a model: append a match arm in `lookup_full` and a sentinel
// path in `sentinel_path`.

use std::path::PathBuf;

use crate::paths;

/// One file the engine needs to download for a given model kind.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub url: String,
    pub dest: PathBuf,
    pub sha256: Option<String>,
    pub approx_bytes: u64,
}

// Public alias — main.rs's download orchestrator refers to the per-file
// type as `ModelFile`. Kept as an alias rather than a rename so existing
// internal call sites in this module continue compiling unchanged.
pub type ModelFile = FileEntry;

/// Bundle of files that, once all on disk, mark the model installed.
#[derive(Debug, Clone)]
pub struct Model {
    pub id: &'static str,
    pub display_name: &'static str,
    pub files: Vec<FileEntry>,
}

/// Outcome of `lookup_full`. The welcome sheet treats `NotYetAvailable`
/// as a friendly note (the row sticks at "Not installed" but with a
/// message); `Unknown` surfaces as an error popup.
#[derive(Debug)]
pub enum LookupResult {
    Found(Model),
    NotYetAvailable {
        display_name: String,
        message: String,
    },
    Unknown,
}

/// Resolve a model_kind string into a downloadable bundle.
///
/// Conventions:
/// - All URLs MUST be on huggingface.co for our privacy story
///   (the only egress the engine performs).
/// - SHA256s are optional but strongly preferred — the downloader
///   verifies when present, skips when absent.
/// - `dest` is absolute under `%LOCALAPPDATA%\FileID\Models\...`.
pub fn lookup_full(model_kind: &str) -> LookupResult {
    let models_root = match paths::models_dir() {
        Ok(p) => p,
        Err(_) => return LookupResult::Unknown,
    };

    match model_kind {
        // ── Face detection (SCRFD) + Face embedding (ArcFace).
        // Bundled together as a single "arcface" install because both
        // are required to populate face_prints + face crops. Aliases
        // accept the C# `ModelInstallerService::SlotFor` model_kinds
        // ("arcface_default" is what `WelcomeSheet` sends today;
        // `arcface_iresnet50` / `arcface_mobileface` are reserved for
        // future per-architecture choice in Settings).
        "arcface" | "arcface_default" | "arcface_iresnet50" | "arcface_mobileface" | "arcface_scrfd" => {
            let arcface_dir = models_root.join("arcface");
            let scrfd_dir = models_root.join("scrfd");
            LookupResult::Found(Model {
                id: "arcface",
                display_name: "Face recognition",
                files: vec![
                    // Immich's Buffalo-L repo lives under per-task subdirs
                    // (recognition/, detection/) with each subdir's ONNX
                    // named `model.onnx`. The remote path differs from the
                    // local filename — keep the local filenames the
                    // tagging stack expects.
                    FileEntry {
                        url: "https://huggingface.co/immich-app/buffalo_l/resolve/main/recognition/model.onnx"
                            .to_string(),
                        dest: arcface_dir.join("w600k_r50.onnx"),
                        sha256: None,
                        approx_bytes: 174_383_860,
                    },
                    FileEntry {
                        url: "https://huggingface.co/immich-app/buffalo_l/resolve/main/detection/model.onnx"
                            .to_string(),
                        dest: scrfd_dir.join("scrfd_10g_bnkps.onnx"),
                        sha256: None,
                        approx_bytes: 16_923_827,
                    },
                ],
            })
        }

        // ── MobileCLIP-S2 image encoder. Apple's own repo ships
        // `.mlpackage` (CoreML, macOS only). For Windows we pull the
        // OpenCLIP ONNX exports from Xenova/mobileclip_s2 — the same
        // mirror transformers.js + the broader ONNX community use. Same
        // weights, ONNX-graph format, runtime-loadable by our ORT setup.
        "mobileclip_s2" | "mobileclip" => {
            let dir = models_root.join("mobileclip");
            LookupResult::Found(Model {
                id: "mobileclip_s2",
                display_name: "MobileCLIP image encoder",
                files: vec![FileEntry {
                    url: "https://huggingface.co/Xenova/mobileclip_s2/resolve/main/onnx/vision_model.onnx"
                        .to_string(),
                    dest: dir.join("mobileclip_s2_image.onnx"),
                    sha256: None,
                    approx_bytes: 143_020_962,
                }],
            })
        }

        // ── CLIP text encoder (for query-time semantic search).
        // BPE vocab + merges from openai/clip-vit-base-patch32; the
        // OpenCLIP-compatible ONNX text encoder from the same Xenova
        // mirror that hosts the MobileCLIP-S2 image encoder above.
        "clip_text" => {
            let dir = models_root.join("clip_text");
            LookupResult::Found(Model {
                id: "clip_text",
                display_name: "CLIP text encoder",
                files: vec![
                    FileEntry {
                        url: "https://huggingface.co/Xenova/mobileclip_s2/resolve/main/onnx/text_model.onnx"
                            .to_string(),
                        dest: dir.join("clip_text.onnx"),
                        sha256: None,
                        approx_bytes: 253_894_023,
                    },
                    FileEntry {
                        url: "https://huggingface.co/openai/clip-vit-base-patch32/resolve/main/vocab.json"
                            .to_string(),
                        dest: dir.join("vocab.json"),
                        sha256: None,
                        approx_bytes: 1_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/openai/clip-vit-base-patch32/resolve/main/merges.txt"
                            .to_string(),
                        dest: dir.join("merges.txt"),
                        sha256: None,
                        approx_bytes: 525_000,
                    },
                ],
            })
        }

        // ── VLMs (Deep Analyze). Pulled as GGUF + mmproj pairs from
        // the official llama.cpp-friendly mirrors. Subprocess runner
        // (`vlm::VlmRunner`) finds them at canonical paths.
        "qwen2.5-vl-3b" | "qwen2_5_vl_3b" => {
            let dir = models_root.join("vlm").join("qwen2.5-vl-3b");
            LookupResult::Found(Model {
                id: "qwen2_5_vl_3b",
                display_name: "Qwen2.5-VL 3B",
                files: vec![
                    FileEntry {
                        url: "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf"
                            .to_string(),
                        dest: dir.join("model.gguf"),
                        sha256: None,
                        approx_bytes: 2_300_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf"
                            .to_string(),
                        dest: dir.join("mmproj.gguf"),
                        sha256: None,
                        approx_bytes: 870_000_000,
                    },
                ],
            })
        }
        "qwen2.5-vl-7b" | "qwen2_5_vl_7b" => {
            let dir = models_root.join("vlm").join("qwen2.5-vl-7b");
            LookupResult::Found(Model {
                id: "qwen2_5_vl_7b",
                display_name: "Qwen2.5-VL 7B",
                files: vec![
                    FileEntry {
                        url: "https://huggingface.co/ggml-org/Qwen2.5-VL-7B-Instruct-GGUF/resolve/main/Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf"
                            .to_string(),
                        dest: dir.join("model.gguf"),
                        sha256: None,
                        approx_bytes: 4_700_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/ggml-org/Qwen2.5-VL-7B-Instruct-GGUF/resolve/main/mmproj-Qwen2.5-VL-7B-Instruct-f16.gguf"
                            .to_string(),
                        dest: dir.join("mmproj.gguf"),
                        sha256: None,
                        approx_bytes: 1_400_000_000,
                    },
                ],
            })
        }
        "gemma_3_4b" | "gemma-3-4b" => {
            let dir = models_root.join("vlm").join("gemma-3-4b");
            LookupResult::Found(Model {
                id: "gemma_3_4b",
                display_name: "Gemma 3 4B",
                files: vec![
                    FileEntry {
                        url: "https://huggingface.co/ggml-org/gemma-3-4b-it-GGUF/resolve/main/gemma-3-4b-it-Q4_K_M.gguf"
                            .to_string(),
                        dest: dir.join("model.gguf"),
                        sha256: None,
                        approx_bytes: 2_500_000_000,
                    },
                    FileEntry {
                        // ggml-org's Gemma repo names the projector
                        // generically as `mmproj-model-f16.gguf` (no
                        // per-model suffix), unlike Qwen / SmolVLM.
                        url: "https://huggingface.co/ggml-org/gemma-3-4b-it-GGUF/resolve/main/mmproj-model-f16.gguf"
                            .to_string(),
                        dest: dir.join("mmproj.gguf"),
                        sha256: None,
                        approx_bytes: 851_251_104,
                    },
                ],
            })
        }
        "smolvlm" => {
            let dir = models_root.join("vlm").join("smolvlm");
            LookupResult::Found(Model {
                id: "smolvlm",
                display_name: "SmolVLM",
                files: vec![
                    FileEntry {
                        url: "https://huggingface.co/ggml-org/SmolVLM-500M-Instruct-GGUF/resolve/main/SmolVLM-500M-Instruct-Q8_0.gguf"
                            .to_string(),
                        dest: dir.join("model.gguf"),
                        sha256: None,
                        approx_bytes: 540_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/ggml-org/SmolVLM-500M-Instruct-GGUF/resolve/main/mmproj-SmolVLM-500M-Instruct-f16.gguf"
                            .to_string(),
                        dest: dir.join("mmproj.gguf"),
                        sha256: None,
                        approx_bytes: 200_000_000,
                    },
                ],
            })
        }

        // ── llama.cpp Windows runtime ZIP. Extracted in-place by
        // `handle_prewarm_model`; the .zip suffix triggers extraction.
        "llama_runtime_x64" => {
            let dir = models_root.join("llama.cpp");
            LookupResult::Found(Model {
                id: "llama_runtime_x64",
                display_name: "llama.cpp runtime",
                files: vec![FileEntry {
                    // Pinned to a specific release for reproducibility.
                    // Bump intentionally and verify the zip still ships
                    // `llama-mtmd-cli.exe` (Deep Analyze CLI) + `llama-server.exe`
                    // (the persistent VlmServer) + `mtmd.dll`.
                    //
                    // b9254 (2026-05-20) verified to contain all three; the
                    // prior pin b4404 (2024-12) predated the mtmd unification
                    // (no llama-mtmd-cli.exe) and Qwen2.5-VL, which is why the
                    // VLM path failed with "runtime not found". This is the
                    // Vulkan build — works on NVIDIA/AMD/Intel/Adreno and is the
                    // dir `VlmRunner`/`VlmServer` probe (`Models\llama.cpp\`).
                    url: "https://github.com/ggml-org/llama.cpp/releases/download/b9254/llama-b9254-bin-win-vulkan-x64.zip"
                        .to_string(),
                    dest: dir.join("llama-runtime.zip"),
                    sha256: None,
                    approx_bytes: 32_681_387,
                }],
            })
        }

        // ── cuDNN for Windows (CUDA 12 line). Public NVIDIA-hosted CDN —
        // same channel NVIDIA's own developer site points at and the
        // redistributable URL the cuDNN docs publish. Auto-installed on
        // NVIDIA hardware by `CudnnAutoInstaller.cs` so the ORT CUDA EP
        // has the cuDNN DLLs on its loader path (10-15% scanning throughput
        // win on RTX-class). Engine startup calls
        // `register_dll_dirs_under(&models_dir.join("cudnn"))` so the
        // LoadLibrary policy can find the DLLs after extraction.
        "cudnn_runtime_x64" => {
            let dir = models_root.join("cudnn");
            LookupResult::Found(Model {
                id: "cudnn_runtime_x64",
                display_name: "NVIDIA cuDNN runtime",
                files: vec![FileEntry {
                    // Pinned version. Bump intentionally and verify the
                    // archive still extracts a `bin/` (or root) directory
                    // containing `cudnn64_9.dll` + friends. NVIDIA hosts
                    // each release under a stable filename pattern, so
                    // URL drift is unlikely between point releases.
                    url: "https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/windows-x86_64/cudnn-windows-x86_64-9.5.1.17_cuda12-archive.zip"
                        .to_string(),
                    dest: dir.join("cudnn-runtime.zip"),
                    sha256: None,
                    approx_bytes: 430_000_000,
                }],
            })
        }

        // ── llama.cpp CUDA runtime ZIP. Same extract-in-place flow as
        // the Vulkan sibling above, but installed into a separate dir
        // (`llama.cpp-cuda`) so both runtimes can coexist. Auto-installed
        // on NVIDIA hardware by `CudaAutoInstaller.cs`; manually
        // installable from Settings → Performance → "CUDA llama.cpp".
        // SentinelDir in the C# auto-installer is keyed to this folder
        // name — keep them in sync.
        "llama_runtime_cuda_x64" => {
            let dir = models_root.join("llama.cpp-cuda");
            LookupResult::Found(Model {
                id: "llama_runtime_cuda_x64",
                display_name: "llama.cpp runtime (CUDA)",
                files: vec![
                    // CUDA-backend llama binaries. b9254 ships
                    // `llama-mtmd-cli.exe` + `llama-server.exe` + `mtmd.dll`
                    // (same surface as the Vulkan build), so the VLM can use the
                    // faster CUDA path on NVIDIA. The prior b4475 pin had none of
                    // the mtmd surface.
                    FileEntry {
                        url: "https://github.com/ggml-org/llama.cpp/releases/download/b9254/llama-b9254-bin-win-cuda-12.4-x64.zip"
                            .to_string(),
                        dest: dir.join("llama-runtime.zip"),
                        sha256: None,
                        approx_bytes: 259_875_510,
                    },
                    // CUDA runtime DLLs (cudart / cublas). b9254 ships these as a
                    // SEPARATE asset (b4475 bundled them). Extract into the same
                    // dir so the CUDA binaries are self-contained — the engine
                    // AddDllDirectory's `llama.cpp-cuda`, so the loader finds
                    // cudart64_12.dll / cublas64_12.dll beside the exes. Without
                    // this the CUDA server won't load and the VLM falls back to
                    // the Vulkan runtime.
                    FileEntry {
                        url: "https://github.com/ggml-org/llama.cpp/releases/download/b9254/cudart-llama-bin-win-cuda-12.4-x64.zip"
                            .to_string(),
                        dest: dir.join("cudart.zip"),
                        sha256: None,
                        approx_bytes: 391_443_627,
                    },
                ],
            })
        }

        // ── Performance Packs (CUDA / OpenVINO / QNN). Hosted on the
        // fileid-app HF dataset repo. If the repo hasn't been populated
        // yet the downloader surfaces the HTTP 404 as a friendly error;
        // until then we surface "not yet available" so the install button
        // stays usable but doesn't hard-fail.
        "cuda_pack_x64" => not_yet_available(
            "CUDA Performance Pack",
            "Pack hosting is not configured yet. Watch shared/docs/SHIP.md for the upload date.",
        ),
        "openvino_pack_x64" => not_yet_available(
            "OpenVINO Performance Pack",
            "Pack hosting is not configured yet. Watch shared/docs/SHIP.md for the upload date.",
        ),
        "qnn_pack_arm64" => not_yet_available(
            "QNN Performance Pack",
            "Pack hosting is not configured yet. Watch shared/docs/SHIP.md for the upload date.",
        ),

        _ => LookupResult::Unknown,
    }
}

fn not_yet_available(display: &str, msg: &str) -> LookupResult {
    LookupResult::NotYetAvailable {
        display_name: display.into(),
        message: msg.into(),
    }
}

/// Sentinel file the engine drops after every file in `model.files`
/// has landed. Welcome sheet + tagging stack poll it to know whether
/// the model is installed without re-validating SHA256s on every
/// launch.
pub fn sentinel_path(model: &Model) -> Option<PathBuf> {
    let root = paths::models_dir().ok()?;
    Some(root.join(".sentinels").join(format!("{}.installed", model.id)))
}
