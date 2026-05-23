# Models — canonical registry

FileID never ships model weights. Every model is downloaded at runtime from its upstream repository, with progress + cancellation visible to the user, after they explicitly trigger the download. SHA256-pinned. No telemetry on the download.

This file is the cross-platform source of truth for what FileID asks for and where it lives. Per-platform installers (`platforms/apple/scripts/install_clip_models.sh`, `platforms/windows/build/install-models.ps1` _(Phase 1+)_, future Linux equivalent) read this list.

## ML stack per platform

| Capability | macOS | Windows | Notes |
|---|---|---|---|
| Image semantic embedding | MobileCLIP-S2 (CoreML `.mlpackage`) | MobileCLIP-S2 (ONNX) | Same logical model, different runtime format. Embeddings are byte-cross-compatible (512-d float32 LE, L2-normalized). |
| Text semantic embedding | OpenAI CLIP text (CoreML) + BPE vocab | OpenAI CLIP text (ONNX) + BPE vocab | Same tokenizer port; embeddings cross-compatible. |
| Face detection + 5-pt landmarks | Apple Vision (`VNDetectFaceRectanglesRequest`) | SCRFD ONNX (Buffalo bundle) | Different detector models; bounding boxes won't be byte-identical but person clusters converge to ±5%. |
| Face quality / pose | `VNDetectFaceCaptureQualityRequest` (roll/yaw/pitch) | `face_quality_assessment.onnx` + PnP solve from SCRFD landmarks | Quality scoring is approximate cross-platform; thresholds calibrated per platform. |
| Face embedding | ArcFace iResNet50 / MobileFace (ONNX via CoreML EP) | ArcFace iResNet50 / MobileFace (ONNX via DirectML / CUDA / CPU EP) | Same weights, same model — embeddings are byte-cross-compatible. Person clustering DBs round-trip across platforms. |
| OCR | Apple Vision `VNRecognizeTextRequest` (fast tier) | Windows.Media.Ocr (built-in WinRT) default; PaddleOCR ONNX opt-in | Built-in OCR is fast + free + multilingual on both. |
| Image classification | _(dropped)_ | _(dropped)_ | Superseded by CLIP semantic similarity on both platforms. |
| Vision-language models (Deep Analyze) | MLX: Qwen 3 VL · Gemma 3 · SmolVLM 2 · PaliGemma | llama.cpp: Qwen 2.5-VL · Gemma 3 · SmolVLM · MiniCPM-V (PaliGemma substitute) | MLX is Apple-Silicon-only; llama.cpp covers Windows on every GPU. Curated lineup per platform to use the best-supported quants. |

## Embedders + OCR — model registry

Files live under each platform's models directory. Downloads triggered by the welcome-sheet onboarding (or Settings) on first launch.

### MobileCLIP-S2 image encoder

