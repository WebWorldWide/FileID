# macOS lockstep (WS-MAC) — build + verify notes

Branch: **`macos-lockstep`**. Mirrors the Windows commercial-clean / RAM++ stack
(merged to `main` 2026-05-30) into the macOS app. **All Swift here was written in
a Windows environment and has NOT been compiled or run — `swift build` on your
Mac and we iterate on the errors.** Cross-platform face-embedding parity is
*approximate*, not byte-exact: Windows detects faces with YuNet, macOS with
Apple Vision, so landmark positions (and thus aligned crops) differ slightly.
The goal is that the same person clusters together on both platforms, not
byte-identical embeddings.

## Part 1 — committed (`ab9b9ae`)

| File | Change |
|---|---|
| `shared/.../AIModels.swift` | `FaceEmbedderKind` → single `.sface` (128-d, Apache, OpenCV Zoo). `AIModelKind` drops non-commercial Qwen-3B → Apache 7B, adds Mistral-Small-3.2, keeps Gemma/PaliGemma. New `migrated()` maps legacy rawValues. |
| `engine/.../ArcFaceService.swift` | SFace input = **raw [0,255] RGB** (was ArcFace's `(px-127.5)/127.5`). |
| `engine/.../FaceAlign.swift` (NEW) | Faithful port of Windows `face_align.rs` — 5-pt similarity alignment to the 112×112 template. **Not yet wired into detection.** |
| `engine/.../IdentityClustering.swift` | Hyperparameters = the on-hardware-calibrated Windows SFace values. |
| `engine/.../Storage/Database.swift` | `v12_face_model_reset` wipes face tables (mirrors Windows v12). |
| `engine/.../DeepAnalyze.swift`, `AIModelsEngine.swift`, `app/.../EngineClient.swift`, `ArcFaceModelInstaller.swift` | Cascade for the enum changes + SFace download URL. |

### Build it first
```bash
cd platforms/apple
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift build
```
Likely first errors to iterate on:
- `ModelConfiguration(id:)` in `DeepAnalyze.vlmConfig` — confirm this initializer exists in the pinned MLX-VLM (`mlx-swift-examples`). If the registry has a `qwen2_5VL7BInstruct4Bit` constant, prefer it.
- **MLX-VLM may not support Mistral-Small-3.2's architecture.** If `ensureLoaded` can't resolve it, drop the `mistralSmall32` case (and its `vlmConfig`/`gpuCacheBudget` arms) — it stays Windows-only.
- Any Settings/onboarding UI that switched over the old `FaceEmbedderKind` variants or `AIModelKind.qwen2VL3B` (grep the `app/` target).
- `grep -rn "512\|2048" engine/.../FaceClustering.swift` etc. — confirm nothing hard-codes the old 512-d / 2048-byte face dimension.

### Verify (on-device)
- Settings → Deep Analyze shows **Qwen2.5-VL 7B** as the default; no 3B.
- Wipe DB, rescan a folder with one recurring person → People tab forms a small
  number of clusters (not one 90% mega-blob, not all singletons). This is the
  same calibration that on Windows cut the largest cluster 90% → 7%.

## Part 2 — remaining (write with the compiler in the loop)

1. **Wire `FaceAlign` into the detection pass.** Today `FaceClustering.cropFaceCGImage`
   crops by bbox only (no alignment). To match Windows: in the Vision
   face-detection code, capture the 5 landmarks from `VNFaceObservation.landmarks`
   (`leftEye`, `rightEye`, `nose`, and two mouth corners from `outerLips`),
   convert from Vision's **normalized-to-bbox, bottom-left-origin** coords to
   **absolute top-left pixel** coords, reorder to FileID order
   `[left_eye, right_eye, nose, mouth_left, mouth_right]`, then
   `FaceAlign.align112(source: fullImageCGImage, landmarks:)` → `ArcFaceService.embed(_:)`.
   Until wired, faces use bbox-resize, which won't match the Windows-aligned
   embeddings the cluster thresholds were calibrated against.

2. **CLIP → ViT-B/32 — DONE (commit `8aef43d`).** `MobileCLIPService` +
   `CLIPTextEncoder` now load the OpenCLIP ViT-B/32 ONNX via ORT (image 224×224
   CLIP mean/std; text `input_ids` int64 [1,77] zero-padded — matches
   `windows/.../clip_text.rs`); `onnxruntime` added to the app target; installer
   + Settings/Welcome UI updated; 512-d so no `clip_embeddings` schema change.
   **With faces + CLIP + VLM done, macOS ships zero research-only models —
   commercial-clean achieved.** Build-iterate spots: the ORT
   `ORTSessionOptions`/`appendCoreMLExecutionProvider` API surface, the ViT-B/32
   input/output tensor names, and that `CLIPTokenizer` emits the same
   BOS/EOS-wrapped tokens as the Windows `clip_tokenizer`. Also update/remove the
   now-superseded offline scripts (`scripts/install_clip_models.sh`,
   `scripts/build_clip_text_encoder.py`).

3. **RAM++ primary tagger.** New `RamPlusService.swift` mirroring
   `ArcFaceService`'s ORT pattern: load `ram_plus.onnx` (384×384, ImageNet
   mean/std), load `ram_plus_tags.txt` + `ram_plus_thresholds.txt` sidecars,
   4585 logits → per-class sigmoid → emit tags above the per-class threshold
   (top ~12). Wire into `Tagging.processFile` as the primary tagger; gate the
   existing CLIP scene-tags to fallback when RAM++ isn't installed (mirror the
   Windows `tagging.rs` `ram_plus_ran` gating). New installer entry →
   `huggingface.co/Web-World-Wide/ram-plus-onnx`.

4. **Docs.** Flip the `shared/docs/MODELS.md` macOS rows from "lockstep pending"
   to live once parts 1–3 build + verify.
