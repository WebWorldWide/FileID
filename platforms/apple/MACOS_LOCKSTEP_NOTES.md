# macOS lockstep (WS-MAC) — build + verify notes

Current branch: **`agent/macos-parity-hardening`**, based on `origin/main` at
`f96a6e1` after a 2026-08-02 fetch found no newer Windows changes. The Swift below
has now been compiled, tested, launched, and exercised natively on Apple Silicon.

## Current native validation — 2026-08-02

- The strict Swift gate passes all 354 tests in 70 suites with complete concurrency
  checking and warnings treated as errors. `run.sh --no-wipe` assembled and launched
  the production `FileID.app` plus its child engine.
- A read-only scan of 181 supported files on the external Adlon corpus exercised
  discovery, full-source bounded image decoding, RAM++, CLIP, SFace, People, Deep
  Analyze, and Restructure. One corrupt GIF failed explicitly rather than being
  counted as a success.
- The corrected face pipeline detected 226 faces, retained and embedded 174,
  assigned 171, and produced 29 visible people. Representative cards render real
  face crops. Same-file cannot-links suppress unsafe merge suggestions, and
  persisted “Different people” verdicts use stable face anchors.
- Qwen3-VL 4B is the recommendation for 8 GB Macs; Qwen3-VL 8B is the default on
  this 16 GB Mac. Across six copied Adlon images, 4B completed in 30.92 s at a
  4.8 GiB peak footprint and 8B completed in 47.49 s at 7.2 GiB. The 8B output was
  generally more concise and grounded. Deterministic decoding plus the year,
  identity, OCR, and filename guards protect both Swift MLX and Rust llama.cpp
  paths from unsupported claims; the final adversarial rerun produced
  `boys-sitting-bench-windows` without inventing names.
- RAM++ uses its static Core ML shapes and four bounded workers. A copied-Adlon
  benchmark improved from 27.73 s to 22.59 s while preserving all 222 emitted tags
  and scores byte-for-byte.
- Live production screenshots confirm Cleanup is pinned to the top and the sidebar
  track ends at the active dot. Geometry tests cover the exact center of all five
  dots. The preview tag field has a visible gold **Apply tag** button and Return
  default action.
- Restructure generated a read-only 27-action plan: 16 tidying and 11 reorganizing,
  with no source-equals-destination rows. The tab now follows the active scanned
  library root, matching Windows and the engine contract. Apply was never invoked.
- Process-level engine tests now use an isolated database path. The 334 fake-JPEG
  failures and eight temporary scan sessions created by older local runs were backed
  up and removed, restoring the live catalog to 181 Adlon rows. Preview tags also
  deduplicate equal VLM/RAM++ labels without discarding source provenance.
- The acceptance-pass Adlon path/size/mtime fingerprint remained
  `e1e52c67d0d93e45704284aa17868fab9bd3885c84b2e9207adc5c79ac44e58f` before
  and after testing. The final audit also found every one of the 181 indexed paths
  present with its cataloged size and modification time unchanged. SQLite
  `quick_check` is `ok`, and `face_verifications` is empty.
- Cross-platform behavior is the contract, not byte-identical face embeddings:
  Windows detects with YuNet while macOS uses Apple Vision, so landmark positions
  and aligned crops can differ slightly. Hosted CI, signing/notarization,
  clean-machine installation, Windows GPU runtime, and ARM64/other-hardware gates
  remain external evidence.

## Historical implementation notes

The sections below record the original `macos-lockstep` bring-up and are retained
for archaeology. Their “not compiled/run” and pending task statements describe the
state at the time they were written, not the current status above.

## Part 1 — committed (`ab9b9ae`)

