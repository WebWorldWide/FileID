# Models — canonical registry

## macOS CLIP cache identity (2026-10-02)

The native shared manifest pins OpenCLIP ViT-B/32 image/text ONNX and BPE vocabulary/merges. Mac loaders verify local bytes against those pins before creating sessions. Cache identity also includes the current RGB stretch-to-224, BPE-77 and L2-normalization contract. Legacy `mobileclip_s2` rows are excluded until fresh inference refreshes them. This is compatibility validation, not promotion of a replacement model or a measured accuracy gain.

FileID never ships model weights. Every model is downloaded at runtime from its upstream repository, with progress + cancellation visible to the user, after they explicitly trigger the download. **Every artifact is SHA256-pinned in `engine/src/models/registry.rs`** — the canonical hash is the `oid sha256:` from each HuggingFace LFS pointer (or the sha256 of the GitHub/NVIDIA release asset); the engine downloader verifies the downloaded bytes against the pin before use, and a CI gate (`windows-engine.yml`) fails the build on any unpinned (`sha256: None`) entry. No telemetry on the download.

This file is the cross-platform source of truth for what FileID asks for and where it lives. Per-platform installers (`platforms/apple/scripts/install_clip_models.sh`, `platforms/windows/build/install-models.ps1`, future Linux equivalent) read this list.

## Licensing posture — commercial-clean (Apache-2.0 project)

As of the 2026-05 commercial-clean pass, **every weight FileID downloads by default is permissively licensed (Apache-2.0 / MIT)** — no non-commercial weights in the core feature set. This keeps the project (Apache-2.0, see root `LICENSE`) free to be open-sourced *and* commercialized later without a weight-licensing blocker. The non-commercial InsightFace face stack (ArcFace + SCRFD) and the research-only Apple MobileCLIP-S2 / Qwen2.5-VL-3B were replaced. The one conditional model, Gemma-3-4B, is commercially usable under Google's Gemma Terms and stays an opt-in, user-initiated download (its terms surface in the install flow).

Both engines now implement RAM++, ViT-B/32 and SFace. Cross-platform face round trips validate provenance and compatible processing spaces; dimensions alone do not establish compatibility. Hardware accuracy and complete port UI acceptance remain separate gates.

## ML stack per platform

