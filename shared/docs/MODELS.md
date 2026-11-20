# Models — canonical registry

## 2026-11-19 — About-100B models at the top of the ladders, and a smaller Win/Linux 2B

The owner asked for much bigger models above the current ladders for machines that can hold them, and for a Qwen3-VL 2B Q4_K_M at the end of the Windows/Linux ladder. **The big models are vetted and wired, but not run on hardware.** No weights were downloaded: the checks below read Hugging Face model cards, file lists and LFS digests (metadata only), GitHub source at the pinned runtime versions, and the GitHub releases list. The `web_search` and `fetch_content` tools were not available to this lane. Nothing here is a speed or quality claim.

**What was added**

| Platform | Model | Build | Download | Memory the engine asks for | Installed memory needed (fits) | Free memory needed to load now |
|---|---|---|---|---|---|---|
| Mac | Qwen3.5 122B-A10B (`qwen3_5_122b`) | MLX 4-bit, `mlx-community/Qwen3.5-122B-A10B-4bit` @ `e9c67b08899964be5fdd069bb1b4bc8907fe68f5` | 69,593,636,939 bytes (14 files) | 72 GB (73,728 MB) | 80 GB or more, so 96, 128 and 192 GB Macs | 74 GB |
| Windows/Linux | GLM-4.6V (`glm_4_6v_gguf_q4_k_m`) | llama.cpp GGUF Q4_K_M + Q8_0 projector, `ggml-org/GLM-4.6V-GGUF` @ `7de678a269f576ae4090efbf59b558a993386fff` | 71,392,337,408 bytes (two files) | 86,132 MB (84.1 GB) | about 92 GB or more, so 96 GB PCs and up | 86.1 GB |
| Windows/Linux | Qwen3-VL 2B Q4_K_M (`qwen3_vl_2b_instruct_gguf_q4_k_m`) | llama.cpp GGUF Q4_K_M + Q8_0 projector, `Qwen/Qwen3-VL-2B-Instruct-GGUF` @ `52d6c8ffea26cc873ac5ad116f8631268d7eb503` | 1,552,463,168 bytes | 2,876 MB (2.8 GB) | 4.8 GB | 3.3 GB |

The engine's one fits rule (`util/model_memory.rs`: reserve a quarter of installed memory, 2–8 GB, plus a free-memory floor) decides where they are recommended; nothing else names a machine size. With memory free at idle, the first rung that fits now wins, and when nothing fits now the smallest model the machine could ever run is recommended.

| Rung | Mac ladder (MLX) | Needs installed memory of | Windows/Linux ladder (GGUF) | Needs installed memory of |
|---|---|---|---|---|
| 1 | **Qwen3.5 122B-A10B** (new) | 80 GB | **GLM-4.6V** (new) | 92 GB |
| 2 | Mistral Small 3.2 24B | 21.3 GB | Qwen2.5-VL 7B | 10.8 GB |
| 3 | Qwen3-VL 8B | 11.3 GB | Qwen3-VL 8B | 10.9 GB |
| 4 | Qwen3-VL 4B | 7.0 GB | Qwen3-VL 4B | 6.9 GB |
| 5 | Qwen3-VL 2B | 5.0 GB | Qwen3-VL 2B Q8_0 | 5.7 GB |
| 6 | | | **Qwen3-VL 2B Q4_K_M** (new) | 4.8 GB |

On the Mac, 64 GB machines stay on Mistral Small 3.2: the 4-bit 122B needs 80 GB installed. On Windows/Linux, a 64 GB PC stays on Qwen2.5-VL 7B.

**Candidates checked** (every figure from the Hugging Face API at the revision shown, 2026-11-19; "split" means the GGUF is more than one shard, which the engine cannot install: its catalog layout is one `model.gguf` plus one `mmproj.gguf`)