| File | Change |
|---|---|
| `shared/.../AIModels.swift` | `FaceEmbedderKind` → single `.sface` (128-d, Apache, OpenCV Zoo). `AIModelKind` drops non-commercial Qwen-3B → Apache 7B, adds Mistral-Small-3.2, keeps Gemma/PaliGemma. New `migrated()` maps legacy rawValues. |
| `engine/.../ArcFaceService.swift` | SFace input = **raw [0,255] RGB** (was ArcFace's `(px-127.5)/127.5`). |
| `engine/.../FaceAlign.swift` (NEW) | Faithful port of Windows `face_align.rs` — 5-pt similarity alignment to the 112×112 template. **Not yet wired into detection.** |
| `engine/.../IdentityClustering.swift` | Hyperparameters = the PRE-retune Windows values (pass1 0.66 / pass2 0.54). **STALE vs Rust as of 2026-07-05**: the Rust engine retuned to pass1 0.50 / pass2 0.45 + mutual-kNN default-on + a pre-clustering quality gate, label-calibrated to People-tab F1 1.0. macOS now carries the mutual-kNN + quality-gate MECHANISMS (`mutualKNN` param + `FILEID_FACE_CLUSTER_MIN_QUALITY`, both DEFAULT-OFF so behaviour is unchanged), but the Rust VALUES do NOT transfer blind — macOS uses Apple Vision quality (different scale) and FaceAlign is **not yet wired into detection** (row above), so its embeddings differ. **On-Mac task:** wire FaceAlign, then run a label-calibration pass (the face-labeler tool works on the Mac's DB + `face_crops`) to set macOS's pass1/pass2/gate. |
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
   BOS/EOS-wrapped tokens as the Windows `clip_tokenizer`. The superseded,
   unpinned offline CLIP installer and obsolete CoreML conversion script were
   removed; installs now use only the verified in-app path.

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

## Part 3 — DB-contract lockstep (PR #5, branch `macos-lockstep`, 2026-06-02)

The persisted-bytes half of lockstep: a Windows-written SQLite library and a
macOS-written one must agree on the contract. Grounded in the **current**
Windows `fileid-engine` source (byte-faithful reference), implemented via a
10-cell workflow + 4 adversarial verifiers. **Edit-only — built nowhere here;
macOS CI (`swift build -c release` + `swift test`, Xcode 16 / Swift 6) is the
build gate. The cross-platform round-trip that DEFINES lockstep still needs a
Mac to validate (open a Windows-written `fileid.sqlite` on macOS + vice-versa).**

Landed (round-trip-critical):
- **Timestamp epoch** 2001-ref → Unix(1970) across the whole chain (writer +
  every reader): `DBWriter` files.created/modified/scanned_at, `DeepAnalyzeRunner`
  vlm_analyzed_at, `FaceClustering` persons.created_at/last_clustered_at,
  `ReadStore` (toFileRow + scan_sessions; fixed a pre-existing writer=1970/
  reader=2001 mismatch), `Restructure` (dropped `+978_307_200`). Existing
  macOS DBs written with the old 2001-ref base read wrong until re-scanned.
- **Tag source** `vision`→`auto` (writer + all readers); rescan DELETE+REPLACE
  + trim/skip-empty; dropped orientation/capability extra tags; hyphen
  sanitizer (byte-faithful to `sanitize_proposed_name`).
- **IPC**: `startScan` → rootPath/rootDisplay?/rescan/excludedPaths? (app
  resolves bookmark; **unsandboxed** model — no `.entitlements`);
  +`purgeExcluded`, +`markPersonsDifferent`, +`wipeLibrary`, +8 reply
  events/DTOs, +`EngineInfo.hardware`/`HardwareInfo`, +`EngineError.modelKind`,
  +`deepAnalyzeAll.tagsOnly`.

Deferred here (need a Mac to behavior-verify; some overlap Part 2):
1. **Face bbox** coordinate-space parity (Windows stores **pixels**, macOS
   **normalized 0..1**). The JSON-format swap was **reverted** — it broke macOS
   clustering (`DBWriter.bboxArea`/`PeopleView.cropFace` still CSV-split) and
   still wasn't byte-faithful. True parity = pixels + JSON + update both
   consumers + the cluster threshold; do it together with Part 2 #1 (FaceAlign).
2. **RAM++ tagger** (Part 2 #3) + `tags.score` column — until then macOS emits
   Vision identifiers (score NULL), Windows emits RAM++ nouns (scored).
3. **content_hash + file_ref + rename/move heal** — macOS scan writes both NULL
   (no inode/BLAKE3), so cross-platform move-heal one-directional.
4. **Restructure routing** rewrite (People/Places/Documents `<Year>` subfolders,
   video/audio arms, month-name) — macOS uses a different semantic architecture.
5. **VLM tag generation** (`source='vlm'`) + the deep-analyze caption knobs.

Known pre-existing drift discovered (NOT a lockstep/DB blocker, latent because
each app talks only to its own engine): the schema + Swift use `...ID`
(uppercase) for person/query/batch id fields, but Rust serde `camelCase`
produces `...Id` (lowercase d) and the C# app matches Rust — so the schema and
the Windows wire disagree on those keys. Only matters for a hypothetical
cross-language IPC pipe; the DB round-trip is unaffected.