| Aspect | Value |
|---|---|
| Source (macOS) | [`apple/coreml-mobileclip`](https://huggingface.co/apple/coreml-mobileclip) — `mobileclip_s2_image.mlpackage` |
| Source (Windows) | [`Xenova/mobileclip_s2`](https://huggingface.co/Xenova/mobileclip_s2) — `onnx/vision_model.onnx` (community ONNX export of Apple's OpenCLIP release; same weights, ONNX format) |
| License | Apple Sample Code License |
| macOS layout | `~/Library/Application Support/FileID/Models/mobileclip_image/` (CoreML `.mlpackage`) |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\mobileclip\mobileclip_s2_image.onnx` |
| Input | 256×256 BGRA (macOS) / RGB (Windows ONNX) |
| Output | 512-d float32, L2-normalized |
| Tag | `mobileclip_s2` (stored in `clip_embeddings.model`) |

### CLIP text encoder

| Aspect | Value |
|---|---|
| Source (macOS) | [`openai/clip-vit-base-patch32`](https://huggingface.co/openai/clip-vit-base-patch32) (ONNX export) |
| Source (Windows) | ONNX text encoder from [`Xenova/mobileclip_s2`](https://huggingface.co/Xenova/mobileclip_s2) — `onnx/text_model.onnx` (OpenCLIP-compatible, paired with the image encoder above). BPE vocab + merges still pulled from `openai/clip-vit-base-patch32`. |
| License | MIT (tokenizer) + Apple Sample Code License (Xenova ONNX export of Apple's release) |
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
| Role | **Phrase-grounded object detection** (`<OD>` / `<CAPTION_TO_PHRASE_GROUNDING>`) — the one capability not covered by the rest of the stack (SmolVLM / Qwen2.5-VL / Gemma 3 cover captioning + tags; Windows.Media.Ocr covers OCR). |
| Status | Registry arm + `models::florence2` skeleton. **Inference is Phase 7b**: 4 ORT sessions + a Rust autoregressive generation loop + the `tokenizers` crate for the BART tokenizer + a `modelKind: "florence2_base"` Deep-Analyze backend. Build out when grounded OD becomes a concrete product need. |

### ArcFace iResNet50 (default ≥ 16 GB hardware)

| Aspect | Value |
|---|---|
| Source | [`immich-app/buffalo_l`](https://huggingface.co/immich-app/buffalo_l) — `recognition/model.onnx` |
| License | InsightFace pre-trained weights — **non-commercial research only** (see note below) |
| macOS layout | `~/Library/Application Support/FileID/Models/arcfaceIResNet50/model.onnx` |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\arcfaceIResNet50\model.onnx` |
| Input | 112×112 RGB, normalized (px - 127.5) / 127.5, NCHW |
| Output | 512-d float32, L2-normalized |

### ArcFace MobileFace (default < 16 GB hardware)

| Aspect | Value |
|---|---|
| Source | [`immich-app/buffalo_s`](https://huggingface.co/immich-app/buffalo_s) — `recognition/model.onnx` |
| License | InsightFace pre-trained weights — **non-commercial research only** |
| macOS layout | `~/Library/Application Support/FileID/Models/arcfaceMobileFace/model.onnx` |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\arcfaceMobileFace\model.onnx` |
| Input | same as iResNet50 |
| Output | 512-d float32, L2-normalized |

### SCRFD face detection (Windows only)

| Aspect | Value |
|---|---|
| Source | [`immich-app/buffalo_l`](https://huggingface.co/immich-app/buffalo_l) — `detection/model.onnx` |
| License | InsightFace pre-trained weights — **non-commercial research only** |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\scrfd\model.onnx` |
| Output | bounding boxes + 5-point landmarks |

> The InsightFace pre-trained weights (Buffalo-L / Buffalo-S) are explicitly licensed **non-commercial research only**, even though InsightFace's own code is MIT. FileID is a personal, non-commercial project and pulls weights directly from the upstream Immich-hosted mirror — same posture Immich itself uses. Commercial use requires licensing the weights from InsightFace directly or swapping in a permissively-licensed face embedder.

### PaddleOCR (Windows opt-in)

| Aspect | Value |
|---|---|
| Source | TBD — published ONNX builds; pinned commit |
| License | Apache 2.0 |
| Windows layout | `%LOCALAPPDATA%\FileID\Models\paddle_ocr\` |
| When used | Settings → Advanced → "Use PaddleOCR instead of built-in Windows.Media.Ocr" |

## Vision-language models — Deep Analyze

### Curated Windows lineup (llama.cpp GGUF Q4_K_M unless noted)

| Model | Size on disk | RAM est. | Use case | Source |
|---|---|---|---|---|
| **Qwen 2.5-VL 3B** | ~2.5 GB | ~6 GB | Recommended for 8–16 GB machines and Snapdragon WoA | [Qwen/Qwen2.5-VL-3B-Instruct-GGUF](https://huggingface.co/Qwen) — pinned commit, GGUF + mmproj |
| **Qwen 2.5-VL 7B** | ~5 GB | ~12 GB | Recommended for ≥ 16 GB + dGPU | same family |
| **Gemma 3 4B (vision)** | ~3 GB | ~8 GB | Alternative captioner, different prose style | [google/gemma-3-4b](https://huggingface.co/google/gemma-3-4b-it) GGUF |
| **SmolVLM** | ~1 GB | ~3 GB | Tiny / battery-conscious / WoA fallback | [HuggingFaceTB/SmolVLM-Instruct](https://huggingface.co/HuggingFaceTB/SmolVLM-Instruct) |
| **MiniCPM-V 2.6** | ~5.5 GB | ~14 GB | PaliGemma substitute | [openbmb/MiniCPM-V-2_6](https://huggingface.co/openbmb/MiniCPM-V-2_6) GGUF |

(Exact pinned commits + SHA256s are recorded in the platform-specific installer scripts; that's where the wire URLs live, so the doc isn't a SHA copy-pasta target.)

### macOS lineup (MLX)

| Model | Source | Notes |
|---|---|---|
| Qwen 3 VL 4B | swift-transformers HF cache | Default recommendation |
| Qwen 2.5-VL 3B | swift-transformers HF cache | Compact alt |
| Gemma 3 4B | swift-transformers HF cache | |
| Gemma 3 12B | swift-transformers HF cache | High RAM only |
| SmolVLM 2 | swift-transformers HF cache | |
| PaliGemma 3B | swift-transformers HF cache | macOS only — replaced by MiniCPM-V on Windows |

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

Each pack has its own canonical URL + SHA256 list. Performance Packs do not contain user data and never report installation back. They install into `%LOCALAPPDATA%\FileID\runtimes\<pack-name>\` and the engine adds them to its DLL search path.

## Why we pull from upstream rather than redistribute

- **Licensing.** The InsightFace weights are non-commercial; we don't have license to host them. Apple Sample Code License + OpenAI MIT are friendlier but we still want users to see *exactly* where their model came from.
- **Auditability.** A user can verify the SHA256 against the upstream HuggingFace repo independently. Mirrored weights are a target for supply-chain attacks.
- **Privacy.** Downloads go user → HF directly. FileID isn't a hop. Network-capture verification is straightforward.
- **Bundle size.** Models add up to several GB. Shipping a lean app + on-demand downloads keeps the install fast.
