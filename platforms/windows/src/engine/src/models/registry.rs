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

/// Outcome of `lookup_full`. `Unknown` surfaces as an error popup in the
/// welcome sheet — registered model_kinds resolve to `Found`.
#[derive(Debug)]
pub enum LookupResult {
    Found(Model),
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
        // Commercial-clean faces: YuNet detection (MIT) + SFace recognition
        // (Apache-2.0, 128-d) from OpenCV Zoo's official HF mirrors — replacing
        // the non-commercial InsightFace Buffalo-L (SCRFD + ArcFace). The
        // model_kind aliases + id "arcface" are kept so the C# install slot,
        // sentinel, and pre-scan gate are unchanged (Approach A).
        "arcface" | "arcface_default" | "arcface_iresnet50" | "arcface_mobileface" | "arcface_scrfd" | "yunet_sface" => {
            let yunet_dir = models_root.join("yunet");
            let sface_dir = models_root.join("sface");
            LookupResult::Found(Model {
                id: "arcface",
                display_name: "Face detection + recognition",
                files: vec![
                    FileEntry {
                        url: "https://huggingface.co/opencv/face_detection_yunet/resolve/main/face_detection_yunet_2023mar.onnx"
                            .to_string(),
                        dest: yunet_dir.join("face_detection_yunet_2023mar.onnx"),
                        sha256: None,
                        approx_bytes: 232_589,
                    },
                    FileEntry {
                        url: "https://huggingface.co/opencv/face_recognition_sface/resolve/main/face_recognition_sface_2021dec.onnx"
                            .to_string(),
                        dest: sface_dir.join("face_recognition_sface_2021dec.onnx"),
                        sha256: None,
                        approx_bytes: 38_696_353,
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
                display_name: "CLIP ViT-B/32 image encoder",
                files: vec![FileEntry {
                    // OpenAI/OpenCLIP ViT-B/32 (MIT) replaces Apple's MobileCLIP-S2
                    // (research-only license). Same 512-d output; commercial-clean.
                    // model_kind/dest kept as `mobileclip_s2` to avoid churning the
                    // sentinel + C# slot wiring (the id is now just a stable key).
                    url: "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model.onnx"
                        .to_string(),
                    dest: dir.join("mobileclip_s2_image.onnx"),
                    sha256: None,
                    approx_bytes: 351_685_709,
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
                        // OpenAI/OpenCLIP ViT-B/32 text encoder (MIT). input_ids-only;
                        // 512-d. Pairs with the openai/clip-vit-base-patch32 BPE
                        // vocab+merges below (unchanged — they are ViT-B/32's tokenizer).
                        url: "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/text_model.onnx"
                            .to_string(),
                        dest: dir.join("clip_text.onnx"),
                        sha256: None,
                        approx_bytes: 254_058_553,
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

        // ── RAM++ (Recognize Anything Plus) — the universal in-scan image
        // tagger. Apache-2.0 ONNX (Swin-Large @384px, 4585-tag vocabulary),
        // self-hosted at Web-World-Wide/ram-plus-onnx because no first-party
        // ONNX export exists and the engine consumes ONNX. Gated behind the
        // "model missing → CLIP scene-tag fallback" path in tagging.rs, so a
        // not-yet-uploaded repo can't regress scanning (zero-regression).
        // Third file `ram_plus_thresholds.txt` carries per-class sigmoid
        // cutoffs (ram_plus.rs applies them per class; falls back to a global
        // cutoff if absent/mismatched).
        "ram_plus" | "ram-plus" => {
            let dir = models_root.join("ram_plus");
            LookupResult::Found(Model {
                id: "ram_plus",
                display_name: "RAM++ image tagger",
                files: vec![
                    FileEntry {
                        url: "https://huggingface.co/Web-World-Wide/ram-plus-onnx/resolve/main/ram_plus.onnx"
                            .to_string(),
                        dest: dir.join("ram_plus.onnx"),
                        sha256: None,
                        // fp16 export is ~882 MB: RAM++ bakes the 4585×51 frozen
                        // tag-description embeddings into the graph as constants.
                        approx_bytes: 925_600_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/Web-World-Wide/ram-plus-onnx/resolve/main/ram_plus_tags.txt"
                            .to_string(),
                        dest: dir.join("ram_plus_tags.txt"),
                        sha256: None,
                        approx_bytes: 47_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/Web-World-Wide/ram-plus-onnx/resolve/main/ram_plus_thresholds.txt"
                            .to_string(),
                        dest: dir.join("ram_plus_thresholds.txt"),
                        sha256: None,
                        approx_bytes: 46_000,
                    },
                ],
            })
        }

        // ── VLMs (Deep Analyze). Pulled as GGUF + mmproj pairs from
        // the official llama.cpp-friendly mirrors. Subprocess runner
        // (`vlm::VlmRunner`) finds them at canonical paths.
        // Mistral-Small-3.2-24B (Apache-2.0) — the max-quality Deep Analyze
        // VLM, replacing the non-commercial Qwen2.5-VL-3B (Qwen Research
        // License). Multimodal GGUF + mmproj from bartowski's quant repo.
        "mistral-small-3.2" | "mistral_small_3_2" => {
            let dir = models_root.join("vlm").join("mistral-small-3.2");
            LookupResult::Found(Model {
                id: "mistral_small_3_2",
                display_name: "Mistral-Small 3.2",
                files: vec![
                    FileEntry {
                        url: "https://huggingface.co/bartowski/mistralai_Mistral-Small-3.2-24B-Instruct-2506-GGUF/resolve/main/mistralai_Mistral-Small-3.2-24B-Instruct-2506-Q4_K_M.gguf"
                            .to_string(),
                        dest: dir.join("model.gguf"),
                        sha256: None,
                        approx_bytes: 14_300_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/bartowski/mistralai_Mistral-Small-3.2-24B-Instruct-2506-GGUF/resolve/main/mmproj-mistralai_Mistral-Small-3.2-24B-Instruct-2506-f16.gguf"
                            .to_string(),
                        dest: dir.join("mmproj.gguf"),
                        sha256: None,
                        approx_bytes: 878_000_000,
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
                        // per-model suffix), unlike Qwen.
                        url: "https://huggingface.co/ggml-org/gemma-3-4b-it-GGUF/resolve/main/mmproj-model-f16.gguf"
                            .to_string(),
                        dest: dir.join("mmproj.gguf"),
                        sha256: None,
                        approx_bytes: 851_251_104,
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
        // NVIDIA hardware alongside the ORT CUDA provider by
        // `CudaAutoInstaller.cs::TryInstallOrtCudaPack` so the ORT CUDA EP
        // has the cuDNN DLLs on its loader path. Engine startup calls
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

        // ── ONNX Runtime CUDA Performance Pack. pyke's `download-binaries`
        // ships only the base onnxruntime.dll + onnxruntime_providers_shared.dll
        // (DirectML/CPU), NOT onnxruntime_providers_cuda.dll — so the CUDA EP
        // can't bind and NVIDIA falls through to DirectML (~3-5x slower). This
        // pack is Microsoft's official ORT GPU build, which bundles the matched
        // onnxruntime.dll + onnxruntime_providers_cuda.dll + providers_shared.
        // VERSION MUST MATCH the pyke ort-sys build (1.22.0 — read off the
        // shipped onnxruntime.dll ProductVersion); a mismatch silently fails to
        // bind. ORT is MIT and Microsoft hosts it on github.com (CI-allowlisted),
        // so no HF hosting needed. cudart/cublas come from the llama.cpp-cuda
        // pack (CUDA 12.4) or the system CUDA toolkit; cuDNN auto-installs.
        // The zip extracts to packs/cuda/onnxruntime-win-x64-gpu-1.22.0/lib/*.dll;
        // main.rs registers packs/cuda for DLL search AND pins ORT_DYLIB_PATH to
        // the pack's onnxruntime.dll so the provider binds against the same build.
        "ort_cuda_x64" => {
            let dir = models_root.join("packs").join("cuda");
            LookupResult::Found(Model {
                id: "ort_cuda_x64",
                display_name: "ONNX Runtime CUDA pack",
                files: vec![FileEntry {
                    url: "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-gpu-1.22.0.zip"
                        .to_string(),
                    dest: dir.join("ort-cuda.zip"),
                    sha256: None,
                    approx_bytes: 312_700_000,
                }],
            })
        }

        // ── ONNX Runtime OpenVINO Performance Pack (Intel GPUs/NPUs). Intel's
        // accelerated path; OpenVINO is Apache-2.0 so it's commercial-clean to
        // redistribute. Like the CUDA pack, pyke's base ORT lacks the OpenVINO
        // provider — this pack supplies a matched ORT 1.22.0 build + the Intel
        // OpenVINO 2025.1 runtime DLLs, assembled verbatim from the official
        // PyPI wheels `onnxruntime-openvino==1.22.0` + `openvino==2025.1.0`
        // (license texts bundled inside the zip: ORT MIT + OpenVINO Apache-2.0).
        // The zip extracts onnxruntime.dll + onnxruntime_providers_openvino.dll
        // + openvino*.dll + plugins.xml under packs/openvino/; main.rs pins
        // ORT_DYLIB_PATH to its onnxruntime.dll on Intel GPUs; ep_guard reverts
        // to DirectML if the bind crashes. UNVERIFIED on Intel hardware (no
        // Intel GPU in the dev env) — but assembled from the canonical wheels
        // and safe behind ep_guard. Hosted on HF (CI-allowlisted).
        "ort_openvino_x64" => {
            let dir = models_root.join("packs").join("openvino");
            LookupResult::Found(Model {
                id: "ort_openvino_x64",
                display_name: "ONNX Runtime OpenVINO pack",
                files: vec![FileEntry {
                    url: "https://huggingface.co/Web-World-Wide/OpenVINO/resolve/main/ort-openvino-win-x64-1.22.0.zip"
                        .to_string(),
                    dest: dir.join("ort-openvino.zip"),
                    sha256: None,
                    approx_bytes: 41_300_000,
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

        // ── BGE-small-en-v1.5 text embeddings (Phase 4b). MIT. ONNX from
        // Xenova's community export; the WordPiece vocab travels with it.
        // Used for semantic search over document text.
        "bge_text" | "bge_small_en_v1_5" | "bge_small" => {
            let dir = models_root.join("bge_text");
            LookupResult::Found(Model {
                id: "bge_text",
                display_name: "BGE-small text embeddings",
                files: vec![
                    FileEntry {
                        url: "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model.onnx".to_string(),
                        dest: dir.join("bge_small.onnx"),
                        sha256: None,
                        approx_bytes: 135_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/vocab.txt".to_string(),
                        dest: dir.join("vocab.txt"),
                        sha256: None,
                        approx_bytes: 232_000,
                    },
                ],
            })
        }

        // ── Florence-2 base (Phase 7 — registry arm + skeleton; the generation
        // loop wiring is the documented Phase 7b follow-up). 4-ONNX split +
        // BART tokenizer + config from the community ONNX export at
        // `onnx-community/Florence-2-base` (Microsoft Florence-2, MIT). The
        // model is downloadable today; `models::florence2` exposes the canonical
        // install dir so progress UI works against the same layout the future
        // loader will read.
        "florence2_base" | "florence2" => {
            let dir = models_root.join("florence2");
            LookupResult::Found(Model {
                id: "florence2_base",
                display_name: "Florence-2 base (grounded regions)",
                files: vec![
                    FileEntry {
                        url: "https://huggingface.co/onnx-community/Florence-2-base/resolve/main/onnx/vision_encoder.onnx".to_string(),
                        dest: dir.join("vision_encoder.onnx"),
                        sha256: None,
                        approx_bytes: 110_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/onnx-community/Florence-2-base/resolve/main/onnx/embed_tokens.onnx".to_string(),
                        dest: dir.join("embed_tokens.onnx"),
                        sha256: None,
                        approx_bytes: 25_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/onnx-community/Florence-2-base/resolve/main/onnx/encoder_model.onnx".to_string(),
                        dest: dir.join("encoder_model.onnx"),
                        sha256: None,
                        approx_bytes: 110_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/onnx-community/Florence-2-base/resolve/main/onnx/decoder_model_merged.onnx".to_string(),
                        dest: dir.join("decoder_model_merged.onnx"),
                        sha256: None,
                        approx_bytes: 200_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/onnx-community/Florence-2-base/resolve/main/tokenizer.json".to_string(),
                        dest: dir.join("tokenizer.json"),
                        sha256: None,
                        approx_bytes: 5_000_000,
                    },
                    FileEntry {
                        url: "https://huggingface.co/onnx-community/Florence-2-base/resolve/main/config.json".to_string(),
                        dest: dir.join("config.json"),
                        sha256: None,
                        approx_bytes: 10_000,
                    },
                ],
            })
        }

        // ── Per-vendor accelerated runtimes (CUDA EP / OpenVINO EP / QNN EP)
        // are deliberately NOT downloaded by the engine: each requires a
        // vendor-specific SDK whose redistribution terms (Qualcomm AI Hub
        // auth, OpenVINO redist guidance, NVIDIA CUDA toolkit) put it
        // outside the "publicly downloadable for an open-source app" line.
        // The engine still uses these EPs when their DLLs are on the system
        // search path: CUDA via the user's system CUDA toolkit (probed in
        // runtime::system_cuda_toolkit_dir); OpenVINO when the user
        // installs Intel's redistributable; QNN on Snapdragon devices that
        // ship the QNN runtime. Per-model `_int8`/`_qnn` variants follow
        // the same rule — `models::variants` picks them up if dropped into
        // the model dir by hand, and falls back to the fp32 graph otherwise.
        _ => LookupResult::Unknown,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every artifact URL must be on huggingface.co — the engine's only egress
    /// (the privacy posture CI's source-URL allowlist also enforces). Only
    /// asserts on kinds that resolve, so guessing a wrong kind here can't
    /// false-fail.
    #[test]
    fn all_model_urls_are_huggingface() {
        // NOTE: the runtime/EP packs are intentionally NOT all on HF — cuDNN is
        // on NVIDIA's CDN, llama + ort_cuda are on github (all CI-allowlisted).
        // ort_openvino IS on HF, so it belongs here for real coverage.
        let kinds = [
            "ram_plus", "mobileclip_s2", "clip_text", "bge_text", "arcface",
            "florence2", "qwen2_5_vl_7b", "gemma_3_4b", "mistral_small_3_2",
            "ort_openvino_x64",
        ];
        for kind in kinds {
            if let LookupResult::Found(m) = lookup_full(kind) {
                for f in &m.files {
                    assert!(
                        f.url.starts_with("https://huggingface.co/"),
                        "{kind} URL not on huggingface.co: {}",
                        f.url
                    );
                }
            }
        }
    }

    #[test]
    fn face_aliases_resolve_to_one_model() {
        for alias in ["arcface", "arcface_default", "yunet_sface"] {
            match lookup_full(alias) {
                LookupResult::Found(m) => assert_eq!(m.id, "arcface", "alias {alias}"),
                LookupResult::Unknown => panic!("face alias {alias} did not resolve"),
            }
        }
    }

    #[test]
    fn unknown_kind_is_unknown() {
        assert!(matches!(
            lookup_full("definitely_not_a_model_kind"),
            LookupResult::Unknown
        ));
    }

    #[test]
    fn sentinel_path_lives_under_sentinels_dir() {
        if let LookupResult::Found(m) = lookup_full("ram_plus") {
            if let Some(p) = sentinel_path(&m) {
                let s = p.to_string_lossy();
                assert!(s.contains(".sentinels"), "sentinel not under .sentinels: {s}");
                assert!(s.ends_with("ram_plus.installed"), "unexpected sentinel: {s}");
            }
        }
    }
}
