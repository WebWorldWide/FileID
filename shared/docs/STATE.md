# FileID — State

> Snapshot of what's working and where we left off. Update at the end of every working session.
>
> **How to read this file:** newest entry at the top. Each entry is a one-day-or-one-release summary of what landed. For *why* a decision was made, see [`DECISIONS.md`](DECISIONS.md). For *what's next*, see [`NEXT.md`](NEXT.md). For *user-visible release notes*, see [`/CHANGELOG.md`](../../CHANGELOG.md).
>
> Older entries below V15.0 are historical context — load-bearing for archaeology, not for current state. Skim if you want the journey; skip if you want the destination.

## 2026-05-21 — V16.15 face crops fixed + 1-2 word tags + download jitter + dead code

- **Faces (root-caused + fixed).** SCRFD emits bbox as `[x1,y1,x2,y2]` corners
  (`scrfd.rs`, rescaled to original-image px by `detect()`), but `tagging.rs` fed it to
  `crop_and_resize_face` + stored it as `[x,y,w,h]` — so the crop ran from the face's
  top-left to the image's bottom-right ("not a face"/blank), and that smear was also fed
  to ArcFace (corrupting clustering). Now converted corners→xywh once at the
  detect→`DetectedFace` site → real face crops, meaningful embeddings, correct persisted
  bbox. (`validate_face_geometry` was already correct.) Follow-up: landmark-aligned
  ArcFace chips for better cluster accuracy.
- **Tags are 1-2 words.** `parse_vlm_tags` drops 3+-word fragments (was >3); the SmolVLM
  TAG_PROMPT already asks for 1-2 words.
