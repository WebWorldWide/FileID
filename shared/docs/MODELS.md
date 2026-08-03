# Models — canonical registry

FileID never ships model weights. Every model is downloaded at runtime from its upstream repository, with progress + cancellation visible to the user, after they explicitly trigger the download. Every artifact is SHA256-pinned in `shared/models/manifest.json`; the Windows registry and macOS `ModelManifest.swift` are compiled mirrors checked by CI. Platform downloaders verify bytes before atomic promotion, and CI rejects unpinned artifacts. No telemetry on the download.

This file documents the cross-platform model contract. Production installs run only through each app's verified in-app downloader; obsolete direct-download shell installers were removed so there is no unpinned alternate path.

## Licensing posture — commercial-clean (Apache-2.0 project)

The core weight stack is permissively licensed (Apache-2.0 / MIT), and no non-commercial weights are allowed. The non-commercial InsightFace face stack (ArcFace + SCRFD) and research-only Apple MobileCLIP-S2 / Qwen2.5-VL-3B were replaced. Gemma models remain available under Google's separate Gemma Terms, and optional NVIDIA cuDNN/CUDA runtime archives remain under NVIDIA's vendor terms. Before the first such download, FileID presents the applicable full-terms link and a default-cancel **I Accept and Download** decision; acceptance is recorded locally and versioned by the policy review date.

`shared/models/manifest.json` is the machine-enforced license registry as well as the artifact registry. `licensePolicies`, `artifactLicenses`, and `vlmRepoLicenses` must cover every downloadable entry. `shared/scripts/check_model_license_policy.py` rejects missing/unknown mappings, malformed license URLs/review dates, or a restricted policy incorrectly marked as not requiring terms acceptance. Any new model or runtime requires a reviewed manifest policy before CI permits it.

> **Both Windows and macOS are on the commercial-clean stack (updated 2026-07).** The macOS Swift swap (RAM++ tagger, ViT-B/32, SFace 128-d) has **landed on `main` and is wired as primary** — verified statically in `shared/docs/MACOS_AUDIT_2026-07.md`. The *(lockstep pending)* markers on macOS cells below now mean **on-hardware embedding-parity verification is pending**, NOT that the Swift swap is unapplied. Cross-platform DB round-trips (esp. 128-d face prints) work once both engines have run on real hardware with the new models; until the macOS on-hardware parity check is done, treat face DBs as platform-local as a precaution.

## ML stack per platform

| Capability | macOS | Windows | Notes |
|---|---|---|---|
| In-scan image tagging (primary) | RAM++ Swin-L @384 *(lockstep pending)* | **RAM++ Swin-L @384 (ONNX, fp16)** | Recognize Anything Plus, 4585-tag multi-label tagger, Apache-2.0. Primary auto-tagger; CLIP zero-shot scene tags are the fallback when RAM++ isn't installed. |
| Image semantic embedding (search) | CLIP ViT-B/32 (CoreML) *(lockstep pending)* | **CLIP ViT-B/32 (ONNX)** | OpenAI/OpenCLIP ViT-B/32, MIT. 512-d float32 LE, L2-normalized — embeddings byte-cross-compatible across platforms. |
| Text semantic embedding (CLIP) | CLIP ViT-B/32 text (CoreML) + BPE vocab *(lockstep pending)* | **CLIP ViT-B/32 text (ONNX)** + BPE vocab | Same OpenAI BPE tokenizer port; embeddings cross-compatible. |
| Face detection + 5-pt landmarks | Apple Vision (`VNDetectFaceRectanglesRequest`) | **YuNet (ONNX, OpenCV Zoo)** | YuNet is MIT. Different detectors → boxes aren't byte-identical, but 5-pt landmarks feed a shared alignment template so embeddings match. |
| Face embedding | SFace (ONNX via CoreML EP) *(lockstep pending)* | **SFace (ONNX via DirectML / CUDA / CPU EP)** | SFace (OpenCV Zoo) is Apache-2.0, **128-d** L2-normalized. Replaces 512-d ArcFace; person-clustering DBs round-trip once both platforms are on SFace. |
| OCR | Apple Vision `VNRecognizeTextRequest` (fast tier) | Windows.Media.Ocr (built-in WinRT) default; PaddleOCR ONNX opt-in | Built-in OCR is fast + free + multilingual on both. |
| Vision-language models (Deep Analyze) | MLX: **Qwen3-VL 8B / 4B** · Qwen 2.5-VL · Gemma 3 · Mistral-Small-3.2 | llama.cpp: Qwen 2.5-VL 7B · Gemma 3 · Mistral-Small-3.2 | MLX is Apple-Silicon-only; llama.cpp covers Windows on every GPU. Qwen3-VL 8B is the measured 16 GB macOS recommendation and 4B is the 8 GB recommendation; each platform uses the best-supported commercial-clean quant. |

