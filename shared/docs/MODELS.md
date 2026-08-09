# Models — canonical registry

FileID never ships model weights. Every model is downloaded at runtime from its upstream repository, with progress + cancellation visible to the user, after they explicitly trigger the download. **Every artifact is SHA256-pinned in `engine/src/models/registry.rs`** — the canonical hash is the `oid sha256:` from each HuggingFace LFS pointer (or the sha256 of the GitHub/NVIDIA release asset); the engine downloader verifies the downloaded bytes against the pin before use, and a CI gate (`windows-engine.yml`) fails the build on any unpinned (`sha256: None`) entry. No telemetry on the download.

This file is the cross-platform source of truth for what FileID asks for and where it lives. Per-platform installers (`platforms/apple/scripts/install_clip_models.sh`, `platforms/windows/build/install-models.ps1`, future Linux equivalent) read this list.

## Licensing posture — commercial-clean (Apache-2.0 project)

As of the 2026-05 commercial-clean pass, **every weight FileID downloads by default is permissively licensed (Apache-2.0 / MIT)** — no non-commercial weights in the core feature set. This keeps the project (Apache-2.0, see root `LICENSE`) free to be open-sourced *and* commercialized later without a weight-licensing blocker. The non-commercial InsightFace face stack (ArcFace + SCRFD) and the research-only Apple MobileCLIP-S2 / Qwen2.5-VL-3B were replaced. The one conditional model, Gemma-3-4B, is commercially usable under Google's Gemma Terms and stays an opt-in, user-initiated download (its terms surface in the install flow).

> **Windows is live on the commercial-clean stack now.** The macOS app mirror (RAM++ tagger, ViT-B/32, SFace) lands in **WS-MAC** — rows below mark macOS cells *(lockstep pending)* where the Swift swap hasn't been applied yet. Cross-platform DB round-trips (esp. 128-d face prints) require both platforms on the new models; until WS-MAC ships, treat face DBs as platform-local.

## ML stack per platform

| Capability | macOS | Windows | Notes |
|---|---|---|---|
| In-scan image tagging (primary) | RAM++ Swin-L @384 *(lockstep pending)* | **RAM++ Swin-L @384 (ONNX, fp16)** | Recognize Anything Plus, 4585-tag multi-label tagger, Apache-2.0. Primary auto-tagger; CLIP zero-shot scene tags are the fallback when RAM++ isn't installed. |
| Image semantic embedding (search) | CLIP ViT-B/32 (CoreML) *(lockstep pending)* | **CLIP ViT-B/32 (ONNX)** | OpenAI/OpenCLIP ViT-B/32, MIT. 512-d float32 LE, L2-normalized — embeddings byte-cross-compatible across platforms. |
| Text semantic embedding (CLIP) | CLIP ViT-B/32 text (CoreML) + BPE vocab *(lockstep pending)* | **CLIP ViT-B/32 text (ONNX)** + BPE vocab | Same OpenAI BPE tokenizer port; embeddings cross-compatible. |
| Face detection + 5-pt landmarks | Apple Vision (`VNDetectFaceRectanglesRequest`) | **YuNet (ONNX, OpenCV Zoo)** | YuNet is MIT. Different detectors → boxes aren't byte-identical, but 5-pt landmarks feed a shared alignment template so embeddings match. |
| Face embedding | SFace (ONNX via CoreML EP) *(lockstep pending)* | **SFace (ONNX via DirectML / CUDA / CPU EP)** | SFace (OpenCV Zoo) is Apache-2.0, **128-d** L2-normalized. Replaces 512-d ArcFace; person-clustering DBs round-trip once both platforms are on SFace. |
| OCR | Apple Vision `VNRecognizeTextRequest` (fast tier) | Windows.Media.Ocr (built-in WinRT) default; PaddleOCR ONNX opt-in | Built-in OCR is fast + free + multilingual on both. |
| Vision-language models (Deep Analyze) | MLX: Qwen 2.5-VL · Gemma 3 · PaliGemma | llama.cpp: Qwen 2.5-VL 7B · Gemma 3 · Mistral-Small-3.2 | MLX is Apple-Silicon-only; llama.cpp covers Windows on every GPU. Curated lineup per platform to use the best-supported quants. |

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

| Model | Source | Notes |
|---|---|---|
| Qwen 2.5-VL 7B | swift-transformers HF cache | Default recommendation (Apache-2.0) |
| Gemma 3 4B | swift-transformers HF cache | Opt-in (Gemma Terms) |
| Mistral-Small-3.2 | swift-transformers HF cache | Max quality (Apache-2.0) — lockstep pending |

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