- **Deep Analyze model reality (verified).** Qwen3-VL-4B has **no GGUF** (ggml-org has
  only Qwen3-VL 2B/30B; macOS uses MLX), and Qwen2.5-VL-7B (~4.7 GB) OOMs on the 4 GB
  card at `-ngl 99`. So Deep Analyze stays **Qwen2.5-VL-3B** (strongest Qwen that fits +
  already a card + full descriptive captions). Gemma-3-4B card swap + 7B-with-VRAM-aware
  `-ngl` flagged as follow-ups (need blind-unverifiable C# x:Name work / an engine change).
  See DECISIONS.
- **Download "freaking out" fixed.** `ModelSlot.UpdateRate` no longer zeroes rate/ETA at
  every per-file fraction reset in a multi-file bundle (carries the prior rate) — that was
  the 0-blip / "Stalled" flicker; sample interval 500→250 ms. `downloader.rs` progress
  throttle 100→50 ms + progress channel 256→512. (Already 12-way parallel range-GET; true
  throughput is near-capped.)
- **Dead code.** Removed the unused `run_ocr_blocking_arc` (live path is
  `run_ocr_blocking`). Remaining engine `#[allow(dead_code)]` are deliberate (test helper
  `ModelStack::empty`, non-Windows cfg-stubs, the pool-path CLIP `embed`). A broad
  slop-comment purge is **deferred** — much of the codebase's verbosity is the
  load-bearing institutional memory the CLAUDE.md says not to strip; touched code is
  WHY-focused.

### Build/test
- Engine `cargo clippy --all-targets -D warnings` clean + `cargo test --lib` **158/0**
  (toolchain 1.90). C# (`ModelSlot`) `dotnet format` clean + BOM intact. WinUI compile is
  the user's VS build. Verify faces/tags/downloads on hardware per NEXT.md V16.15.

## 2026-05-21 — V16.14 small-screen / anti-clipping UI pass

User reported laptop UI content getting cut off. XAML audit (read-only — can't render
here) + conservative responsive fixes to the clear overflow patterns:
- **Deep Analyze action row** (7 controls: Whole library / Selected / Current / Skip
  toggle / Propose renames / Cancel) wrapped in a horizontal ScrollViewer (the
  PeopleView/CleanupView header pattern), so its right-hand controls can't clip on a
  narrow window — the most likely "cut off" culprit.
- **Oversized modal sheets shrunk to fit a laptop** (each already has an inner
  ScrollViewer for overflow): `FilePreviewSheet` 1080×720 → **880×520** (the worst —
  720-tall didn't fit a 768-px screen once title bar + taskbar are subtracted);
  `PersonDetailSheet` 480→440 H; `SuggestedMergesSheet` 520→440 H; `DrillDownSheet`
  700×520 → 640×440; `MainWindow` WelcomeOverlay MinWidth 660 → 580.
- Left as-is (degrades gracefully, doesn't hard-clip): Settings storage path
  (TextTrimming + tooltip), PersonDetail name fields (tight but fit), FilePreview
  toolbar (the `*` filename column absorbs the squeeze before buttons clip), sidebar
  (260 px with a Ctrl+Shift+S toggle).

All 6 edited `.xaml` parse as well-formed XML + BOM intact. **Not render-verified**
(no WinUI build/display here) — the user must eyeball on the laptop and report any
remaining clipping (which view + element).

## 2026-05-21 — V16.13 model-load timeout fix + tagging/Deep-Analyze split (first on-hardware run)

The build finally ran on the user's box (NVIDIA **~4 GB VRAM / DirectML**) after they
installed the VS WinUI PRI component (the CLI can't build WinUI here). First scan failed
with a false "models took >30 s / corrupted" — root-caused from the engine log to the
**21.5 s CLIP scene-matrix build** blowing the 30 s `load_default` timeout. Fixed, plus
the user's model-role ask.

- **Scene-label matrix is disk-cached** (`scene_vocab.rs`): build once (~21 s, first
  launch), reload ~instantly after (raw LE f32 + content-hash-keyed header under
  `Models/clip_scene_cache/`; the hit path also skips loading the 253 MB text session).
  **Model-load timeout 30 → 120 s** (`scan.rs`) so the one-time build can't false-fail.
  → first launch slow once, later launches <10 s. Immediate workaround for the user: a
  second "Start Scan" in the same session already worked (matrix cached process-static).
- **Tagging vs Deep Analyze split.** Auto-tag hardwired to **SmolVLM**
  (`EngineClient.AutoTriggerDeepAnalyzeAsync`, gated on SmolVLM weights present); **Deep
  Analyze defaults to Qwen 2.5-VL 3B** (`AppSettings.SelectedVlmModelKind` default → qwen
  + v2→v3 migration off the leaked smolvlm). SmolVLM auto-installs; Qwen installs
  on-demand from the Deep Analyze card.
- **Deep Analyze cards now honest** (V16.12.1): `DeepAnalyzeView.SyncCards` checks each
  model's gguf on disk instead of mirroring the shared "any VLM" slot — Qwen no longer
  falsely shows "Installed".
- **Hardware tailoring confirmed from logs:** DXGI vendor probe (NVIDIA), VRAM probe
  (3935 MB), EP chain cuda→tensorrt→directml→cpu, pool clamped to 1 to fit 4 GB, per-vendor
  runtime auto-install (Vulkan + SmolVLM + CUDA llama runtime + cuDNN all present). Open
  gap: ONNX runs on **DirectML** (the `cuda` ORT pack is `not_yet_available` → ~3-5×
  slower); the VLM path already uses CUDA. Sourcing the ORT CUDA EP DLLs is a follow-up.

### Build/test
- Engine `cargo clippy --all-targets -D warnings` clean (toolchain 1.90, the CI pin).
- C# (`AppSettings`, `EngineClient`, `DeepAnalyzeView`, build-all.ps1 SDK fix) —
  `dotnet format` clean + UTF-8 BOM intact; full WinUI compile is the user's VS build (the
  dotnet CLI here lacks `Microsoft.Build.Packaging.Pri.Tasks.dll`).
- Verify on hardware per NEXT.md V16.13.

## 2026-05-21 — V16.12 first-scan tagging + first-run download contention + VLM payload fallback

Targeted pass on the user's three complaints ("tagging doesn't have what macOS
has", "very slow", "very buggy"), grounded in a full re-read. The app was found
to be feature-complete in code but never runtime-verified (V16.7–V16.11 all
"compiles + tests, NOT verified on hardware") — so the work was making the
already-written paths actually fire correctly + fast.

- **Tagging (the #1 fix). SmolVLM tags now land on the FIRST scan.** The
  tags-only auto-pass was reachable only via `ScanComplete → FaceClustering →
  AutoTriggerDeepAnalyze`, gated on `Vlm.Status==Installed` — but on a first run
  SmolVLM is still downloading then, so it skipped and the user saw only sparse
  CLIP placeholders. `EngineClient` now also fires the pass when the `Vlm` slot
  flips to `Installed` after a scan completed this session (re-entrancy-gated so
  it can't double-fire with the cluster path). See DECISIONS.md.
- **VLM reliability. Server payload self-test + CLI fallback.** After
  `VlmServer::start`, a one-shot tiny-image probe (`vlm_server_payload_ok`)
  confirms the server accepts our `image_url` payload (never HW-verified); on
  rejection it emits a non-fatal `vlm_server_payload_rejected` warning and falls
  back to the per-file CLI for the whole batch instead of failing every file.
- **VLM input. Transcode non-JPEG/PNG → JPEG** in `rasterize_for_vlm` (webp/bmp/
  tiff/gif) so llama.cpp's stb_image loader (no WebP) doesn't silently reject a
  file. JPEG/PNG pass through.
- **Slow first-run. CUDA llama runtime now defers** until a VLM is installed
  (re-triggered via `Vlm.PropertyChanged`), so engine-ready no longer fires
  three big concurrent downloads (~650 MB CUDA + ~700 MB SmolVLM + Vulkan) into
  the first scan. CUDA is a 15-25% speed upgrade with nothing to accelerate
  until a VLM exists.
- **Install robustness.** No-progress watchdog raised 30→60 s and now treats
  ANY model's progress as engine-liveness (`_lastAnyProgressAt`), so
  multi-download contention can't false-fail an install. Auto-installers expose
  `ResetAttempt()`, called from the `ReadyEvent` arm, so a mid-download engine
  crash + respawn re-evaluates sentinels instead of abandoning the model for the
  session.
- **CLIP batch/pool default documented.** Fixed the contradictory `tagging.rs`
  comments (struct doc said batch was "opt-in via =1"; `load_default` had it
  default-ON; a VRAM comment claimed "MODEL_POOL_SIZE default of 1" while it's
  4). Kept batch as the default + flagged it PENDING a hardware A/B
  (`FILEID_CLIP_USE_BATCH=0` is the escape hatch).
- **Kept SmolVLM at Q8_0** — rejected Q4_K_M; the ~200 MB saving isn't worth tag
  quality on a 500M model (see DECISIONS.md).
- **Verified-no-change (investigated, already correct):** thumbnail decode is
  already serialized (single-reader channel) + cancels on recycle; `utilization`
  is plumbed but never surfaced (the hardcoded 0.0 misleads nobody); the
  NVIDIA→DirectML "install CUDA pack (~3-5× slower)" perf hint already logs
  (`runtime.rs`); the "click sidebar mid-scan crashes" class is comprehensively
  defended (every view unsubscribes on Unload; `DetailHostView` lazy-builds +
  disposes prior child; `LibraryView` documents its disposal order) — and the
  CUDA-defer change shrinks the hang-prone first-run window further.

### Build/test
- Engine `cargo check` 0 + `cargo clippy --all-targets -D warnings` clean
  (toolchain 1.90, the CI pin).
- **C# (FileID.App) NOT compile-verified in this environment:** building WinUI 3
  via the dotnet CLI here fails at `MrtCore.PriGen` — `Microsoft.Build.Packaging.Pri.Tasks.dll`
  ships only with VS's MSBuild (`v17.0\AppxPackage`), absent from both the
  SDK 8 and SDK 10 installs on this box. The C# edits (A1 install-trigger, C1
  reset, B1 CUDA defer, B2 watchdog) are mechanical + self-reviewed against
  types/namespaces; **they must be built + verified in the user's VS
  environment** (`pwsh build/build-all.ps1 -Run` from a VS Developer shell).
- **Not yet runtime-verified on hardware** (see NEXT.md acceptance criteria).

## 2026-05-21 — V16.11 thumbnails (real root cause) + Deep Analyze runtime + SmolVLM auto-tagging

Three persistent bugs, root-caused from log + disk forensics, fixed in one pass.

- **Track 1 — thumbnails render blank (THE fix).** Logs proved bitmaps decoded +
  assigned (`DECODE_OK`/`TILE_THUMBNAIL_ASSIGNED`) yet the image area was blank.
  Root cause was **layout collapse**, not rendering: `TileRoot` had
  `Height="{Binding ActualWidth, RelativeSource=Self, Converter=IdentityDouble, ConverterParameter=68}"`.
  `ActualWidth` is **not an observable DP**, so the OneWay bind read 0 before
  layout → `0+68=68` → the `*` image row collapsed to ~0 while the fixed 68px
  caption row still showed, and it never re-fired after arrange. Removed the
  binding; added `SizeChanged="OnTileSizeChanged"` that sets `Height = width+68`
  (guarded >0.5 to break the set→SizeChanged loop). Even if SizeChanged never
  fired, dropping the bad binding lets UniformGridLayout's `MinItemHeight=248`
  give the image row ~180px — no more collapse. Added px-dim + TILE_SIZED logs.
- **Track 2 — Deep Analyze "runtime too old" toast (runtime was fine).** Disk had
  b9254 with `llama-mtmd-cli.exe` (89 KB), `llama-server.exe`, `mtmd.dll`. Root
  cause: `vlm.rs::sanity_check_binary` required **3 MB–200 MB**; modern llama.cpp
  ships a thin ~89 KB launcher (heavy code in DLLs), so the floor rejected a
  valid binary → `VlmRunner::find()` reported "missing" → toast, blocking BOTH
  the CLI and persistent-server paths. Floor 3 MB → **20 KB**. Also reordered
  `run_deep_analyze_batch`: resolve server-weights + CLI up front, try the
  persistent server first (it only needs `llama-server.exe`), require the CLI
  binary only when the server can't start — and keep the "runtime missing" error
  *before* `DeepAnalyzeStarting` so the Error (which doesn't reset DeepAnalyze
  state) can't strand the UI on a "Loading model…" banner.
- **Track 3 — SmolVLM auto-tagging (CLIP stays as instant placeholder).**
  - `AnalyzeMode::TagsOnly` (1 VLM call/file vs 3 → ~3× faster) on both the CLI
    `analyze_file` and the server `analyze_file_via_server` (now mode-aware).
  - Additive IPC field `tags_only: bool` on `DeepAnalyzeAllPayload` (Rust
    `#[serde(default)]`, C# `DeepAnalyzeAllCommand`, `ipc.schema.json`, round-trip
    + omitted-defaults-false tests). Auto-chain sends `tags_only:true`; manual
    Deep Analyze sends `false` (full caption+rename+tags).
  - `SmolVlmAutoInstaller` (mirrors `LlamaRuntimeAutoInstaller`) silently prewarms
    SmolVLM weights at engine-ready; opt-out `DisableAutoInstallSmolVlm`.
  - Default tagger → **smolvlm** (`AppSettings.SelectedVlmModelKind`), with a
    one-time **schema v1→v2 migration** flipping existing users still on the old
    `qwen2_5_vl_3b` default (the user's settings.json had exactly that); fresh
    installs start at v2 so the migration can't clobber a deliberate re-pick.
  - Aligned the welcome-sheet VLM + `ModelInstallerService` (`_vlmModelKind`, slot
    label/bytes, `UpdateVlmRecommendation`) to SmolVLM universally on Windows so
    Welcome auto-install never pulls a redundant ~1.65 GB Qwen; Qwen 3B/7B + Gemma
    stay available in the Deep Analyze model picker.
  - CLIP `SCENE_COSINE_THRESHOLD` 0.24 → **0.18** so placeholder scene chips
    actually show during the scan (VLM tags supersede via ReadStore's
    `source='vlm'` ordering).
  - Re-surfaced a single **"Tag automatically with AI after scans"** switch in the
    Settings → Cleanup card (bound to `AutoChainDeepAnalyze`).
  - Fixed stale "SmolVLM 256M / ≈300 MB" UI labels → **500M / ≈700 MB** (registry
    installs SmolVLM-500M-Instruct).

### Build/test (all green)
- Engine **158** tests + `cargo check` 0 + **clippy -D warnings clean**;
  `FileID.App.Tests` **98**; `FileID.IpcSchema.Tests` **31**; .NET build x64 **0/0**;
  `dotnet format --verify-no-changes` 0; BOM intact on edited/new `.cs`/`.xaml`.
- **Not yet runtime-verified on hardware** (user runs the build): thumbnails
  render square during+after scan; Deep Analyze captions with no toast
  (`[VLM-SERVER] ready`); SmolVLM auto-tags after a scan and resumes mid-pass.

## 2026-05-20 — V16.10 full bug sweep before first run (1 real bug + 4 hardening)

Pre-run audit of the whole session's changes: full test suites + two independent
read-only code-review agents (engine + app). Found one genuine bug + four
defensive fixes; everything else verified clean.

- **BUG (fixed): `vlm_server.rs` used `n_predict` on the OpenAI `/v1/chat/completions`
  endpoint**, which reads `max_tokens` — so the 80/40/30 token caps were silently
  ignored and every server-path caption/tag/rename ran to the server default
  (long, slow; a rename could come back as a paragraph). Now sends both
  `max_tokens` + `n_predict`. The CLI path was already correct (`--n-predict`).
- **Hardening:** image data URIs now carry the real MIME (magic-byte sniff)
  instead of always `image/jpeg`; per-request VLM timeout 180→300 s (large/CPU
  models); `MergeById` defensively skips duplicate Ids; both auto-installers
  check the `bin/` layout too (the engine accepts both — a flat-only check could
  loop re-downloading a future nested build).
- **Verified clean (no bugs):** SCRFD shape-classification (no panic on odd
  shapes; strides can't collide N), cosine scorer + `embed_batch` fallback,
  CUDA→Vulkan guarded fallback, `analyze_file` ownership/cancellation, the
  Library `MergeById` index bounds + selection sync, `DetailHostView` lazy-build
  (no zombie views), ReadStore SQL (4 sites identical), Settings deletions (no
  dangling refs), dispatcher safety + subscription teardown.

### Build/test (final, post-fix)
- Engine **158** tests + `cargo check` 0 + **clippy clean**; `FileID.App.Tests`
  **98**; `FileID.IpcSchema.Tests` **30**; .NET build x64 **0/0**;
  `dotnet format --verify-no-changes` 0; BOM intact on edited `.cs`.

## 2026-05-20 — V16.9 Settings → macOS parity (Advanced disclosure + trim toggles) + CUDA VLM path

User: "Yes do this all" (the two V16.8 follow-ups).

- **Settings (C2).** Trimmed the 3 Windows-only Behavior toggles macOS lacks
  (Hide-unknown-clusters, Restructure-tree-diff, Auto-chain-Deep-Analyze) — XAML
  + their handlers + the `HydrateToggles` sync; kept the Cleanup toggle (renamed
  the card "Behavior" → "Cleanup" to match macOS). The underlying AppSettings
  keys stay (other views read them at their defaults), so no behavior breaks —
  the toggles just leave Settings. Converted the verbose **Diagnostics card into
  a collapsed `Expander`** ("Advanced — diagnostics & tools"), mirroring macOS's
  collapsed Advanced disclosure, so CPU/Mem/GPU/Power/scene/thumbnail info +
  force-retag/refresh hide behind a chevron by default. Done in-place (no card
  reorder) to stay safe on a render I can't see. Kept all genuinely
  Windows-specific sections (GPU EP override, CUDA llama.cpp, cuDNN).
- **CUDA VLM path (D).** Bumped `llama_runtime_cuda_x64` → b9254 cuda-12.4 **plus
  the now-separate cudart asset** (b9254 split cudart out; both extract into
  `llama.cpp-cuda\` so the CUDA binaries are self-contained — the engine
  AddDllDirectory's that dir). Routed BOTH `VlmRunner::find` (CLI) and
  `VlmServer::start` to **prefer the CUDA dir, with a guarded fallback to
  Vulkan**: `find` skips a CUDA binary whose `--version` probe fails (missing
  cudart), and `VlmServer::start` tries each candidate (CUDA → Vulkan) with an
  early child-exit check so a broken CUDA build falls back in <1 s instead of a
  120 s health timeout. This fixes the latent bug where the CUDA runtime was
  installed but `VlmRunner` only ever looked in the Vulkan dir — so the promised
  "15-25% faster Deep Analyze on NVIDIA" never actually engaged. `CudaAutoInstaller`
  got the same stale-detection (mtmd-cli missing → re-fetch).

### Build/test
- Engine `cargo check` 0; clippy + .NET build + format running. App tests 98.
- **NVIDIA download note:** the CUDA runtime is now ~650 MB (259 MB llama +
  391 MB cudart) vs the old 210 MB self-contained zip — the auto-installer
  pulls it on NVIDIA (opt-out via the Settings toggle). The Vulkan runtime
  (33 MB) remains the universal default and the safe fallback.

## 2026-05-20 — V16.8 VLM activated (runtime b9254) + persistent llama-server speedup + Settings declutter

User: "Implement it all" (runtime + persistent server) and "make the settings
page the same as macOS, no extra junk that isn't windows specific."

- **A — llama runtime bumped + auto-reactivated.** `registry.rs`
  `llama_runtime_x64` → **b9254** (`ggml-org/llama.cpp`, 2026-05-20). Verified
  by downloading the win-vulkan-x64 zip and listing it: it ships
  `llama-mtmd-cli.exe` + `llama-server.exe` + `mtmd.dll` (the prior b4404 pin
  had none of the mtmd surface and predated Qwen2.5-VL — the root of the
  "runtime not found" toast). `LlamaRuntimeAutoInstaller` now treats
  "sentinel present but `llama-mtmd-cli.exe` missing" as stale → deletes the
  sentinel + cached zip and re-installs, so the bump auto-activates on next
  launch (no manual wipe). This alone fixes the toast and makes Deep Analyze
  runnable. (CUDA runtime left at its pin — `VlmRunner`/`VlmServer` use the
  Vulkan dir, and the new CUDA build splits cudart out; documented in NEXT.)
- **B — persistent `VlmServer` (the speedup).** New `models/vlm_server.rs`
  spawns `llama-server.exe` ONCE (model resident) and serves images over
  `/v1/chat/completions` multimodal (base64 data URI; reqwest built without the
  `json` feature so the body/response are hand-(de)serialized via serde_json;
  `kill_on_drop`). `run_deep_analyze_batch` now starts one server for the whole
  batch and routes each file through it (~1-3 s/file vs reload-per-file), with
  graceful fallback to the per-file CLI (`analyze_file`) when weights are
  missing or the server can't start. Refactored `analyze_file` to share
  `rasterize_for_vlm` + `persist_vlm_results` with the new
  `analyze_file_via_server`. So the V16.7 VLM tagging now runs at usable speed
  over a whole library.
- **C — Settings deduped toward macOS.** macOS Settings = Cleanup toggle + 3
  model cards + a collapsed "Advanced" (engine/storage/scans/logs). Windows
  keeps its genuinely-Windows-specific sections (GPU EP override, CUDA
  llama.cpp, cuDNN, hardware diagnostics — macOS has none of these; it uses the
  ANE). Removed two clear non-Windows extras: the pure-documentation **"Models"
  card** (duplicated the Local AI card + About) and the **disabled "Performance
  profile" combo** ("Eco/Performance — coming soon" placeholder). Both
  XAML-only, zero code-behind deps (`ProfileCombo` unreferenced). Left the
  functional Behavior toggles + the thumbnail/scene diagnostics in place rather
  than delete working controls I can't visually verify — a fuller "collapse
  diagnostics under Advanced + trim the 3 extra behavior toggles" restructure is
  noted in NEXT for user confirmation.

### Build/test
- Engine `cargo check` 0 (clippy running); `deep_analyze` 11/11; .NET build
  pending (running). VlmServer compiles with the existing crate features
  (base64 0.22 direct dep; reqwest manual JSON).
- **Activation is now in-code:** rebuild + relaunch → the auto-installer pulls
  b9254 (mtmd-cli + server), Deep Analyze works, and "Analyze all" tags the
  library via the persistent server. **Runtime-verify on hardware** that
  b9254's `llama-server` answers `/v1/chat/completions` with an image for
  Qwen2.5-VL (the one thing a compile can't prove).

## 2026-05-20 — V16.7 VLM tagging implemented (reuses Deep Analyze; CLIP now removable) + accurate runtime error

User: "ensure the VLM thing is totally implemented so if we decide to go that
way we can simply remove the CLIP." Implemented VLM scene/content tagging by
**reusing the existing Deep Analyze "Analyze all" pipeline** rather than
building a parallel job — that path is already a resumable, cancellable,
whole-library VLM pass with IPC + progress + a UI + a model picker; the only
thing it lacked was writing tags.

- **Engine (`pipeline/deep_analyze.rs`, `models/vlm.rs`):** `analyze_file` in
  `Both` mode now runs a dedicated `TAG_PROMPT` against the already-rasterized
  frame, parses the completion with `parse_vlm_tags` (splits on comma/newline,
  strips numbering/bullets/quotes, drops >3-word fragments, dedups, caps at 8),
  and persists them to the `tags` table as **`source='vlm'`** (DELETE prior vlm
  tags for the file, then INSERT). Fully separate from CLIP's `source='auto'`.
  5 new `parse_vlm_tags` unit tests.
- **Read path (`ReadStore.cs`, all 4 GROUP_CONCAT sites):** added `'vlm'` to the
  source filter and an `ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1
  ELSE 2 END, score DESC, rowid` so VLM tags lead the 2-chip slice over CLIP.
- **Trigger/UI/IPC:** none added — the existing Deep Analyze "Analyze all"
  (`deepAnalyzeAll` → `run_deep_analyze_batch` → `analyze_file` in `Both`) now
  produces VLM tags as a side effect of the full-enrichment pass. Resumable via
  the existing `skip_existing` (on `vlm_description`).
- **"Simply remove CLIP":** the CLIP scan-time scene tags are a single
  self-contained block in `pipeline/tagging.rs` (`if let (Some(labeler), …)`),
  and ReadStore already prefers `vlm` over `auto` — so dropping CLIP is deleting
  that block (or gating it). VLM tags then lead unchallenged.
- **llama toast (accurate now):** `VlmRunner::find()` previously bailed "runtime
  not found → install from Settings" even though the runtime IS installed —
  it's just **too old** (b4404, Dec 2024: ships `llama-server.exe` +
  `llama-llava-cli.exe` + `llama-qwen2vl-cli.exe` but NOT the unified
  `llama-mtmd-cli.exe` this code drives, and predates Qwen2.5-VL). `find()` now
  detects a stale-but-present runtime and says "too old — update it" instead.

**ACTIVATION PREREQUISITE (the one thing not done — see NEXT.md):** none of the
VLM path (Deep Analyze OR the new tagging) can actually RUN until the llama
runtime is bumped to a current build that ships `llama-mtmd-cli.exe` + supports
Qwen2.5-VL. The registry URL is pinned to b4404. I did **not** blind-guess a new
release URL (a wrong tag breaks the user's download); the bump + re-install +
verification is the documented activation step.

### Build/test
- Engine `cargo check` 0; `dotnet build` x64 0/0 (pending — running);
  `deep_analyze` tests (parse_vlm_tags) pending. CLIP/thumbnail/faces fixes from
  V16.6 unchanged.

## 2026-05-20 — V16.6 thumbnails persist (collection churn), CLIP cosine threshold, faces fixed (SCRFD ordering), llama toast diagnosed

After V16.5c made tiles *visible*, the user reported: thumbnails still blank
during a scan, tagging "10% accurate / awful", faces not detected like macOS,
and a "llama.cpp runtime not found" toast. Diagnosed all four; fixed three,
scoped the fourth into the VLM build (Track 3). Plan:
`~/.claude/plans/okay-so-thumbnails-are-validated-castle.md`.

- **Thumbnails blank during scan = collection-reset churn (FIXED).** The
  `LastBatch` event drove `RefreshAsync` → `ReplaceItems` →
  `BatchObservableCollection.ReplaceAll`, which raised a single `Reset` ~1 Hz.
  ItemsRepeater re-realized every visible element against brand-new `FileTile`
  instances (`Thumbnail==null`), so each thumbnail was nulled + reloaded every
  second and raced the next reset (`TILE_THUMBNAIL_ASSIGNED` high, `IMAGE_OPENED`
  zero). macOS keeps stable tile identities. Fix: `LibraryViewModel.MergeById`
  — an identity-stable, in-place merge keyed by `FileTile.Id` that keeps
  surviving instances (and their loaded `Thumbnail`), merges only mutable
  display fields (`MergeMutableFrom`: Tags/TopTwoTags/ProposedName/HasFaces/
  HasText), and emits granular Add/Remove (never Move). Made those FileTile
  fields change-guarded settable. Disjoint result sets (a new search) fall back
  to `ReplaceAll`. Also **deleted the dead Image-opacity dance**
  (`OnTileImageOpened`/`EnsureThumbnailVisible`/`FindBoundImage` + the XAML
  `ImageOpened` hook) — `ImageOpened` never fires for pre-decoded/cached
  BitmapImages, nothing set Image opacity to 0, so it was dead code + a
  composition fast-fail vector. Bonus: fixed a latent bug where selection
  silently cleared on every mid-scan refresh. 8 new `MergeById` unit tests.
- **Tagging "worthless" = softmax-prob threshold (FIXED).** `score_labels`
  softmaxed cosine×temp100 over 164 labels and thresholded the *probability* at
  0.12 — razor-peaky, so the top label scored ~0.99 even when the true cosine
  was mediocre → every file got a confident WRONG tag. Image+text towers are the
  same `Xenova/mobileclip_s2` export (shared 512-d space — mismatch ruled out).
  Fix: threshold the **raw cosine** (`SCENE_COSINE_THRESHOLD = 0.24`,
  the primary tuning lever), persist the cosine as `tags.score`, drop softmax.
  A no-match image now gets NO tag instead of a confident wrong one. Rewrote the
  `score_labels` tests for cosine semantics.
- **Faces not detected = SCRFD output ordering (FIXED).** `engine.jsonl` was
  full of `SCRFD bbox/kps tensor undersized — skipping stride`. `scrfd.rs::detect`
  indexed the 9 ONNX outputs **positionally** assuming `[score,bbox,kps]×stride`
  interleave; this export groups by type (`[score_8,score_16,score_32,bbox_8,…]`),
  so every stride grabbed the wrong tensor, failed its size check, and detected
  ZERO faces. Fix: classify outputs by **shape** — last-dim channels (1=score,
  4=bbox, 10=kps), distinct anchor counts sorted descending → strides [8,16,32].
  Robust to ordering AND naming. (Decode math unchanged.)
- **"llama.cpp runtime not found" toast = outdated runtime (SCOPED to Track 3).**
  Not "missing" — the runtime IS installed (sentinels + `llama-server.exe` +
  per-model CLIs present, prewarm `outcome=installed`). But it's pinned to
  release **b4404** (Dec 2024), which lacks `llama-mtmd-cli.exe` (the binary
  `VlmRunner::find()` requires) and predates Qwen2.5-VL. So every Deep Analyze
  emits `EngineError{kind:"llama_cpp_missing"}`. Crucially `llama-server.exe`
  IS present — exactly what Track 3's persistent VLM uses. Fix folded into
  Track 3: bump the runtime registry URL to a current build with mtmd-cli +
  Qwen2.5-VL (invalidate sentinel to force re-install) AND route VLM through
  `llama-server.exe`. Did NOT blind-guess a release URL.

**User decision (this session):** tagging = "Both — fixed CLIP default + a VLM
background upgrade." Track 3 (VLM-server background tagging) is the next big
build; it also resolves the toast.

### Build/test
- Engine `cargo check` 0; `scene_vocab` 5/5; .NET build x64 **0/0**;
  `FileID.App.Tests` **98 pass** (was 90; +8 MergeById tests); SCRFD `cargo check`
  0 (tests running). BOM intact on edited `.cs`.
- **No re-scan needed for thumbnails.** Faces + tagging need a re-tag
  (`build-all.ps1 -WipeDbOnly`) to re-run SCRFD + the cosine tagger.

## 2026-05-20 — V16.5c the SAME invisible-tile bug, root cause this time: tile-root entry spring + tab-crash hardening

User rebuilt V16.5b and reported the trifecta again: "tagging not working,
can't click sidebar tabs without the app crashing, thumbnails not loading."
Log + DB forensics on the user's own session proved **all three engine
features work** — the failures were two .NET-side defects, one of which V16.5b
half-fixed.

- **Invisible tiles (reads as BOTH "thumbnails not loading" AND "tags not
  showing").** Same forensic signature V16.5b chased: `app.log` had **8611
  `[THUMB] TILE_THUMBNAIL_ASSIGNED`** + 9148 L1/L2 hits + 346 `BITMAP_SET`, but
  **zero `IMAGE_OPENED`/`OPACITY_SET`** — bitmaps bound, tiles invisible.
  V16.5b fixed the *image*-level opacity pin in `OnRepeaterElementClearing` but
  the bug survived because the real culprit was one level up:
  `LibraryView.AnimateTileEntry` drove the **tile-root** composition `Opacity`
  0 → 1 via a `SpringScalarNaturalMotionAnimation`. Under mid-scan churn the
  `ItemsRepeater` re-realizes elements ~1 Hz (each throttled refresh raises a
  Reset → 8721 PREPARE events in the log), and the interrupted opacity spring
  stranded the **entire** tile — thumbnail, filename, AND tag chips all live
  under that root — at Opacity 0. Fix: the tile-root opacity is now pinned to 1
  on every prepare and **never animated**; the entrance is a scale-only pop
  (0.96 → 1, the scale half of the macOS `.opacity.combined(with:.scale)`
  transition) that can never hide content, gated to once per element instance
  (a `ConditionalWeakTable`) so it doesn't replay on every Reset and pulse the
  grid. This is why the DB had 24,762 auto-tags across 7,961 files (100%
  coverage; `rainbow` 0.99, `wedding` 0.99, `storm` 0.98) yet the user "saw no
  tags": the chips were rendering into an invisible root.
- **Intermittent tab-switch crash (native fast-fail, no managed trace).** Two
  `session-died-without-handler` breadcrumbs 9 s apart, then a clean 7-min
  session — a timing race, not a deterministic bug. Hardened the teardown path
  in `DetailHostView.Sync`: it used to build the next view **eagerly** and, on
  a rapid second tab click, `Stop()` the in-flight storyboard — whose
  `Completed` then never ran, so the just-built view was never mounted, never
  `Unloaded`, and leaked as a zombie still subscribed to
  `EngineClient.PropertyChanged` (re-querying a never-shown `ReadStore` on every
  engine event). Now the view is built **lazily inside the fade-out
  completion** (a superseded swap constructs nothing) and committed through one
  synchronous helper so the outgoing view always tears down cleanly. Also added
  a `_unloaded` guard to `LoadThumbAsync`'s UI continuation so a thumbnail that
  resolves after a tab switch can't touch `Repeater` / composition visuals on a
  detached view (a fast-fail vector).
- **Engine hardening (not this user's bug, but flagged high-priority in
  NEXT.md):** `SceneLabeler::build` now falls back to per-prompt `ClipText::embed`
  when `embed_batch` errors, so a batch-pinned text ONNX export on another
  machine degrades to sequential encoding instead of silently disabling all
  scene tags (`labeler = None`). This user's export has a dynamic batch axis —
  `[TAGGING] scene-label embeddings built n_labels=164` in their log — so the
  fast path is unaffected.

### Build/test

- Engine `cargo check` 0; .NET build x64 **0 warning / 0 error**; BOM intact on
  edited `.cs`. Targeted tests below.
- **No re-scan required** — tags + the thumbnail disk cache are already on disk.
  Rebuild the app, relaunch: tiles are now visible (so are their chips), tab
  switching is hardened. The engine `embed_batch` fallback only matters on a
  fresh model install on a different machine.

## 2026-05-20 — V16.5b two display bugs: thumbnails loaded-but-invisible + scene tags hidden by tag order

User rebuilt V16.5 and reported "thumbnails still not loading, tagging still
sucks." Live log + DB forensics proved **both features work; the UI wasn't
showing their output.** Both are display-only .NET fixes — no engine change,
no re-scan needed.

- **Thumbnails loaded but invisible.** `app.log` had 4711
  `[THUMB] TILE_THUMBNAIL_ASSIGNED` + 929 `BITMAP_SET` (bitmaps WERE assigned)
  but zero `IMAGE_OPENED`/`OPACITY_SET` — the reveal never ran. Root cause:
  `OnRepeaterElementClearing` pinned the Image's *composition* opacity to 0,
  and the reveal-back-to-1 (`EnsureThumbnailVisible` via
  `Repeater.TryGetElement(IndexOf(tile))`) is unreliable under heavy mid-scan
  re-virtualization on a 15K-item list → tiles stuck at opacity 0 despite a
  valid Source. Fix: stop pinning image opacity to 0 on recycle; the Image
  keeps its XAML default (1), and `ClearThumbnailForRecycle` already nulls the
  Source, so a recycled tile shows the shimmer (not a stale bitmap), never a
  permanent blank.
- **Scene tags hidden by tag order.** DB had 3771/4004 files (94%) with scene
  tags — `garage` 0.35, `tools` 0.48, `wedding` 0.99, `museum` 0.95 — but the
  Library showed only `Has Location`/`2024`. `ReadStore`'s `GROUP_CONCAT(tag)`
  had no ORDER BY, so enriched extras (NULL score) preceded scene tags and
  `TopTwoTags` (first 2) never reached the scene label. Fix: order the
  GROUP_CONCAT subquery `ORDER BY score DESC, rowid` at all four query sites
  so scene tags lead by confidence, extras trail. Validated on the live DB:
  visible files now emit `garage|Year_2024|Has Location` etc.

### Build/test

- .NET build 0/0; `FileID.App.Tests` 90 pass; `dotnet format --verify-no-changes` clean.
- **No re-scan required** — tags + the thumbnail cache are already on disk;
  rebuild the app + relaunch, and the Library re-queries (reordered tags) and
  re-renders (now-visible thumbnails).

## 2026-05-19 — V16.5 CLIP zero-shot tagging + thumbnail recycle fix + People double-tap + classifier removal

Replaced the scan-time scene tagger and fixed the "thumbnails render from
anything" recycle bug, then swept the same bug class app-wide. **Engine:**
153 `cargo test --lib` pass + clippy `-D warnings` clean. **.NET:** build
0/0, 90 `FileID.App.Tests` pass (was 86), `dotnet format --verify-no-changes`
clean. All pending user verification on real hardware (rebuild + clean
rescan / force re-tag).

### Tagging — CLIP zero-shot replaces the ImageNet classifier

- macOS tags scenes via Apple Vision (a scene taxonomy); Windows used a
  MobileNetV3 **ImageNet object** classifier whose argmax labels
  (`breakwater`, not `beach`) were the wrong taxonomy — the "horrible /
  nothing like macOS" tags. Replaced with **CLIP zero-shot**: the per-file
  MobileCLIP image embedding (already computed at `tagging.rs:945`) is
  cosine-scored (softmax temp 100, threshold 0.12, top-4) against a curated
  ~170-label scene vocabulary embedded by the matched MobileCLIP-S2 **text**
  encoder (both towers ship from the same `Xenova/mobileclip_s2` repo → a
  shared 512-d space). New `models/scene_vocab.rs` (vocabulary +
  prompt-ensembled label matrix + the pure, tested `score_labels`);
  `clip_text.rs` gained `embed_batch`. The label matrix builds once per
  launch (process-static `OnceLock`); the 253 MB text session is dropped
  after.
- **No new download** — both CLIP halves are already required for scans, so
  the redundant ~22 MB ImageNet classifier is **gone**: engine
  `models/classifier.rs` + its registry arm deleted; .NET
  `ClassifierAutoInstaller`, the Settings install slot, and the Library
  "Install Scene Classifier" banner removed. This removes the "downloading
  something for identifying" the user saw **and** drops a whole ONNX
  inference + a 224×224 resize from the per-file hot path (net perf win).
- **Confidence persisted.** `TaggedFile.tags` is now
  `Vec<(String, Option<f32>)>`; the softmax probability lands in the
  existing `tags.score` column (no migration — the column already existed).
  Scene tags are pushed score-descending, so a card's top-2 chips are the
  highest-confidence scene labels.
- **Force re-tag** — Settings → "Re-scan everything (force re-tag)" calls the
  already-plumbed `StartScanAsync(..., rescan: true)` against the current
  library root, so a tagging/threshold change is visible without deleting
  `fileid.sqlite`.

### Thumbnails — recycle stale-bitmap fix

- `OnRepeaterElementClearing` zeroed the Image opacity but never nulled
  `tile.Thumbnail`; since the Image binds `Source="{x:Bind Thumbnail}"`, a
  recycled element kept the previous file's bitmap and flashed it on reveal
  (and off-screen tiles retained every bitmap → memory bloat). New
  `FileTile.ClearThumbnailForRecycle()` releases the bitmap before
  `IsDetached` flips; `[THUMB] RECYCLE_NULLED` traces it. 4 new headless
  recycle/shimmer-contract tests.

### People — double-tap fixed (same bug class)

- `OnClusterDoubleTapped` read `el.DataContext is PersonCluster` with **no
  Tag fallback**, so under x:Bind (DataContext null) double-tapping a cluster
  silently did nothing. Added `OnClusterElementPrepared` (mirrors Library) to
  bridge index→DataContext; drag/drop kept their Tag fallback. Cleanup
  (classic `{Binding}`) and Restructure/Sidebar (display-only / stable
  container) audited clean.

### Deferred (see NEXT.md)

- Verify `embed_batch` against the real text ONNX (assumes a dynamic batch
  axis). Tune the vocabulary + threshold against real photos. Move the
  one-time label-matrix build off the first scan if it's slow. Explicit
  `ORDER BY score` in the ReadStore GROUP_CONCAT (insertion order already
  approximates it).

## 2026-05-19 — V16.4 two real bugs found via log+DB forensics: thumbnail trigger + classifier coverage

User rebuilt V16.3 and reported "still broken thumbnails, still terrible
tagging." A screenshot confirmed the V16.3b kind chip works but tiles were
blank and chips were enriched-extras-only. Investigated the live
`app.log` + `fileid.sqlite` (read-only) and found **both root causes sit
in a layer no prior fix had touched.**

### Thumbnails — `ThumbnailService` was never being called

- `app.log` (3.1 MB, live scan) had **zero** `[THUMB]` lines.
  `RequestAsync` logs `[THUMB] REQUEST` as its first statement, so it was
  never invoked. The L2 disk cache (`thumbs.cache`) had **0 files across
  every session** — not one thumbnail had ever rendered.
- Root cause: `OnRepeaterElementPrepared` opened with
  `if (... el.DataContext is not FileTile tile) return;`. **x:Bind in the
  ItemsRepeater ItemTemplate does not populate the realized element's
  DataContext** (compiled bindings bypass it), so the guard returned on
  every tile, before `LoadThumbAsync` (the sole caller of
  `RequestAsync`). Every V15.5→V16.2 thumbnail "fix" had patched the
  `ThumbnailService` fallback chain — a layer that's never reached.
- Fix (`LibraryView.xaml.cs`): resolve the tile from the authoritative
  `args.Index` against `ViewModel.Items`, then **set `el.DataContext =
  tile`** so the four sibling code-behind handlers that read
  `el.DataContext` (Prepared / Clearing / Tapped / DragStarting — lines
  443/617/746/932) all resolve correctly with no further change. Added a
  `[THUMB] PREPARE idx=… dcWasNull=… resolved=…` diagnostic at the top of
  the handler so the next run confirms the path.

### Tagging — classifier is healthy; ImageNet-1k coverage is the problem

- The classifier loads and runs on every file (`warmup complete
  label_count=1000`, `pool loaded pool_size=2`; the screenshot file was
  scanned twice *after* the model loaded). It is **not** a load failure.
- DB forensics: 2,196 / 3,334 files (66%) had only enriched extras (no
  scene label) at threshold 0.30. The labels that did fire were
  object-specific ImageNet oddities (`breakwater`, `radio telescope`,
  `dust jacket`), not scene categories.
- Root cause: ImageNet-1k is an **object** classifier (wrong taxonomy for
  "scene" tags) and 0.30 is too high for its diffuse softmax on
  out-of-distribution personal photos.
- Fix (`pipeline/tagging.rs`): lowered `CLASSIFIER_THRESHOLD` 0.30 → 0.20
  to recover coverage. Confidence persistence into `tags.score` and a
  Places365 scene-model swap were scoped but deferred (see NEXT.md) —
  Places365 has no MobileNet ONNX on HF, and the score change ripples the
  `TaggedFile.tags` type with no user-visible effect this round.

### Build/test status

- .NET: `dotnet build FileID.sln -c Debug -p:Platform=x64` 0/0;
  `dotnet format --verify-no-changes` exit 0.
- Engine: `cargo clippy --lib -D warnings` clean; `cargo test --lib`
  152/152 pass.

### Verification still pending (user, real hardware)

1. **Re-tag is required** — existing rows are from the 0.30 ImageNet run
   and incremental rescan skips current files. Delete
   `%LOCALAPPDATA%\FileID\fileid.sqlite*`, rebuild, rescan.
2. Thumbnails: tiles should render; `app.log` shows `[THUMB] PREPARE`
   (with `dcWasNull=True` expected, confirming the diagnosis) →
   `REQUEST` → `BITMAP_SET`; `thumbs.cache` fills; Settings → Diagnostics
   → Thumbnails `ok>0`.
3. Tagging: scene-label coverage should rise well above 34%; spot-check
   chips name plausible content. If still too sparse, lower the threshold
   further (or pursue Places365 per NEXT.md).

## 2026-05-19 — V16.3 four-problem follow-up: file-type chip + classifier diagnostics + broken-image placeholder + video COM fix

Picked up the "four problems" directive (tag accuracy / file-type chip /
thumbnails / video). On audit, V16.1+V16.2 (commit 3c7ae32) had already
landed most of it: classifier URL verified + both SHA256s pinned in
`registry.rs`, preprocessing correct (RGB 224 ImageNet mean/std NCHW),
softmax + threshold 0.30 + top-K 8 + 1001-class background offset,
silent auto-installer, FileKind enum + `kind` DB column + IPC + ReadStore
projection + thumbnail-corner kind icon badge, the V15.6 thumbnail
fallback move, the Settings thumbnail diagnostics, and the Media
Foundation keyframe→tagging path. This session closed the genuinely-open
gaps and fixed the one real defect the audit surfaced.

### What landed

- **File-type text chip (Problem 2).** `TagChip` gained a `Variant` DP
  (`Auto` gold | `Kind` gray). `FileTile` gained `ShowKindChip`
  (`Kind != "other"`) + `HasChips` (`ShowKindChip || HasTags`). The card
  chip row is now a `StackPanel` with the gray kind chip first, the gold
  AI chips after; collapses entirely on bare Other-kind files. 16 new
  unit tests (`FileTileKindChipTests`) cover the kind→display map and
  the suppress-on-Other rule. The V16.2 icon badge is retained — badge
  is glanceable on the thumbnail, chip is text-readable in the caption.
- **Classifier diagnostics (Problem 1).** Settings → Diagnostics gained
  a `ClassifierDiagnosticsText` line: installed/not + class count +
  threshold 0.30 + top-K 8 + model MB. Disk-probe (sentinel +
  `imagenet_classes.txt` line count + ONNX size) — no IPC change. Lets
  the user confirm whether scene tags should appear without tailing
  `engine.jsonl`. Re-bound on the existing Refresh button.
- **Broken-image placeholder (Problem 3).** `FileTile` gained
  `ThumbnailFailed` + `ShowShimmer` (`Thumbnail == null && !failed`).
  `LoadThumbAsync` sets `ThumbnailFailed = true` (UI-dispatched) when
  `RequestAsync` returns null; `OnRepeaterElementPrepared` clears it on
  re-attach so a retry can fall back to shimmer. XAML shows a muted
  procedural `FontIcon` (`&#xE91F;`) when failed; shimmer binding moved
  from `Thumbnail`-null to `ShowShimmer`. No asset binary added.
- **Video COM init (Problem 4 — the one real defect).**
  `shell::video::keyframe_25pct` called `MFStartup` but never
  `CoInitializeEx`. The decoder pool spawns raw OS threads
  (`run_decoder_thread`, tagging.rs:520) and Deep Analyze runs keyframe
  extraction on tokio blocking-pool threads — only the one thread that
  won the `MFStartup` `Once` race had a COM apartment, so
  `MFCreateSourceReaderFromURL` would `CO_E_NOTINITIALIZED` on the rest.
  Added a thread-local `CoInitializeEx(COINIT_MULTITHREADED)` guard
  inside `keyframe_25pct` (covers both call paths), matching the
  per-thread COM init the other `shell::*` modules already do. The
  BGRA→RGB conversion (video.rs:188-190) and the keyframe→classifier
  wiring were audited and confirmed correct.

### Build/test status

- Engine: `cargo check --lib` clean, `cargo clippy --lib -D warnings`
  clean, `cargo test --lib` 152/152 pass.
- .NET: `dotnet build FileID.sln -c Debug -p:Platform=x64` 0 warnings /
  0 errors; `FileID.App.Tests` 86/86 pass (was 70).

### Verification still pending (user on real hardware)

1. Scan `C:\Users\adamm\Desktop\Test Data`; confirm each card leads with
   a gray file-type chip, gold AI chips follow.
2. Settings → Diagnostics: Classifier line shows `Installed · 1000
   classes · threshold 0.30 · top-K 8 · model ~21 MB`. If it says "Not
   installed", the auto-installer hasn't completed a download yet.
3. Scroll Library; tiles whose thumbnail render fails show the
   image-glyph placeholder, not perpetual shimmer.
4. Drop 10-20 `.mp4`/`.mov`/`.mkv` files in the corpus + rescan:
   keyframe thumbnails render, kind chip says "Video", classifier emits
   scene chips from the keyframe. (This is the COM-fix verification —
   pre-fix, most videos would have produced no keyframe.)

## 2026-05-18 — V16.0 four-regression sweep: perf + thumbnails + classifier + tag chips

Single-session pass against the directive in `/loops/four-regressions-windows.md`
covering the user's observed 0.04 files/sec scan rate (~3,500× off the 140 f/s
target), the 100% placeholder-gradient Library, missing semantic tags, and
the absence of tag chips on cards. Engine + .NET both green; cargo check
clean, cargo build clean, dotnet build clean.

### WP1 — Perf: 0.04 → target ≥40 files/sec

- **`pipeline/tagging.rs` decoder pool (B3)** — split decode out of the
  worker hot path. New `PreDecoded { file, decoded }` struct flowing
  through a second async-channel; `M = clamp(p_cores+e_cores, 2, 12)`
  sync OS threads pull from raw discovery, run image/video decode via the
  existing `image::Reader` / `shell::video::keyframe_25pct` paths, and
  push pre-decoded RGB into the worker channel. Workers drop their inline
  decode call and consume `PreDecoded` directly. CPU was at 12% during
  the user's baseline scan because workers stalled waiting on the GPU
  semaphore; the decoder pool keeps the decoded-frame buffer warm so
  workers never sit on the CPU-bound path.
- **Batch CLIP default-on (B1)** — `FILEID_CLIP_USE_BATCH` is now a
  kill-switch (`=0` opts out), not an opt-in. `DEFAULT_BATCH_SIZE` bumped
  4→8 based on the user's 3.2 GB VRAM headroom (baseline reported 2.8/6 GB
  peak). The single-session `ClipBatchCoordinator` path now drives
  most installations; the pool path is preserved for boxes that OOM
  under sustained batch load.
- **CUDA-pack info hint (B5)** — `models/runtime.rs::RuntimeProbe::detect()`
  emits a one-time `tracing::info!` when NVIDIA hardware is detected but
  the CUDA Performance Pack isn't installed. Surface-only — no auto-install,
  no UI prompt; install is gated behind Settings → Performance.
- **Per-stage perf trace (WP1-A)** — `FILEID_PERF_TRACE=1` enables
  `[PERF] stage=X path=… elapsed_ms=N` lines for every per-file stage
  (image_decode / exif / dhash / scrfd / arcface / clip / ocr / db_write /
  total). Default-off, zero cost when not set. The directive's
  BASELINE PERF REPORT table can be filled from these logs in one pass.
- **B2 / B4 deferred** — CLIP_CONCURRENCY tuning requires iterative
  TDR-watch on real hardware; shell-thumbnail-for-CLIP fast path is
  redundant given the decoder pool. Both documented in NEXT.md V16.0.

### WP3 — Thumbnails (100% placeholder → real bitmaps)

- **`Services/ThumbnailService.cs`** — added `[THUMB] REQUEST / L1_HIT /
  L1_MISS / L2_HIT / L2_MISS / SHELL_OK / SHELL_NULL / SHELL_EX /
  IMG_FB_OK / IMG_FB_NULL / IMG_FB_EX / BITMAP_SET / RENDER_FAILED`
  trace lines through every decision point. The chain from `RequestAsync`
  through the disk cache, shell provider, and image-fallback now leaves
  a forensic trail per file so silent failures name themselves.
- **`Views/Library/LibraryView.xaml`** — removed `Opacity="0"` from the
  card `<Image>` element. The shimmer overlay's `NullToVisibility`
  converter already provides the load indicator; the tile is now
  guaranteed visible the moment `Thumbnail` is bound. (Prior behavior
  relied on `ImageOpened` to spring opacity 0→1; if that handler
  failed to fire — e.g., for already-decoded BitmapImages arriving via
  the LRU — the tile stayed invisible against the Border background
  gradient, which is what the user saw as "100% placeholder gradient".)
- **`Views/Library/LibraryView.xaml.cs::OnTileImageOpened`** — replaced
  the spring 0→1 with a direct `visual.Opacity = 1f` after stopping any
  prior animation. Preserves the macOS-parity fade (still triggered by
  `AnimateTileEntry` on `OnRepeaterElementPrepared`) while removing
  the flicker-then-fade pattern for cached bitmaps. Added `[THUMB]
  IMAGE_OPENED / OPACITY_SET` trace + a `[THUMB] TILE_THUMBNAIL_ASSIGNED`
  trace in `LoadThumbAsync` after the dispatcher writes `tile.Thumbnail`.

### WP2 — Scene classifier + enriched extras

- **`models/classifier.rs` (new, ~250 LOC)** — `ClassifierSession` mirrors
  `mobileclip.rs` shape: `load(model_path, labels_path)` resolves
  EP chain via the existing `RuntimeProbe`, warmup, ImageNet mean/std
  normalize, NCHW 1×3×224×224 input. `classify_batch(images, top_k,
  threshold)` returns top-K labels per image with confidences (matches
  macOS Vision behaviour: top_k=8 default, threshold=0.30). Label parser
  accepts both plain one-per-line and ImageNet synset (`n01440764 tench,
  Tinca tinca`) formats. 4 unit tests cover softmax + label parsing.
- **`models/registry.rs`** — new `"classifier_mobilenetv3"` slot with
  TODO(verify) URL + TODO(sha256) for both the ONNX export and the
  ImageNet label file. URLs are plausible (onnx-community mirror) but
  unverified; SHA256 left None pending a verified first download.
  NEXT.md V16.0 tracks the pinning work.
- **`pipeline/tagging.rs`** — `ModelStack` gained `classifier: Option<Vec<Mutex<ClassifierSession>>>`,
  loaded as a small pool same shape as ArcFace/SCRFD/MobileCLIP. New
  `CLASSIFIER_CONCURRENCY = 2` semaphore + `CLASSIFIER_TOP_K = 8` +
  `CLASSIFIER_THRESHOLD = 0.30` constants. `process_file_predecoded`
  runs the classifier after CLIP (reusing the decoded RGB, resized to
  224×224) and pushes top-K labels into `tagged.tags`. Missing model →
  one-time `[CLASSIFIER] model_not_installed` log, pipeline continues
  with enriched-extras only.
- **Enriched extras (`push_enriched_extras`)** — derives `Year_YYYY`
  (from `modified_unix` via a tiny proleptic-Gregorian helper, no
  chrono call), camera family (iPhone / iPad / Canon / Nikon / Sony /
  Fuji / Leica / GoPro / Samsung / Pixel), `Has Faces` / `Has Text` /
  `Has Location`. Cheap (no inference, no I/O) and gives the Library
  a baseline of useful chips even when the classifier model isn't installed.
  Mirrors macOS `Tagging.swift::extraTags`.
- **`TaggedFile.tags: Vec<String>`** — new field flowing through to
  DBWriter. Deduped + truncated to 16 max per file before persist.
- **`pipeline/dbwriter.rs`** — `flush()` now also deletes the file's
  prior `source='auto'` tag rows and inserts the new ones using the
  same INSERT pattern as `bulk.rs::handle_apply_tags` (source `'auto'`
  vs the user's `'user'` — both coexist in the `tags` table per the
  composite PK `(file_id, tag, source)`).

### WP4 — UI tag chips on Library cards

- **`FileID.Theme/Controls/TagChip.xaml(.cs)` (new)** — small chip user
  control with one `Tag` DependencyProperty. Visual spec mirrors macOS
  LibraryView.swift:729-744 (`.caption2.weight(.medium)`, 11pt Segoe UI
  Medium, foreground+background at 80%/10% white opacity, 4 px corner
  radius, 5×2 padding, `TextTrimming=CharacterEllipsis`). Brushes
  cached as `static readonly` per CLAUDE.md line 91 (no per-binding
  brush allocation). `FormatTag(string)` static helper is the 1:1 port
  of the macOS formatter (`"animal_dog"` → `"Dog"`, `"Has Faces"` →
  `"Has Faces"`, `"iPhone-14"` → `"Iphone 14"`).
- **`Services/ReadStore.cs`** — `FileRow` record gained `Tags:
  IReadOnlyList<string>?` (nullable, defaults to null). `ReadRow`
  reads the optional 8th column if present (`FieldCount > 7` check
  keeps existing queries that project only 7 columns working unchanged).
  `RecentAsync` now projects the tags via a correlated subquery
  (`(SELECT GROUP_CONCAT(tag, '|') FROM tags WHERE file_id = files.id
  AND source = 'auto') AS auto_tags`).
- **`ViewModels/LibraryViewModel.cs`** — `FileTile` gained `Tags` +
  `HasTags` + `TopTwoTags` properties. `TopTwoTags` is pre-sliced to
  match macOS `prefix(2)` so the ItemsControl binding doesn't re-take
  the slice on every layout pass.
- **`Views/Library/LibraryView.xaml`** — chip row added below the
  filename in the card template. `ItemsControl` bound to `TopTwoTags`
  with the new `TagChip` as `ItemTemplate`. Collapses cleanly via
  `HasTags`+`BoolToVisibility` so the card height stays unchanged
  when a file has no tags.
- **`Tests/FileID.App.Tests/ViewModelBindingTests.cs`** — 8 new
  `TagChipFormatTests` covering the macOS-parity matrix.

### Build/test status (Windows engine)

- Rust engine: `cargo check --lib` clean (exit 0). New classifier
  module compiles. Decoder pool refactor builds without warnings.
  Existing tests preserved (`tagger_passes_discovered_through_to_tagged`
  still asserts a non-existent path produces a failed TaggedFile;
  decoder pool propagates the decode Err through `PreDecoded`).
- .NET app: `dotnet build FileID.Theme.csproj` clean. Full
  `FileID.App.csproj` + tests pending verification (in-flight).

### Verification still pending (user on real hardware)

1. Launch app, scan `C:\Users\adamm\Desktop\Test Data`. Sidebar
   `Tagged` counter should climb at ≥40 files/sec (vs the 0.04 baseline).
   `engine.jsonl` `[STATS]` line should show `clip_avg_batch_x10` near
   60-80 (batch CLIP averaging 6-8 per dispatch).
2. Library cards should render real thumbnails within ~2 s of becoming
   visible. After restart, the same tiles should hit L2 disk cache.
3. Tap a tagged card — 1-2 tag chips should appear below the filename.
   Without the classifier model installed: only enriched-extras chips
   (`Year_YYYY`, camera family, `Has Faces`, `Has Text`, `Has Location`).
4. Set `FILEID_PERF_TRACE=1` and check `engine.jsonl` for `[PERF]`
   lines to populate the BASELINE PERF REPORT table.
5. Drop a MobileNetV3 ONNX export + ImageNet label file at
   `%LOCALAPPDATA%\FileID\Models\classifier\{mobilenetv3_large.onnx,imagenet_classes.txt}`
   and rescan — chips should now include semantic labels like `"Dog"`,
   `"Beach"`, `"Document"`.

## 2026-05-18 — V15.9 discovery throughput + thumbnails + adaptive hardware

Three-part push in one commit. Engine + .NET both green: 121 Rust lib tests pass (was 99), `cargo clippy -D warnings` clean, `cargo build --release` 0 warnings 0 errors, `dotnet build` 0/0. New synthetic benchmark `tests/discovery_throughput.rs` clocks **23,191 files/sec** for the walk phase (vs. the user's observed **22 files/sec** before the fix — 1,054× faster; ≥11.5× the 2,000 files/sec acceptance target).

### Issue 1 — Discovery throughput (was 22 files/sec on NVMe; target ≥2,000)

- **`platforms/windows/src/engine/Cargo.toml`** — added `jwalk = "0.8"` (MIT, dep approved by user) + `Win32_System_Ioctl` and `Win32_System_IO` to the windows-rs feature list.
- **`pipeline/discovery.rs`** rewritten: jwalk parallel walk (rayon-backed) with thread count from `platform::walk_concurrency_for(root)` (NVMe → 16, SATA SSD → 8, HDD → 2, USB/net → 2). `process_read_dir` callback prunes noise directories (`node_modules`, `.git`, `target`, etc.) at the read_dir level — entire subtrees become invisible to the walk after a single per-directory name check. Channel cap raised 1,024 → 32,768. `count.fetch_add(1)` moved BEFORE `blocking_send` so the "Discovered N" counter reflects FS-walk progress even when the channel briefly fills (the V15.9 decouple invariant).
- **`pipeline/dbwriter.rs`** — per-row `SELECT id FROM files WHERE path_text = ?` eliminated via `INSERT … RETURNING id` (SQLite 3.35+, bundled is 3.46+; RETURNING fires on both insert and ON CONFLICT DO UPDATE paths). Statement count per batch: 2N → N. Batch size now memory-tier-adaptive (Low=64 / Balanced=250 / High=500), re-evaluated every 30 s by the dbwriter loop.
- **Tests** — 4 new in `pipeline::discovery::tests` (noise-dir recognition, case-insensitive, synthetic tree walk, count-before-send invariant) + 1 in `pipeline::dbwriter::tests` (`insert_returning_id_yields_same_id_on_conflict`). Synthetic benchmark `tests/discovery_throughput.rs` covers the 10K-file walk + a "consumer stalled" decouple test, both `#[ignore]`'d so normal `cargo test` is unaffected.

### Issue 2 — Thumbnails never render (V15.6 follow-up)

- **`Services/ThumbnailService.cs`** — `RenderAsync` restructured: disk cache → shell path (try/catch, log on throw but FALL THROUGH) → image-extension fallback (try/catch). Previously the outer `catch` returned null directly, bypassing the fallback for every shell-throwing JPEG. Now the fallback runs whether the shell returned null OR threw. Exception **type** is logged at every catch (was just `.Message`).
- **`Services/ThumbnailDiskCache.cs`** (new, ~200 LOC) — persistent on-disk LRU at `%LOCALAPPDATA%\FileID\thumbs.cache\v1\<2hex>\<rest>.bin`. SHA256(path|mtime) keying invalidates on file edit. 500 MB cap with oldest-LRU eviction down to 80 % headroom. Atomic temp+rename so concurrent reads never see partial files. Files >500 KB are NOT written (in-memory LRU still serves them).
- **`ThumbnailDiagnostics`** record extended with `DiskHits / DiskWrites / DiskSweeps / DiskBytes`. Settings → Diagnostics renders both the in-memory and disk-cache counters.
- **`App.xaml.cs`** — `ThumbnailDiskCache.Prime()` called once at startup so the diagnostics panel shows real cache-size numbers without waiting for the first sweep.

### Issue 3 — Adaptive hardware utilization (first pass + stubs)

- **`platform.rs`** — new primitives:
  - `CpuTopology { p_cores, e_cores, logical }` + `cpu_topology()` via `GetLogicalProcessorInformationEx(RelationProcessorCore)`. `EfficiencyClass == 0` ⇒ E-core; non-hybrid CPUs collapse into `p_cores`.
  - `default_worker_cap()` now uses `P + E + max(1, P/2)` clamped at logical cores and [2, 32] — macOS-parity formula. Replaces the old `physical * 1.7`.
  - `MemoryTier { Low, Balanced, High }` + `memory_tier()` from `GlobalMemoryStatusEx.ullAvailPhys` (<8 / 8–32 / >32 GB).
  - `StorageType` + `storage_type_for_path()` via `DeviceIoControl(IOCTL_STORAGE_QUERY_PROPERTY, StorageDeviceSeekPenaltyProperty)` + `GetDriveTypeW` short-circuit for removable/network/CD.
  - `walk_concurrency_for(path)` maps storage type to walk-thread count (the Issue 1 connection).
  - `PowerSource { Ac, Battery, Unknown }` + `power_status()` via `GetSystemPowerStatus`. Reports source + battery percent.
  - `available_memory_mb()` + `dbwriter_batch_size_for(tier)`.
  - 11 unit tests covering all of the above in `platform::adaptive_tests`.
- **`ipc/mod.rs`** — `HardwareInfo` extended with 11 optional fields (`pCores`, `eCores`, `logicalCpuCores`, `workerCap`, `ramTotalMB`, `ramAvailableMB`, `memoryTier`, `vramMB`, `npuPresent`, `powerSource`, `batteryPercent`, `activeProfile`). All `#[serde(default, skip_serializing_if = ...)]` so old C# builds talking to a new engine still deserialize cleanly, and vice versa.
- **`commands/hardware.rs`** — `build_hardware_info()` populates the new fields. First-pass NPU detection is Qualcomm-only (reuses the existing QNN probe); Intel AI Boost / AMD XDNA report `false` for now (NEXT.md entry tracks).
- **`FileID.IpcSchema/Dtos.cs`** — `HardwareInfo` record extended with matching defaults.
- **`Views/Settings/SettingsView.xaml` + `.cs`** — new Diagnostics card between Performance and Behavior. Shows CPU topology (P/E split if hybrid + logical threads + worker cap), Memory (avail/total + tier), GPU/NPU (vendor/adapter/VRAM/NPU presence), Power (source + battery + active profile), Thumbnails (in-mem + disk counters in monospace), and a disabled "Performance profile" ComboBox with Eco/Auto/Performance — wired to "auto" only; Eco/Performance are grayed with "(coming soon)" copy.
- **`shared/ipc-schema/ipc.schema.json`** — `EngineInfo._0` documents the `hardware` field and lists the V15.9-added properties.

### Build/test status

- Rust engine: 121/121 lib tests pass (was 99); `cargo clippy --lib -D warnings` clean; `cargo build --release` 0 warnings.
- Benchmark (release): `cargo test --release --test discovery_throughput -- --ignored` → 10K walk in **0.43 s = 23,191 files/sec**; decouple test green.
- .NET: `dotnet build FileID.sln -c Debug -p:Platform=x64` → 0 warnings, 0 errors.

### Verification still pending (user on real hardware)

1. Launch app, scan `C:\Users\adamm\Desktop\Test Data`. The "Discovered" counter should climb at NVMe walk speed (target ≥2,000/sec sustained) and reach the corpus total within ~5 s, independent of tagging progress.
2. Library view tiles should render actual image content (not the gradient placeholder) within 2 s of becoming visible. After app restart, the same library should hit the disk cache and render instantly.
3. Settings → Diagnostics should show the actual detected CPU (with P/E split if on Intel 12th-gen+), RAM, GPU, VRAM, NPU presence, storage type for the scan-root, active profile "auto", and live thumbnail cache counters.
4. On the user's RTX 2060 (no P/E split, NVMe Samsung 970/980-class): expect `cpu cores · logical threads · worker cap N`, `~XX GB available of 64 GB · tier: balanced` (or high), `nvidia · GeForce RTX 2060 · 6 GB VRAM`, `AC power · profile: auto`.

## 2026-05-17 (continuation 3) — V15.8d follow-up parity session

Comprehensive cleanup pass picking up items the previous session deferred.

### Completed
- **Comment surgery**: 0 V-version / Mirror-of-macOS comments remain across `platforms/windows/src/` (387+54 removed). Engine + app build clean; clippy `-D warnings` clean; `dotnet build` 0/0; `dotnet format --verify-no-changes` clean.
- **PDF rasterization**: implemented under `pdf-analyze` Cargo feature with `pdfium-render = "0.8"`; `analyze_file()` wired for PDF kind; tests cover both feature-on and feature-off paths. DECISIONS.md entry written.
- **C# ViewModel tests**: 26 new test cases in `ViewModelBindingTests.cs` covering `ModelSlot.Apply` state transitions, `PersonCluster.BuildCropPath` (new static helper), `ScanProgress`/`DeepAnalyzeProgress`/`HardwareInfo` DTO surface, and `WelcomeSheetModelSizeTests` parameterized across 7 model_kinds via new `ModelDisplaySize.GetDisplaySizeMB`. Total C# tests: 62 (was 36).
- **SCRFD decode**: extracted `decode_scrfd_stride()` + `decode_scrfd_single_anchor()` as pure functions; added 3 regression tests + 1 proptest exercising bbox bounds across randomized inputs. End-to-end DB verification against a face photo was not possible because the user's Pictures folder contains only screenshots — the pure-function tests are the proper invariant gate.
- **VRAM calibration on RTX 2060**: measured ~940 MB peak engine attribution above 1.65 GB idle baseline during a scan. Kept `VRAM_PER_POOL_INSTANCE_MB = 1500` (preserves ~560 MB margin against DirectML fragmentation); comment in `tagging.rs` updated with measurement and method.
- **publish-bundle.ps1**: installed pwsh 7.6.1 via winget; fixed 3 WiX 4 wixproj issues (DebugType=portable rejected by wix.exe; ItemGroup DefineConstants form silently no-op'd in WiX 4; `<bal:Condition>` body syntax replaced with `Condition` attribute). Engine + app published; FileID-x64.msi built; privacy gate scan finds 0 hits across 513 binaries.
- **Proptests**: `scrfd_decoded_bbox_within_image_bounds` added; existing dbwriter `embedding_le_bytes_round_trip` (bit-pattern-strict, broader than spec) and hmac `appending_byte_to_msg/key_changes_mac` already satisfy G2 + G3.

### Remaining gaps after this session
- **Bundle build** (`FileIDSetup.exe`) still fails on (a) `WixStdbaLicenseUrl` theme variable not declared, and (b) Bundle.wxs hardcodes both x64 and ARM64 MSIs so `-SkipArm64` chokes on missing ARM64 MSI payload. These are bundle-only — the per-arch MSI + binary publish succeed and the privacy gate against the published binaries is clean.
- **End-to-end face-detection DB verification** still needs a face photo in the scan corpus (the user's Pictures folder has only screenshots). The pure-function decode tests cover the invariants; a single face photo dropped into Pictures and re-scanned would close the loop in seconds.

## 2026-05-17 (continuation 2) — V15.8c smoke script + UNC + cluster invariants + spring map + SQL comment

Third pass picking up smaller-scope items. Cumulative: 99/99 Rust lib tests pass (was 82 at start of day), 66/66 C# tests pass, all gates green.

**Engine smoke test script (Section 10d, NEW):**
- `platforms/windows/build/engine-smoke.ps1` — spawns FileIDEngine.exe, asserts the ready event has the schema-required fields (version, pid, workerCap, physicalMemoryGB), sends shutdown, asserts clean exit. Works in Windows PowerShell 5.1 (no pwsh dep).
- Verified end-to-end on this box: NVIDIA RTX 2060 detected, 10 worker cores, 63.9 GB RAM, DirectML EP, clean exit 0.

**UNC path containment (Section 8c, NEW):**
- 2 new tests in `util/path_safety::tests` for the SEC-7 restore-from-trash check: nested UNC path matches authorized UNC root; cross-server UNC paths don't collide (different `\\srv\share` prefixes).

**identity_clustering invariants (Section 10b, NEW):**
- 2 new tests: all-identical embeddings collapse to one cluster; 5 orthogonal unit vectors produce 5 distinct cluster IDs.

**Pool load serialization (Section 7d, verified-as-correct):**
- `ModelStack::load_default` calls `load_pool` 3 times sequentially (no spawn/rayon between them). Each `load_pool` loads slots sequentially with 250ms intra-pool stagger. TDR detection during warmup aborts the whole pool. No fix needed.

**Sankey diagram parity (Section 5d, verified-as-correct):**
- `SankeyFlowControl.cs` is a 1:1 mirror of macOS `SankeyFlowView`: source-folder column → category-ribbon column, gold for sources, lavender/cyan/pink rotation for categories, bezier per (source, category) pair, hover highlights. The directive's "Anchor=gold/Mixed=lavender/Junk=gray" coloring is a separate feature (RestructureRecommendationRow), not Sankey itself.

**ARM64 worker priority (Section 7f, verified-as-correct):**
- `set_worker_background_priority` uses `Win32::System::Threading::SetThreadPriority` + `THREAD_PRIORITY_LOWEST`. API surface is identical across x64 and ARM64 in windows-rs 0.58.

**SQL case parity (Section 6a/6b, comment fix):**
- The comment in `db/migrations.rs` claimed "GRDB lowercases column types" — actually wrong. GRDB's `Database.ColumnType.text` returns "TEXT" (uppercase) and the DSL emits it verbatim. Rust SQL uses UPPERCASE which matches. Comment rewritten to reflect reality; no SQL changes (would have broken parity in the wrong direction).

**SwiftUI ↔ WinUI spring mapping (Section 9b, documented):**
- DECISIONS.md entry written. The mapping is direct: `response (s)` ↔ `Period.TotalSeconds`, `dampingFraction` ↔ `DampingRatio`. Canonical FileID values already in `Theme.xaml` as `SpringResponseStandard` / `SpringDampingStandard` / `SpringResponseTight` / `SpringDampingTight`.

**publish-bundle dry run (Section 11f, deferred):**
- Requires PowerShell 7 (`$PSNativeCommandUseErrorActionPreference`) + WiX SDK. Neither available in this session. The lighter `engine-smoke.ps1` (new in this round) covers the engine-binary smoke path; full release-cutting is a separate documented workflow.

## 2026-05-17 (continuation) — V15.8b SCRFD + SEC-3 + TDR coverage + EP tests + color tokens

Continuation of V15.8 picking up items previously deferred. All gates remain green: 95 Rust lib tests pass (was 82 earlier in the day), 66 C# tests pass (30 IpcSchema + 36 App), `cargo clippy -D warnings` clean, `cargo deny check` clean, `dotnet build` 0 warnings 0 errors, `dotnet format --verify-no-changes` clean.

**SCRFD `detect()` shipped:**
- Wrote the full post-processing against the Buffalo_L SCRFD-10g (insightface) reference: distance-encoded bbox decode + 5 landmarks per face + NMS @ IoU 0.4 across strides 8/16/32 with 2 anchors per spatial location. Defensive parsing: wrong-variant ONNX silently degrades to empty result rather than producing nonsense scores that would poison the People tab's cluster IDs.
- 6 new unit tests (5 for nms/iou helpers, 1 for pose estimation).
- Golden-set validation (4 known images: clear face / small / multi / no-face) is the next-session work item.

**LavaLamp Composition status corrected:**
- Audit found the V14.6 rewrite already moved off Win2D's `CanvasAnimatedControl` to `Microsoft.UI.Composition` (3 SpriteVisuals + ExpressionAnimation-driven Offset + CompositionRadialGradientBrush falloff). My earlier deferral entry in DECISIONS.md was wrong; superseded by a new "already shipped" entry.

**Security tightening:**
- SEC-3 SetDefaultDllDirectories hoisted to the very first statement in `fn main`, before tokio runtime construction and before logging::init. Closes the gap an audit would flag.
- Found 1 of 5 `session.run` sites (`models/clip_text.rs:69`) missing the `classify_inference_error` wrap. Added it + the import. All 5 now uniformly guarded → TDR detection coverage is 100% across the models tree.
- Added GPU-dead short-circuit at the top of `pipeline/tagging::process_file`. Once `coord.mark_gpu_dead()` fires, remaining queued files return immediately with `failed=false` instead of hanging on doomed inference calls.

**EP chain test scaffolding:**
- 7 new tests across `models::runtime` mocking each vendor; the expected `priority_chain` ordering is now documented as a regression guard. Includes a global invariant: every vendor's chain terminates at CPU and (if vendor != None) includes DirectML.

**C# warning sweep:**
- Build was already at 0 warnings before this turn. `dotnet format --verify-no-changes` initially flagged CRLF + 2 IDE0003 violations in the V15.7-modified `FilePreviewSheet.xaml.cs`; `dotnet format` auto-fixed them.
- 1 test regression caught: `AppSettingsTests.NewInstance_HasDocumentedDefaults` was asserting `CleanupAutoTagKept == false` but V15.5b's macOS-parity work flipped the default to `true`. Updated the assertion + comment.

**Color token audit:**
- 5 bare `#FFCC00` literals in `SettingsView.xaml`, `SidebarEngineStatus.xaml`, `SidebarProcessingControl.xaml` replaced with `{StaticResource GoldBrush}` and `{StaticResource GoldSelectedBackgroundBrush}`. Brand-color drift detection is now centralized in `Theme.xaml`.
- Alpha-variant gold tokens (e.g. `#33FFCC00`) kept inline where the alpha differs from the existing `GoldSelectedBackgroundBrush` (18%) or `GoldSelectedStrokeBrush` (55%) — adding new tokens for each one-off alpha would be over-engineering.

**WAL checkpoint guard:**
- Added `debug_assert!(conn.is_autocommit())` before the periodic `PRAGMA wal_checkpoint(PASSIVE)` in `dbwriter::flush`. Catches a future regression where someone adds a `BEGIN` before the checkpoint block.

**Engine respawn backoff (Section 5g) verified already-shipped:**
- 1s/4s/16s exponential backoff in `EngineClient.OnProcessExited` ✓
- 3-strike-in-60-seconds cap ✓
- `CrashReason` bound to `SidebarEngineStatus.xaml.cs:61-62` for the permanent error banner ✓

**Privacy gate verified:**
- Source-level telemetry scan: 0 real hits (only false positives — "low-amplitude noise" in a bench comment, "segmented" in UI control identifiers, "PROCESSENTRY32W" Win32 type).
- URL allowlist scan: 6 unique hosts, all on the documented allowlist (huggingface.co, github.com, developer.{download.,}nvidia.com, schemas.{microsoft,openxmlformats}.org).

**Verification still pending (user hardware):**

1. SCRFD on a face-heavy library — verify cluster IDs look right after a rescan. If wrong, run `det_10g.onnx` through Netron and adjust the decode index math.
2. Forced TDR test — kill the GPU driver mid-scan, verify the engine doesn't continue spamming inference calls (the new `is_gpu_dead` short-circuit should make this fast).
3. All previously-pending V15.7 verifications still apply (sidebar memory/total/ETA/failures rendering).

## 2026-05-17 — V15.8 IPC schema parity + security hardening + dead-code prune

Single-session pass focused on the Windows port's IPC contract correctness, security posture, and dead-code cleanup. No new features — every change is either a contract correction, a hardening, or a deletion. Build remained green throughout (`cargo check` + `cargo clippy -D warnings` + `cargo deny check` all pass; lib tests 74 → 82).

**IPC schema (Section 4 of the spec audit):**

- `shared/ipc-schema/ipc.schema.json` was missing 5 event variants the Rust engine emits and the C# app consumes: `restructurePlan`, `restructureApplyResult`, `bulkActionResult`, `clipTextEmbedding`, `mergeSuggestions`. Added all 5 with the correct `SinglePositional/_0` wrapping that matches Swift Codable's auto-synthesized shape. macOS Swift IPC doesn't have these because macOS uses synchronous returns; documented as a legitimate cross-platform divergence (DECISIONS.md 2026-05-17).
- Also added `startScan.rescan: bool` to the schema (Rust had it, schema didn't).
- Verified all 27 `CommandPayload` variants have explicit handler arms in `main.rs::handle_line` — no `_ =>` wildcard arm, so Rust's exhaustiveness check guarantees no silent drops.

**Security (Section 8 of the spec audit):**

- `is_safe_filename` now rejects `COM0` and `LPT0` (Microsoft's docs list both as reserved). New proptest `reserved_device_names_are_rejected` covers all casings + extensions.
- `util/zip::extract_into_parent` now caps a single entry at 1 GiB (half the cumulative 2 GiB cap), so a single bomb entry can't exhaust the whole budget.
- `commands/trash_log.rs::read_batch` no longer accepts entries without an HMAC suffix. The pre-V14.7.2 grace window expired months ago — see DECISIONS.md 2026-05-17.
- `pipeline/restructure_apply.rs::apply` now reparse-point-checks the destination's ancestor chain BOTH before and after `create_dir_all` (SEC-5 defense in depth).

**DB correctness (Section 6 of the spec audit):**

- Incremental-rescan skip query in `scan_session.rs` now filters `failed = 0` so previously-failed files retry automatically. Documented that `modified_at IS NULL` rows fall out via SQL three-value logic.
- FTS5 round-trip test in `db::migrations` strengthened: asserts `rowid == files.id` and that a known-absent word returns zero hits (was just `COUNT(*) == 1`).
- New embedding byte-order proptest in `dbwriter` verifies `floats_to_le_bytes` → `f32::from_le_bytes` is byte-for-byte lossless, including NaN bit patterns. Guards against a future `to_ne_bytes` regression silently corrupting embeddings when DBs move between architectures.

**Test coverage (Section 10 of the spec audit):**

- HMAC proptests: appending any byte to msg / any non-zero byte to key changes the MAC. (Pure zero byte appended to a short key is correctly a no-op per RFC 2104's zero-padding rule — caught by the first proptest run and the invariant was tightened.)
- PathRedactor: UNC path keeps only last 2 components, drive root collapses to `…`, app structural paths pass through unchanged. The redaction function itself was fixed in the process — was leaking the drive letter for `C:\`.

**Dead-code prune (Section 1 of the spec audit, scoped):**

- `shell/sleep.rs` deleted (duplicate of `platform::SleepGuard`; only platform.rs's was ever called).
- `Discovery::new` + `pipeline/discovery.rs::enumerate` deleted (orphan convenience wrappers).
- `db::open_reader` deleted (C# opens its own SQLite connection; no Rust caller).
- 6 cargo clippy warnings fixed (deny.toml lint rename, orphan doc comment, redundant `.into_iter()`, two manual checked divisions, sort_by → sort_by_key).
- 40+ `#[allow(dead_code)]` attrs intentionally LEFT in place — they're either the documented Linux Phase 0 cross-platform stubs (V15.5b) or items genuinely used by lib (tests/benches) but not by bin. A deeper per-item audit was out of scope.

**Deferred to a hardware-equipped session (DECISIONS.md 2026-05-17 entries):**

1. **SCRFD `detect()` implementation.** Needs the actual `det_10g.onnx` loaded + a 4-image golden set. Speculative decode math against the wrong export variant would silently corrupt cluster IDs across the entire People tab.
2. **LavaLampBackground Composition API migration.** Needs render verification on Windows 11 26200+ to confirm it avoids the `DXGI_ERROR_DEVICE_HUNG` that wedged the Win2D `CanvasAnimatedControl` version.
3. **Multi-vendor GPU EP chain validation.** Needs physical NVIDIA / AMD / Intel / Snapdragon boxes. Unit-test coverage (mocked `pack_present`) stays; live-fire deferred.

**Build/test:** 82/82 Rust lib tests pass (was 74); `cargo check` clean; `cargo clippy -- -D warnings` clean; `cargo deny check` clean. `.NET` build not re-run in this session — none of the C# files were modified.

**Verification still pending (user runs on Windows hardware):**

- Confirm V15.7 sidebar stats parity (memory / total / ETA / failures) renders correctly — this verification was already pending from V15.7 and is not affected by V15.8.
- Trash + restore round-trip: trash a few files, restart the app, verify `restoreFromTrash` still works (no entries should be rejected; the HMAC tightening only blocks pre-V14.7.2 entries, of which there should be none after months of organic rotation).
- Restructure apply on a path with a deliberately-planted directory junction inside `library_root` should now reject the move (defense-in-depth check).

## 2026-05-16 (late night) — V15.7 sidebar stats parity with macOS (memory/total/eta/failures)

User asked for the sidebar stats to be 1:1 with macOS. Phase 1 dual-Explore parity audit against `platforms/apple/Engine/ScanCoordinator.swift:174-186` and `DBWriter.swift:268` found four engine-side regressions where Windows hardcoded zeros instead of measuring real values:

| Stat | Was | Now |
|---|---|---|
| **Memory** | hardcoded `resident_mb: 0` at `scan_session.rs:249/418/460` — sidebar always showed "0 MB" mid-scan | new `platform::process_memory_mb()` using `Win32::System::ProcessStatus::GetProcessMemoryInfo` (WorkingSetSize). macOS uses `task_info MACH_TASK_BASIC_INFO`; Windows now matches. Linux/POSIX path reads `/proc/self/status` VmRSS. |
| **Total during Tagging** | `total: stats.processed_total` at line 454 — progress bar always at 100% during tagging because total == processed | now `discovered_count.load(Relaxed).max(processed_total)` — persists the discovery total into Tagging Progress events so the progress bar fills as files are processed against the real total |
| **ETA** | hardcoded `eta_seconds: None` — sidebar stuck on "computing…" forever | computed as `(total - processed) / files_per_second` once both signals are available (≥0.5 fps + remaining > 0); None during ramp-up (matches macOS gating behavior) |
| **Failures** | hardcoded `failed: 0` — sidebar always showed "0 failures" even on a scan with corrupt files | added `failed_total` field to `BatchStats`, populated from DBWriter's existing `failed` counter at `dbwriter.rs:120`; plumbed through `maybe_emit_progress` to the Progress event's `failed` field |

**Files touched:**

- `engine/src/platform.rs` — new `process_memory_mb()` (cross-platform: Win32 GetProcessMemoryInfo on Windows, /proc/self/status VmRSS on Linux, 0 stub elsewhere).
- `engine/Cargo.toml` — added `Win32_System_ProcessStatus` to the windows-rs features.
- `engine/src/pipeline/dbwriter.rs` — added `failed_total: u64` to `BatchStats`; populated in `flush()` from the existing `*failed` counter.
- `engine/src/scan_session.rs`:
  - Discovery ticker Progress now includes `resident_mb: process_memory_mb()`.
  - `emit_batch_summary` populates `resident_mb` from `process_memory_mb()`.
  - `maybe_emit_progress` signature gained `discovered_total: u64` parameter; body computes real `total`, `eta_seconds`, `failed`, `resident_mb`.
  - Call site clones `discovered_count` once for the ticker, once for the tagging callback; passes the Tagging callback's load result into `maybe_emit_progress`.

**Build/test:** Rust engine 74/74 tests pass on release Windows target. .NET app builds clean (0 warn / 0 err).

**Verification still pending (user runs on Windows hardware):**

1. Launch the rebuilt engine + app, scan `Test Data/` (or any folder ≥100 files).
2. **Memory:** sidebar should show non-zero RSS during scan (typically 600 MB-1.2 GB once ML models are loaded). Should rise as inference warms up, plateau during tagging.
3. **Progress bar:** during tagging, bar should fill from left to right, not show 100% the whole time. Discovered count and Tagged count should differ during tagging (Discovered = final total after discovery completes; Tagged climbs toward it).
4. **ETA:** should switch from "computing…" to a real number after a few seconds (≥0.5 files/sec + non-zero remaining). Should count down as scan progresses.
5. **Failures:** if a file errors during tagging, the counter should increment in real time (was previously stuck at 0 until ScanComplete event).

**Unaddressed in this turn (user redirected mid-Settings work):**

- Settings page install-state detection — buttons for "CUDA llama.cpp for Deep Analyze" and "cuDNN for scanning" don't check the engine sentinel files at page load, so they show "Install" even when already installed. Sentinels exist at `%LOCALAPPDATA%\FileID\Models\.sentinels\{llama_runtime_cuda_x64,cudnn_runtime_x64}.installed`. Same pattern as `ModelInstallerService.SentinelInstalled` at line 755-762. Add a sync method called from the SettingsView `Loaded` handler (line 48). Captured for next turn.

## 2026-05-16 (night) — V15.6 thumbnail decode fix + CompletionRipple removal

User screenshot showed three issues after the V15.5 round shipped:

1. **Thumbnails still blank.** Every tile in a 549-.jpg `Test Data/` scan stayed on the loading shimmer (yellow→lavender gradient at `FileID.Theme/Motion/ShimmerView.xaml:37-42`, bound visible-when-Thumbnail-null via `LibraryView.xaml:235-236`).
2. **Sidebar visually unstable** during scan (Processing panel area).
3. **Yellow ring pulse on "Tagged N" stat** — `CompletionRipple`. macOS doesn't have it.

Root causes (Phase 1 dual-Explore agents):

- **Thumbnail:** V15.5 `RenderImageFallbackOnDispatcherAsync` at `Services/ThumbnailService.cs:282-310` used `BitmapImage { UriSource = uri }` — a **lazy** decode that runs when the BitmapImage is first put on a UI element. Combined with `LibraryView.xaml:241-246` `<Image Opacity="0" ImageOpened="OnTileImageOpened" />` (image only fades in after `ImageOpened` fires), if the lazy decode silently failed `ImageOpened` never fired → Image stayed invisible → shimmer kept showing. UriSource for file:/// URIs in WinUI 3 is reliable in the common case but fails silently for mid-scan files (file lock contention, path encoding edge cases, decode errors with no `ImageFailed` handler).
- **Yellow ring + sidebar instability:** `SidebarProcessingControl.xaml.cs:71-81` fired `CompletionRipple.SetTrigger(TaggedStatBorder, batch)` on every `LastBatch` event (≈5 Hz during scan). The ripple Storyboard at `Theme/Motion/CompletionRipple.cs:136-157` ran 0.9 s without canceling prior animations → 4-5 overlapping rings + a fresh Popup + Ellipse + Storyboard per trigger. Compositor churn was the dominant contributor to the "spazing" feel.

**Fixes:**

1. **`Services/ThumbnailService.cs:282-310`** — rewrote `RenderImageFallbackOnDispatcherAsync` to open the file as a stream and **eager-decode** via `await bmp.SetSourceAsync(stream)` on the UI dispatcher. Mirrors the working shell-path `RunSetSource` at lines 260-280. Extracted a new `RunFallbackSetSource` helper so the lambda passed to `TryEnqueueWithRetry` stays sync-shaped (matches `DispatcherQueueHandler`). On any open/decode failure: log via `DebugLog.Warn`, return null, and the existing `_renderedFailed` counter bumps via the wrapper. Stream lifetime is bounded by the lambda — `Dispose()` in `finally`.
2. **`Views/Sidebar/SidebarProcessingControl.xaml.cs:71-81`** — deleted the entire `if (e.PropertyName == nameof(EngineClient.LastBatch))` block + comment. Replaced with a V15.6 comment explaining why. `CompletionRipple` class and `TaggedStatBorder` XAML element kept (harmless when no trigger references them).

**Build/test:** `dotnet build FileID.sln -c Debug -p:Platform=x64` clean (0 warn, 0 err). `dotnet test FileID.App.Tests` 36/36 pass.

**Verification still pending (user runs on Windows):**

1. Wipe state + thumb cache, point at `C:\Users\adamm\Desktop\Test Data`, click Start Scan.
2. **Thumbnails:** within ~5 s of files entering the Library, tile shimmer should be replaced by actual JPEG content. Settings diagnostics line should show `Thumbnails: N ok / N failed / N dropped / N fallback` — `fallback` count > 0 (shell cache cold) and `ok` close to visible tile count.
3. **Sidebar:** "Tagged N" stat should not show concentric gold rings expanding outward. Whole Processing panel should look stable.
4. `pwsh build/gui-regression.ps1 -Corpus C:\Users\adamm\Desktop\Test Data -TimeoutMinutes 10` — expected `[PASS]`.

**Open observation:** if user STILL sees the shimmer on every tile after this build, the failure has moved out of the fallback path entirely — either the shell call is throwing before reaching the fallback, the worker channel is dropping requests, or `LoadThumbAsync` isn't being invoked. The `Stats` counters will name the bucket (`renderedFailed` rising with `fallback` near 0 = shell throw; `droppedDispatcher` rising = enqueue race; all near 0 = load not invoked).

## 2026-05-16 (late evening) — V15.5b cross-platform parity sweep + Linux platform scaffold

Following the V15.5 crash/harness work, a parity audit identified 7 user-visible divergences between Windows and macOS. macOS confirmed as canonical. Six were addressed (the seventh, model-load timeout, was deferred — wasn't in the user's explicit fix list). Plus the engine was made portable for the Linux platform, and the Linux GTK4 + libadwaita scaffold was created.

**Parity fixes (Windows-side, macOS canonical):**

- **D1 face crop padding** — `tagging.rs:112` `FACE_CROP_PAD: f32 = 0.25` → `0.15` (matches macOS `FaceClustering.swift:988`). Closes the cross-platform ArcFace embedding drift; same library now produces same cluster IDs across platforms. Re-run `iterate.ps1` to flush prior cluster IDs.
- **D2 file size cap** — `discovery.rs:31` removed the 500 MB `MAX_FILE_BYTES` const; the `size > MAX_FILE_BYTES` check is gone. Zero-byte skip kept. Large videos / disk images now scan on Windows like they do on macOS.
- **D4 thumbnail request size** — `ThumbnailService.cs:135` `ThumbnailRequestPx: 256 → 192` to match macOS `ThumbnailService.swift:27` `size: 192`. Same display target, ~44% less memory per cached tile.
- **D5 Library tile sizing** — `LibraryView.xaml:159-160` `UniformGridLayout MinItemWidth: 256 → 160` and `MinItemHeight: 256 → 160` to match macOS `.adaptive(minimum: 160, maximum: 220)`. WinUI's `ItemsStretch="Fill"` produces the same "grow until a new column fits" behavior.
- **D7 CleanupAutoTagKept default** — `AppSettings.cs:48` `false → true` to match the macOS default. Same user no longer sees different post-cleanup behavior on different OSes.
- **D6 tile hover scale** — `LibraryView.xaml` tile template gained `PointerEntered`/`PointerExited` events; `LibraryView.xaml.cs` got an `ApplyTileScale` helper that uses existing `FileID.Theme.Motion.SpringEasing.AnimateScalar` to spring `Scale.X`/`Scale.Y` to 1.012 (response 0.18s, damping 0.8) on enter / 1.0 on exit. CenterPoint set on each event using the tile's current ActualWidth/Height. Mirrors macOS `LibraryView.swift:681-682`.

**Engine portability (so Linux can reuse it):**

- `Cargo.toml` — moved `ort` from `[dependencies]` to `[target.'cfg(windows)'.dependencies]` (with `directml`, `cuda`, `openvino`, `qnn` features) + new `[target.'cfg(not(windows))'.dependencies]` block with `ort` (CPU + CUDA + OpenVINO, no DirectML) and `libc` (for `platform.rs`'s POSIX `getppid()`). `windows`/`windows-core` already correctly gated.
- `src/paths.rs::root()` — split into `#[cfg(windows)]` (LOCALAPPDATA/USERPROFILE) and `#[cfg(not(windows))]` (XDG_DATA_HOME → ~/.local/share/FileID). Layout helpers (db_path, logs_dir, etc.) unchanged.
- `src/shell/mod.rs` — Win32 submodules (`reveal`, `tags`, `thumbnail`, `trash`, `ocr`, `video`) cfg-gated to Windows. Non-Windows targets get inline stub modules with matching public surface (`Result::Err("…not implemented on this platform")` or `vec![false; n]` for trash). `sleep` was already cross-platform. Call sites (`commands/bulk.rs`, `pipeline/tagging.rs::try_shell_thumbnail`, `pipeline/deep_analyze.rs`) compile unchanged.
- Verified: `cargo check --target x86_64-unknown-linux-gnu` — all FileID code compiles cleanly. Only failure is transitive `ring` v0.17 (rustls native-crypto dep) needing `x86_64-linux-gnu-gcc`, an environment prereq, not a code issue. Resolution paths: build on real Linux/WSL, use `cargo-zigbuild`, or switch rustls to `aws-lc-rs` backend.
- Windows side: 74/74 cargo tests pass + 36/36 .NET tests pass after the Cargo.toml restructure. No regression.

**Linux platform — Phase 0 scaffold:**

- New `platforms/linux/` matching the macOS/Windows directory shape.
- `platforms/linux/CLAUDE.md` — full platform conventions, toolkit rationale (GTK4 + libadwaita chosen over Qt/Iced/egui/Tauri — GNOME-native, mature gtk4-rs bindings, satisfies "no web tech" + "native primitives"), shell-module TODO table.
- `platforms/linux/src/app/` Cargo project — `gtk4-rs` + `libadwaita` + reuses the shared engine via `fileid-engine = { path = "../../../windows/src/engine" }`. Workspace at `platforms/linux/Cargo.toml`.
- `src/app/src/main.rs` — `adw::Application` bootstrap, brand CSS provider (gold #FFCC00, lavender #B19BCE, cyan #A0E2EA, pink #F2A6C0), forced dark mode via `adw::StyleManager`.
- `src/app/src/window.rs` — `adw::ApplicationWindow` + `adw::HeaderBar` + folder picker (`gtk::FileDialog::select_folder`) + start-scan button. Engine status pumped from `async_channel::Receiver` into the GTK main context. Placeholder `adw::StatusPage` where Phase 1 lands the six tabs (Library/People/Cleanup/DeepAnalyze/Restructure/Settings).
- `src/app/src/engine_client.rs` — minimal stdio JSON client. Spawns the engine subprocess, reader thread parses NDJSON for `ready`/`scanComplete`/`error`, sends `startScan` on user click. Phase 1 replaces with full `IpcCommand`/`IpcEvent` routing.
- `data/io.github.fileid.FileID.desktop` — XDG desktop entry.
- `build/build.sh` — bash equivalent of `build.ps1`. Builds engine + app, stages into `dist/fileid/` with the engine next to the app exe so `locate_engine_binary` finds it.
- `README.md` — quickstart + status table.

**What this turn does NOT ship:**

- Linux app actually built on real Linux (no Linux environment in this session). Rust code compiles for Linux per cargo check; the GTK/libadwaita link step + system-libs would need a real Linux host.
- Real Linux implementations of `shell/` ops — `trash`, `thumbnail`, `ocr`, `video`, `reveal`, `tags` all still return Err on non-Windows. Implementations are sized in the Linux CLAUDE.md (~17 days total).
- Move of the engine crate from `platforms/windows/src/engine/` to `shared/engine/`. The Cargo path dep works today but the structural move is the proper home; tracked as a NEXT.md follow-up.
- Linux CI workflow. Add later under `.github/workflows/linux-engine.yml` + `linux-app.yml`.
- D3 (model-load timeout) — wasn't in the user's explicit fix list; staying at 30 s.

**Verification still pending (user runs on Linux hardware):**

1. `cd platforms/linux && ./build/build.sh` on Ubuntu 24.04 or Fedora 40 (must have `libgtk-4-dev` + `libadwaita-1-dev`).
2. Expected: clean Cargo build of engine + app; `dist/fileid/fileid-linux` launches showing dark Adwaita window with "FileID for Linux" StatusPage.
3. Pick a folder, click Start Scan; engine state label flips through `spawning → ready → scanning → done`.

## 2026-05-16 (evening) — V15.5 Windows scan-crash fixes + GUI regression harness + thumbnail visibility

User reported the Windows app "keeps crashing when scanning" with thumbnails that "don't show anything like how the macOS version does," and called out a testing gap: "the testing and safety harnesses for this app must not be working or something."

V15.4 had landed `[APPLY:N]` / `[ENGINE-SUB:*]` tracing + two Pattern B fixes (SidebarQueueList, SidebarPipelineProgress) and concluded that the remaining `new BitmapImage(` sites were "confirmed safe (await propagates via DispatcherQueueSynchronizationContext)." That conclusion held in theory but evidently doesn't hold under burst-load conditions — per CLAUDE.md, the convention is to *treat UI-thread affinity as untrusted* even when it nominally holds. V15.5 tightens to that discipline and adds the GUI-driven harness that should have caught the class of bug before user discovery.

**Phase 1 — four Pattern B sites patched defensively:**
- `Views/DeepAnalyze/DeepAnalyzeView.xaml.cs::LoadStreamThumbAsync` — `new BitmapImage()` + `StreamImage.Source = bmp` now both run inside a `this.DispatcherQueue.TryEnqueue` lambda, after the `GetThumbnailAsync` await resumes on whatever thread.
- `Views/Library/FilePreviewSheet.xaml.cs::LoadShellThumbnailAsync` — same wrap. User's heavy local rework of this file preserved; only the leaf thumbnail-load was replaced.
- `Views/Restructure/DrillDownSheet.xaml.cs::LoadThumbAsync` — static method; captures `img.DispatcherQueue` before await.
- `Views/Sidebar/SidebarProcessingControl.xaml.cs::Sync` — was allocating four fresh `SolidColorBrush` per progress event (10 Hz during scan = 40 DispatcherObject allocations/sec). Now uses ctor-cached `_memoryWarnBrush` + `_statDefaultBrush` instance fields (same pattern V15.4 SidebarPipelineProgress adopted). Reuses existing static `FailedTextBrush` for the failures-alert color (same #FFFF6B6B).

**Phase 2 — `Services/ThumbnailService.cs` made silent-failures observable:**
- New `public static ThumbnailDiagnostics Stats` record exposing `RenderedOk` / `RenderedFailed` / `DroppedDispatcher` / `FallbackUsed` counters (Interlocked-incremented). Wireable into Settings diagnostics block in a follow-up.
- `RenderShellThumbOnDispatcherAsync` factored out; uses new `TryEnqueueWithRetry` helper (one retry after 50 ms) so transient shutdown-race TryEnqueue==false doesn't silently null the tile.
- Image-extension fallback path: when the shell `IThumbnailProvider` chain returns nothing for `.jpg/.jpeg/.png/.gif/.bmp/.webp`, falls back to `BitmapImage(new Uri(path)) { DecodePixelWidth = 256 }` (what Explorer's Photos uses, WIC-backed). Bumps `FallbackUsed` counter. This is the single most likely reason the user's tiles looked blank vs macOS — shell providers can return zero-size thumbs even for valid images.

**Phase 3 — GUI regression harness (the missing piece):**
- `Program.cs` + `App.xaml.cs` now honor `--auto-scan-folder <path>` and `--auto-exit-after-scan` CLI flags. App.OnLaunched dispatches a `StartScanAsync` once `EngineClient` reaches Ready (60 s timeout); on `Phase=Completed` (or `Failed`) the window closes, which runs the normal shutdown path → `MarkCleanExit()` flips `last-session.txt` to `clean_exit=true`.
- New `platforms/windows/build/gui-regression.ps1` (~150 LOC):
  - Wipes prior state, snapshots existing WER dumps, spawns the app with the new CLI flags.
  - Polls `%LOCALAPPDATA%\FileID\logs\app.log` for `[AUTO-SCAN] starting scan` then `[AUTO-SCAN] scan ended ok=True`.
  - On exit: asserts `clean_exit=true`, zero new WER dumps in `%LOCALAPPDATA%\CrashDumps`, no unmatched `[APPLY:N] enter` (would name the killer subscriber via the trailing `[ENGINE-SUB:*]` line).
  - Exit codes 0/1/2 match `iterate.ps1` shape.

**Phase 4 — synthetic 50K corpus generator:**
- New `platforms/windows/build/gen-corpus.ps1` (~140 LOC) generates a deterministic 60% JPG / 20% PNG / 10% PDF / 5% TXT / 5% DOCX tree under `$OutDir/AA/BB/file_NNNNN.ext` (~676 leaf dirs for 50K files). JPG/PNG via `System.Drawing.Bitmap`, PDF via hand-built minimal one-page spec, DOCX via Office Open XML zip with the 4 required parts. MP4 deferred (needs a binary seed; not needed for crash repro).

**Phase 5 — deferred:** EngineClientTests as scoped would require either factoring `Apply` into a pure function (touches the user's heavy local edits to `EngineClient.cs`) or building a new WinAppSDK UI test csproj (~150 MB new deps) since `EngineClient`'s ctor throws if not on a UI thread. The GUI harness from Phase 3 covers the Pattern B class anyway — the `[APPLY:N] enter`/`[ENGINE-SUB:*]` trace pair already pinpoints the killer subscriber. Unit-test layer can revisit once `Apply` is factored.

**Verification still pending (user runs on Windows hardware):**
1. `pwsh platforms/windows/build/build.ps1` then `dotnet build FileID.sln -c Debug` — ensure Phase 1 + 2 edits compile clean.
2. `pwsh platforms/windows/build/gen-corpus.ps1 -Count 50000 -OutDir C:\Temp\FIDCorpus` — generate the corpus (~10 min).
3. `pwsh platforms/windows/build/gui-regression.ps1 -Corpus C:\Temp\FIDCorpus -TimeoutMinutes 30` — full end-to-end. Expected: `[PASS] GUI regression: scan completed cleanly.`
4. Manual: open the app, scan a real folder, scroll Library — confirm thumbnails render (image-extension fallback should fix the blank-tile cases the user reported).

**Risk acknowledged:** the user has 1470+/991- uncommitted local edits across 30 files including FilePreviewSheet (+383), SidebarProcessingControl (+103), EngineClient.cs (+103). Phase 1 edits were applied to the exact documented crash-site line ranges only; surrounding rework preserved. Re-read each file at the target range immediately before editing to confirm no shift.

## 2026-05-16 (afternoon) — V15.4 scan-crash autopsy + per-subscriber tracing + Pattern B fix

User reported "click Start Scan → entire app crashes" on Windows. Forensics from `%LOCALAPPDATA%\FileID\logs\` from a fresh repro (16:26:14 → 16:26:22):

- `last-session.txt` → `clean_exit=false`
- `app.log` last line at 16:26:18.940 — engine tracing `[SCAN] preloaded skip set ... files_under_root=0`; then 3.5 s of silence on the C# side
- `engine.jsonl` shows engine kept running 3.5 s longer, processed 100 files, then saw `stdin EOF` at 16:26:22.444 and exited cleanly

That's the **native fast-fail signature** — same class as V15.2 (ThumbnailService cross-thread BitmapImage) and V15.2.1 (ModelSlot.PropertyChanged thread-affinity). The CLR is killed by `RaiseFailFastException` from inside a native WinUI 3 / Composition component; every managed sink is bypassed. With 17 places subscribed to `EngineClient.PropertyChanged`, the offending handler isn't identifiable from app.log alone — Apply only logs for errors and ModelDownloadProgress, so the burst of FileDone / Progress / BatchSummary events between scan start and process death is invisible.

**Phase 1 — diagnostic tracing** (additive, on by default):
- `EngineClient.Apply()` now emits `[APPLY:N] enter {EventName} tid=X` before the switch and `[APPLY:N] exit {EventName}` after. Monotonic counter (`_applySeq`); the highest seq with no matching `exit` after a death names the killer event.
- Every subscriber (`SidebarProcessingControl`, `SidebarPipelineProgress`, `SidebarEngineStatus`, `SidebarQueueList`, `LibraryView`, `PeopleView`, `DeepAnalyzeView`, `RestructureView`, `SettingsView`, `AutoPilotTracker`, `CleanupView`, `SuggestedMergesSheet`, `AppViewModel`, `WorkflowAutoTabRouter`, `CudaAutoInstaller`, `LlamaRuntimeAutoInstaller`, `ClipSearchService`, `ModelInstallerService`) now emits `[ENGINE-SUB:ClassName] {PropertyName}` after its property filter. The trailing ENGINE-SUB line before a death names the killer subscriber.
- `DebugLog.Write` already flush-on-write (uses `File.AppendAllText` which opens+writes+closes per call) — confirmed; no buffering changes needed.

**Phase 2 — two proactive fixes for high-confidence Pattern B candidates:**
- **`SidebarQueueList.Sync`** previously called `JobsRepeater.ItemsSource = null` and then mutated the parent panel's `Children` (nuke siblings, insert fresh `StackPanel`) on every `QueueState` event. Visual-tree mutation racing with a layout pass mid-burst is a fast-fail vector. Now: a lazily-created stable `_rowsContainer` `StackPanel` is inserted exactly once; subsequent syncs only `Clear()`+`Add()` its `Children`. Parent's child list never changes again.
- **`SidebarPipelineProgress.SyncStage`** previously allocated four `SolidColorBrush` instances and ran three `Application.Current.Resources` lookups on each `LastProgress` event (10 Hz during a scan). Brushes are `DispatcherObject`s — allocating fine on UI thread, but the per-event churn was wasteful and surfaced as recurring tagged-pinned-allocations. Now cached at ctor time.

**Phase 4 — hardening sweep:**
- Every engine-event handler wrapped in `DebugLog.SafeRun("ClassName.OnEngineChanged", () => { ... })` — managed exceptions log + write `crash-*.txt` instead of escaping the dispatcher.
- Phase 4b cross-thread audit: grep for `new BitmapImage(` outside `DispatcherQueue.TryEnqueue` blocks returned 3 sites — all confirmed safe (ThumbnailService V15.2 fix intact; DeepAnalyzeView and DrillDownSheet thumb loads run on UI thread because the start point is UI-dispatched and awaits propagate via DispatcherQueueSynchronizationContext).
- `platforms/windows/CLAUDE.md` now documents the subscriber convention + brush-caching rule + Pattern B rule under "Conventions (WinUI 3 app)".

**Build:** `dotnet build FileID.sln -c Debug -p:Platform=x64` clean, 0 warnings, 0 errors.

**Verification still pending (user runs on Windows hardware):**
1. Repro the crash. With Phase 1 tracing, `app.log` now identifies the offending `[APPLY:N] {EventName}` + last `[ENGINE-SUB:ClassName]`.
2. If the Phase 2 SidebarQueueList/brush fixes already prevent the crash (plausible — the user's scan emits QueueState + LastProgress at 10 Hz), Phase 3 verification proceeds: Discovering → Tagging → PostScan → Completed → face clustering → Deep Analyze.
3. Either way the diagnostic infrastructure is now in place to surface the next variant quickly.

## 2026-05-16 — V15.3.1 macOS CI fix + V15.3.2 test/bench expansion + privacy gates

Two-pass session.

**V15.3.1 — Make all 3 GitHub workflows green again.** The `macOS app` workflow had been red since V15.2 because the engine-startup smoke step asserted `grep -q '"executionProvider"' engine.stdout`, but the macOS `EngineInfo` struct has no such field (executionProvider is the Windows-only ORT execution-provider picker output; macOS dispatches through MLX + ANE + CoreML with no exposed enum). Two iterations to land the fix: first removed the bogus assertion (commit 131780f); then the diagnostic dump showed engine.stdout was 0 bytes because the macOS engine writes IPC events to STDERR (per `apple/.../IPCSink.swift:108`, `FileHandle.standardError.write(contentsOf: blob)`). Changed the ready-event grep to scan engine.stderr instead (commit 06dcecc). Windows engine writes to stdout — that asymmetry is documented in both workflow files now. All 3 CI surfaces green on `main`.

**V15.3.2 — Tier-1 test + bench + privacy gates.**
- **N7 IPC round-trip tests.** Added two tests to `ipc::tests`: `every_command_variant_round_trips` encodes + decodes every `CommandPayload` variant (26 today) and asserts `std::mem::discriminant` survives; `start_scan_root_path_round_trips` proptests arbitrary `[\PC]{1,200}` paths through StartScan encode/decode. Catches serde rename drift between Rust + Swift schema and missing `#[serde(default)]` regressions.
- **N7 dbwriter ingest-idempotence tests.** Three new tests against `pipeline::dbwriter` exercising `INSERT_FILE_SQL` directly: duplicate inserts produce 1 row (ON CONFLICT contract); duplicate inserts UPDATE size/modified (not just IGNORE); proptest with random mix asserts `row count == distinct paths` regardless of insertion order. Guards the scan resume cursor + People-tab dedup invariants.
- **N3 criterion bench scaffold.** Restructured the engine crate as lib+bin (added `[lib] name = "fileid_engine" path = "src/lib.rs"` re-declaring the 13 submodules) so `benches/*.rs` can `use fileid_engine::*`. Two bench targets shipped: `tagging_hashes.rs` (compute_dhash + resize_rgb_nearest at multiple input sizes) and `face_clustering_5k.rs` (cluster() on 5K synthetic 512-d L2-normalized embeddings). Smoke-verified with `cargo bench -- --quick`: dhash ~360ns regardless of input; resize_rgb_nearest ~184ns. Dev compile cost +30% (modules build once for lib + once for bin); runtime cost zero (shipped bin still gets release LTO).
- **N9 cargo audit re-tightened.** Flipped `.github/workflows/windows-engine.yml` from `continue-on-error: true` back to `cargo audit --deny warnings`. Paired with a new `actions/cache@v4` step that caches `~/.cargo/advisory-db` keyed weekly so the audit corpus stays stable across CI runs. Triage path documented in DECISIONS.md (bump dep version OR add `--ignore RUSTSEC-YYYY-NNNN` WITH a rationale entry; never silent).
- **N9 source URL allowlist scan.** New CI step (both Windows + macOS workflows) scans every `*.{rs,cs,xaml,xaml.cs,swift}` source for any `https?://` URL and asserts every host is on the 6-entry allowlist (`huggingface.co`, `github.com`, `developer.download.nvidia.com`, `developer.nvidia.com`, plus the two XAML namespace identifiers). Source-scan (not binary-scan) because a binary URL scan drowns in false positives from ORT/rustc/windows-rs strings. Flips the no-telemetry posture from "ship anything except these 22 deny-listed strings" to "ship only these 4 documented egress hosts". Belt + suspenders.

**Test counts:** Rust 74 (was 71, +3 dbwriter), IpcSchema 30, FileID.App.Tests 28, FileID.Theme.Tests 16 = **148 total** (was 127, +21 net counting the new IPC tests + criterion smoke).

**Still pending (NEXT.md V15.3):** N5b mock-heavy .NET tests (gated on EngineProcessManager + IpcDispatcher extraction from `EngineClient.cs`), Tier-2 macOS extractions (user verifies on Mac), Windows XAML user-control extraction, parity tests, chaos harness, Phase 10 a11y, Phase 11 release engineering, Phase 14b code-comment hygiene sweep.

## 2026-05-15 (afternoon) — V15.3 Phase 6 + 7 + 11 CI hardening

Continuation of the morning's V15.3 engagement. This session locked in the lint + test + CI gates from Phases 6, 7, and 11 of the polish-mochi plan.

**Rust lint gate (Phase 6):** `cargo clippy --all-targets --target x86_64-pc-windows-msvc -- -D warnings` is now **clean**. Approach: targeted `[lints.clippy]` allows for style-only pedantic rules (`uninlined_format_args`, `doc_markdown`, `manual_let_else`, etc.) with documented justifications, leaving correctness lints as `warn → deny`. Per-site fixes for the 4 real lints that remained (PathBuf debug formatting in `restructure_apply.rs`, BITMAPINFO struct-init in `shell/thumbnail.rs`, &&str to_string in `logging.rs`, `!=` redundancy in `pipeline/deep_analyze.rs`). Zero `TODO`/`FIXME` in production code; zero `.unwrap()` outside `#[cfg(test)]` + `fn main()`; 33 `#[allow(dead_code)]` annotations remain as documented Phase 5+ placeholders.

**.NET lint gate (Phase 6):** `dotnet format --verify-no-changes` is now **clean**. Approach: ran `dotnet format` once to auto-apply IDE0003 (this. simplification) across all view code-behind files; added IDE1006 (private-field-prefix style) to `Directory.Build.props` NoWarn list with a documented justification. `<TreatWarningsAsErrors>true</TreatWarningsAsErrors>` + `<AnalysisLevel>latest-recommended</AnalysisLevel>` + `<EnforceCodeStyleInBuild>true</EnforceCodeStyleInBuild>` already in place; no csproj edits needed.

**Property tests (Phase 7):** `proptest = "1"` adopted as Rust dev-dep. 9 property tests now ship across `util/path_safety`, `util/zip`, and `pipeline/face_clustering`. **proptest paid for itself by catching two real bugs the example tests missed:**
- `is_safe_filename("A\\")` was accepted because `std::path::Path::components()` silently strips trailing separators. Fixed by adding an explicit `contains('/') || contains('\\')` reject before the components walk. Comment cites the proptest test as the regression guard. Security-relevant: this function is the path-traversal guard for `renameFiles`.
- `identity_clustering::cluster` produced **non-deterministic cluster IDs across runs** because `for (_, members) in root_members` iterated a HashMap in random order. Fix: collect into a `Vec`, sort by root, iterate sorted. Without this, a re-scan of the same library could renumber the People-tab clusters between sessions (user-visible: "I named Person #1 as Mom, and after a re-scan she's Person #5 now"). Comment cites the proptest test.

**.NET test expansion (Phase 2):** `SafeOpenTests` shipped with 17 cases including a `[Theory]` over 14 executable extensions (`.exe`, `.lnk`, `.bat`, `.ps1`, `.vbs`, etc.) confirming SEC-9's allowlist rejects each. Total `FileID.App.Tests` count: **28** (was 11). Remaining .NET test classes (`EngineProcessManagerTests`, `IpcDispatcherTests`, `ModelInstallerServiceTests`, `ReadStoreTests`, `AppSettingsTests`, etc.) deferred to NEXT.md N5 — each needs significant mock infrastructure (Process, HttpClient, in-memory SQLite).

**Perf scaffolding (Phase 3):** Added `[profile.release-pgo]` to `Cargo.toml` for PGO instrument-train-use flows (8–15% expected on CPU-bound paths; build-time-only cost). Removed `fast_image_resize = "4"` from deps — was declared but never imported, audited via grep. Verified `serde_json::to_writer` is already the direct path in `ipc/sink.rs:90` (the perf-candidate was already realized). Criterion bench scaffold deferred (needs lib+bin crate restructure to expose `pub fn`s to a `benches/` target — tracked in NEXT.md N3).

**CI gate landing (Phase 8):** `.github/workflows/windows-engine.yml` now runs:
- `cargo fmt --check` (formerly placeholder).
- `cargo clippy --all-targets -- -D warnings` (formerly narrowed to specific lint groups).
- `cargo deny check` (new gate, enforces `engine/deny.toml`: license allowlist + advisory + duplicate-version + source allowlist).
- `cargo audit` (was `continue-on-error: true`, now a hard gate).
- Rust toolchain bumped from 1.78 → 1.90 to match `rust-toolchain.toml`.

`.github/workflows/windows-app.yml` now runs:
- `dotnet format --verify-no-changes` (new gate, x64 only).
- `dotnet list package --vulnerable --include-transitive` with an explicit fail on hits (new gate, x64 only).
- `dotnet test FileID.sln` (was IpcSchema-only + `continue-on-error: true`, now runs all test projects + fails on red).

**Pre-commit hook (Phase 11):** `tools/git-hooks/pre-commit` shipped — bash script that runs on every `git commit` to catch what's fixable locally faster than CI can: privacy-string scan + `cargo fmt --check` + `cargo clippy --no-deps -D warnings` + `dotnet format --verify-no-changes` + `swift-format lint` (if installed). Designed to finish in < 15 seconds on a warm cache. `tools/git-hooks/README.md` documents the one-command install: `git config core.hooksPath tools/git-hooks`. `CONTRIBUTING.md` references this.

**Final test count this session:** 69 Rust + 30 IpcSchema + 28 App.Tests = **127 tests, all green** (was 105 at start of session, +22; was 44 at engagement start, +83).

**Still pending (NEXT.md V15.3 follow-ups):** macOS Swift extractions (user verifies on Mac), Windows XAML user-control extraction, remaining .NET test classes, criterion benches (needs lib+bin restructure), cargo-fuzz harness, Phase 9 robustness suite (UI E2E, large-library stress, fault injection, migration roll-forward), Phase 10 a11y + i18n readiness, Phase 11 release-engineering polish (reproducible builds, signing, CI cache).

## 2026-05-15 — Phase 1 bloat reduction + Phase 2 test seed + Phase 3 perf wins (Windows)

Per a comprehensive "trim bloat + comprehensive tests + push perf" engagement (plan in `~/.claude/plans/i-want-you-to-polished-mochi.md`). Phase 1 reorg + Phase 2 test seed + Phase 3 perf wins applied to the Windows side; macOS work pending (user verifies on Mac).

**Windows Rust engine** — `main.rs` 3,463 → 678 LOC (−80.4%) without a single behavior change.
- New `commands/` directory (one submodule per IPC domain): `hardware`, `embed`, `restructure`, `face_clustering`, `bulk`, `trash`, `trash_log`, `deep_analyze`, `prewarm`, `scan`.
- New `util/` directory: `hmac` (HMAC-SHA256 hand-roll + log-tamper key), `path_safety` (filename/traversal guards + `stable_path_hash` — de-duplicated with `dbwriter.rs`), `zip` (hardened extract with slip + bomb + symlink defenses).
- New `logging.rs` (tracing init + panic-hook factory) and `ipc/bounded_read.rs` (`BoundedRead` enum + `bounded_read_line` + `drain_to_newline`).
- `cargo test --release` clean: **58 passed, 0 failed** (was 44 before this work; +14 new).

**Windows .NET app** — `internal sealed partial class EngineClient` split:
- `ViewModels/EngineClient.cs`: 1,378 → 970 LOC (kept process lifecycle, stdout/stderr loops, Apply event router, observable surface, `Set<T>` helper).
- `ViewModels/EngineClient.Commands.cs` (new, 419 LOC): every `*Async` command facade + AutoPilot orchestration (`RunAutoPilotAsync`, `AwaitPhaseAsync`, `AutoPilotStage` enum).
- `Services/ModelInstallerService.cs`: 1,017 → 735 LOC.
- `Services/ModelSlot.cs` (new, 282 LOC + header): `ModelSlot` class + `ModelInstallStatus` enum split out as separate class.
- `dotnet build` clean; `dotnet test` clean (30 IpcSchema tests pass).

**Phase 3 perf wins (Windows engine):**
- `pipeline/tagging.rs`: replaced the **double image decode** (`image::ImageReader::open(&p)` × 2 per file) with a single `memmap2::Mmap` and two `ImageReader::new(Cursor::new(&bytes))` calls. Saves the second open + read per file across every scan (~5 s on a 50k library, more on slow disks).
- `db/mod.rs`: added `PRAGMA cache_spill = 0` to `SETUP_PRAGMAS`. Pins the 64 MB page cache instead of spilling to a temp file mid-transaction. Worst-case write is a 100-row batch (well under cache); spill never wins.

**Phase 2 tests** added inline for the new modules:
- `util/hmac` — 2 RFC 4231 test vectors + long-key + constant-time-eq edge cases.
- `util/zip` — round-trip extract + zip-slip rejection.
- `ipc/bounded_read` — line read, CR/LF strip, EOF, partial-line-at-EOF, oversized rejection, drain resync.
- `util/path_safety` — preserved + already had safe-filename + traversal-rejected tests.

**Documented in `DECISIONS.md`** under five new 2026-05-15 entries: (a) main.rs decomposition rationale, (b) EngineClient partial-class split rationale, (c) mmap decode fast path, (d) `cache_spill=0`. Existing perf candidates (batched CLIP inference, prepare_cached audit, PGO, ORT GPU residency check) are listed in the engagement plan but deferred — they need a criterion benchmark harness or shipped-binary measurements before merging.

**Still pending (per the engagement plan):**
- macOS Swift refactors (LibraryView/PeopleView/RestructureView decomposition; SankeyFlowView layout extraction; ReadStore split + GRDB `cachedStatement` migration; FileIDEngineMain dispatcher extract; FaceClustering decomposition). User to execute + verify on macOS hardware.
- Windows XAML user-control extraction (SettingsView, RestructureView, WelcomeSheet, DeepAnalyzeView).
- `tagging.rs` helper extraction (image_io + geometry submodules) — deferred as secondary cleanup.
- Phase 2 .NET test projects (`FileID.App.Tests`, `FileID.Theme.Tests`) and Phase 2 Swift test extensions (`AppTests/`, extended `EngineTests/` + `SharedTests/`).
- Phase 3 remaining perf candidates needing measurement: batched CLIP image inference, per-worker thread-local buffer pools, `prepare_cached` audit across hot paths, vectorized L2-normalize, JSON encoding via `to_writer` direct, ORT GPU residency check, PGO release profile.

## V15.2.1 (2026-05-14) — Fix three V15.2 regressions + one-button GPU pack

V15.2 shipped three regressions that broke first-launch on the user's machine. Forensics: `engine.jsonl` showed clean engine teardown after the engine was killed by the new C# watchdog; `app.log` showed the rest of the failure cascade.

**Regression 1 — Stdout watchdog killed idle engines.** V15.2's 5-min idle watchdog (`EngineClient.StdoutLoopAsync`) tripped after the engine auto-installed llama runtimes and went legitimately quiet waiting for user input. The watchdog can't distinguish "engine hung" from "engine idle waiting for user"; it punished idle. **Fix:** removed entirely. The engine's parent-PID watchdog covers the inverse case (C# app dying); GPU TDR is caught by V14.9-Y's `is_gpu_dead`; per-command timeouts (WaitForReadyAsync, CudaAutoInstaller's 30 min) are the right granularity.

**Regression 2 — Respawn CAS gate double-bookkeeping.** Immediately after Bug 1 fired, the respawn path set `_isStarting=1` *before* calling StartAsync; StartAsync's own strict V15.2 CAS saw "already starting" and bailed. Net: every auto-respawn was silently dropped. **Fix:** removed the outer CAS in `OnProcessExited`. StartAsync's own gate handles the race.

**Regression 3 — `ModelSlot.PropertyChanged` thread-affinity crash.** After Bug 2 left the engine dead, Install all fired and `slot.Fail("Engine not running")` invoked PropertyChanged from a thread-pool thread. The welcome sheet's x:Bind forwarded it to `TextBlock.Text` → `COMException 0x8001010E` (RPC_E_WRONG_THREAD). Same class of cross-thread XAML violation as the V15.2 BitmapImage fix, different surface. **Fix:** `ModelSlot.Set<T>` now captures the UI DispatcherQueue at construction and marshals every PropertyChanged invocation through `TryEnqueue` when called off the UI thread.

**Feature — One-button GPU Acceleration Pack on welcome sheet.** Per the user's ask. A 4th row appears on the welcome sheet:
- **NVIDIA**: "Unlocks ~15% faster scanning on NVIDIA GPUs (~430 MB)." Live Install button → engine downloads cuDNN via `cudnn_runtime_x64` registry arm. Becomes "Installed" badge once sentinel lands.
- **AMD**: "DirectML is already optimal for your AMD GPU — no install needed." No badge, no button.
- **Intel**: same, "Intel".
- **Qualcomm**: same, "Snapdragon" (DirectML + QNN).
- **CPU only**: "No GPU detected — scanning will run on CPU."
- **Detection pending**: "Detecting GPU…" until engine Ready event arrives.

Wired through the existing `ModelInstallerService` pattern. New `ModelSlot Accelerator` property; new `AcceleratorIsRealInstall` flag distinguishes "real cuDNN install" from "pseudo-installed for non-NVIDIA"; `UpdateAcceleratorForVendor` adapts on engine Info events. Engine side is unchanged (cuDNN registry arm has been there since V14.9-U).

**Cleanup — runtime-pack progress noise.** ~30 `[INSTALL] no slot for model_kind 'llama_runtime_cuda_x64'` warnings per launch came from the auto-installer's progress events reaching `ModelInstallerService` for kinds it doesn't track. Demoted to Debug-level for known auto-installer kinds (`llama_runtime_x64`, `llama_runtime_cuda_x64`, `llama_runtime_vulkan_x64`).

### Files touched (V15.2.1)
- `platforms/windows/src/FileID.App/ViewModels/EngineClient.cs` — removed stdout watchdog; removed outer respawn CAS.
- `platforms/windows/src/FileID.App/Services/ModelInstallerService.cs` — `ModelSlot.Set<T>` UI-thread marshaling; `Accelerator` slot + `AcceleratorIsRealInstall` flag + `UpdateAcceleratorForVendor` + `IsAutoInstallerOnly` helper.
- `platforms/windows/src/FileID.App/Views/WelcomeSheet.xaml` — 4th row for GPU Acceleration Pack.
- `platforms/windows/src/FileID.App/Views/WelcomeSheet.xaml.cs` — `OnAcceleratorActionClicked` + per-row XAML binding helpers (`ShowAcceleratorButton`, `ShowAcceleratorInstalledBadge`, `AcceleratorGlyph`, `AcceleratorIconBrush`, `AcceleratorSize`).

### Verification plan (user)
1. Launch the app. Engine spawns, runtimes auto-install, app sits idle. Wait 10 minutes; engine stays alive (no watchdog respawn line).
2. Welcome sheet shows 4 rows. 4th row reads "GPU Acceleration Pack (NVIDIA) — Unlocks ~15% faster scanning on NVIDIA GPUs (~430 MB)" with live Install button.
3. Click "Install all". All 4 rows download in parallel; progress percentages tick visibly.
4. After installs, scan a folder. Tiles populate with thumbnails. No crash, no `crash-*.txt`. `last-session.txt` ends with `clean_exit=true`.

## Earlier releases (condensed)

Headlines only — for full session notes `git log` or scroll back through this file's history. Decision rationale lives in [`DECISIONS.md`](DECISIONS.md). User-visible release notes live in [`/CHANGELOG.md`](../../CHANGELOG.md).

- **V15.2** (2026-05-14) — Scan crash root-caused: native fast-fail in `ThumbnailService.RenderAsync` from cross-thread BitmapImage construction. Full stability sweep (every P0/P1/P2 audit finding). Last-session breadcrumb detects native crashes the 3 managed sinks miss. CI workflows brought to parity (Windows app publishes + privacy gate + smoke; macOS smoke-launches engine).
- **V15.1** (2026-05-15) — Top-level crash capture (Application + AppDomain + Task scheduler → `crash-*.txt` with last 50 lines of app.log). `_startInFlight` button gate matching macOS `@State startRequested`. cuDNN auto-installer deleted; replaced by Settings → Performance manual button. `StartScanCommand.Rescan` plumbed through DTO + EngineClient.
- **V15.0** (2026-05-15) — Scale to 1M files: streaming discovery, bounded WAL growth, adversarial-input hardening (decompression bomb caps, malformed-image `catch_unwind`, path-traversal SEC), per-file backpressure across the pipeline.
- **V14.9-Y** (2026-05-15) — Safe GPU saturation. TDR safety net + lowered worker priority + concurrency revert (4→2 CLIP, 8→4 SCRFD/ArcFace). Full 15K corpus in 424s @ 35fps, zero hangs.
- **V14.9-V** (2026-05-14) — clip_text install gap, ORT EP wiring, runtime DLL bundling (`onnxruntime.dll` + `DirectML.dll` ship with the build).
- **V14.9-U** (2026-05-14) — Kill the Deep Analyze model-missing banners; auto-install everything on welcome sheet.
- **V14.9-T** (2026-05-14) — Windows live-scan parity with macOS (per-batch summary cards). CUDA registry. Build wizard.
- **V14.9-S** (2026-05-13) — Fixed model-download 404s in welcome sheet (HF repo paths drifted).
- **V14.9-R** (2026-05-13) — Zero-warning Windows build + macOS CI workflow shipped.
- **V14.9-Q** (2026-05-13) — Full code cleanup + warning-banner UI + cross-platform IPC sync.
- **V14.9-P** (2026-05-13) — Windows end-to-end scan completeness pass.
- **V14.9-O** (2026-05-13) — Windows CI unblock + IdentityClustering port + Ctrl+R silent-failure fix.
- **V14.9-N** (2026-05-13) — Welcome ETA garbage + scan stuck on "Discovering" (two user-reported).
- **V14.9-K-M** (2026-05-13) — Risk-tightening + macOS live caption parity + Restructure ApplyBar port.
- **V14.9-G-J** (2026-05-13) — cuDNN verify UX + Deep Analyze live caption + Restructure tier cleanup + scan log access.
- **V14.9-F-A** (2026-05-13) — Start Scan no-op + sidebar-mid-scan crash (Phase A of ship plan).
- **V14.8.5** (2026-05-12) — Downloader timeout + resume rewrite (Qwen 2.5-VL 3B "reading chunk" fix).
- **V14.8.4** (2026-05-11) — Drag, scan-feedback, Settings sync, install-all pre-stamp, telemetry-button removal.
- **V14.8.3** (2026-05-11) — Install-all "Queued" caption + start-scan crash defenses + honest NVIDIA acceleration.
- **V14.8.2** (2026-05-11) — GPU Performance Packs removed (no shippable URLs).
- **V14.8.1** (2026-05-11) — Welcome-sheet install error cross-wiring fix.
- **V14.8** (2026-05-11) — Parity + GPU coverage + hardening pass.
- **V14.7.16** (2026-05-06) — Sidebar toggle button, new icon, [INSTALL] log trail, smoke harness.
- **V14.7.15** (2026-05-05) — Strict-parity strip + bug audit fixes.
- **V14.7.12** (2026-05-05) — Welcome sheet 1:1 macOS parity rewrite.
- **V14.7.11** (2026-05-05) — Welcome polling NPE + full UI/repo audit fixes.
- **V14.7.4** (2026-05-05) — UI is unbroken: encoding, dynamic resize, accessibility, downloader maxed out.
- **V14.7.1–V14.7.3** (2026-05-05) — Encoding fix, FileID logo wiring, bulletproof startup, V14.7 NEXT.md queue closed.
- **V14.7** (2026-05-05) — Unified build dispatcher + comprehensive audit pass.
- **V14.6** (2026-05-05) — Deep Analyze + ship plumbing + pixel-perfect polish.
- **V14.5** (2026-05-03) — Security pass + bug sweep + every macOS-only feature except VLM.
- **V14.4** (2026-05-03) — Real thumbnails, smooth LavaLamp, working welcome, every macOS UX surface.
- **V14.3** (2026-05-02) — Real ML loop + every shell helper + bulk action sheets + WiX MSI.
- **V14.2** (2026-05-02) — Tier-by-tier parity push (Settings, AutoPilot scaffold, preview sheet, cheat sheet, tab crossfade, real tags).
- **V14.1** (2026-05-02) — Window-size fix + UX polish + perf wins from the audit.
- **V14** (2026-05-02) — Ship-plan execution: LavaLamp restored, Restructure E2E, perf surface, IPC additions.
- **V13** (2026-05-02) — Quality sweep + Install All works + GPU/perf surface.
- **V12.2** (2026-05-02) — App launches end-to-end + clean Desktop install + consolidated README.
- **V12.1** (2026-05-02) — Bug fixes + unified build script + WiX Burn bundle (Pattern 2).
- **V12** (2026-05-02) — Phase 2 → 8 scaffolds across the Windows port.
- **V11** (2026-05-02) — Phase 1 of Windows port: app shell + theme parity + sidebar + welcome.
- **V10** (2026-05-02) — Multi-platform repo restructure + Phase 0 of Windows port.
- **V9** (2026-04-30) — V1 deletion, organizational pass, security audit.
- **V8.5** (2026-04-30) — Restructure V3, Sankey perf + polish, V5 cleanup pass.
- **V7** (2026-04-30 evening) — Restructure redesign (Sankey + dual-pane Tree) + Deep Analyze coverage extended to video + doc.
- **V2** (2026-04-29) — Face clustering V2 (IdentityClustering, two-pass density) + split-process rewrite (engine as child of app over JSON stdio).

---

Earlier history is in `~/.claude/plans/in-media-library-i-temporal-acorn.md`.