| Capability | macOS | Windows | Notes |
|---|---|---|---|
| In-scan image tagging (primary) | RAM++ Swin-L @384 (ONNX) | **RAM++ Swin-L @384 (ONNX, fp16)** | Recognize Anything Plus, 4585-tag multi-label tagger, Apache-2.0. Primary auto-tagger; CLIP zero-shot scene tags are the fallback when RAM++ isn't installed. |
| Image semantic embedding (search) | CLIP ViT-B/32 (ONNX, CoreML EP/CPU) | **CLIP ViT-B/32 (ONNX)** | OpenAI/OpenCLIP ViT-B/32, MIT. 512-d float32 LE, L2-normalized — embeddings byte-cross-compatible across platforms. |
| Text semantic embedding (CLIP) | CLIP ViT-B/32 text (ONNX) + BPE vocab | **CLIP ViT-B/32 text (ONNX)** + BPE vocab | Same OpenAI BPE tokenizer port; embeddings cross-compatible. |
| Face detection + 5-pt landmarks | Apple Vision (`VNDetectFaceRectanglesRequest`) | **YuNet (ONNX, OpenCV Zoo)** | YuNet is MIT. Different detectors → boxes aren't byte-identical, but 5-pt landmarks feed a shared alignment template so embeddings match. |
| Face embedding | SFace (ONNX via CoreML EP/CPU) | **SFace (ONNX via DirectML / CUDA / CPU EP)** | SFace (OpenCV Zoo) is Apache-2.0, **128-d** L2-normalized. Replaces 512-d ArcFace; person-clustering DBs round-trip once both platforms are on SFace. |
| OCR | Apple Vision `VNRecognizeTextRequest` (fast tier) | Windows.Media.Ocr (built-in WinRT) default; PaddleOCR ONNX opt-in | Built-in OCR is fast + free + multilingual on both. |
| Vision-language models (Deep Analyze) | MLX: Qwen2.5-VL · Qwen3-VL · Gemma 3 · Mistral Small 3.2 · PaliGemma | llama.cpp: Qwen 2.5-VL 7B · Gemma 3 · Mistral-Small-3.2 | MLX is Apple-Silicon-only; llama.cpp covers Windows on every GPU. Curated lineup per platform to use the best-supported quants. |

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
| Source (macOS) | Pinned ViT-B/32 ONNX through ONNX Runtime; CoreML execution provider is attempted with CPU fallback (see `MobileCLIPService.swift`) |
| Source (Windows) | [`Xenova/clip-vit-base-patch32`](https://huggingface.co/Xenova/clip-vit-base-patch32) — `onnx/vision_model.onnx` (community ONNX export of OpenAI's MIT CLIP) |
| License | **MIT** (OpenAI CLIP) |
| macOS layout | `~/Library/Application Support/FileID/Models/mobileclip_image/clip_vitb32_image.onnx` |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\mobileclip\mobileclip_s2_image.onnx` (dir/filename kept as a stable key through the swap; contents are ViT-B/32) |
| Input | 224×224 RGB, CLIP mean/std normalized |
| Output | 512-d float32, L2-normalized |
| Cache identity | macOS uses `CLIPEmbeddingSpace.modelID`, tied to pinned artifacts and preprocessing. Legacy `mobileclip_s2` rows need fresh inference; Windows retains its existing registry key pending PC implementation. |

### CLIP text encoder

| Aspect | Value |
|---|---|
| Source (macOS) | [`Xenova/clip-vit-base-patch32`](https://huggingface.co/Xenova/clip-vit-base-patch32) — pinned `onnx/text_model.onnx`; tokenizer artifacts from the OpenAI model |
| Source (Windows) | [`Xenova/clip-vit-base-patch32`](https://huggingface.co/Xenova/clip-vit-base-patch32) — `onnx/text_model.onnx`. BPE vocab + merges from [`openai/clip-vit-base-patch32`](https://huggingface.co/openai/clip-vit-base-patch32) (ViT-B/32's own tokenizer). |
| License | **MIT** (OpenAI CLIP + tokenizer) |
| macOS layout | `~/Library/Application Support/FileID/Models/clip_text/clip_text.onnx` + `vocab.json` + `merges.txt` |
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

### SFace face embedding (Windows; macOS via CoreML EP — lockstep pending)

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

(Exact pinned commits + SHA256s live in the platform-specific installer scripts, so the doc isn't a SHA copy-pasta target.)

### macOS lineup (MLX)

Current `AIModels.swift` exposes seven model kinds; repositories/revisions are pinned in the shared manifest and mirrored by `ModelManifest.swift`. Qwen3-VL is available on macOS; Windows/Linux Qwen3-VL runtime parity is still pending.

| Model | Source repository | Notes |
|---|---|---|
| Qwen2.5-VL 7B | `mlx-community/Qwen2.5-VL-7B-Instruct-4bit` | Apache-2.0 |
| Qwen3-VL 4B | `lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit` | Apache-2.0; compact current option |
| Qwen3-VL 8B | `lmstudio-community/Qwen3-VL-8B-Instruct-MLX-4bit` | Apache-2.0; current 16 GB recommendation |
| Gemma 3 4B / 12B | `mlx-community/gemma-3-{4b,12b}-it-qat-4bit` | Gemma Terms; explicit acceptance |
| Mistral Small 3.2 24B | `mlx-community/Mistral-Small-3.2-24B-Instruct-2506-4bit` | Apache-2.0; larger-memory option |
| PaliGemma 3B | `mlx-community/paligemma-3b-mix-448-8bit` | Gemma Terms; explicit acceptance |

## VLM storage

VLMs cache to:
- macOS: `~/Documents/huggingface/models/<repo>/` (MLX / swift-transformers convention)
- Windows: `%LOCALAPPDATA%\FileID\Models\HuggingFace\<repo>\` (FileID's own download path; outside Documents to avoid surprising users with several GB in there)

## Performance Packs (Windows GPU runtimes)

Optional. Settings → Performance → "Get faster on this hardware". Auto-suggested when matching hardware is detected. Same downloader pattern as model downloads.

| Pack | Size | Activates EP | Hardware target |
|---|---|---|---|
| NVIDIA CUDA Pack | ~600 MB | ORT CUDA EP + cuDNN runtime + llama.cpp CUDA backend | NVIDIA GPUs (any RTX-class) |
| Intel OpenVINO Pack | ~300 MB | ORT OpenVINO EP | Intel iGPU + Arc dGPU |
| Snapdragon NPU Pack | ~150 MB | ORT QNN EP + (when available) llama.cpp QNN backend | Snapdragon X Elite (Hexagon NPU) on WoA |

Each pack has its own canonical URL + SHA256 list. Performance Packs do not contain user data and never report installation back. They install into `%LOCALAPPDATA%\FileID\runtimes\<pack-name>\` and the engine adds them to its DLL search path. **Without a CUDA Pack, NVIDIA cards run on DirectML (~3–5× slower for ML inference but fully functional) — verified on an RTX 2060.**

## Why we pull from upstream rather than redistribute

- **Licensing.** Even with a commercial-clean (Apache/MIT) weight set, we want users to see *exactly* where their model came from rather than trusting a re-host.
- **Auditability.** A user can verify the SHA256 against the upstream HuggingFace repo independently. Mirrored weights are a target for supply-chain attacks. (RAM++ is the one model we self-host — an unmodified Apache-2.0 ONNX export — because no upstream ONNX exists; it is SHA-pinned the same way.)
- **Privacy.** Downloads go user → HF directly. FileID isn't a hop. Network-capture verification is straightforward.
- **Bundle size.** Models add up to several GB. Shipping a lean app + on-demand downloads keeps the install fast.

## Next-version research candidates — not installed or promoted

Primary-source research checked October 1, 2026, with the runtime follow-up below verified October 2. These candidates are not new shipping registry entries. Exact converted artifacts, immutable revisions/hashes, processor compatibility, runtime support, commercial distribution policy, and FileID benchmarks are still required. Weight-only size is not a memory budget: reserve OS/UI memory, KV/context caches, vision buffers, decoder buffers, and other resident models.

| Role | Initial candidate | Comparisons | Source/license review |
|---|---|---|---|
| Fast chat/request parsing | Qwen3.5 2B; 0.8B constrained tier | Granite 4.0 Micro; Ministral 3 3B | [Qwen3.5](https://huggingface.co/Qwen/Qwen3.5-4B), [Granite](https://huggingface.co/ibm-granite/granite-4.0-micro), [Ministral](https://docs.mistral.ai/models/ministral-3-3b-25-12); Apache-2.0 sources |
| Balanced visual/temporal analysis | Qwen3.5 4B | Gemma 4 E2B/E4B | [Gemma 4 card](https://ai.google.dev/gemma/docs/core/model_card_4); Apache-2.0 differs from existing Gemma 3 terms |
| Thorough analysis | Qwen3.5 9B | Gemma 4 12B | Same family-source review; measure native processor/runtime support |
| Workstation optional tier | Qwen3.8 27B | Qwen3.6-35B-A3B; Gemma 4 26B A4B | [Qwen3.8](https://huggingface.co/Qwen/Qwen3.8-27B); permissive upstream sources, not a 16 GB default |
| Embedding/reranking | Existing CLIP + BGE-small retained | Qwen3-Embedding/Reranker 0.6B | [Embedding](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B), [Reranker](https://huggingface.co/Qwen/Qwen3-Reranker-0.6B); Apache-2.0 |
| Timestamped speech | whisper.cpp with hardware-sized Whisper weights | Qwen3-ASR 0.6B + forced alignment | [whisper.cpp](https://github.com/ggml-org/whisper.cpp), [Qwen3-ASR](https://huggingface.co/Qwen/Qwen3-ASR-0.6B); runtime MIT / Qwen Apache-2.0; verify weight artifacts separately |
| Faces | Existing SFace retained | Licensed alternatives only after held-out evaluation | Detection/alignment/normalization and calibration precede replacement |

[Qwen3.8 Flash-Next](https://huggingface.co/Qwen/Qwen3.8-Flash-Next) is a research reference with 125B base parameters plus substantial embedding/prediction weights; do not offer it as a default desktop download.

Use [llama.cpp server capabilities](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md) and [speculative decoding](https://github.com/ggml-org/llama.cpp/blob/master/docs/speculative.md) as runtime research, with MLX Swift as a verified Apple adapter. Parallel loading, concurrent requests to one model, and concurrent different-model execution are separate admission decisions. FileID still needs its own memory budget, interactive priority, bounded batching, residency reuse/eviction, structured short outputs, and measured quantization/cache/speculation choices. The current MLX dependencies/model factories do not establish support for this new shortlist. ONNX/NPU paths require actual model/device validation with CPU fallback.

CPU/low-memory machines serialize heavy work. Start the 16 GB M1 Pro experiments with compact chat and 4B visual models under measured pressure; larger machines may keep two models resident. Promote candidates only after all quality gates and either meaningful accuracy improvement or at least 20% latency improvement without material regression on identical FileID fixtures/hardware. No claim that one model is best on every hardware tier.

Enhancement/conversion research: [Real-ESRGAN](https://github.com/xinntao/Real-ESRGAN) (BSD-3-Clause code; verify chosen weights) for tiled photos, [FlashVSR](https://github.com/OpenImagingLab/FlashVSR) (Apache-2.0 source) as an optional higher-memory video pack, with temporal/fidelity tests. No enhancement pack was bundled. FFmpeg, LibreOffice, libarchive, Assimp, and FreeCAD workers remain pending exact build/distribution review; avoid nonfree FFmpeg builds and non-commercial test assets/weights. Codec patent obligations need release-specific review.

## Initial exports do not add models

The October 2026 photo/chapter export milestone uses system ImageIO/CoreGraphics on macOS and the existing locked image-rs/SHA-256 crates in Rust. Downsize and format conversion do not imply AI upscaling. No research candidate or enhancement weight was promoted or bundled; model benchmark, hash, and distribution gates above remain unchanged.

2026-10-01 face processing clarification: the historical `ArcFaceService` symbol loads OpenCV SFace, not InsightFace/Buffalo. SFace consumes raw RGB [0,255] Float32 NCHW. Shared geometry fixtures now verify template order and similarity transforms across Swift/Rust; no replacement face weights or measured identity-accuracy claim accompanies this change.

## 2026-10-01 — Chat inference integration

Initial macOS chat reuses the already loaded, existing MLX model; no candidate weight or runtime has been promoted. Catalog evidence is bounded to eight results, captions to 600 characters each, and generation to 192 tokens at temperature zero. Keyword retrieval works without weights. This reduces context/output work but does not establish latency or grounding quality; benchmark identical fixtures under background contention before model promotion. Rust chat generation and separate resident task models remain pending.

Initial runtime memory admission now checks physical/system reserves and available headroom before native MLX loads or Rust VLM process startup. Registry and byte-based estimates are provisional; dedicated-GPU/context/quantization measurements remain required. Native residency leases prevent a prewarm swap from overlapping active inference. No new model candidate is promoted by these policy tests. See SCHEDULER.md.

## Face weight provenance (2026-10-01)

Loaded SFace sessions now record an actual SHA-256 fingerprint of their selected ONNX file, including portable execution-provider variants. v22 stores that hash separately from the platform processing version and source revision; unknown legacy vectors are not stamped as verified current weights. This is cache provenance, not model promotion or identity-confidence calibration. New catalog face observations use confidence zero until held-out calibration exists. The legacy mobileclip_s2 storage label remains ambiguous for older/native CLIP vectors and must be resolved through verified producers and model-aware retrieval; do not relabel existing vectors by dimensionality.

Native MP4 transcodes use AVFoundation operating-system codecs and no AI model. Conversion is not enhancement or best-take understanding; no model comparison/promotion follows from generated video fixtures.

Face comparison now rejects unknown or mixed model/processing namespaces, stale revisions and invalid 128-d vectors before persistence. Legacy person centroids lack provenance and cannot drive inheritance. See FACE_CACHE.md for whole-pass refresh limitations and remaining incremental/stable-ID/calibration gates.


### Native retrieval cache compatibility (2026-10-02)

The v23 native index accepts only `CLIPEmbeddingSpace.modelID`, which identifies the verified artifact hashes and preprocessing descriptor, and finite normalized 512-dimensional vectors. Cache namespaces include that identity and dimensionality. CLIP's model weights and runtime have not changed in this milestone; no new model is promoted. Text and other catalog embedding namespaces have independent transactional tracking and await separate index adapters. The Library now queries the engine-owned persistent index; legacy ambiguous labels remain excluded.


## Runtime follow-up — October 2, 2026

[MLX Swift LM 3.32.3](https://github.com/ml-explore/mlx-swift-lm/releases/tag/3.32.3) includes Qwen3.5 state/sanitization, Gemma 4 loading and image processing, cancellation and guided-generation fixes. FileID still resolves `mlx-swift-examples` 2.29.1, revision `9bff95ca5f0b9e8c021acc4d71a2bbe4a7441631`. A dependency transition must verify API/model-factory compatibility, OS/SDK requirements, offline behavior including automatic MTP downloads, licenses and identical-fixture regressions. No dependency or model was promoted by this research.

[Gemma 4's card](https://ai.google.dev/gemma/docs/core/model_card_4) lists Apache-2.0 and substantial separate embedding storage for its E-series. Effective parameter counts do not determine resident memory. Budget actual converted tensors, context/recurrent states and visual buffers. [llama.cpp's router](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md) supports `--models-max` (default four; zero unlimited), but that is a model-count cap, not FileID memory admission. Measure loading concurrency, resident models and parallel requests separately; keep interactive work responsive on the 16 GB Mac before changing defaults.


Native search-index admission now estimates live/raw vector storage and graph/manifest decoding/snapshot buffers, including historical cache files. It uses existing system/headroom checks before rebuilding or loading. This is not shared multi-model allocation accounting or a measured replacement-model benefit. Task routing, simultaneous residency and pressure benchmarks remain required.