## In-scan tagger

### RAM++ (Recognize Anything Plus) image tagger

| Aspect | Value |
|---|---|
| Source | [`Web-World-Wide/ram-plus-onnx`](https://huggingface.co/Web-World-Wide/ram-plus-onnx) — `ram_plus.onnx` + `ram_plus_tags.txt` + `ram_plus_thresholds.txt` (self-hosted ONNX export of `xinyu1205/recognize-anything-plus-model`) |
| License | **Apache-2.0** (model + code) |
| Architecture | Swin-L backbone @384px, multi-label head over a 4585-tag vocabulary |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\ram_plus\{ram_plus.onnx, ram_plus_tags.txt, ram_plus_thresholds.txt}` |
| Input | 384×384 RGB, ImageNet mean/std normalized, NCHW |
| Output | 4585 logits → per-class sigmoid; emitted when above the per-class threshold (`ram_plus_thresholds.txt`, index-aligned). `FILEID_RAMPLUS_THRESHOLD` overrides globally. Top ~12 tags/image. |
| Precision | fp16 default (~882 MB) with fp32 I/O + sensitive ops blocked; fp32/int8/NPU variants drop in via `variants::resolve_model_path`. |
| Tag | tags stored in `tags(source='auto')`. When RAM++ is present it is the tagger; CLIP zero-shot scene tags are gated off (run only as fallback). |

## Embedders + OCR — model registry

Files live under each platform's models directory. Downloads triggered by the welcome-sheet onboarding (or Settings) on first launch.

### CLIP ViT-B/32 image encoder

| Aspect | Value |
|---|---|
| Source (macOS) | OpenAI/OpenCLIP ViT-B/32 CoreML `.mlpackage` *(lockstep pending — WS-MAC)* |
| Source (Windows) | [`Xenova/clip-vit-base-patch32`](https://huggingface.co/Xenova/clip-vit-base-patch32) — `onnx/vision_model.onnx` (community ONNX export of OpenAI's MIT CLIP) |
| License | **MIT** (OpenAI CLIP) |
| macOS layout | `~/Library/Application Support/FileID/Models/mobileclip_image/` (CoreML `.mlpackage`) *(lockstep pending)* |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\mobileclip\mobileclip_s2_image.onnx` (dir/filename kept as a stable key through the swap; contents are ViT-B/32) |
| Input | 224×224 RGB, CLIP mean/std normalized |
| Output | 512-d float32, L2-normalized |
| Tag | `mobileclip_s2` (stored in `clip_embeddings.model`; kept as a stable key, no schema churn) |

### CLIP text encoder

| Aspect | Value |
|---|---|
| Source (macOS) | [`openai/clip-vit-base-patch32`](https://huggingface.co/openai/clip-vit-base-patch32) (ONNX export) *(lockstep pending)* |
| Source (Windows) | [`Xenova/clip-vit-base-patch32`](https://huggingface.co/Xenova/clip-vit-base-patch32) — `onnx/text_model.onnx`. BPE vocab + merges from [`openai/clip-vit-base-patch32`](https://huggingface.co/openai/clip-vit-base-patch32) (ViT-B/32's own tokenizer). |
| License | **MIT** (OpenAI CLIP + tokenizer) |
| macOS layout | `~/Library/Application Support/FileID/Models/clip_text/` (CoreML `.mlpackage` + `vocab.json` + `merges.txt`) |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\clip_text\clip_text.onnx` + `vocab.json` + `merges.txt` |

### BGE-small text embeddings (Windows — semantic doc search)

| Aspect | Value |
|---|---|
| Source | [`Xenova/bge-small-en-v1.5`](https://huggingface.co/Xenova/bge-small-en-v1.5) — `onnx/model.onnx` + `vocab.txt` (community ONNX export of BAAI's MIT-licensed BGE) |
| License | MIT |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\bge_text\{bge_small.onnx, vocab.txt}` |
| Input | WordPiece tokens up to 256 — `input_ids` / `attention_mask` / `token_type_ids` (i64) |
| Output | last_hidden_state `(1, seq, 384)` → mean-pooled (mask-weighted) + L2-normalized to 384-d |
| Persistence | `text_embeddings(file_id, embedding BLOB, model)` (migration v11); the `model` column lets future text-embedding families coexist. |
| Role | Semantic search over extracted document text (Phase 4). Skipped when not installed; FTS5 (`doc_fts`) still serves keyword search. |

### Florence-2 base (Phase 7 — grounded regions, foundation only)

| Aspect | Value |
|---|---|
| Source | [`onnx-community/Florence-2-base`](https://huggingface.co/onnx-community/Florence-2-base) — `onnx/{vision_encoder,embed_tokens,encoder_model,decoder_model_merged}.onnx` + `tokenizer.json` + `config.json` |
| License | MIT (Microsoft Florence-2) |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\florence2\{vision_encoder,embed_tokens,encoder_model,decoder_model_merged}.onnx` + `tokenizer.json` + `config.json` |
| Approx size | ~445 MB total (vision + embed + encoder + decoder + tokenizer) |
| Role | **Phrase-grounded object detection** (`<OD>` / `<CAPTION_TO_PHRASE_GROUNDING>`) — the one capability not covered by the rest of the stack. |
| Status | Registry arm + `models::florence2` skeleton. **Inference is Phase 7b**. Build out when grounded OD becomes a concrete product need. |

## Faces — commercial-clean (YuNet + SFace)

The non-commercial InsightFace stack (ArcFace `w600k_r50` + SCRFD, *"non-commercial research only"*) was replaced by OpenCV Zoo's permissively-licensed pair. A v12 migration wipes `face_prints` / `persons` / `face_verifications` so 128-d SFace prints re-derive cleanly (old 512-d ArcFace prints are dimensionally incomparable). The `face_prints.model` column lets families coexist.

### YuNet face detection (Windows)

| Aspect | Value |
|---|---|
| Source | [`opencv/face_detection_yunet`](https://huggingface.co/opencv/face_detection_yunet) — `face_detection_yunet_2023mar.onnx` |
| License | **MIT** (OpenCV Zoo) |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\yunet\face_detection_yunet_2023mar.onnx` (~0.2 MB) |
| Input | letterboxed to 640×640, BGR raw [0,255], NCHW |
| Output | per-stride (8/16/32) cls/obj/bbox/kps → score = √(cls·obj), center/exp box, 5-point landmarks remapped to the FileID order |

### SFace face embedding (Windows; macOS via CoreML EP — native validation complete, cross-platform byte comparison pending)

| Aspect | Value |
|---|---|
| Source | [`opencv/face_recognition_sface`](https://huggingface.co/opencv/face_recognition_sface) — `face_recognition_sface_2021dec.onnx` |
| License | **Apache-2.0** (OpenCV Zoo) |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\sface\face_recognition_sface_2021dec.onnx` (~37 MB) |
| Input | aligned 112×112 RGB, **raw [0,255]** (the ONNX bakes its own `(x-127.5)/128` normalization) |
| Output | **128-d** float32, L2-normalized (`face_prints.print_data` = 512 bytes) |
| Alignment | 5-point similarity transform (least-squares, 4×4 normal equations) onto the ArcFace 112×112 template, shared with macOS so cross-platform embeddings agree |

> Install slot, sentinel (`.sentinels/arcface.installed`), and the pre-scan model gate keep the `arcface` model_kind id as a stable key — only the underlying files changed (YuNet + SFace). Re-tuned cluster cosine bands for SFace are provisional (anchored to OpenCV's ~0.36 same-identity threshold) pending labeled-corpus calibration.

### PaddleOCR (Windows opt-in)

| Aspect | Value |
|---|---|
| Source | TBD — published ONNX builds; pinned commit |
| License | Apache 2.0 |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\paddle_ocr\` |
| When used | Settings → Advanced → "Use PaddleOCR instead of built-in Windows.Media.Ocr" |

## Vision-language models — Deep Analyze

All default/recommended VLMs are commercial-clean (Apache-2.0). Gemma-3-4B is optional under Google's Gemma Terms (commercial use permitted; terms surfaced at install). The non-commercial Qwen2.5-VL-**3B** (Qwen Research License) was dropped in favor of the Apache-2.0 7B.

### Curated Windows lineup (llama.cpp GGUF Q4_K_M unless noted)

| Model | Size on disk | RAM est. | Use case | License | Source |
|---|---|---|---|---|---|
| **Qwen 2.5-VL 7B** | ~5 GB | ~12 GB | **Recommended default** (≥ 16 GB + dGPU) | Apache-2.0 | [Qwen/Qwen2.5-VL-7B-Instruct-GGUF](https://huggingface.co/Qwen) — pinned, GGUF + mmproj |
| **Gemma 3 4B (vision)** | ~3 GB | ~8 GB | Lighter / weak-box fallback | Gemma Terms (opt-in) | [google/gemma-3-4b-it](https://huggingface.co/google/gemma-3-4b-it) GGUF |
| **Mistral-Small-3.2 24B** | ~14.3 GB | ~20 GB | Max-quality captioner | Apache-2.0 | [bartowski/Mistral-Small-3.2 GGUF](https://huggingface.co/bartowski) + mmproj |

(Exact artifact SHA-256s and repository revisions live in `shared/models/manifest.json`; platform conformance tests lock their runtime tables to that canonical file.)

### macOS lineup (MLX)

| Model | Source | Notes |
|---|---|---|
| **Qwen3-VL 4B (4-bit)** | [`lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit`](https://huggingface.co/lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit) | **Recommended for 8 GB Macs** (Apache-2.0); ~3.5 GB download and 4.8 GiB measured peak footprint. |
| **Qwen3-VL 8B (4-bit)** | [`lmstudio-community/Qwen3-VL-8B-Instruct-MLX-4bit`](https://huggingface.co/lmstudio-community/Qwen3-VL-8B-Instruct-MLX-4bit) | **Recommended for 16 GB Macs** (Apache-2.0); exact 5,776,636,403-byte download and 7.2 GiB measured peak footprint. A six-image copied-Adlon A/B found it generally more concise and grounded than 4B. Revision `a0afc48efd9308fb14b4d58bbd49d382f7d4f845`. |
| Qwen 2.5-VL 7B | swift-transformers HF cache | Proven alternative (Apache-2.0); ~4.3 GB download and ~7 GB RAM. |
| Gemma 3 4B | swift-transformers HF cache | Opt-in (Gemma Terms) |
| Mistral-Small-3.2 | swift-transformers HF cache | Max-quality option for Macs with at least 30 GB RAM (Apache-2.0). |

## VLM storage

VLMs cache to:
- macOS: `~/Documents/huggingface/models/<repo>/` (MLX / swift-transformers convention)
- Windows: `%LOCALAPPDATA%\FileID\Models\HuggingFace\<repo>\` (FileID's own download path; outside Documents to avoid surprising users with several GB in there)

## Audio + 3D understanding — Deep Analyze (license-vetted)

Deep Analyze already names audio + `.obj` from EMBEDDED metadata (tags / object names) with no model.
*True* on-device AI understanding of those types is layered on top — all commercial-clean (Apache-2.0 / MIT),
download-from-`huggingface.co` only (or built-in OS frameworks on macOS), opt-in like the VLMs. Owner-approved
2026-06-17 ("use other models as long as they follow the licenses"). **Status as of 2026-06-17 below.**

### Whisper — speech transcription (audio) — **MIT** — ✅ SHIPPED (both platforms)

| Aspect | Value |
|---|---|
| Use | Transcribe spoken audio (voice memos, podcasts, lectures) → a descriptive name + caption. Music keeps the metadata path (title/artist); speech gets content. The `name_from_transcript` logic (first ~8 words → name) is byte-faithful across engines. |
| Windows | **whisper.cpp** subprocess (`WhisperRunner`, mirrors the llama.cpp VLM pattern) — the CPU pack ([`ggml-org/whisper.cpp` v1.9.0 release](https://github.com/ggml-org/whisper.cpp)) + the multilingual `ggml-base` model ([`ggerganov/whisper.cpp`](https://huggingface.co/ggerganov/whisper.cpp), ~148 MB), both sha256-pinned in `registry.rs` as `"whisper"`. Audio → 16 kHz mono WAV via the `symphonia` we already ship (`pipeline::audio_decode`). Installed from Settings → its "Speech transcription (Whisper)" card. |
| macOS | **Apple Speech** (`SFSpeechRecognizer`, `requiresOnDeviceRecognition`) — the built-in-framework analogue, no model download. Needs `NSSpeechRecognitionUsageDescription` (added to the app Info.plist; the engine's `Bundle.main` resolves to the enclosing app). |
| License | **MIT** (OpenAI Whisper code + weights; whisper.cpp port also MIT). Apple Speech is OS-provided. |

### Sound-event classification (non-speech audio) — macOS ✅ SHIPPED · Windows ⏳ DEFERRED

| Aspect | Value |
|---|---|
| Use | For audio with no metadata title AND no speech (field recordings, sound effects, ambience): classify the dominant sound → a descriptive name (rain → "Rain", a dog bark → "Dog Bark"). The cascade is metadata title → speech transcript → sound event → original name. |
| macOS | **Apple SoundAnalysis** (`SNClassifySoundRequest .version1`) — built-in classifier, no model, no microphone permission (file analysis). `nameFromSoundLabel` humanizes + drops generic labels (speech/music/noise). Shipped. |
| Windows (YAMNet) | **Deferred, tracked.** YAMNet (TF-Hub, Apache-2.0) → ONNX would reuse the existing ONNX Runtime, BUT needs (a) a license-vetted self-hosted ONNX *and* (b) a hand-rolled log-mel (STFT) frontend — the common ONNX exports take a `(64,96,1)` log-mel patch, not a waveform, and there's no FFT crate in the locked set. That DSP **can't be verified without on-hardware labeled audio**, so it isn't shipped blind. Phase 1 Whisper already covers the speech case on Windows; this only adds the narrow non-speech tail. See NEXT.md. |
| License | YAMNet **Apache-2.0**; Apple SoundAnalysis is OS-provided. |

### 3D models (`.obj`) — render → existing VLM (NO new model) — ✅ SHIPPED (both platforms)

| Aspect | Value |
|---|---|
| Use | Render the `.obj` to an image → feed the **already-installed Deep Analyze VLM** (Qwen2.5-VL / Gemma 3) → caption + name. The AI literally "looks at" the model. Falls back to embedded object/material names (no VLM, an unrenderable `.obj`, or a VLM failure). |
| Windows | A **hand-rolled software rasterizer** (`pipeline::obj_render`, NO new dependency) — parses `.obj`/`.mtl`, fixed 3/4 camera, z-buffered flat-shaded triangles (per-face `.mtl` Kd + one Lambert light), 512² PNG via the `image` crate. Wired as the `"model"` arm of `rasterize_for_vlm`. |
| macOS | The **OS QuickLook 3D generator** (`DeepAnalyze.quickLookThumbnail`, already the loader's fallback) renders the `.obj` → the MLX VLM. No new code, no new dep. |
| License | No new weights. No new dependency on either platform. |

All build-verifiable in the dev env; on-device inference (VLM/Speech/SoundAnalysis quality) is hardware-verified
(owner's RTX 2060 / Mac), like every other model. The metadata paths stay as the no-model fallback + the
music/named-object fast path.

## Performance Packs (Windows GPU runtimes)

Optional. Settings → Performance → "Get faster on this hardware". Auto-suggested when matching hardware is detected. Same downloader pattern as model downloads.

| Pack | Size | Activates EP | Hardware target |
|---|---|---|---|
| NVIDIA CUDA Pack | ~2.1 GB | ORT CUDA EP + full CUDA 12.9 math runtime (cudart/cublas/cuFFT/NVRTC) + cuDNN 9.8 + llama.cpp CUDA backend | NVIDIA GPUs (any RTX-class incl. Blackwell/RTX 50) |
| Intel OpenVINO Pack | ~300 MB | ORT OpenVINO EP | Intel iGPU + Arc dGPU |
| Snapdragon NPU Pack | ~150 MB | ORT QNN EP + (when available) llama.cpp QNN backend | Snapdragon X Elite (Hexagon NPU) on WoA |

Each pack has its own canonical URL + SHA256 list. Performance Packs do not contain user data and never report installation back. They install into `%LOCALAPPDATA%\FileID\runtimes\<pack-name>\` and the engine adds them to its DLL search path. **Without a CUDA Pack, NVIDIA cards run on DirectML (~3–5× slower for ML inference but fully functional) — verified on an RTX 2060.**

## Why we pull from upstream rather than redistribute

- **Licensing.** Even with a commercial-clean (Apache/MIT) weight set, we want users to see *exactly* where their model came from rather than trusting a re-host.
- **Auditability.** A user can verify the SHA256 against the upstream HuggingFace repo independently. Mirrored weights are a target for supply-chain attacks. (RAM++ is the one model we self-host — an unmodified Apache-2.0 ONNX export — because no upstream ONNX exists; it is SHA-pinned the same way.)
- **Privacy.** Downloads go user → HF directly. FileID isn't a hop. Network-capture verification is straightforward.
- **Bundle size.** Models add up to several GB. Shipping a lean app + on-demand downloads keeps the install fast.