| Model | Parameters | Licence | MLX (mlx-swift-lm 3.32.3) | GGUF (llama.cpp b9254) | 4-bit size | Verdict |
|---|---|---|---|---|---|---|
| [Qwen3.5-122B-A10B](https://huggingface.co/Qwen/Qwen3.5-122B-A10B) `dc4d3484…` | 122B, 10B active | Apache-2.0 (`LICENSE` file is the Apache text; card says `apache-2.0`) | Supported: `qwen3_5_moe` is in both the VLM and LLM registries; [mlx-community 4-bit](https://huggingface.co/mlx-community/Qwen3.5-122B-A10B-4bit) holds the vision tower (333 tensors) | Architecture `qwen35moe` is supported, but every build is split: [lmstudio-community](https://huggingface.co/lmstudio-community/Qwen3.5-122B-A10B-GGUF) Q4_K_M 2 shards 74.2 GB, [bartowski](https://huggingface.co/bartowski/Qwen_Qwen3.5-122B-A10B-GGUF) 2 shards 77.6 GB, [unsloth](https://huggingface.co/unsloth/Qwen3.5-122B-A10B-GGUF) 3 shards | MLX 69.6 GB; GGUF 74–78 GB | **Added on the Mac.** Not added on Windows/Linux until the engine can install split GGUFs |
| [GLM-4.6V](https://huggingface.co/zai-org/GLM-4.6V) `4e2d47eb…` | 106B, 12B active | MIT (card `license: mit`; the [GLM-V](https://github.com/zai-org/GLM-V) code repository is Apache-2.0) | **Not supported**: its type `glm4v_moe` is in neither factory of 3.32.3, and also not in 3.33.0 (2026-11-17) | `glm4moe` architecture and the `GLM4V` projector are in b9254; [ggml-org](https://huggingface.co/ggml-org/GLM-4.6V-GGUF) Q4_K_M is one 70.4 GB file | GGUF 70.4 GB + 0.94 GB projector | **Added on Windows/Linux.** Not on the Mac |
| [GLM-4.5V](https://huggingface.co/zai-org/GLM-4.5V) `ed47433b…` | 106B, 12B active | MIT | Same missing `glm4v_moe` | [ggml-org](https://huggingface.co/ggml-org/GLM-4.5V-GGUF) Q4_K_M is split in two (9 MB + 63.6 GB) | 63.6 GB | Not added: split, and GLM-4.6V is its successor |
| [Qwen3-VL-235B-A22B-Instruct](https://huggingface.co/Qwen/Qwen3-VL-235B-A22B-Instruct) `710c1386…` | 235B, 22B active | Apache-2.0 (card) | `qwen3_vl_moe` is supported, but the only 4-bit build, [mlx-community](https://huggingface.co/mlx-community/Qwen3-VL-235B-A22B-Instruct-4bit) (133.4 GB), has no model card, no licence field and no declared base model, so its lineage can't be verified by the catalog rules | Official [Qwen GGUF](https://huggingface.co/Qwen/Qwen3-VL-235B-A22B-Instruct-GGUF) Q4_K_M is split in three (142 GB) | 133–142 GB | Not added: no verifiable Mac build, split GGUF. Qwen's Qwen3.5 card claims Qwen3.5 outperforms the Qwen3-VL models on visual understanding, so the 122B is the better pick for the same machines |
| [Mistral-Small-4-119B-2603](https://huggingface.co/mistralai/Mistral-Small-4-119B-2603) `a11f36be…` | 119B | Apache-2.0 (card) | **Not supported**: its text config is `mistral4` (a MoE with latent attention); the `mistral3` VLM in 3.32.3 implements a dense transformer only, and `mistral4` appears nowhere in the package | `mistral4` is in b9254; [lmstudio-community](https://huggingface.co/lmstudio-community/Mistral-Small-4-119B-2603-GGUF) Q4_K_M is split in two (72 GB) | 67.8 GB MLX | Not added |
| [Qwen3.5-397B-A17B](https://huggingface.co/Qwen/Qwen3.5-397B-A17B) | 397B, 17B active | Apache-2.0 | Supported (`qwen3_5_moe`) | Supported, split (7 shards at Q4_K_M) | MLX 223.9 GB | Not added: needs more than a 192 GB Mac can give |
| [InternVL3.5-241B-A28B](https://huggingface.co/OpenGVLab/InternVL3_5-241B-A28B) | 241B | Apache-2.0 (card) | No `internvl_chat` type; no MLX build | Only third-party split GGUFs | — | Not added |
| Qwen2.5-VL-72B | 72B | `other` / Qwen licence | — | — | — | **Rejected:** not permissive (already on the denylist) |
| [Qwen3.8-Flash-Next](https://huggingface.co/Qwen/Qwen3.8-Flash-Next) | 180B | `other` / `qwen-community-1.0` | — | — | — | **Rejected:** custom licence |
| [Nemotron 3.5 Super VL 120B-A12B](https://huggingface.co/nvidia/NVIDIA-Nemotron-3.5-Super-VL-120B-A12B-BF16) | 120B | `other` / `openmdw-1.1` | — | — | — | **Rejected:** custom licence, not Apache-2.0 or MIT |
| Llama-4-Scout, Mistral-Large-3, Pixtral-Large, Devstral-2-123B | 109–675B | Llama 4, Mistral research or revenue-capped terms | — | — | — | **Rejected:** on the denylist |

No training dataset is named on any card of the added models, and none of them names a non-commercial dataset. That is the same evidence level as the Qwen3-VL models already shipped; a card that names none is not proof that none was used.

**Qwen3.5 122B-A10B (Mac).** [Qwen/Qwen3.5-122B-A10B](https://huggingface.co/Qwen/Qwen3.5-122B-A10B) at `dc4d348443bc740c68e2d77492492c11606384d5`: `LICENSE` is Apache-2.0 (11,544 bytes, SHA-256 `bbedc3fda3305820b977265f01b8619d87570a6739de3a5582c3464840f1e57a`) and the card says `license: apache-2.0`. The mlx-community conversion's card says it was converted from that repository with mlx-vlm 0.3.12 and declares Apache-2.0 with the same `license_link`; its `config.json` is 4-bit affine, group size 64, with no per-layer overrides and `model_type: qwen3_5_moe`. `model-catalog.json` now holds the entry with the pinned revision, the 14 safetensors files with LFS SHA-256 and sizes, and `Qwen/Qwen3.5-122B-A10B` as the base; the manifest pins the same revision, so the downloader verifies it as it does Mistral Small 3.2. The 72 GB figure is the weights (64.8 GiB) plus about 7 GiB for the vision tower, cache and activations, and is an estimate: it was not measured. macOS commonly caps the GPU's wired memory at about three quarters of installed memory (about 72 GB on a 96 GB Mac, from general knowledge, not measured here), so the 122B on a 96 GB Mac may need the limit raised (`iogpu.wired_limit_mb`) or may refuse to load; a 128 GB Mac is comfortably above it. The Mac engine passes `enable_thinking: false` to every MLX model.

**GLM-4.6V (Windows/Linux).** [zai-org/GLM-4.6V](https://huggingface.co/zai-org/GLM-4.6V) at `4e2d47eb0b41c5280d8294b17cef9e94fdcfff46`: the card says `license: mit`; the repository has no `LICENSE` file; the GLM-V code repository's `LICENSE` is Apache-2.0 (SHA-256 `086c6e7f5082b4640dfb03a892f32504f24afd247c702c38a3aae8c3474c54f0`). Both are on the allowed list. [ggml-org/GLM-4.6V-GGUF](https://huggingface.co/ggml-org/GLM-4.6V-GGUF) at `7de678a269f576ae4090efbf59b558a993386fff` has no licence field and names `zai-org/GLM-4.6V` as its base (it was converted with `convert_hf_to_gguf.py`). Files, checked against the `X-Linked-Etag` and size headers of the resolve URLs:

| File | Bytes | SHA-256 |
|---|---|---|
| `GLM-4.6V-Q4_K_M.gguf` | 70,448,211,840 | `09a6f00ed07552a0187c2271019509260639e95671bbef55b2b77e0f77a74ed2` |
| `mmproj-GLM-4.6V-Q8_0.gguf` | 944,125,568 | `ad7f4a9d50ec3041ca795fec69f39c1cb75af107ea46dd1dc055eca210ba8ccc` |

Risk: GLM-4.6V is a model that thinks by default, and the engine's llama.cpp requests do not switch thinking off (the Mac path does). Unmeasured, so it may spend many tokens before answering. Check this on the first hardware run, together with seconds per photo (the Linux picker's 30 s is a guess). The generator's default 80 GB ceiling had excluded it (82.7 GB estimated); it is now in `REVIEWED_BUILDS`.

**Qwen3-VL 2B Q4_K_M (Windows/Linux).** [Qwen/Qwen3-VL-2B-Instruct-GGUF](https://huggingface.co/Qwen/Qwen3-VL-2B-Instruct-GGUF) at `52d6c8ffea26cc873ac5ad116f8631268d7eb503` (Qwen's own repository; the card says `license: apache-2.0` and `base_model: Qwen/Qwen3-VL-2B-Instruct`, itself Apache-2.0; the [Qwen3-VL code repository](https://github.com/QwenLM/Qwen3-VL) is Apache-2.0). The repository has no `LICENSE` file.

| File | Bytes | SHA-256 |
|---|---|---|
| `Qwen3VL-2B-Instruct-Q4_K_M.gguf` | 1,107,409,952 | `089d75c52f4b7ffc56ba998ffc50aae89fcafc755f9e7208aacca281dca6c2ae` |
| `mmproj-Qwen3VL-2B-Instruct-Q8_0.gguf` | 445,053,216 | `f9a68fabba69c3b81e153367b2c7521030b0fa8bb0de400c9599c8e6725f9c82` |

It is the same model as the Q8_0 build already vetted, at a smaller quantization, with the Q8_0 projector (the F16 projector would add 374 MB and push the free-memory need above what a typical 8 GB PC has). On an 8 GB PC with 45% free (3.7 GB), the Q8_0 build needs 4.25 GB free and is refused; the Q4_K_M build needs 3.3 GB and loads. Test `a_typical_8_gb_pc_is_recommended_a_model_that_fits_right_now` shows that. Not measured: caption quality of Q4_K_M against Q8_0. It stays last on the ladder, so it is recommended only when the Q8_0 build won't load.

**Why the catalog entries look the way they do.** `build_model_catalog.py --reviewed-builds` adds `REVIEWED_BUILDS` (the two GGUF entries above) to the existing catalog without regenerating the other 309 entries, running every licence, lineage and identity check on each. The Mac entry was written by hand into `model-catalog.json` (that catalog has no generator) and checked by `check_model_catalog.py` and `check_model_license_policy.py`.

**What it would take to add the rest**
- Qwen3.5 122B-A10B on Windows/Linux (and Qwen3-VL-235B, Mistral-Small-4, GLM-4.5V): the engine must install split GGUFs. That means downloading every shard into one folder under llama.cpp's `-NNNNN-of-NNNNN.gguf` names, passing the first shard to `llama-server`, summing all shards for memory admission (admission today reads one file), and hashing each shard. This changes `models/registry.rs`, `vlm.rs`, `vlm_server.rs`, `util/model_memory.rs` and the catalog schema, and needs the owner's approval.
- GLM-4.6V / GLM-4.5V on the Mac: an `mlx-swift-lm` release that registers `glm4v_moe`. None of 3.32.3 or 3.33.0 does, so this means upstream work or our own port.
- Mistral-Small-4 on the Mac: `mlx-swift-lm` support for the `mistral4` text model.
- Qwen3-VL-235B on the Mac: a 4-bit conversion whose card names its base model and licence.

## 2026-11-08 — Qwen3.5 0.8B for tag/rename passes: measured, not adopted

The owner approved downloading Qwen3.5 0.8B to see whether tag and rename passes could run on a smaller model to save memory. It lost a blinded comparison to the current default (Qwen3-VL 2B), so nothing was added to the manifest or the app. It stays in the Mac catalog as an opt-in candidate (`hf:mlx-community/Qwen3.5-0.8B-MLX-4bit`), as before.

**Licence.** [Qwen/Qwen3.5-0.8B](https://huggingface.co/Qwen/Qwen3.5-0.8B) at `2fc06364715b967f1860aea9cf38778875588b17`: the repository's `LICENSE` file is Apache-2.0 and the card says `license: apache-2.0` (base model `Qwen/Qwen3.5-0.8B-Base`, also Apache-2.0). The card describes "prototyping, task-specific fine-tuning, and other research or development purposes" as intended uses because of the model's size. That is a statement of what the model is suited for, not a licence term. Both conversions name it as their base and declare Apache-2.0.

**Downloaded** (evaluation only, into `~/Library/Developer/FileIDModels-eval`):

| Build | Repository @ revision | File, bytes, SHA-256 |
|---|---|---|
| MLX 4-bit | [mlx-community/Qwen3.5-0.8B-MLX-4bit](https://huggingface.co/mlx-community/Qwen3.5-0.8B-MLX-4bit) @ `5d894f8cc4ef3e6c88537bf3746ed262f549da6a` | `model.safetensors`, 625,229,487, `f5a0d9dd3efa73510542a8023d610ff26be2b4b020d181cfc4bedaa1fcc5dd9e` (folder total 634 MB) |
| GGUF Q4_K_M | [lmstudio-community/Qwen3.5-0.8B-GGUF](https://huggingface.co/lmstudio-community/Qwen3.5-0.8B-GGUF) @ `26bab2c9369648924251c0ebb3dae012f5147707` | `Qwen3.5-0.8B-Q4_K_M.gguf`, 527,502,816, `f5b14da98939b60bbe1019a964eba656407e1e0b64f1fe3003ff6d650e93bfec`; `mmproj-Qwen3.5-0.8B-BF16.gguf`, 207,345,952, `6fdd1b4bdc3d2ae8bd15d783e23260dd07dcf83f45604a21dabfd6efad8f8bc5` |

Both match the pins already in `catalog.json` and `model-catalog.json`. The GGUF build was not run, because there is no llama.cpp server on this Mac.

**Blinded comparison.** 120 photos, spread evenly through copies of the private people set (family events from 2010–2015 and 2026: birthdays, Christmas, baseball, portraits). The Swift release engine ran on a scratch catalog: Describe Files (`deepAnalyzeAll`, full pass with renames) once with each model, M1 Pro 16 GB, nothing else running. Each photo's two results (description, tags, proposed name) were shuffled into A/B with a cryptographic RNG. I judged every pair against the photo (more accurate and useful, or a tie) and unblinded only after all 120 judgments. Position check: side A won 56 and side B won 53.

| | Qwen3-VL 2B (default) | Qwen3.5 0.8B |
|---|---|---|
| Pairs won | **105** | 4 |
| Ties | 11 | 11 |
| 120 photos, Describe Files | 160 s (1.33 s per photo) | 62 s (0.51 s per photo) |
| Engine peak physical footprint | 3.09 GB | 2.07 GB |
| Mean description length | 47 words | 21 words |
| Proposed names ending in "Jpg"/"Png"/"Txt" | 2 | 14 |
| Results with snake-case or file-like tags (`man_glasses_brendan`, `baby_baby`) | 4 | 16 |

The 0.8B model was 2.6× faster and used about 1 GB less, but it was less accurate. It misread objects (a sandwich as a banana, a bat as a hockey stick, a gift bag as a cake) and dropped people from group shots. It invented personal names in tags and file names ("Brendan", "Jason"), echoed file extensions into names, and added fewer useful details (signs, jersey text, places). The default stays. The Mac's existing memory controls (idle parking, a capped MLX cache, `clearCache()` between passes) remain the way FileID compresses AI memory.

## 2026-11-08 — Face Mesh V2 for eye openness (shared engine, opt-in)

The shared Rust engine judges eye openness in Best Takes with Google's MediaPipe Face Mesh V2 (the 478-point face landmark network inside the Face Landmarker bundle) when that model is installed. YuNet's five landmarks can't tell open from closed. It is pinned in `shared/models/manifest.json` as `face_mesh_landmarks` (kind `face_mesh`, platforms `macos` and `windows`) and installs into `models/face_mesh/face_landmarks.onnx` through `downloadModel {modelKind: "face_mesh"}`. Since 2026-11-10 the Mac offers it too (Settings → "Closed eyes in Best Takes", into `~/Library/Application Support/FileID/Models/face_mesh/`); the Swift engine runs the same steps on Vision's faces and keeps Vision's eye contours when it is missing. Nothing downloads it on its own. Settings offers it as "Eye check · optional" in WinUI and GTK, and the CLI installs it with `fileid models download face_mesh`.

| | |
|---|---|
| Repository | [naklitechie/face-landmarks-onnx](https://huggingface.co/naklitechie/face-landmarks-onnx) at commit `575c33816c840c5b156398adb485e4f8d138adc2` |
| File | `face_landmarks.onnx`, 4,920,995 bytes, SHA-256 `f38c3321ceffbc9e95103480ad38cc3f52e7e1bde2bcee7cd9355d0b9138ac0c` |
| License | Apache-2.0: the repository's own `LICENSE` file is the Apache-2.0 text (byte-identical to MediaPipe's), and its card says Apache-2.0 |
| Upstream | Google's [Face Landmarker](https://ai.google.dev/edge/mediapipe/solutions/vision/face_landmarker), float16 bundle v1. The [Face Mesh V2 model card](https://storage.googleapis.com/mediapipe-assets/Model%20Card%20MediaPipe%20Face%20Mesh%20V2.pdf) (2022-09-15, authors at Google) says "Licensed under Apache License, Version 2.0" |
| Training data | The card says the images were captured on smartphone cameras in real-world conditions through an AR application and annotated by Google's own 11 annotators. No third-party or non-commercial dataset is named |
| Card scope | Intended for AR effects. Out of scope: life-critical decisions, and face recognition or identification. Eye openness is neither |

**Why this copy.** Google hosts the weights only on `storage.googleapis.com`, which FileID never downloads from. Byte-exact mirrors of the official `face_landmarker.task` bundle exist on Hugging Face (for example [Azng0/autoshade-mirror-mediapipe-face-landmarker](https://huggingface.co/Azng0/autoshade-mirror-mediapipe-face-landmarker), SHA-256 `64184e22…9ff`, the same digest in four unrelated repositories), but the bundle holds TFLite, which ONNX Runtime can't run. The pinned file is a format-only tf2onnx conversion. It was checked against the official weights: inside the mirrored bundle, `face_landmarks_detector.tflite` (SHA-256 `c7d54204…43d5`) holds 251 weight tensors with 1,199,316 values. Every one of its 219 tensors with more than 16 values appears in the ONNX file with exactly the same values (compared as sorted fp32 multisets, so layout changes don't matter), and the totals match. A second independent conversion ([senty-au/face_landmarks_detector-ONNX](https://huggingface.co/senty-au/face_landmarks_detector-ONNX)) passes the same check. The pinned copy was chosen because it carries its own `LICENSE` file.

**How it runs.** Crop a square of 1.5× the YuNet box, rolled level using YuNet's eye points. Run the mesh, refit the crop to the mesh's own points, and run it again. The eye aspect ratio is (|160−144| + |158−153|) / 2|33−133| for one eye, the mirrored points for the other, averaged over both eyes. Openness = (ratio − 0.14) / (0.24 − 0.14), clamped to 0…1, so Best Takes' existing "eyes closed below 0.35" rule fires at a ratio under 0.175 (tuned on the tune split). Faces under 24 px are skipped, and a crop whose presence logit is below 0 gives no judgement. Release build on the M1 Pro: 9.6 ms median per face for both passes (p90 18 ms). The take-signals cache key becomes `yunet-mesh-v1`, so stored scores refresh once the model is installed. Since 2026-11-10 the mesh reads each face from the full decode resized (that face's square only) to the 2048 px scale, not from the 1024 px analysis image (key `yunet-mesh-v2`); on the Mac it reads the camera JPEG's preview or a 2048 px decode. See DECISIONS 2026-11-10.

**Measured** on the private people set (`FileIDEval/people/eye-labels.json`, new): 825 faces at the sizes where Best Takes judges eyes (shorter side ≥ 40 px at 1024 px, or at 2048 px for large sources). One labeller marked each face open, closed or unsure from full-resolution eye crops: 253 open, 135 closed (blinks, squints and downcast eyes that read as closed) and 437 unsure (profiles, sunglasses, too dark). Both checks used the same upright downscaled images. The Mac check is the engine's `VisionTakeFaceAnalyzer.eyesOpen` formula run on Vision's landmarks. The mesh used the labelled box as its first crop, because YuNet isn't installed here and FileID doesn't download it; the second pass follows the mesh's own points. "Eyes closed" means openness < 0.35.

| Eyes-closed flag | Tune (104 open / 73 closed): P / R | Held-out (149 / 62): P / R | Faces judged |
|---|---|---|---|
| Mac Vision eye-contour check | 0.78 / 0.25 | 0.76 / 0.31 | 85% |
| Face mesh, chosen ratio 0.175 | 0.79 / 0.67 | **0.82 / 0.68** | 94% |
| Both, only faces both judged (held-out n = 179) | Vision 0.76 / 0.43 | mesh 0.79 / 0.77 | — |

**Mac, 2026-11-10** (new private set, 1,211 faces, 81 closed; 2048 px images; threshold 0.175): held-out precision/recall Vision 0.68 / 0.44, Mac mesh 0.65 / 0.79, Rust mesh 0.62 / 0.77; 12.7–15.1 ms median per face (release, CPU). At the engines' 1024 px analysis size precision drops for both checks (held-out Vision 0.48 / 0.47, mesh 0.45 / 0.84); see DECISIONS 2026-11-10.

The mesh flags more than twice as many of the closed eyes at the same precision. Most of its misses are downcast eyes and squints that still show a sliver of iris; most of its false flags are narrow smiling eyes. Not established: real YuNet boxes as the first crop (the Windows RTX 2060 run should confirm), and other families' photos.

## 2026-11-06 — Extended catalog license and identity audit

Every `shared/models/catalog.json` entry was checked against its pinned repository's own files and its true upstream's LICENSE file or model card. Result: 342 → 334 entries (262 Apache-2.0, 46 MIT, 26 Gemma Terms with `termsRequired`). Removed:
- [Qwen2.5-VL-3B](https://huggingface.co/Qwen/Qwen2.5-VL-3B-Instruct/blob/main/LICENSE): Qwen Research License.
- `mlx-community/gemma-3-4b-it-qat-4bit`: Gemma 3 under the Gemma Terms, listed as InternVL3-1B under Apache-2.0.
- The three [DeepSeek-R1-Distill-Llama 8B/70B](https://huggingface.co/deepseek-ai/DeepSeek-R1-Distill-Llama-8B#7-license) entries: Llama 3.1/3.3 license.
- Both EmbeddingGemma 2 entries: the license is Apache-2.0, but the card requires the Gemma Prohibited Use Policy; the owner decided to keep it excluded.
- One Gemma 4 QAT MLX build whose declared upstream is not public.

Corrected: PaddleOCR-VL 1.5 (was ERNIE-4.5-0.3B), olmOCR-2 7B MLX (was Qwen2.5-VL-7B), Instruct/`-it` names for mislabelled base/`-pt` conversions, and upstreams that pointed at conversions or moved organisations. [Gemma 4's license](https://ai.google.dev/gemma/docs/gemma_4_license) is the plain Apache-2.0 text, so Gemma 4 and DiffusionGemma stay Apache-2.0 without terms. `check_model_license_policy.py` now gates the catalog: denylisted models, name/repository family or size mismatches, and Gemma Terms repositories without `termsRequired` fail CI.

Follow-up the same day: image-generation and text-to-speech models (Qwen-Image family, ERNIE-Image-Turbo, Qwen3-TTS, VoxCPM2) left the shared catalog (334 → 323), and FrogNano-4B is role `text` because its card says image input is unsupported. The Mac `model-catalog.json` dropped 47 safety-stripped (abliterated, uncensored, heretic, OBLITERATED, Dolphin) and vendor-named (Claude/Opus, Fable/Composer, DeepSeek-V4-Pro, FuseO1) community fine-tunes (265 → 218); the shared denylist now covers both catalogs.

2026-11-07 regeneration (323 → 309; 238 Apache-2.0, 45 MIT, 26 Gemma Terms): reviewed `UPSTREAM_OVERRIDES` keep the hand-corrected entries whose cards still name the wrong base (mlx-community Qwen2.5 Instruct, gemma-3 -it MLX, olmOCR-2 MLX, PaddleOCR-VL-1.5 MLX, SmolLM v0.2, GLM-4-32B). Community fine-tunes named after their base and REAP expert-pruned builds are rejected, Dolphin 3.0 is denylisted, and duplicate builds of one upstream collapse to one (MLX prefers 4-bit). Details in `DECISIONS.md`.

## 2026-10-20 — Handwriting OCR candidate (not installed, no download wired)

Handwriting currently reads through Apple Vision's accurate tier on macOS and, on every engine, a VLM transcription of text-bearing images during Describe Files (Deep Analyze). A dedicated ONNX handwriting reader would let Windows and Linux read handwriting during the scan without a VLM. Licenses were read from the Hugging Face model cards and API on 2026-10-20. Nothing below is in `catalog.json`; adding one needs the owner's sign-off, a pinned revision and hashes, the license gate, and a measurement on the dev machines.

| Candidate | License (code / weights) | Size (ONNX) | Published speed | Verdict |
|---|---|---|---|---|
| [PP-OCRv6 small det + rec](https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx) | Apache-2.0 / Apache-2.0; upstream ships the ONNX files | rec 21.2 MB + det 9.9 MB (2.5M + 5.2M params) | 0.61 s per full page on a Xeon 8350C, 1.29 s on Apple M4 (ORT, includes I/O and pre/post-processing) | **Recommended candidate.** CPU tier; detector + line recognizer, fits the existing PaddleOCR opt-in slot below |
| [PP-OCRv6 medium det + rec](https://huggingface.co/PaddlePaddle/PP-OCRv6_medium_det_onnx) | Apache-2.0 / Apache-2.0 | rec 76.6 MB + det 62.0 MB | 3.3 s per page on CPU, 1.4 s with OpenVINO | GPU tier of the same family |
| [TrOCR small / base handwritten](https://huggingface.co/microsoft/trocr-base-handwritten) | MIT code; `base` card says MIT, `small` and `large` cards carry no license | small 62M params (Xenova ONNX: 87 + 159 MB fp32, no license field) | about 0.3 s per line on 2 CPU threads, so 7–9 s per 25-line page; needs a separate line detector | **Fails the strict gate:** fine-tuned on IAM, whose terms are non-commercial research, and two of three cards have no license |
| Kraken PP-OCRv6 HTR | Apache-2.0 / Apache-2.0 | 13–64 MB safetensors, no ONNX | not published | Hold: trained partly on IAM, would need our own ONNX export |
| Surya, Chandra, MinerU2.5, Qwen2.5-VL-3B | restrictive or non-commercial | — | — | Excluded |

Accuracy figures for PP-OCRv6 are Baidu's own benchmark (handwritten English 61.1 for small, 67.8 for medium; large VLMs score about 73). The handwriting training data isn't documented. On Windows, `Windows.Media.Ocr` reads printed text only, and the Windows AI `TextRecognizer` that does read some handwriting needs a Copilot+ PC NPU, Windows 11 25H2 and Windows App SDK 1.7.1+ with package identity, so it isn't a general answer.

## 2026-10-15 — Gap research: families not yet in the catalog

Checked against the 333-entry catalog for widely used local models it lacks. Licenses are from the upstream model cards; none of these is added to `catalog.json` yet, because each entry needs a pinned revision, file hashes and a runtime check. The new macOS `benchmarkModel` and the shared engine's benchmark are the measurement gate for promotion.

| Candidate | Role | License | Runtime notes | Verdict |
|---|---|---|---|---|
| [Granite-Docling 258M](https://huggingface.co/ibm-granite/granite-docling-258M) | Document conversion and OCR for scans, PDFs, tables | Apache-2.0 | Official [MLX build](https://huggingface.co/ibm-granite/granite-docling-258M-mlx); ONNX conversions exist | Shortlist: tiny doc/OCR tier on every platform |
| [Florence-2 base/large](https://roboflow.com/model/florence-2-image-captioning) | Fast captions, OCR, detection | MIT | 0.23B/0.77B; ONNX ports run on ONNX Runtime | Shortlist: CPU-friendly caption/OCR tier for Windows and Linux |
| [SigLIP 2 SO400M / base](https://mixpeek.com/model/google/siglip2-so400m-patch14-384) | Image-text embeddings for semantic search | Apache-2.0 | 109 languages; needs a cache-identity migration like the CLIP space | Shortlist: multilingual successor to CLIP ViT-B/32 |
| [Kimi-VL-A3B-Instruct](https://huggingface.co/moonshotai/Kimi-VL-A3B-Instruct) | Vision chat and captions, MoE 16B / 3B active | MIT weights; code Apache/MIT | [MLX conversions](https://huggingface.co/mlx-community/Kimi-VL-A3B-Thinking-4bit) exist; mlx-swift-lm architecture support unverified | Candidate for 32 GB+ Macs |
| [GLM-4.1V-9B-Thinking](https://huggingface.co/mlx-community/GLM-4.1V-9B-Thinking-8bit) | Thorough visual reasoning | MIT | MLX 4/5/8-bit exist; thinking must be disabled for short structured output | Candidate thorough tier |
| [Phi-4-multimodal-instruct](https://huggingface.co/microsoft/Phi-4-multimodal-instruct/blob/main/LICENSE) | Vision + audio + text, 5.6B | MIT | No mlx-community port; Microsoft ships ONNX | Candidate for the Windows ONNX path |
| [Pixtral 12B](https://huggingface.co/mistralai/Pixtral-12B-2409) | Vision chat | Apache-2.0 | [mlx-community 4-bit](https://huggingface.co/mlx-community/pixtral-12b-4bit) ~7.2 GB | Candidate large tier |
| [Ovis2.5 2B/9B](https://huggingface.co/AIDC-AI/Ovis2.5-9B/blob/main/README.md) | Charts, STEM, native-resolution vision | Apache-2.0 | No MLX or GGUF port found | Hold until a supported runtime exists |
| [DeepSeek-OCR](https://huggingface.co/deepseek-ai/DeepSeek-OCR/blob/main/LICENSE) | Dense OCR, ~3B | MIT | mlx-community 8-bit exists | Candidate OCR tier; DeepSeek-OCR-2 license needs a separate check |
| [Molmo 2 4B/8B](https://huggingface.co/allenai/Molmo2-8B) | Video, pointing, counting | Apache-2.0 weights | Card notes training data limited to non-commercial research | Hold: needs a commercial-use review before it can be offered |
| [Moondream 3 / 3.1](https://huggingface.co/moondream/moondream3-preview) | Small vision MoE | BSL 1.1 / custom | Moondream 2 is Apache-2.0 | Excluded (3.x); Moondream 2 is an option for the tiny tier |
| [LFM2-VL / LFM2.5-VL](https://www.liquid.ai/lfm-license) | Edge vision | LFM Open License, $10M revenue threshold | — | Excluded: not commercial-clean |

First macOS measurements (`benchmarkModel`, M1 Pro 16 GB, 2026-10-15): Qwen3.5 0.8B 4-bit — 3.1 s load, 611/45 tok/s prefill/decode, 3.5 s per image, 1.2 GB peak, valid JSON and correct colour probe on every run. Qwen3-VL 4B 4-bit — 3.5 s load, 145/24 tok/s, 4.4 s per image, 3.9 GB peak (decimal GB; first recorded as 1.1 and 3.6 GiB), same quality checks passed. SmolVLM 256M/500M cannot run on the pinned mlx-swift-lm (fixed 384 px Idefics3 tiles vs the checkpoints' 512 px) and are filtered from the Mac catalog.

Measurement plan: install each shortlisted model only with the owner's approval, run `benchmarkModel` on the 16 GB M1 Pro and the RTX 2060 PC, and promote only on the existing gate (meaningful accuracy gain, or at least 20% faster without regression on the same fixtures).

## 2026-10-13 — Extended local model catalog (333 opt-in models)

`shared/models/catalog.json` (generated by `shared/scripts/build_model_catalog.py`; rationale in `DECISIONS.md`) adds opt-in models beside the curated defaults below, which remain unchanged and always win. Current contents: 113 vision (77 llama.cpp GGUF, 36 MLX), 195 text, 13 embedding, 10 speech, 2 reranker; 256 Apache-2.0, 51 MIT, 26 Gemma-terms. Vision tiers by estimated resident memory: 17 tiny (≤3 GB), 24 small (≤6 GB), 35 medium (≤12 GB), 26 large (≤24 GB), 11 extra-large (≤48 GB).

The shared engine runs catalog GGUF vision models through the same verified download, provenance, resident server, Deep Analyze and timeline paths as curated models (`vlm/catalog-<id>/`). Text, embedding, speech and reranker entries are listed for future consumers; macOS currently reports its curated MLX models only. Each entry is a `candidate` until `benchmarkModel` (Settings → Model catalog, or `examples/model_bench`) measures it on the user's machine. Architecture support depends on the bundled llama.cpp build; an entry newer than the runtime fails at load and the benchmark records that failure.

First real measurement (Linux VM, 4 vCPU, no GPU, llama.cpp b9254): SmolVLM-256M-Instruct Q8_0 loaded in 0.72 s, decoded 194 tok/s, captioned in 1.28 s/image with 0.67 GB peak; 80% probe accuracy but 0% valid moment JSON — fast enough for tags, not for significant-moment drafts. Qwen3-VL-2B Q8_0 was correctly refused by memory admission on the 7 GB VM.

Regenerate with `python3 shared/scripts/build_model_catalog.py --cache <dir>` (anonymous Hugging Face API access allows 500 requests per 5 minutes; the cache makes reruns cheap), then run the runtime egress gate and the engine's catalog tests.
## 2026-10-10 — Offline Mac model catalog

The bundled Mac resource `FileIDModelInfo/Resources/model-catalog.json` adds 265 pinned, permissive MLX conversion entries: 72 vision and 193 text models, including quantization variants. It is Mac-specific (filtered to the pinned `mlx-swift-lm` runtime) and separate from the portable `shared/models/catalog.json` above; it is checked by `shared/scripts/check_model_catalog.py`, the existing license-policy release gate, and native SharedTests. The current entries and their recorded upstream chains declare Apache-2.0. The policy also permits MIT, BSD-2-Clause, BSD-3-Clause and ISC. No additional restricted/gated model was admitted; the existing eight favorites and separately accepted Gemma terms remain unchanged.

Metadata was audited against Hugging Face repository cards/configurations and the actual pinned `mlx-swift-lm 3.32.3` factory registries. Each entry records an immutable conversion revision, safetensors SHA-256/size, role, architecture, affine quantization and recursive upstream license lineage (bounded to six levels/twenty repositories). Unsupported quantization, incomplete processor/tokenizer templates, unavailable/gated lineage, and potential additional Llama terms were excluded. These are registered runtime candidates, not hundreds of measured-quality recommendations. The catalog is bundled and does not contact Hugging Face during browsing or inference. Updating it is an explicit developer audit, never a background update.

Mac model identifiers use the existing string IPC field: `hf:<owner>/<repository>`. Old tokens/aliases round-trip unchanged. Unknown repositories fail before downloads; new catalog downloads and offline attestations must match the audited weight hashes and sizes. The existing pinned downloader is the sole network path. MLXLLM is a product of the already locked package, not a new dependency. Text models are available for catalog chat; image/frame jobs require vision models. The native browser filters role, physical-memory fit, installed state and search, and requires a separate Download & load action. Selection alone starts no download. Optional IPC v1.15 `ChatRequest.modelKind` prevents a different loaded model from silently answering the selected chat request.

The explicitly approved Qwen3.5 2B evaluation download passed (1,749,080,975 bytes in the internal test cache). Actual sequence inference was refused by the unchanged available-memory guard on this 16 GB Mac; no accuracy result/default promotion follows. The synthetic ball/bucket fixture and opt-in moment/goal tests remain available for a Mac with sufficient free/inactive memory. No new catalog weight was downloaded or bundled. The approved local tokenizer passed a CPU-only template-context test: optional reasoning is disabled for bounded organizer output. This does not load model weights or establish inference accuracy. Windows/Linux need their own audited GGUF/ONNX artifacts and compatible runtime adapters; the MLX catalog is Mac-specific.


## 2026-10-08 — Compact macOS vision model candidate

The macOS picker now exposes opt-in `qwen3_5_2b`, pinned to `mlx-community/Qwen3.5-2B-MLX-4bit` revision `93760be4f1f69842a46bc13dbdc0f19e291392a3` (Apache-2.0, approximately 1.75 GB download). Its 3 GB resident-memory estimate and 3 seconds/image ETA are provisional; it is not a recommended default or an accuracy claim. The [upstream model](https://huggingface.co/Qwen/Qwen3.5-2B) and [MLX conversion](https://huggingface.co/mlx-community/Qwen3.5-2B-MLX-4bit) both declare Apache-2.0. No weights are bundled. A separate user-initiated download is required for measured inference and promotion against the same rights-cleared FileID fixtures as the incumbent model.

The macOS runtime moved from `mlx-swift-examples` 2.29.1 to [mlx-swift-lm 3.32.3](https://github.com/ml-explore/mlx-swift-lm/releases/tag/3.32.3), which registers the Qwen3.5 VLM architecture. `swift-transformers` 1.3.4 provides only local tokenizer loading; FileID's existing pinned, integrity-checked `VLMDownloader` remains the sole VLM network path. A local-only factory adapter rejects any different repository and preserves the existing per-model prompt and stop-token configuration. Existing memory admission is unchanged. Runtime inference, latency, accuracy, and packaging regression still require on-device evaluation before changing the default or release claims.

IPC v1.11 related-take proposals reuse each platform's existing installed image/video CLIP embedding space (`CLIPEmbeddingSpace.modelID` on macOS, `mobileclip_s2` in the Rust engine). The same-model cosine is displayed as visual similarity, not an event-outcome probability. No weight, download, runtime, or license choice changed; threshold calibration remains a release task.

## 2026-10-08 opt-in moment-model evaluation

The approved, verified Qwen3-VL 4B MLX fixture reached the existing `FileID.ModelMemoryAdmission` guard on the 16 GB M1 Pro and did not run inference. The Metal test-bundle path issue was diagnosed in TEST.md. No new weights, model promotion, latency result, or recognition-quality claim resulted; keep the memory guard and use an internal rights-clear corpus for a later measured run.

## 2026-10-08 evaluation status

The isolated macOS UI fixture and default native test run used no downloaded model weights. The registered visual-model evaluation tests require an explicit internal frame corpus and model cache; they skipped in this checkpoint. No candidate was promoted or quality claim changed. Keep the existing memory-admission and license gates when resuming model-backed assessment.

FileID never ships model weights. Downloads are user-initiated, show progress and cancellation, and verify each static artifact's SHA256 before use; no download telemetry. `shared/models/manifest.json` is the one definition of the curated download set: each artifact's Hugging Face repo, revision, path, SHA256, size, platforms and license (the URL is `https://huggingface.co/<repo>/resolve/<revision>/<path>`). The shared Rust engine embeds it (`models/manifest.rs`) and the registry only adds install paths; the macOS table `ModelManifest.swift` is generated from it by `shared/scripts/gen_model_manifest.py` (`--check` in CI). MLX VLM repositories pin immutable revisions and resolve their files' hashes from that revision. `check_runtime_egress.py` verifies every manifest URL (Hugging Face only, immutable commit revision, SHA256, permitted license, fail-closed parsing); every artifact is pinned to a commit, and a moving ref such as `main` fails the gate.

## macOS CLIP cache identity (2026-10-02)

The native shared manifest pins OpenCLIP ViT-B/32 image/text ONNX and BPE vocabulary/merges. Mac loaders verify local bytes against those pins before creating sessions. Cache identity also includes the current RGB stretch-to-224, BPE-77 and L2-normalization contract. Legacy `mobileclip_s2` rows are excluded until fresh inference refreshes them. This is compatibility validation, not promotion of a replacement model or a measured accuracy gain.


## Licensing posture — commercial-clean (Apache-2.0 project)

As of the 2026-05 commercial-clean pass, **every weight FileID downloads by default is permissively licensed (Apache-2.0 / MIT)** — no non-commercial weights in the core feature set. This keeps the project (Apache-2.0, see root `LICENSE`) free to be open-sourced *and* commercialized later without a weight-licensing blocker. The non-commercial InsightFace face stack (ArcFace + SCRFD) and the research-only Apple MobileCLIP-S2 / Qwen2.5-VL-3B were replaced. The one conditional model, Gemma-3-4B, is commercially usable under Google's Gemma Terms and stays an opt-in, user-initiated download (its terms surface in the install flow).

Both engines now implement RAM++, ViT-B/32 and SFace. Cross-platform face round trips validate provenance and compatible processing spaces; dimensions alone do not establish compatibility. Hardware accuracy and complete port UI acceptance remain separate gates.

## ML stack per platform

| Capability | macOS | Windows | Notes |
|---|---|---|---|
| In-scan image tagging (primary) | RAM++ Swin-L @384 (ONNX) | **RAM++ Swin-L @384 (ONNX, fp16)** | Recognize Anything Plus, 4585-tag multi-label tagger, Apache-2.0. Primary auto-tagger; CLIP zero-shot scene tags are the fallback when RAM++ isn't installed. |
| Image semantic embedding (search) | CLIP ViT-B/32 (ONNX, tract CPU) | **CLIP ViT-B/32 (ONNX)** | OpenAI/OpenCLIP ViT-B/32, MIT. 512-d float32 LE, L2-normalized — embeddings byte-cross-compatible across platforms. |
| Text semantic embedding (CLIP) | CLIP ViT-B/32 text (ONNX) + BPE vocab | **CLIP ViT-B/32 text (ONNX)** + BPE vocab | Same OpenAI BPE tokenizer port; embeddings cross-compatible. |
| Face detection + 5-pt landmarks | Apple Vision (`VNDetectFaceRectanglesRequest`) | **YuNet (ONNX, OpenCV Zoo)** | YuNet is MIT. Different detectors → boxes aren't byte-identical, but 5-pt landmarks feed a shared alignment template so embeddings match. |
| Face embedding | SFace (ONNX, tract CPU) | **SFace (ONNX, tract CPU)** | SFace (OpenCV Zoo) is Apache-2.0, **128-d** L2-normalized. Replaces 512-d ArcFace; person-clustering DBs round-trip once both platforms are on SFace. |
| OCR | Apple Vision `VNRecognizeTextRequest` (fast tier) | Windows.Media.Ocr (built-in WinRT) default; PaddleOCR ONNX opt-in | Built-in OCR is fast + free + multilingual on both. |
| Vision-language models (Deep Analyze) | MLX: Qwen2.5-VL · Qwen3-VL · Gemma 3 · Mistral Small 3.2 · PaliGemma | llama.cpp: Qwen 2.5-VL 7B · optional Qwen3-VL 4B/8B · Gemma 3 · Mistral-Small-3.2 | MLX is Apple-Silicon-only. The pinned Windows llama.cpp runtimes are x64; Linux and Windows ARM64 VLM runners remain unverified. |

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
| Source (macOS) | Pinned ViT-B/32 ONNX, run by the shared Rust engine on tract (CPU) |
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

### SFace face embedding (tract CPU on every platform)

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

| Model | Disk (GGUF + projector) | RAM est. | Use case | License | Source |
|---|---|---|---|---|---|
| **Qwen 2.5-VL 7B** | ~6.1 GB | ~12 GB | **Recommended default** (≥ 16 GB + dGPU) | Apache-2.0 | [ggml-org/Qwen2.5-VL-7B-Instruct-GGUF](https://huggingface.co/ggml-org/Qwen2.5-VL-7B-Instruct-GGUF) |
| **Qwen3-VL 4B** | **3,333,461,920 bytes** | Not measured | Optional, manually selected | Apache-2.0 | [Official Qwen GGUF, revision `1cd86afb9a95c410a6038ab3b40d8b578c892266`](https://huggingface.co/Qwen/Qwen3-VL-4B-Instruct-GGUF/tree/1cd86afb9a95c410a6038ab3b40d8b578c892266) |
| **Qwen3-VL 8B** | **6,186,814,624 bytes** | Not measured | Optional, manually selected | Apache-2.0 | [Official Qwen GGUF, revision `f982a07559d4a2f6c8744d840bf6fccab30eea96`](https://huggingface.co/Qwen/Qwen3-VL-8B-Instruct-GGUF/tree/f982a07559d4a2f6c8744d840bf6fccab30eea96) |
| **Qwen3-VL 2B** (catalog GGUF Q8_0) | 2,279,480,352 bytes (1,834,427,296 + 445,053,056 projector) | Not measured; engine admission estimate ~3.2 GB | End of the default ladder: what an 8 GB PC runs when the 4B is refused for memory | Apache-2.0 | [ggml-org/Qwen3-VL-2B-Instruct-GGUF @ `ea6a11058182570be6436b9a2e4ee7f7b49f908d`](https://huggingface.co/ggml-org/Qwen3-VL-2B-Instruct-GGUF/tree/ea6a11058182570be6436b9a2e4ee7f7b49f908d) (pinned in `shared/models/catalog.json` as `qwen3_vl_2b_instruct_gguf_q8_0`) |
| **Gemma 3 4B (vision)** | ~3.35 GB | ~8 GB | Lighter / weak-box fallback | Gemma Terms (opt-in) | [ggml-org/gemma-3-4b-it-GGUF](https://huggingface.co/ggml-org/gemma-3-4b-it-GGUF) |
| **Mistral-Small-3.2 24B** | ~15.18 GB | ~20 GB | Large opt-in captioner | Apache-2.0 | [bartowski/mistralai_Mistral-Small-3.2-24B-Instruct-2506-GGUF](https://huggingface.co/bartowski/mistralai_Mistral-Small-3.2-24B-Instruct-2506-GGUF) |

The Qwen3-VL 4B pair is 2,497,281,664 bytes Q4_K_M GGUF + 836,180,256 bytes F16 vision projector; the 8B pair is 5,027,784,800 + 1,159,029,824 bytes respectively. Their four immutable-revision URLs and LFS SHA256 pins are in `shared/models/manifest.json`. The existing RAM figures above are estimates, not measured Qwen3 working sets.

The pinned [llama.cpp b9254](https://github.com/ggml-org/llama.cpp/releases/tag/b9254) source recognizes [Qwen3-VL 4B/8B](https://github.com/ggml-org/llama.cpp/blob/b9254/src/models/qwen3vl.cpp) and its [vision projector](https://github.com/ggml-org/llama.cpp/blob/b9254/tools/mtmd/clip.cpp). FileID has **not** run these weights for image inference or measured their VRAM, RAM, caption quality, or throughput. Keep Qwen2.5-VL 7B as the default pending a representative benchmark. Linux has no supported installed VLM runner yet (the engine probes `.exe`/PE and the runtime registry pins Windows ZIPs); Windows ARM64 likewise has no pinned native runner. Do not advertise Qwen3 inference on those platforms.

### macOS lineup (MLX)

Current `AIModels.swift` exposes seven model kinds; repositories, revisions, sizes and licenses come from the shared manifest through the generated `ModelManifest.swift`. Qwen3-VL is available on macOS; Windows/Linux Qwen3-VL runtime parity is still pending.

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
- macOS: `~/Library/Application Support/FileID/Models/huggingface/models/<repo>/` (the sandbox container's copy in Mac App Store builds); a Developer ID install that already holds models in `~/Documents/huggingface/models/` keeps using that folder, nothing is moved.
- Windows: `%LOCALAPPDATA%\FileID\Models\vlm\<model-dir>\{model.gguf,mmproj.gguf\}`; registry-managed Hugging Face models use `%LOCALAPPDATA%\FileID\Models\HuggingFace\<repo>\` (user-initiated installs; outside Documents).

## GPU runtimes

There are no Performance Packs. Every ONNX model runs on tract on the CPU. The only
optional GPU runtime is the CUDA llama.cpp build for Describe Files and Chat, which
the user installs locally (FileID does not download vendor runtimes); Vulkan llama.cpp
ships in the box.

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

[MLX Swift LM 3.32.3](https://github.com/ml-explore/mlx-swift-lm/releases/tag/3.32.3) includes Qwen3.5 state/sanitization, Gemma 4 loading and image processing, cancellation and guided-generation fixes. The macOS dependency transition described above landed as an opt-in candidate; model-backed identical-fixture regressions, OS/SDK packaging, and MTP/offline behavior remain evaluation gates. No model was promoted to the automatic default.

[Gemma 4's card](https://ai.google.dev/gemma/docs/core/model_card_4) lists Apache-2.0 and substantial separate embedding storage for its E-series. Effective parameter counts do not determine resident memory. Budget actual converted tensors, context/recurrent states and visual buffers. [llama.cpp's router](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md) supports `--models-max` (default four; zero unlimited), but that is a model-count cap, not FileID memory admission. Measure loading concurrency, resident models and parallel requests separately; keep interactive work responsive on the 16 GB Mac before changing defaults.


Native search-index admission now estimates live/raw vector storage and graph/manifest decoding/snapshot buffers, including historical cache files. It uses existing system/headroom checks before rebuilding or loading. This is not shared multi-model allocation accounting or a measured replacement-model benefit. Task routing, simultaneous residency and pressure benchmarks remain required.
