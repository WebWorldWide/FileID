# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

---

## V16.15 — verify on hardware: face crops + 1-2 word tags + smooth downloads (2026-05-21)

**Landed (engine clippy + 158 tests; C# format+BOM; build in VS).** Rebuild:
`pwsh -File platforms\windows\build\build-all.ps1 -Run`.
1. **Faces:** re-scan a folder with people → the People tab shows real cropped faces (not
   blank, not whole-image smears); same-person faces group; merge works. Existing DBs hold
   the OLD bad crops — use `-WipeDbOnly` (or re-scan) to regenerate. `SELECT COUNT(*) FROM
   face_prints` > 0; the `face_crops/*.jpg` look like faces.
2. **Tags:** `SELECT tag FROM tags WHERE source='vlm'` → all 1-2 words (no 3+-word phrases).
3. **Deep Analyze:** tab defaults to Qwen2.5-VL-3B; "Whole library" → full-sentence captions
   + smart names. (Qwen3-VL-4B unavailable as GGUF; 7B OOMs on 4 GB — see DECISIONS.)
4. **Downloads:** the rate/ETA rise smoothly and do NOT blink to 0 / "Stalled" at file
   boundaries in multi-file model bundles.

## V16.13 — verify on hardware: scan starts (no timeout) + SmolVLM tags / Qwen Deep Analyze (2026-05-21)

**Landed (engine clippy `-D warnings` clean; C# `dotnet format` + BOM clean — build in VS).**
Rebuild from the repo root: `pwsh -File platforms\windows\build\build-all.ps1 -Run`
(`-WipeDbOnly` for a fresh DB). Fixes the 4 GB-VRAM/DirectML model-load timeout + the
tagging/Deep-Analyze model split:

1. **Scan starts — no 30 s timeout.** First launch: scene matrix builds once
   (`engine.jsonl`: `[TAGGING] scene-label embeddings built elapsed_ms≈21000`) and the scan
   runs (no `model_load_timeout`). EVERY later launch logs `scene-label matrix loaded from
   cache` (no 21 s build) and starts <10 s. A `Models\clip_scene_cache\scene_matrix.bin`
   appears.
2. **Tagging = SmolVLM.** After a scan: `app.log` `Auto-chaining Deep Analyze (tags-only).
   model=smolvlm`; `SELECT COUNT(*) FROM tags WHERE source='vlm'` climbs.
3. **Deep Analyze = Qwen.** The Deep Analyze tab shows **Qwen 2.5-VL 3B active** by default
   (existing settings migrated off smolvlm); SmolVLM still selectable. Qwen cards show
   **Install** (not a false "Installed") until downloaded; after Install + "Whole library",
   `SELECT DISTINCT vlm_model FROM files` shows `qwen…` with captions. (On 4 GB VRAM Qwen 3B
   may be slow / spill to RAM — SmolVLM is the fast option.)
4. **(Follow-up, not this pass) faster ONNX:** ONNX runs on DirectML (perf-hint logs it).
   The CUDA ORT pack that would make ArcFace/SCRFD/CLIP ~3-5× faster is `not_yet_available`
   — needs the ORT 2.0.0-rc.10 CUDA provider DLLs sourced + hosted.

## V16.12 — verify on hardware: first-scan tagging + first-run speed + VLM fallback (2026-05-21)

**Landed (engine cargo check + clippy -D warnings clean; C# self-reviewed but
NOT compile-verified — WinUI CLI build is blocked on the dev box, build in VS).**
Rebuild from a VS Developer shell: `pwsh build/build-all.ps1 -Run` (add
`-WipeDbOnly` for a fresh DB, or `-Wipe -PreserveModels` to re-test first-run
install ordering without re-downloading multi-GB weights).

1. **First-scan tags (THE fix).** On a clean profile (`-Wipe -PreserveModels`
   keeps SmolVLM so this exercises the *installed* path; for the genuine
   first-run, use `-Wipe`): scan a folder. CLIP placeholder chips appear during
   the scan. When SmolVLM finishes installing after the scan, `app.log` shows
   `[AUTO-ADVANCE] SmolVLM finished installing after a scan — triggering
   tags-only auto-pass.` (this is the NEW path) — not just "no VLM installed;
   skipping." `SELECT COUNT(*) FROM tags WHERE source='vlm'` climbs from 0 on
   the FIRST scan's lifetime, and chips switch placeholder → VLM tags. No
   double-pass (only one `Auto-chaining Deep Analyze (tags-only)` per cycle).
2. **VLM server payload.** `engine.jsonl` shows `[VLM-SERVER] persistent server
   up; payload self-test OK`. If instead `payload self-test failed; falling back
   to per-file CLI` + a `vlm_server_payload_rejected` warning — tags still land
   (slower), and the logged probe error tells us the server's expected payload
   shape to fix. Either way the batch must produce `source='vlm'` rows.
3. **Odd formats.** A `.webp`/`.bmp` in the library gets VLM tags (transcoded),
   not a per-file failure.
4. **First-run speed.** With `-Wipe` (true first run, NVIDIA): `app.log` shows
   `[CUDA-AUTO] deferring CUDA runtime until a VLM is installed`; the CUDA
   ~650 MB pack does NOT download until after SmolVLM's sentinel lands. First
   scan `files_per_second` is materially higher than before (no triple-download
   contention). No false "No response from engine — try again" install failures.
5. **Crash-during-scan.** Click around the sidebar / switch tabs rapidly during
   a live scan — no crash (this class is already defended; the CUDA-defer
   shrinks the hang-prone window). If it still dies, grab the crash dump under
   `%LOCALAPPDATA%\FileID\logs\` — it now pinpoints the offending event.
6. **CLIP batch/pool A/B (perf tuning, optional).** Scan the same folder twice:
   once default, once with `FILEID_CLIP_USE_BATCH=0`. Compare `clip_p95_ms` +
   `files_per_second` from the sidebar/batch stats; lock in the winner (the
   default is currently batch-ON, flagged pending this measurement). Watch for
   `[FILEID_GPU_DEVICE_REMOVED]` — if it appears, the setting exceeded the TDR
   ceiling and must be lowered.

## V16.11 — verify on hardware: thumbnails + Deep Analyze runtime + SmolVLM auto-tag (2026-05-21)

**Landed (compiles + clippy -D warnings + all tests + format + BOM; see STATE V16.11).**
Three root-caused fixes + SmolVLM auto-tagging. A clean rebuild is required
(`pwsh build/build-all.ps1 -Run`, or `-WipeDbOnly -Run` for a fresh DB). These
are GUI/timing/runtime behaviors a compile cannot prove:

1. **Thumbnails render (the NOW fix).** Scan a folder. Every visible card shows
   its image immediately — square, image area NOT collapsed — during a live scan
   AND at rest. The bug was the `TileRoot` `Height="{Binding ActualWidth …}"`
   self-binding (non-observable DP → stuck at 68 → image row collapsed); now set
   via `OnTileSizeChanged`. If a card is still blank, `app.log` `[THUMB]` lines
   tell which: `TILE_SIZED w=… h=…` (layout) + `TILE_THUMBNAIL_ASSIGNED … px=WxH`
   (bitmap). px>0 with no/!square TILE_SIZED ⇒ layout; px=0 ⇒ decode.
2. **Deep Analyze: no "runtime too old" toast.** Deep Analyze a single image →
   caption succeeds, no toast (the 3 MB→20 KB `sanity_check_binary` floor fix —
   the thin 89 KB `llama-mtmd-cli.exe` now passes). `engine.jsonl` shows
   `[VLM-SERVER] ready` on a batch; no orphan `llama-server.exe` after.
3. **SmolVLM auto-tagging.** Existing settings.json (had `qwen2_5_vl_3b`) is
   migrated to `smolvlm` on first launch (`[INSTALL]`/AppSettings v2). SmolVLM
   auto-installs (`[SMOLVLM-AUTO] … installing`). Scan → CLIP placeholder chips
   appear immediately (threshold 0.18); after the scan completes + SmolVLM is
   installed, the next scan's auto-chain runs the tags-only pass
   (`tags_only:true`) and `SELECT tag,COUNT(*) FROM tags WHERE source='vlm'
   GROUP BY tag` climbs with real tags; cards switch from placeholder → VLM tags.
   Kill + relaunch mid-pass → resumes (only untagged files). The single Settings
   → Cleanup "Tag automatically with AI after scans" switch toggles it.

**Known follow-ups (non-blocking):**
- **First-scan auto-tag latency.** On the very first scan SmolVLM may still be
  downloading when the auto-chain checks `Vlm.Status`, so auto-tagging starts
  from the *second* scan. If we want first-scan coverage, trigger the auto-tag
  pass on SmolVLM install-complete (listen for the smolvlm sentinel/slot →
  Installed transition) rather than only on the scan→cluster→caption chain.
- **"Remove CLIP" switch** is still `ENABLE_CLIP_SCENE_TAGS=false` (engine) once
  VLM tagging is validated as strictly better; left on as the placeholder.

## V16.8 — VLM activated (runtime b9254) + persistent server + Settings declutter (2026-05-20)

**Landed (compiles + clippy + tests; closes the V16.7 activation prerequisite):**
- ✅ **Runtime bumped to b9254** (`registry.rs` `llama_runtime_x64`), verified to
  ship `llama-mtmd-cli.exe` + `llama-server.exe` + `mtmd.dll`. The auto-installer
  re-fetches when the stale b4404 runtime is detected (sentinel present but
  mtmd-cli missing), so it self-activates on next launch. Fixes the toast.
- ✅ **Persistent `VlmServer`** (`models/vlm_server.rs`) — `run_deep_analyze_batch`
  loads the model once via `llama-server.exe` and serves all files (~1-3 s/file),
  CLI fallback retained.
- ✅ **Settings decluttered** — removed the pure-doc "Models" card + the disabled
  "Performance profile" placeholder.

**Blocking hardware verification (a compile can't prove these):**
1. **Runtime auto-activation.** Rebuild + relaunch on the user's box (which has
   the stale b4404). Confirm the auto-installer logs `[VULKAN-AUTO] … stale … —
   reinstalling`, downloads b9254, and `Models\llama.cpp\llama-mtmd-cli.exe`
   appears. Then Deep Analyze a single image → caption succeeds (no toast).
2. **Persistent-server multimodal.** Run "Analyze all" on a small folder. Confirm
   `[VLM-SERVER] persistent server up` in `engine.jsonl`, the server answers
   `/v1/chat/completions` with an image for Qwen2.5-VL, and `SELECT COUNT(*) FROM
   tags WHERE source='vlm'` climbs. If the server 400s on the image payload,
   check the `image_url` data-URI format against b9254's server API (the one
   unknown I couldn't test from the build host).
3. **No orphan `llama-server.exe`** after the job completes / is cancelled /
   the engine exits (kill_on_drop should handle it — verify in Task Manager).

**Optional follow-ups (NOT done — flagged for a decision):**
- **CUDA runtime bump.** Left `llama_runtime_cuda_x64` at its old pin: the VLM
  uses the Vulkan dir (`VlmRunner`/`VlmServer` probe `Models\llama.cpp\`), and the
  current b9254 CUDA build splits `cudart` into a separate zip, so bumping it
  needs the cudart handled too. Vulkan runs on the RTX 2060 fine. Only worth it
  if CUDA-accelerated VLM is wanted.
- **Settings: fuller macOS parity.** A bigger pass could collapse the Windows
  diagnostics (CPU/Mem/GPU/Power/thumbnail) under an "Advanced" disclosure like
  macOS, and trim the 3 extra Behavior toggles macOS lacks (Hide-unknown,
  Restructure-tree-diff, Auto-chain-Deep-Analyze). NOT done this round — those
  are *functional* controls; deleting them needs user confirmation, and the
  WinUI render can't be visually verified from the build host.

## V16.7 — VLM tagging implemented; runtime bump is the activation step (2026-05-20)

**Landed (compiles + tests; reuses the existing Deep Analyze pipeline):**
- ✅ VLM scene/content tags written as `source='vlm'` during Deep Analyze
  `Both` mode (`pipeline/deep_analyze.rs` `analyze_file` + `parse_vlm_tags` +
  `models/vlm.rs::TAG_PROMPT`). ReadStore surfaces + prefers them. CLIP
  (`source='auto'`) and VLM tags coexist; VLM leads the chip slice.
- ✅ One-line CLIP kill switch: `scene_vocab::ENABLE_CLIP_SCENE_TAGS` (set
  `false` to drop CLIP scan-time tagging entirely — VLM tags then lead
  unchallenged; no other code change needed).
- ✅ `VlmRunner::find()` now emits an accurate "runtime too old — update it"
  error when a stale-but-present runtime lacks `llama-mtmd-cli.exe`.

**ACTIVATION PREREQUISITE — VLM cannot run until the llama runtime is bumped.**
The runtime is pinned to **b4404** (`registry.rs` `llama_runtime_x64` /
`llama_runtime_cuda_x64`), which ships `llama-server.exe` + the per-model CLIs
but NOT the unified `llama-mtmd-cli.exe` this code drives, and predates
Qwen2.5-VL. So Deep Analyze AND VLM tagging both fail until the runtime is
current. To activate (do this with the ability to verify a download — I did NOT
blind-guess a URL):
1. Find a current llama.cpp release that ships `llama-mtmd-cli.exe` in its
   `*-bin-win-vulkan-x64.zip` (and a CUDA `*-bin-win-cuda-*-x64.zip`). Verify
   by downloading + listing the zip.
2. Bump both `url:`s in `registry.rs` (vulkan: `llama_runtime_x64`; cuda:
   `llama_runtime_cuda_x64`). Note the vulkan entry still uses the
   `ggerganov/llama.cpp` org (redirects); the cuda entry uses `ggml-org`.
3. Force re-install: the auto-installer skips when the `.installed` sentinel
   exists, so delete `%LOCALAPPDATA%\FileID\Models\.sentinels\llama_runtime_x64.installed`
   (+ the cuda one) and `Models\llama.cpp\` (+ `llama.cpp-cuda\`), then relaunch
   (auto-install re-fires) or click Settings → Performance → "Install llama.cpp
   runtime". Confirm `Models\llama.cpp\llama-mtmd-cli.exe` now exists.
4. Verify a Qwen2.5-VL caption succeeds (Deep Analyze a single image), then run
   "Analyze all" and confirm `source='vlm'` rows land
   (`SELECT COUNT(*) FROM tags WHERE source='vlm'`).

**Perf follow-up (the original Track-3 design — optional optimization):** the
current path spawns `llama-mtmd-cli.exe` per file (model reload each time) +
adds one tag call per file, so a full-library pass is many hours. A persistent
`llama-server.exe` (`/v1/chat/completions` multimodal, load once) would cut that
to ~1–3 s/file. `llama-server.exe` ships in the runtime; build a `VlmServer`
wrapper (HTTP via the existing `reqwest` dep) and route `analyze_file` through
it. Deferred — correctness first; this is a speed optimization.

**To "simply remove CLIP" once VLM is validated:** set
`ENABLE_CLIP_SCENE_TAGS=false` (engine), optionally delete the gated scene block
in `pipeline/tagging.rs` and `models/scene_vocab.rs`. VLM tags already lead in
ReadStore, so nothing else changes.

---

## V16.5c follow-ups — invisible-tile root cause + tab-crash hardening (2026-05-20)

**Landed (pending user rebuild + relaunch — NO re-scan needed):**
- ✅ **Invisible Library tiles fixed at the root.** `AnimateTileEntry` no longer
  animates the tile-root composition opacity (it's pinned to 1 every prepare);
  the entrance is scale-only (0.96→1), gated once-per-element via a
  `ConditionalWeakTable`. This was the surviving cause of "thumbnails not
  loading / tags not showing" after V16.5b fixed only the image-level opacity
  pin. Forensics: 8611 `TILE_THUMBNAIL_ASSIGNED` + 0 `IMAGE_OPENED`; DB had
  24,762 tags / 7,961 files (100%).
- ✅ **Tab-switch crash hardened.** `DetailHostView.Sync` builds the incoming
  view lazily inside the fade-out completion (no zombie views from a superseded
  rapid swap) + commits synchronously via `CommitChild`. `LoadThumbAsync`'s UI
  continuation gained a `_unloaded` guard.
- ✅ **`embed_batch` sequential fallback** added in `SceneLabeler::build` (closes
  the high-priority V16.5-followup #1 below).

**Blocking user verification (the whole point — these are GUI/timing bugs that
a compile cannot prove):**
1. **Tiles visible.** Rebuild (`dotnet build … -p:Platform=x64`), relaunch,
   open Library on the already-scanned folder. Every card must show its
   thumbnail + filename + tag chips immediately (no blank/dim cards), during a
   live re-scan AND at rest. If any card is blank, capture `app.log` and look
   for `[THUMB] TILE_THUMBNAIL_ASSIGNED` without the tile appearing.
2. **Tab switching doesn't crash.** With a scan running, click rapidly through
   Library → People → Cleanup → Deep Analyze → Restructure → Settings and back,
   several times. The app must not die. If it does, the new
   `session-died-without-handler-*.txt` + the last `[ENGINE-SUB]`/`[THUMB]`
   lines name the surviving vector. (Note: `app.log` is truncated per session —
   grab it from the crashed session before relaunch overwrites it, or disable
   the truncate-on-start to preserve the crash trace.)
3. **Tags + thumbnails are NOT regressed by the scale-only entrance** — confirm
   the tile "pop" still reads as a gentle scale-in, not a flash, and doesn't
   pulse the grid every second during a scan.

**Follow-up worth doing next session:**
- **Preserve the crash-session `app.log`.** The truncate-on-launch behavior
  destroyed the crashed session's trace this round (only the *surviving*
  session's log remained), forcing reasoning-from-signature instead of
  reading the actual fatal line. Rotate (`app.log` → `app.log.1`) on launch
  instead of truncating, or write crash breadcrumbs to a session-stamped file.
- **Consider re-realization throttling.** 8721 PREPARE events for a single
  folder view means the `ReplaceAll`→Reset on every throttled refresh is doing
  a full clear+re-realize of the visible window each second. A diff-based
  collection update (or suppressing the refresh when the visible set is
  unchanged) would cut UI-thread churn and remove the scale-pop-per-second
  entirely. Medium effort; the once-per-element gate already hides the visual
  symptom.

---

## V16.5 follow-ups — CLIP zero-shot tagging + thumbnail recycle + People fix (2026-05-19)

**Landed (pending user rebuild + clean rescan / force re-tag):**
- ✅ Scan tags via **CLIP zero-shot** against a curated scene vocabulary
  (`models/scene_vocab.rs`), replacing the ImageNet classifier; reuses the
  per-file MobileCLIP embedding, no new download.
- ✅ MobileNetV3 classifier **removed** end-to-end (engine module + registry
  arm; .NET auto-installer, install slot, Library banner; Settings
  "Classifier" diagnostic → CLIP-zero-shot status line). Supersedes V16.4-3
  (Places365) and kills the "downloading for identifying" UX.
- ✅ Confidence **persisted** to `tags.score` (`TaggedFile.tags` →
  `Vec<(String, Option<f32>)>`; no migration). Closes V16.4-2.
- ✅ **Force re-tag** button (Settings → "Re-scan everything"). Closes
  V16.4-1 / V15.1-N1.
- ✅ Thumbnail recycle stale-bitmap fix + memory bound
  (`FileTile.ClearThumbnailForRecycle`).
- ✅ People double-tap fixed (ElementPrepared DataContext bridge). Partially
  closes V16.4-4: Library + People done; Cleanup uses classic `{Binding}`,
  Restructure/Sidebar are display-only/stable — audited clean.

**New follow-ups:**

1. ✅ **DONE (V16.5c): sequential `embed` fallback added.** `SceneLabeler::build`
   now catches an `embed_batch` error and falls back to per-prompt `embed`
   (a `(1,77)` input) for the remaining chunks, so a batch-pinned text export
   no longer disables all scene tags. The real-text-ONNX verification question
   is moot for the canonical Xenova export (dynamic batch axis confirmed in the
   field: `[TAGGING] scene-label embeddings built n_labels=164`), but the
   fallback protects fresh installs on other machines. Was **Priority: high** —
   gated all scene tags.
2. **Tune the scene vocabulary + threshold against real photos.** ~170
   labels + threshold 0.12 + temp 100 are a first cut. Force re-tag a real
   folder, inspect `tags.score`, adjust `SCENE_LABELS` / `SCENE_THRESHOLD` /
   `SCENE_TEMPERATURE` in `models/scene_vocab.rs`. The main accuracy lever now.
3. **Move the one-time label-matrix build off the first scan.** Built lazily
   in `Models::load` (process-static) on the first scan after launch — N×M
   batched text encodes. If the first-scan delay is noticeable, build during
   model prewarm/install or cache the matrix to disk (keyed by vocab + model
   hash).
4. ✅ **DONE (V16.5b): Explicit `ORDER BY score DESC, rowid` in the ReadStore
   tag GROUP_CONCAT** (all four query sites). Insertion order did NOT survive
   `GROUP_CONCAT` — enriched extras (NULL score) preceded scene tags, so the
   2-chip `TopTwoTags` slice showed `Has Location`/`2024` and never the scene
   label despite 94% scene-tag coverage. Now scene tags lead by confidence.
   Paired with the V16.5b thumbnail-visibility fix (image composition opacity
   was pinned at 0 on recycle and never reliably reset → loaded thumbnails
   stayed invisible).

---

## V16.4 follow-ups — thumbnail trigger + classifier coverage (2026-05-19)

**Landed (pending user rebuild + clean rescan):**
- ✅ Thumbnail trigger fixed — `OnRepeaterElementPrepared` resolves the tile by `args.Index` and sets `el.DataContext` (x:Bind didn't, so the old `DataContext is not FileTile` guard bailed before `LoadThumbAsync`). Closes the multi-version "thumbnails never render" saga — every prior fix patched the unreachable `ThumbnailService` fallback chain.
- ✅ Classifier threshold 0.30 → 0.20 (`tagging.rs`) — 66% of personal photos cleared zero labels at 0.30.

**New follow-ups discovered this session:**

1. **Re-tag affordance (was V15.1-N1, now load-bearing).** Existing tags
   are from the old 0.30 ImageNet run; incremental rescan skips current
   files, so the threshold change isn't visible without a force re-tag.
   `StartScanCommand.Rescan` is wired through IPC + `EngineClient` but has
   no UI. Add a Sidebar/Settings "Re-scan everything (force re-tag)"
   button. Until then the only way to validate a tagging change is to
   delete `fileid.sqlite*`. **Priority: high** — blocks validation of any
   future tagging tweak.
2. **Persist classifier confidence into `tags.score`.** `dbwriter` writes
   `score = NULL` today; the classifier computes the confidence and
   `classify_batch` returns it, but `process_file_predecoded` discards it
   (`for (label, _score)`). Carry `(label, score)` through
   `TaggedFile.tags` (type change `Vec<String>` → e.g. `Vec<(String,
   Option<f32>)>`, enriched extras = `None`) → `dbwriter` binds `?3`.
   Enables data-driven threshold tuning (instead of guessing) and
   confidence-ordered chips. Deferred from V16.4 — type ripple, no
   user-visible effect alone.
3. **Places365 scene classifier (the real tagging fix).** ImageNet-1k is
   an object classifier; its labels are object-specific (`breakwater`,
   not `beach`). The directive's own examples (`Beach`/`Kitchen`/
   `Document`) are Places365 categories. No MobileNet-Places365 ONNX
   exists on HF (only a heavier ViT-base, likely not ONNX-exported), so
   this needs sourcing/converting a Places365 model to ONNX, a new
   registry entry (URL+SHA), a 365-line label file, preprocessing verify
   (Places365 uses the same ImageNet mean/std), re-download + rescan, and
   a per-file latency check on DirectML. **Priority: medium** — the
   highest-leverage tagging-relevance improvement, but a real chunk of
   work + a model-hosting question.
4. **Audit other x:Bind+ItemsRepeater code-behind handlers app-wide.**
   The DataContext-null-under-x:Bind gotcha that broke Library thumbnails
   may affect People / Cleanup / Restructure repeaters too. Now that
   Library sets `el.DataContext` in its prepared handler, grep for
   `ItemsRepeater` + `el.DataContext is` elsewhere and apply the same
   bridge.

---

## V16.3 follow-ups — file-type chip + classifier diagnostics + broken-image placeholder + video COM fix (2026-05-19)

**Closed this session (the "four problems" directive):**
- ✅ Classifier URL verified + both SHA256s pinned — was already done in V16.2 (`registry.rs`); confirmed.
- ✅ File-type chip on cards — gray `Variant=Kind` `TagChip` leads the caption chip row.
- ✅ Broken-image placeholder — procedural `FontIcon`, gated on new `ThumbnailFailed` flag (closes the V15.5 `PreviewUnavailable.png` item — shipped procedurally, no asset).
- ✅ Classifier installed-state diagnostic in Settings.
- ✅ Video keyframe per-thread COM init (`CoInitializeEx` MTA in `keyframe_25pct`) — the one real defect; decoder-pool + Deep Analyze threads were missing it.

**New follow-ups discovered this session:**

1. **HEIC decode lacks per-thread COM init too.** `shell::heic::decode`
   (WinRT `BitmapDecoder`, also called from decoder-pool threads) has no
   explicit `CoInitializeEx`. WinRT is agile so it may work implicitly,
   but the same latent risk video had applies. If HEIC files fail to
   decode on decoder threads, add an `ensure_com_initialized()` call
   (or hoist video.rs's guard to a shared `shell` helper). Low priority
   until a HEIC decode failure is observed. Surfaces in:
   `platforms/windows/src/engine/src/shell/heic.rs:31`.
2. **Video `durationSeconds` → `mm:ss` overlay.** `keyframe_25pct`
   already pulls `MF_PD_DURATION` but discards it. A bottom-right
   `mm:ss` overlay on video cards (directive's optional Problem-4 item)
   needs a 7-layer plumb: `TaggedFile.duration_seconds` → DB migration
   v8 (`duration_seconds REAL NULL`) → `dbwriter` insert → IPC
   `FileDoneEvent`/`FileRecord` → `ReadStore` projection → `FileTile`
   `DurationDisplay` → XAML overlay. ~half a day. Deferred — polish, not
   a regression.
3. **FileKind taxonomy expansion (RawPhoto / Archive / Code).** Directive
   proposed splitting these out. Blocked on a scope decision: `Archive`
   (zip/7z) and `Code` (py/js/rs) are currently `FileKind::Other` and
   filtered out at the discovery walk (`discovery.rs:259-260`), so they
   never enter the corpus. Admitting them means deciding whether FileID
   should scan code/archive files at all. `RawPhoto` (cr2/nef/dng) is
   currently lumped under `Image`; splitting it is lower-risk. Punt
   until the user wants code/archive files in the library.
4. **macOS-side kind chip parity.** The file-type chip shipped Windows-
   first per the directive. macOS `LibraryView.swift` should get the
   equivalent leading chip in a follow-up (the macOS `FileKind` enum +
   `kind` column already exist).

---

## V16.0 follow-ups — perf+thumbnails+classifier+chips landed; verification + classifier SHA pinning (2026-05-18)

**User verification (blocking):**

1. **Throughput**: launch the app, scan `C:\Users\adamm\Desktop\Test Data` (15K JPEGs).
   The sidebar "Tagged" counter should climb at ≥40 files/sec (target floor; the cross-platform target is ≥140). Steady-state CPU should be >50% across the 12 threads (the decoder pool removes the prior 12% ceiling). Compare against baseline (0.04 files/sec) by checking `engine.jsonl` for the
   `[STATS]` line — `clip_avg_batch_x10` should hover near 60-80 (batch CLIP averaging 6-8 images per dispatch instead of the prior 1-2). VRAM peak ≤ 5.5 GB.
2. **Thumbnails**: scroll the Library after a scan. Every visible image card
   should render the real bitmap (no placeholder gradient) within ~2 s. After
   restart, the same tiles should hit the L2 disk cache and render
   instantly. The `[THUMB]` lines in `app.log` will trace the exact code path
   per file (`L1_HIT` / `L2_HIT` / `SHELL_OK` / `IMG_FB_OK` / `BITMAP_SET` /
   `TILE_THUMBNAIL_ASSIGNED` / `IMAGE_OPENED` / `OPACITY_SET`). If a tile
   still doesn't render, the missing log line names the broken hop.
3. **Tag chips**: tap a card after the scan completes. Up to 2 chips below
   the filename should show — `"Year_YYYY"` (or formatted year), `"iPhone"` /
   `"Canon"` / camera family, `"Has Faces"`, `"Has Text"`, `"Has Location"`,
   and (if classifier model installed) scene labels like `"Dog"`, `"Beach"`,
   `"Document"`. Without the classifier installed: only enriched-extras
   chips. Screenshot one card so the maintainer can verify visual parity
   against macOS LibraryView.swift:729-744.
4. **Diagnostic perf trace**: set `FILEID_PERF_TRACE=1` before launching
   and run a 100-file subset scan. `engine.jsonl` should emit `[PERF]` lines
   for each stage with elapsed ms. Aggregate the per-stage averages and
   confirm `decode_us` < 50 µs (after WP1-B3 decoder-pool decouple) and
   `clip_us` < 30 ms (after WP1-B1 batch default).

**Stubbed-but-landed items (close out before V16.1):**

1. ~~**Classifier model + labels SHA256 pinning.**~~ ✅ **DONE (V16.2).**
   `models/registry.rs` `"classifier_mobilenetv3"` slot pins both SHA256s
   (ONNX `a88a7545…`, labels `8800e392…`). 1000-class export confirmed;
   `ClassifierSession::load` handles the 1001-class background offset too.
2. ~~**Classifier registry URL verification.**~~ ✅ **DONE (V16.2).** URL
   `onnx-community/mobilenetv3_large_100.ra_in1k/.../onnx/model.onnx`
   verified against a live download; `approx_bytes = 21_949_218`.
3. **Phase model parity (Windows vs macOS).** Windows shows 5 pipeline
   phases (`Scan / Tag / People / Captions / Done`) in `SidebarPipelineProgress.xaml.cs:23-28`; macOS shows 3 active phases (`discovering / tagging / postScan` in `IPCProtocol.swift:138-146`). Decision needed:
   - Option A: Collapse Windows to 3 phases (matches macOS; loses People /
     Captions granularity that Windows users may find useful).
   - Option B: Expand macOS to 5 phases (breaking IPC change; requires macOS work).
   - Option C: Keep divergent models with a shared enum + platform-specific display.
   Surfaces in: `platforms/windows/src/FileID.App/Views/Sidebar/SidebarPipelineProgress.xaml.cs::BuildStages` and
   `platforms/apple/shared/Sources/FileIDShared/IPCProtocol.swift:138-146`.
   Owner: unassigned. Priority: low (visual polish, not a regression).
4. **CLIP_CONCURRENCY tuning.** Left at 2 because the batch-CLIP default
   (V16.0 WP1-B1) renders the per-call CLIP semaphore mostly irrelevant —
   batch coordinator owns the single Session and serializes batches
   internally. If a user reports the pool path (set `FILEID_CLIP_USE_BATCH=0`
   to use) is bottlenecked, run the directive's iterative `CLIP_CONCURRENCY+1`
   TDR-watch procedure on a 500-file subset.
5. **Shell-thumbnail fast path for CLIP (WP1-B4 deferred).** Directive
   asked for shell-thumbnail-then-resize for the CLIP path while keeping
   full decode for SCRFD/ArcFace. Skipped because the decoder pool already
   hides decode latency from workers. Revisit only if perf-trace shows
   resize_rgb_nearest on the full image is the dominant per-file cost.
6. **B2 CLIP_CONCURRENCY iterative TDR-watch.** Directive's "raise +1
   until TDR fires" procedure requires runtime testing. The default `2`
   stays until measurement justifies a change.

---

## V15.9 follow-ups — verification + stubbed adaptive items (2026-05-18)

**User verification (blocking):**

1. **Discovery throughput.** `pwsh build/build.ps1 -RunTests` then launch the app and scan `C:\Users\adamm\Desktop\Test Data`. The sidebar "Discovered N" counter should climb at NVMe walk speed — target ≥2,000 files/sec sustained, expected ~5–20K files/sec on a Samsung 970/980-class NVMe. The counter must reach the corpus total within ~5 s **independent of tagging progress** (this is the V15.9 decouple invariant). If the counter still tracks ML throughput (~22/sec from V15.8d), the channel-or-walk-thread budget didn't take effect — check `app.log` for `[DISCOVERY] adaptive parallel walk walk_threads=N storage=nvme`.
2. **Thumbnails.** Point the Library at the same Test Data folder. Every visible image tile should render its actual content within 2 s of becoming visible. After app restart, the same folder should render instantly (disk cache hit). Settings → Diagnostics → Thumbnails should show `ok > 0`, `failed` near 0, and `disk: hits=N` climbing across the second visit.
3. **Adaptive hardware.** Settings → Diagnostics should show the detected CPU (with P/E split if on Intel 12th-gen+), RAM avail/total + tier, GPU vendor + VRAM, NPU presence (false on the RTX 2060 box), power source ("AC power"), worker cap, and active profile "auto". The Performance Profile ComboBox shows Eco/Auto/Performance with Eco/Performance grayed.

**Stubbed items the V15.9 push deferred (with the design landed, just not the impl):**

1. **NPU routing — Intel AI Boost + AMD XDNA detection.** Qualcomm Hexagon already detected via the existing QNN probe (reused). Intel/AMD report `npu_present = false` for now. Needed: a probe in `models/runtime.rs` that loads the OpenVINO `npu` device (Intel) and the VitisAI EP (AMD) and flips `npu_present = true` when found. Routing CLIP / face-detection inference to the NPU when present is a separate piece — design the `NpuRouter` trait, fall back to GPU then CPU. ~1–2 days; needs a Meteor Lake or Ryzen AI box for live-fire.
2. **Battery throttling (currently report-only).** `power_status()` lands the source + battery percent in HardwareInfo. Throttling on battery + low charge (drop to low-memory mode + 50% pool reduction + sidebar "Battery saver active" banner) deferred to next push. Reason: report-only first so users see what the engine thinks before behavior shifts under them. ~0.5 day once we trust the readings.
3. **Eco / Performance profile selectors.** ComboBox in Settings is present but disabled. Eco needs the throttling code from item 2; Performance needs the "uncap pool size + ignore VRAM safety budget" path with a confirmation dialog. ~1 day to ship both.
4. **Storage SATA-SSD vs NVMe discriminator.** Currently `IncursSeekPenalty == FALSE` is treated as NVMe-class (16 walk threads). Adding `STORAGE_ADAPTER_DESCRIPTOR.BusType` ⇒ NVMe vs SATA distinction would let SATA SSDs use the 8-thread budget. Half-day. Low priority — over-parallelism on SATA SSDs still beats single-threaded walkdir.
5. **Pending_files DB queue (alternative decouple).** V15.9 hit the throughput target via channel-resize + count-before-send. A `pending_files` v8 migration would add crash-durability (resume scan after engine kill) but is more invasive. Open question; only worth doing if users report resumability requests.
6. **GPU pool size adaptive to memory tier.** ML pool size is currently VRAM-clamped only. On Low memory tier we should also clamp pool to 1 even when VRAM allows 4. Trivial change in `pipeline/tagging::resolve_pool_size`; reason it wasn't shipped: wanted to validate the diagnostics surface first so a regression is visible.

---

## V15.8d follow-ups — bundle assembly + face-photo verification (2026-05-17)

**Acceptance criteria for each item below: described in the bullet.**

1. **Bundle assembly (`FileIDSetup.exe`).** `publish-bundle.ps1 -SkipSign` now gets through engine + app + MSI + privacy gate. Bundle step still fails on two WiX 4 surface items: (a) `WixStdbaLicenseUrl` bind variable not declared; (b) `Bundle.wxs` hardcodes both x64 and ARM64 `MsiPackage` entries so `-SkipArm64` errors on the missing ARM64 MSI payload. Fix: declare `<WixVariable Include="WixStdbaLicenseUrl" Value="…" />` (or equivalent) and either generate the chain entries from a wixproj property or split into two `Bundle.wxs` files per arch. **Done when**: `pwsh build/publish-bundle.ps1 -SkipSign -SkipArm64` exits 0 and `dist/installer/FileIDSetup.exe` exists with size ≥ MSI size.
2. **End-to-end face-detection DB verification.** Add a single photo containing a face to a folder + scan it; assert `SELECT COUNT(*) FROM face_prints WHERE arcface_embedding IS NOT NULL > 0`. The SCRFD decode pure-function tests now cover the invariants but the full path (image → SCRFD → ArcFace → DB row) hasn't been observed end-to-end. **Done when**: a face photo + a scan produces a non-zero `face_prints` row count.
3. **LavaLampBackground Composition migration on 26200+.** Win2D `CanvasAnimatedControl` fast-fails on Win11 build 26200+. The Composition rewrite is in the tree but real-world "renders without crashing on 26200+" still needs a Win11 Insider box. **Done when**: a screenshot from build 26200+ shows the three drifting orbs.
4. **Multi-vendor GPU EP live-fire.** Unit tests cover the priority chain logic per vendor; physical AMD/Intel/Qualcomm boxes needed for live-fire verification that QNN/OpenVINO/CUDA EPs actually bind and survive a forced TDR.
5. **Re-cluster on Windows after the V15.5 face-padding change.** Existing libraries' cluster IDs are invalidated. Run `iterate.ps1 -SkipBuild` against a face-heavy corpus and verify clusters stabilize.
6. **Settings install-state detection.** Install buttons for CUDA llama.cpp + cuDNN don't reflect already-installed state at page load. `ModelInstallerService.SentinelInstalled` already encapsulates the probe; add a `SyncInstallButtonStates()` call in `SettingsView.xaml.cs`'s `Loaded` handler. ~30 LOC.
7. **Move `platforms/windows/src/engine/` → `shared/engine/`**. The crate is bi-platform now; the directory name lies. Path-dep updates ripple through `FileID.sln`, `platforms/linux/src/app/Cargo.toml`, all `build/*.{ps1,sh}`, and `.github/workflows/windows-engine.yml`. ~half a day.
8. **Linux UI Phase 1** (6 tab views in GTK4) + `shell/` Linux implementations (~17 days total).
9. **PDF Deep Analyze acceptance test.** `pdf-analyze` Cargo feature now works; needs a real PDF + a Deep Analyze run to confirm the rendered page reads cleanly through the VLM caption flow. **Done when**: a PDF deep-analyzed produces a non-empty `vlm_description`.
10. **User verification still inherited**: trash a couple files, restart, verify `restoreFromTrash` finds them; restructure apply on a path with a deliberately-planted directory junction; rename to `COM0.txt` / `LPT0.png` (engine must reject).

---

## V15.7 follow-ups — sidebar stats parity landed; Settings install-state pending (2026-05-16)

**Blocking user verification:**
1. Build + launch; scan a 100+ file folder.
2. Sidebar Memory should be non-zero (typically 600 MB-1.2 GB during tagging).
3. Progress bar should fill correctly (not stuck at 100% during tagging).
4. ETA should transition from "computing…" to a real countdown after a few seconds.
5. Failures counter should react if any files error mid-scan.

**Settings install-state detection (deferred from V15.7):**
The user reported install buttons for CUDA llama.cpp + cuDNN don't reflect already-installed state at page load. Sentinels exist at `%LOCALAPPDATA%\FileID\Models\.sentinels\{llama_runtime_cuda_x64,cudnn_runtime_x64}.installed` (engine writes them atomically after install). `ModelInstallerService.SentinelInstalled` at `Services/ModelInstallerService.cs:755-762` already encapsulates the probe. Implementation:
- Add a private `SyncInstallButtonStates()` method to `SettingsView.xaml.cs`.
- Call it from the existing `Loaded += (_, _) =>` handler at line ~48.
- For each of `InstallCudaLlamaButton` (line 322) and `InstallCudnnButton` (line 370): if sentinel exists, set `Content = "Installed"`, `IsEnabled = false`, and the matching status text to the "✓ installed" message.
- Optionally also call from `OnInstallCudaLlamaClicked` / `OnInstallCudnnClicked` finally blocks so a re-visit after install reflects state. Cost: ~30 LOC.

**Inherited (still pending):**
- Re-cluster on Windows after V15.5 face-padding change.
- Move engine crate from `platforms/windows/src/engine/` → `shared/engine/`.
- Linux UI Phase 1 (6 tabs).
- `shell/` Linux implementations.

---

## V15.6 follow-ups — thumbnails eager-decode; CompletionRipple removed (2026-05-16)

**User verification (blocking):**
1. Build + launch; point at a fresh image folder with NO cached Explorer thumbnails (`Test Data` works because Explorer's thumb cache is cold for it).
2. Watch `app.log` for `ThumbnailService image-fallback decode (...)` lines — these surface any per-file decode failures that the V15.5 lazy-decode path swallowed silently.
3. Settings → look for `Thumbnails: ok=N / failed=M / fallback=K`. If `fallback` > 0 and `ok` ≈ visible tile count, the fix landed. If `failed` is high, look at the warn lines for the actual exception type (`SharingViolation` would suggest the engine has the file locked — needs `FileShare.Read` confirmation; `COMException 0x88982F8B` would suggest WIC decode error).
4. Sidebar Processing panel during scan — should be visually stable, no concentric rings on "Tagged N."

**If thumbnails STILL don't show after this build:**
- Bug has moved out of the fallback. Counters in `ThumbnailService.Stats` will name the bucket.
- Most likely next suspect: the shell `GetThumbnailAsync` itself throwing for these JPEGs (caught by the outer try/catch at `ThumbnailService.cs:234-240`), in which case `ImageExtensions.Contains(ext)` never gets evaluated and the fallback is bypassed. Fix would be to move the fallback INTO the outer catch as a last-resort path for known image extensions.

**Inherited from V15.5b (still pending):**

- Re-cluster on Windows after the V15.5 face-padding change (D1, `tagging.rs:112` 0.25→0.15). Run `iterate.ps1 -SkipBuild` against a face-heavy corpus.
- Move `platforms/windows/src/engine/` → `shared/engine/` for proper cross-platform home.
- Linux UI Phase 1: port the 6 tabs from macOS Swift to GTK4. Library is the biggest; rest are smaller.
- `shell/` Linux implementations (trash 3d, thumbnail 3d, ocr 5d, video 2d, reveal 1d, tags 1d, sleep 1d).

---

## V15.5b follow-ups — Linux scaffold landed; engine shared; deferred work (2026-05-16)

**User verification (Linux):**
1. On a real Linux box (Ubuntu 24.04+ / Fedora 40+ / Arch): `sudo apt install build-essential libgtk-4-dev libadwaita-1-dev` (or distro equivalent).
2. `cd platforms/linux && ./build/build.sh` — expected: builds the shared engine + the GTK app, stages `dist/fileid/fileid-linux`.
3. `./dist/fileid/fileid-linux` — expected: Adwaita dark window with HeaderBar, "FileID for Linux" StatusPage; folder picker works; engine status label transitions on Start Scan.

**Cross-platform parity ripple (high-priority):**
- **Re-cluster on Windows after the D1 face-padding change.** Existing libraries' cluster IDs will be invalidated. Run `iterate.ps1 -SkipBuild -SkipWipe=$false` against a face-heavy corpus and verify clusters stabilize. Document the migration in CHANGELOG.
- **Apply the equivalent D2-D7 fixes to macOS where they're missing.** Most are Windows-side; the cross-OS-divergent ones are CleanupAutoTagKept (Windows now matches macOS), tile sizing (macOS already adaptive), thumbnail size (macOS already 192). The face-padding alignment is the only one with a corresponding macOS code path — already at 0.15 there.

**Engine portability follow-ups:**
- **Move `platforms/windows/src/engine/` → `shared/engine/`.** The crate is now bi-platform; the directory name lies. Includes path-dep updates in `platforms/windows/FileID.sln`, `platforms/linux/src/app/Cargo.toml`, all `build/*.ps1` + `build/*.sh`, and `.github/workflows/windows-engine.yml`. ~half a day; do once and never touch.
- **Resolve `ring` on Linux cross-compile from Windows.** Three options: (a) document "build on real Linux/WSL" as the only supported path, (b) add `cargo-zigbuild` instructions to the Linux CLAUDE.md, (c) switch rustls's crypto backend from `ring` to `aws-lc-rs` (changes one feature flag in `Cargo.toml` for `reqwest`/`rustls`). Recommend (a) for v1; (c) for v1.1.
- **Linux CI workflow.** New `.github/workflows/linux-engine.yml` + `linux-app.yml`. Mirror the Windows shape (cargo check, clippy, fmt, audit, deny, privacy gate). Use `ubuntu-latest` runner.

**Linux app — Phase 1 (the actual work):**
- **Six tab views.** Port `LibraryView` / `PeopleView` / `CleanupView` / `DeepAnalyzeView` / `RestructureView` / `SettingsView` from macOS Swift to GTK4. Each is a `adw::NavigationPage` with the same data shape as the macOS sibling. Library is the biggest (`gtk::GridView` virtualized for 50K+ files); the others are smaller.
- **`shell/` implementations.** Sized in `platforms/linux/CLAUDE.md` (trash 3d, thumbnail 3d, ocr 5d, video 2d, reveal 1d, tags 1d, sleep 1d = ~17 days total). Trash + thumbnail are most user-visible.
- **LavaLampBackground for GTK.** Custom `gtk::DrawingArea` with cairo `RadialGradient` blobs at the same response/dampingFraction periods as macOS + Windows. ~1 day.
- **Single-instance gate.** `gtk::Application::set_flags(NON_UNIQUE)` is the default; need to flip to D-Bus single-instance + raise-existing-window pattern matching macOS + Windows.

**Linux app — Phase 2 (distribution):**
- **Flatpak manifest** at `platforms/linux/flatpak/io.github.fileid.FileID.yml`. Bundle ONNX Runtime + the bundled SQLite from the engine. Submit to Flathub.
- **AppImage** as a fallback for non-Flatpak distros.
- **`.deb` + `.rpm`** if user demand materializes; Flatpak covers most desktops.

---

## V15.5 follow-ups — Windows GUI harness landed; user verification pending (2026-05-16)

**User verification step (single, blocking):**
1. `pwsh platforms/windows/build/build.ps1` + `dotnet build platforms/windows/FileID.sln -c Debug -p:Platform=x64` — confirm V15.5 edits compile clean.
2. `pwsh platforms/windows/build/gen-corpus.ps1 -Count 50000 -OutDir C:\Temp\FIDCorpus` (~10 min, one-time).
3. `pwsh platforms/windows/build/gui-regression.ps1 -Corpus C:\Temp\FIDCorpus -TimeoutMinutes 30` — expected `[PASS] GUI regression: scan completed cleanly.` If `[FAIL]`, the script prints the unmatched `[APPLY:N] enter` event + last `[ENGINE-SUB:*]` line which names the killer subscriber for the next targeted fix.
4. Manual UI check: open the app, scan a real folder, scroll Library — Phase 2 image-extension fallback should fix the "blank tiles" the user reported.

**Follow-ups (non-blocking):**
- **Wire `ThumbnailService.Stats` into Settings diagnostics.** The counters exist; add one line to the existing Settings diagnostics block: `Thumbnails: N ok / N failed / N dropped / N fallback`. ~10 LOC change in `Views/Settings/SettingsView.xaml(.cs)`.
- ~~**PreviewUnavailable glyph asset.**~~ ✅ **DONE (V16.3).** Shipped procedurally: a XAML `FontIcon` placeholder gated on a new `FileTile.ThumbnailFailed` flag, with the shimmer moved to a derived `ShowShimmer` so "loading" and "failed" are distinct states. No asset PNG — see DECISIONS.md 2026-05-19.
- **CI integration of `gui-regression.ps1`.** Add `workflow_dispatch` trigger in `.github/workflows/windows-app.yml` so the harness can fire on demand. WinUI 3 unpackaged apps on GitHub Actions `windows-latest` have a fragile interactive-session story — start with manual trigger; auto-trigger later if reliable.
- **EngineClientTests revisit.** Phase 5.1 deferred because `EngineClient`'s ctor requires a UI dispatcher. Two paths: (a) factor `Apply` into a pure function taking state as parameters, or (b) add a `FileID.App.UiTests` csproj with WinAppSDK test infrastructure. Path (a) is cleaner but touches user's heavy local edits to `EngineClient.cs` (+103). Coordinate before starting.

---

## V15.4 follow-ups — Windows scan-crash verification + scope reduction (2026-05-16)

**User verification step (single, blocking):**
1. Launch the Windows app. Pick `C:\Users\adamm\Desktop\Test Data` (or any folder with 100+ files). Click **Start Scan**.
2. **If the scan completes** (sidebar reaches `Scan complete -- N files in Xs`): the Pattern B / brush-caching fixes were sufficient. Proceed to face clustering + Deep Analyze verification (see plan `~/.claude/plans/when-i-run-start-curried-swan.md` Phase 3).
3. **If the app dies again**: read `%LOCALAPPDATA%\FileID\logs\app.log` and identify the last `[APPLY:N] enter {Event}` without a matching `[APPLY:N] exit`, plus the trailing `[ENGINE-SUB:ClassName] {PropertyName}` line. That pair names the killer event + subscriber. Apply the targeted fix (almost certainly a cross-thread DispatcherObject construction inside that subscriber — same shape as V15.2/V15.2.1).

**Tracing scope reduction (after the crash is closed):** the per-event `[APPLY:N]` + per-subscriber `[ENGINE-SUB]` tracing is intentionally verbose during a scan (10 Hz × N subscribers). Once V15.4 is verified stable for a week, downgrade the `[ENGINE-SUB]` lines from `Debug` to `Trace` (add a `Trace` level to `DebugLog` if missing) and keep `[APPLY:N]` at `Info` as the always-on forensic trail. The Apply-level trace is cheap (one pair per event); the subscriber-level trace is what produces the noise.

**Related N5b item still pending:** the `EngineProcessManagerTests` / `IpcDispatcherTests` / `EngineClientTests` extraction. With those tests, a synthetic burst of 1000 progress events would have exercised every subscriber and caught the Pattern B SidebarQueueList bug before it shipped. Bumping this up the priority list.

---

## V15.3.2 — Tier-1 test/bench/privacy expansion (2026-05-16)

Following V15.3.1: shipped IPC 26-variant round-trip + StartScan path proptest (`ipc/mod.rs`), dbwriter ingest-idempotence proptest (3 cases against `INSERT_FILE_SQL`), criterion bench scaffold via crate `lib+bin` restructure with two benches (`tagging_hashes.rs`, `face_clustering_5k.rs`), `cargo audit` re-tightened to hard gate w/ weekly advisory-DB cache, and a source URL allowlist scan (both Windows + macOS workflows) that asserts every `https?://` URL in source matches the 4-host egress allowlist. Test count: 74 Rust + 30 IpcSchema + 28 App + 16 Theme = **148**. Detail in DECISIONS.md 2026-05-16 entries.

---

## V15.3.1 — macOS CI fix (2026-05-16)

Single change: removed the `executionProvider` grep assertion from `.github/workflows/macos.yml`'s engine smoke step. That field is Windows-only (ORT EP picker output); macOS engine never emitted it, so the step failed 100% of the time on macOS regardless of engine health. Pre-existing — failing since V15.2. Fix detail in `DECISIONS.md` 2026-05-16 entry.

---

## V15.3 — Polish engagement follow-ups (2026-05-15)

The polish-mochi engagement landed Phases 1–8 partially. Plan: `~/.claude/plans/i-want-you-to-polished-mochi.md`. Done so far: Windows-side `main.rs` decomposition (3463→678 LOC), `EngineClient.cs` + `ModelInstallerService.cs` partial splits, 135 tests across Rust + IpcSchema + App.Tests (up from 44), Phase 6 lint gates green (`cargo clippy -D warnings`, `dotnet format --verify-no-changes`), 3 perf wins (mmap decode, `cache_spill=0`, prepare_cached hoist), CI gates tightened, pre-commit hook + `tools/git-hooks/`. proptest caught 2 real bugs: `is_safe_filename("A\\")` accepted (SEC); cluster IDs non-deterministic across re-scans (UX). Both fixed.

### Still pending

**N1 — macOS Swift extractions (user verifies each on Mac).**
- `Database/ReadStore.swift` (999) split + GRDB `cachedStatement` migration — **largest single macOS read-path perf win**.
- `LibraryView.swift` (1465), `PeopleView.swift` (1428), `RestructureView.swift` (1478) — subview extraction. Also a SwiftUI body-diff perf win.
- `SankeyFlowView.swift` — remaining path-math after the `SankeyLayout.swift` nested-types extraction.
- `FileIDEngineMain.swift` (758) → `IPCDispatcher.swift` + `EngineLifecycle.swift`.
- `FaceClustering.swift` (1019) → `ArcFaceEmbedder.swift` + `HNSWClusterer.swift` + `IdentityMerger.swift`.

**N2 — Windows XAML user-control extraction (UI smoke required).** Six controls: `PrivacyDisclosureCard`, `PerformancePackCard` (from `SettingsView.xaml`); `StatHeroCard`, `RecommendationCard` (from `RestructureView.xaml`); `ModelInstallerCard` (from `WelcomeSheet.xaml`); `ModelPickerCard` (from `DeepAnalyzeView.xaml`).

**N3 — Phase 3 perf candidates needing criterion benches.** Already shipped: mmap decode, `cache_spill=0`, `prepare_cached` hoist, PGO release profile (`[profile.release-pgo]`), `serde_json::to_writer` (audited — already direct), `fast_image_resize` (audited + removed as unused dep). Still ahead, each gated on a bench delta recorded in `DECISIONS.md`:
- Batched CLIP image inference (1/call → 8/call).
- Per-worker thread-local buffer pools in `pipeline/tagging::process_file`.
- Vectorized L2-normalize via `wide::f32x8`.
- `crossbeam-channel` in IpcSink hot path (bench vs. tokio mpsc).
- ORT GPU residency audit.
- Criterion bench infrastructure (needs crate `[lib]` + `src/lib.rs` re-exports).

**N4 — .NET app perf candidates needing measurement.**
- `System.Text.Json` source generators for `FileID.IpcSchema` types.
- `SqliteCommand` reuse in `ReadStore.cs`.
- Batch UI event dispatch (16ms / 60Hz coalescing) in `EngineClient.cs`.

**N5 — Remaining .NET test classes.** Done so far in this engagement: `PathRedactorTests`, `UndoStackTests`, `SafeOpenTests`, `AppSettingsTests` (36 cases total). Still ahead, all need mock infrastructure:
- `EngineProcessManagerTests` — mock `Process`.
- `IpcDispatcherTests` — synthetic stdout stream.
- `EngineClientTests` — state machine.
- `ModelInstallerServiceTests` — mock HTTP via DelegatingHandler.
- `ReadStoreTests` — in-memory SQLite.
- New `FileID.Theme.Tests` project: `SpringEasingTests`, `ReducedMotionTests`, `BadgePillTests`, `ThemedSegmentedControlTests`.

**N6 — macOS Swift test expansion (user runs on Mac).** New `AppTests/` target: `EngineClientStateMachineTests`, `ReadStoreTests`, `ClusterSuggestionsTests`, `CLIPTokenizerParityTests`. Extend `EngineTests/`: `ScanCoordinatorTests`, `JobQueueTests`, `TaggingTests`, `DBWriterTests`, `IdentityClusteringTests`, `DeepAnalyzeStateMachineTests`, `RestructureTests`. Extend `SharedTests/`: `StreamingDownloadTests`, expanded `TagWriterTests`, `PathRedactionTests`.

**N7 — Advanced testing.** Done: 9 Rust proptests across `util/path_safety`, `util/zip`, `pipeline/face_clustering` (caught 2 real bugs). Still ahead:
- Rust proptests for `pipeline/dbwriter` (ingest idempotence), `models/clip_tokenizer` (round-trip), `ipc/mod.rs` (every variant round-trip).
- `cargo-fuzz` harness for `ipc::mod.rs` decoder + `pipeline::dbwriter` row deserializer; weekly cron.
- .NET property tests via `FsCheck.Xunit` (for `PathRedactor`, `UndoStack`, `IpcCoder`).
- Swift property tests via `@Test(arguments:)` parameterized.
- Cross-platform parity tests in `shared/parity-tests/`: `path_hash`, `dHash`, CLIP tokenizer, FolderClassifier, HNSW assignments. **Biggest single regression guard.**
- Snapshot tests (macOS) via `swift-snapshot-testing` for the six main views.

**N8 — Lint sweep finalization.** Done: `cargo clippy --all-targets -- -D warnings` clean (tuned `[lints.clippy]` Cargo.toml with documented allows for style-only pedantic rules; fixed 4 real lints); `dotnet format --verify-no-changes` clean. Still ahead:
- Sweep 33 remaining `#[allow(dead_code)]` annotations — most are Phase 5+ placeholders, but the audit should retire any that are now used.
- Swift: write `.swift-format` config + add CI `swift-format lint --strict` gate (user runs on Mac).

**N9 — CI gate landing.** Done in `.github/workflows/windows-engine.yml`: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo deny check`, `cargo audit --deny warnings` (hard gate, paired with an `actions/cache` of `~/.cargo/advisory-db` keyed weekly for stability). Done in `windows-app.yml`: `dotnet format --verify-no-changes`, `dotnet list package --vulnerable` (hard gate), `dotnet test FileID.sln` runs every project. Still ahead:
- `swift-format lint --strict` job in `macos.yml`.
- Coverage gate (drop > 2 pp blocks merge against `COVERAGE.md` baseline).
- Parity gate (depends on N7 parity tests existing).
- Fuzz cron (depends on N7 cargo-fuzz harness existing).

**N10 — Docs polish.** Done: `COVERAGE.md`, `TESTING.md`, `CONTRIBUTING.md` shipped. Still ahead:
- Refresh `ARCHITECTURE.md` component + IPC sequence diagrams.
- Refresh `ONBOARDING.md` 10-minute new-contributor guide.
- Refresh per-platform `CLAUDE.md` to reflect the new `commands/` + `util/` (Windows) and (forthcoming) `Database/Queries/` (macOS) module maps.

### Robustness + a11y + release engineering (Phases 9–11 — new scope)

**Phase 9 — Robustness suite.** WinAppDriver + XCUITest E2E smoke. Large-library stress (50K, 100K, 500K). SIGKILL-mid-scan recovery. Two-app-instance race. Disk-full simulation. Network drop. GPU TDR. TOCTOU + ACL edge cases. Image decompression bombs. Unicode (NFC vs NFD). Migration roll-forward + `PRAGMA integrity_check`. DB backup + restore. Memory soak (10× iterate runs; RSS plateau < 50 MB growth).

**Phase 10 — Accessibility + i18n readiness.** `AutomationProperties.{Name,HelpText}` on every interactive control (Windows); `accessibilityLabel` + `accessibilityHint` on every interactive view (macOS). Keyboard-only walkthrough. Color-blindness audit on gold/lavender/cyan/pink palette. Reduced-motion respect. High-contrast mode. String extraction to `.resw` / `.strings` (English-only fine; wires future translation work). IPC error codes (not English strings).

**Phase 11 — Release engineering.** Reproducible builds via `SOURCE_DATE_EPOCH`. EV cert + `notarytool` signing. Anti-malware false-positive procedure documented in `SHIP.md`. CI cache via `actions/cache` (5–15 min/run saved). `git tag vX.Y.Z` → automated signed-build artifact upload. Pre-commit hook shipped at `tools/git-hooks/pre-commit`. Editor config bundle: `.vscode/{extensions,settings,launch}.json`.

---

## Older follow-ups (archived)

Items from V15.2 down to V14.7 follow-up queues are no longer load-bearing — most were closed at the time they were written, the rest were rolled into V15.0–V15.3 work. The original text lives in `git log shared/docs/NEXT.md`. Genuinely-still-pending leftovers from those rounds:

- **V15.1-N1 Rescan UI affordance** — `StartScanCommand.Rescan` is wired through the IPC DTO + `EngineClient.StartScanAsync(rootPath, rootDisplay, rescan)` but has no UI surface. Add a Sidebar context-menu "Re-scan everything" or a Settings → Library "Force re-scan files even if up to date" toggle.
- **V15.1-N4 / V14.9-Y-N2 WIC native JPEG decode** — `Win32_Graphics_Imaging` features already in Cargo.toml. `IWICImagingFactory::CreateDecoderFromFilename` is generally 15–30% faster than zune-jpeg on photo JPEGs. Pure code add in `pipeline/tagging.rs::load_image_rgb`. Higher priority since V15.0 incremental rescan exposed JPEG decode as the dominant per-file CPU cost on warm-cache scans.
- **V14.9-Y-N3 Real-time VRAM monitor** — `IDXGIAdapter3::QueryVideoMemoryInfo` polled per batch to populate a Settings card showing VRAM pressure; would make the empirically-derived VRAM_PER_POOL_INSTANCE_MB constant in tagging.rs auditable rather than guessed.
- **V14.9-Y-N4 FP16 ONNX variants** — generally 1.5–2× throughput on consumer GPUs that support FP16 (most do). Cost: weight retraining/conversion, careful eval against the deterministic clustering parity guard.
