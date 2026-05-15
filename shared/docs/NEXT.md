# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

---

## V15.2 follow-ups (2026-05-14)

V15.2 root-caused + fixed the scan-start crash (native fast-fail from `BitmapImage` cross-thread construction in `ThumbnailService`), did the full P0/P1/P2 stability sweep from the audit, added a last-session breadcrumb that detects native crashes V15.1's three managed sinks miss, and brought the CI workflows to parity (Windows app workflow now publishes, runs a privacy gate, and smoke-launches the EXE; macOS workflow now smoke-launches the engine).

### V15.2-N1 — Real-hardware verification of every sweep item

Plan v15.2's verification list (STATE.md, item 1-7) should be exercised one at a time so a regression in any of the hardening pieces surfaces. The crash-gone test is the headline; items 2-7 should each visibly behave per the expected outcome.

### V15.2-N2 — Decide whether the new stdout 5-min idle watchdog is too aggressive

The watchdog in `EngineClient.StdoutLoopAsync` kills the engine process when no stdout has arrived for 5 minutes. Healthy scans emit events at 10+ Hz; idle the engine still pumps `ready`/`info` events within seconds. But: cold-starting a massive prewarm-then-scan pipeline on a slow machine might silently miss the budget. If we see any spurious respawns on real hardware, bump the budget to 10 minutes or instrument the watchdog with the time-since-last-stdout in a periodic log line so we can tune empirically.

---

## V15.1 follow-ups (2026-05-15)

V15.1 added top-level crash capture (`Application.UnhandledException` + `AppDomain.CurrentDomain.UnhandledException` + `TaskScheduler.UnobservedTaskException` → `crash-*.txt` with last 50 lines of `app.log` attached), a `_startInFlight` gate on the Start Scan button matching macOS's `@State startRequested`, the cuDNN policy reversal (auto-installer deleted, replaced by a manual button in Settings → Performance), and plumbed `StartScanCommand.Rescan` through the C# DTO + `EngineClient.StartScanAsync`. The follow-ups below assume crash dumps will be available the next time the UI dies.

### V15.1-N1 — `Rescan` UI affordance

`StartScanCommand.Rescan` is now wired through the IPC DTO + `EngineClient.StartScanAsync(rootPath, rootDisplay, rescan)` but has no UI surface. Add either (a) a Sidebar context-menu item "Re-scan everything" next to the existing Start Scan button, or (b) a Settings → Library toggle "Force re-scan files even if up to date" that flips a session-scoped flag the sidebar reads. Engine side is done — the V15.0 incremental-skip path honors the flag.

### V15.1-N2 — Handler try/catch hardening sweep across views — CLOSED in V15.2

Closed in V15.2: `DebugLog.SafeRun`/`SafeRunAsync` helpers introduced; high-risk click/toggle handlers in `LibraryView`, `SettingsView`, `SidebarFolderHeader`, `FilePreviewSheet` routed through them. Each catch writes a `crash-*.txt` so handler-side faults leave a forensic trail without tearing down the UI. (Async handlers that already had explicit try/catch were left alone.)

### V15.1-N3 — Decide whether the cuDNN button should ride on the welcome card

User asked during V15.1 scoping whether the cuDNN install belongs on the first-launch welcome sheet. Settings → Performance was chosen for V15.1 because: (a) welcome sheet is already dense with the four required model rows, (b) cuDNN is a niche 10-15% speedup that shouldn't compete with face/tagging/captioning model installs for the user's first-impression attention, (c) NVIDIA-only — would need conditional rendering. Leave the decision pending real telemetry-free user feedback ("did anyone notice the Settings button exists?"). If not, surface a one-line notice on the welcome card *for NVIDIA users only*: "Faster scanning available — see Settings → Performance after install."

### V15.1-N4 — WIC native decode replacing the `image` crate JPEG path

(Carried from V14.9-Y-N2.) `Win32_Graphics_Imaging` features are already in `Cargo.toml`'s windows-rs config; no new dep. `IWICImagingFactory::CreateDecoderFromFilename` is generally 15-30% faster than zune-jpeg on photo JPEGs. Pure code add in `pipeline/tagging.rs::load_image_rgb`. Higher priority now that V15.0 incremental rescan exposed JPEG decode as the dominant per-file CPU cost on warm-cache scans.

---

## V14.9-Y follow-ups (2026-05-15)

The TDR safety net + lowered priority + concurrency revert proved out — full 15K corpus scans in 424 s at 35 fps with zero hangs. The next round of optimizations should keep that stability budget intact.

### Y-N1 — Wire `shell::thumbnail::render` for non-face scans

The existing `shell/thumbnail.rs::render(path) -> Thumbnail` already binds `IShellItemImageFactory::GetImage` and returns the pre-cached 512×512 RGBA8 Explorer thumbnail. For a corpus where the user explicitly disables face detection (`SettingsView.xaml.cs` could expose a toggle), the thumbnail path skips the dominant 140 ms decode cost.

For face-on scans the full decode is still required by SCRFD (would lose accuracy on small faces at 512×512), so this is a Settings-gated speedup, not a default. Estimated win when active: decode 140 ms → ~2 ms.

### Y-N2 — Native WIC decode replacing the `image` crate JPEG path

`Win32_Graphics_Imaging` features are already in `Cargo.toml`'s windows-rs config; no new dep. `IWICImagingFactory::CreateDecoderFromFilename` is generally 15-30 % faster than zune-jpeg on photo JPEGs. Pure code add in `pipeline/tagging.rs::load_image_rgb`.

### Y-N3 — Real-time VRAM monitor via `IDXGIAdapter3::QueryVideoMemoryInfo`

Defense in depth. Spawn a tokio task in `scan_session::run` that polls every 500 ms; if `current_usage / budget > 0.85` for 3 samples (1.5 s), call `coord.request_cancel()` + emit `EngineError { kind: "gpu_memory_pressure" }`. We didn't need this in V14.9-Y because the TDR catcher is sufficient — but on bigger pools it'd add another layer.

### Y-N4 — FP16 ONNX variants

ArcFace + MobileCLIP have FP16 variants on HuggingFace. Half VRAM, often faster on RTX tensor cores. Requires `registry.rs` URL swap + smoke test for accuracy regressions. Could allow larger pool sizes within the same VRAM budget.

### Y-N5 — CUDA EP with cuDNN

V14.9-U landed cuDNN auto-fetch. If the user installs the CUDA Toolkit DLLs separately (or we bundle the small subset of cuda runtime DLLs), the CUDA EP would be available on NVIDIA hardware and is generally faster + more predictable than DirectML on long-running workloads. Would address the underlying TDR susceptibility rather than just catching it.

---

## V14.9-V follow-ups (2026-05-14)

### V1 — Batched CLIP/SCRFD/ArcFace inference to push GPU utilization >20%

After V14.9-V wired the EPs correctly, an RTX 2060 sits at ~19% GPU during scan while CPU stays at ~65% (10 workers doing JPEG decode + resize + hash). Each model is wrapped in `parking_lot::Mutex<Session>` so only one inference runs at a time per model; the CPU finishes decoding faster than the GPU mutex can drain. The fix is either:

- **Per-worker Session instances:** load CLIP/ArcFace/SCRFD N times (one per worker) and hand each worker its own. Simple, but doubles memory per added copy (~250 MB CLIP, ~80 MB ArcFace, ~50 MB SCRFD). At worker_cap=10 that's a few GB of model weights resident.
- **Batched inference (preferred):** preprocess N images on CPU, batch them into one `Tensor::from_array` call, run inference once. ORT handles the batch dimension natively; GPU latency per call grows sub-linearly so throughput goes 3-5x. Requires reworking `process_file` to either (a) buffer files into a batch before firing inference, or (b) have the worker push preprocessed tensors into a batched-inference task that flushes when full or after a short timeout (5-20 ms).
- **Move image decode/resize to GPU:** Windows.Media.Imaging via WIC, or DirectXTex. Would offload the dominant CPU cost (~20 ms/file JPEG decode) onto the GPU. Larger refactor.

Recommend (b) — batched inference. CLIP batch=4 is the sweet spot per Microsoft's DirectML EP perf docs. Tag this V15 if it's not blocking ship.

### V2 — Don't rely on `download-binaries` for the `ort` crate; document the manual fetch

The `ort` 2.0-rc.10 `download-binaries` Cargo feature is set but doesn't actually download anything with our `cuda + directml` feature combo. We work around it with `build/fetch-runtime-deps.ps1`. Bump-check this if upgrading to ort 2.0-rc.11 / 2.0-rc.12 / 2.x stable — if the crate's download script starts working, the fetch-runtime-deps step becomes redundant.

### V3 — Verify ORT is picking DirectML (not silently falling back to CPU)

The diagnostic line `tracing::info!(model = "...", chain = ?chain_labels, "EP priority chain registered")` now lands in `engine.jsonl` per model load. After a scan, also `grep -i "DML\|DirectML\|cpu execution provider" engine.jsonl` to confirm DirectML kernels are actually being selected (ORT itself logs the EP it picks during session creation, at info level). If you see "CPU EP selected" lines, the DirectML registration is failing — most likely cause is `DirectML.dll` missing from the engine's working directory.

---

## V14.9-U follow-ups (2026-05-14)

### U1 — Smoke-test the new auto-installers on Windows + NVIDIA hardware

Pull on Windows. `./build.sh` → "Iterate" preset. App launches with a clean `%LOCALAPPDATA%\FileID\` (or after wiping just sentinels to force re-fire).

**Vulkan runtime auto-install:** engine reaches Ready → `[VULKAN-AUTO] no sentinel — silently installing` in the log → ~80 MB downloads → `Models/llama.cpp/llama-mtmd-cli.exe` + `Models/.sentinels/llama_runtime_x64.installed` land → Deep Analyze opens with no banner. Second launch: `[VULKAN-AUTO] llama.cpp runtime already installed; skipping.`

**cuDNN auto-install (NVIDIA only):** engine reports `gpuVendor=nvidia`. If `ExecutionProvider != cuda` at startup → `[CUDNN-AUTO] silently fetching cuDNN` → ~430 MB downloads → `Models/cudnn/cudnn-windows-x86_64-9.5.1.17_cuda12-archive/` extracts → `register_dll_dirs_under` picks it up → next engine restart sees `ExecutionProvider=cuda` → Settings → Performance reports "CUDA EP active." If user already has a system CUDA Toolkit install with cuDNN → `[CUDNN-AUTO] system cuDNN already detected (EP=cuda); skipping our pack.`

**Privacy spot-check:** Wireshark/Fiddler attached on first launch. Expected egress: HuggingFace (when triggering VLM weight downloads), `github.com` / `objects.githubusercontent.com` (llama.cpp runtimes), `developer.download.nvidia.com` (cuDNN, NVIDIA only). Nothing else.

**`-PreserveModels` round-trip:** wizard → Fresh install → "Build artifacts + library DB (preserves models)". Confirm `Models/.sentinels/` survives. Launch app — welcome sheet sees existing sentinels, skips download.

### U2 — Decide whether cuDNN auto-install needs a first-launch toast disclosure

PRIVACY.md now lists `developer.download.nvidia.com` as a disclosed egress. The CudnnAutoInstaller fires silently after the engine reports NVIDIA hardware. PRIVACY.md's "every network egress is initiated by you, with visible UI" line implies the user should see *something* signaling the cuDNN fetch is starting. Currently they'll see it in download progress under Settings → Performance if they look, but it's not toast-level visible.

If you want a first-launch toast like "FileID is fetching cuDNN from NVIDIA to enable CUDA scanning (~430 MB)…" file a small follow-up. The auto-installer plumbing is already there; just wire a one-shot toast off the same trigger.

---

## V14.9-T follow-ups (2026-05-14)

### T1 — Smoke-test the live-streaming + CUDA + wizard changes on Windows hardware

Pull on Windows. Run `./build.sh` with no args to exercise the new wizard; pick "Iterate" for Windows. App launches.

Live streaming check (Library tab + Cleanup tab): point at an unscanned folder, click Start Scan. Both tabs should populate visibly while the scan is still running — tile count grows every ~1 second, not all-at-once at completion. (macOS reference: `LibraryView.swift:99-108`.)

CUDA registry check (NVIDIA-only): on launch, the `CudaAutoInstaller` should silently begin downloading `llama-b4475-bin-win-cuda-cu12.4-x64.zip` into `%LOCALAPPDATA%\FileID\Models\llama.cpp-cuda\`. After completion, the sentinel lands at `Models\.sentinels\llama_runtime_cuda_x64.installed`. Settings → Performance → manual "Install CUDA llama.cpp" button shows the same flow without the previous "not registered" toast. On a second launch, the auto-installer logs `[CUDA-AUTO] CUDA llama.cpp already installed; skipping.` (proving the sentinel-path fix).

Wizard sanity: `./build.sh` → "Fresh install" → "Build artifacts + library DB (preserves downloaded models)" → confirm Models/ survives but Models/.sentinels and the SQLite WAL are gone. The wizard's echoed `→ Equivalent for next time` line should be a valid flag string that produces the same outcome.

Sankey verification (not a code change, just a check): in Restructure, click "Generate plan" on a folder with at least one classifiable file. Ribbons should render by default. If they don't appear, the issue is plan-emptiness or visibility-gating in `SyncPlan` (see `RestructureView.xaml.cs:78-81`) — file a fresh issue with the move count.

---

## V14.9-R follow-ups (2026-05-13)

R1, R2, R3 closed in-session (zero-warning Windows build + new macOS CI workflow + parity-gap audit confirming the prior backlog is already coded). One item needs the user.

### R1 — Push + watch CI

Working tree: 1 C# fix in `AutoPilotTracker.xaml.cs`, dead-code annotations across ~17 Rust files, new `.github/workflows/macos.yml`, plus this doc bump. Suggested grouping: (1) R1 build cleanup (CS0414 + Rust allows); (2) R2 macOS CI workflow; (3) R3 docs. Push, watch `gh run watch windows-engine.yml`, `gh run watch windows-app.yml`, `gh run watch macos.yml`. First macOS run will warm the SwiftPM cache (~10–15 min cold); follow-ups should land in ~5 min.

### R2 — Smoke-test on Windows hardware

Pull on Windows. `pwsh platforms/windows/build/build-all.ps1 -Release -Run`. Build prints "Build complete." with zero warnings. App launches; engine handshake completes; cold scan of a small folder runs through Discovering → Tagging → Completed; the 7 audited parity flows behave per macOS reference (multi-merge People, ApplyBar in Restructure, name-gate in Deep Analyze, sibling nav + badges + tag draft in FilePreviewSheet, Library tiles fetch thumbnails, AutoPilot tracker advances Scan→Cluster→Caption→Plan, Restructure folders classified Anchor/Mixed/Junk).

---

## V14.9-Q follow-ups (2026-05-13)

V14.9-O + P + Q together closed every audited Windows scan-flow gap, ported the IdentityClustering algorithm, synced the IPC schema across platforms, added the warning-banner UI, and stripped the session's narrative comments. P3, P4, P6 from V14.9-P are now done in-session. Two items still need the user's hardware/git.

### P1 — Commit + push (user-side)

Working tree spans Rust engine, .NET app, Apple Swift, schema, CI, scripts, docs, and `.gitignore`. Suggested grouping: (1) Q1+Q2+Q3 cleanup (comment strip + unused imports + clippy); (2) Q4 LastWarning channel + banner UI; (3) Q5 .gitignore cherry-pick; (4) Q6 comparison harness; (5) Q7 IPC schema sync (schema + Swift + mac dispatch + tests); (6) Q8 docs. Push, watch `gh run watch windows-engine.yml`.

### P2 — Smoke-test on real Windows hardware

Pull on Windows. `./build.sh -windows --no-wipe`. Open the app, pick a folder, click Start Scan. Pre-flight succeeds; phases transition Discovering → Tagging → Completed; Library/Cleanup populate live without tab switching; warning banner shows if models were absent. Ctrl+R re-scans cleanly. Settings → Performance "Verify CUDA pack" renders diagnostics. `%LOCALAPPDATA%\FileID\logs\engine.jsonl` shows redacted paths.

### P5 — Cross-platform clustering parity

`shared/scripts/compare_face_clustering.sh <mac.sqlite> <windows.sqlite>` after scanning the same library on both. Drift > 10% or Jaccard < 0.85 → tune `pass1Cosine` / `pass2Cosine` in `identity_clustering.rs` (or swap brute-force kNN for `instant-distance`).

---

## V14.9 — Deferred from the V14.8 parity + GPU + hardening pass (2026-05-11)

Six items were scoped out of V14.8 because they need engine-schema changes, multi-screen view rewrites, or visual review on real hardware that can't be done blind.

### A2.2 — FilePreviewSheet badges + tag input

**Status:** toolbar + sibling-nav + Analyze already shipped (V14.7.2). Still missing vs macOS:
- OCR / face badge overlays *on the preview surface* (not just on Library tiles)
- A drafted-tag input row beneath the metadata strip

**Files:** `platforms/windows/src/FileID.App/Views/Library/FilePreviewSheet.xaml(.cs)`.

**Acceptance:** Open a preview for a file with `HasFaces=true` AND `HasText=true`; both badges visible top-left over the preview image. Type two tags into the new row, hit save, reopen the sheet — the tags survive (round-trip through the existing tag-write IPC).

### A6 — AutoPilot 4-step stage tracker

The engine has the `autoPilot` IPC handler; no UI surfaces per-stage progress. Need to add an event + sidebar overlay control.

**Engine:**
- `shared/ipc-schema/ipc.schema.json` — add `AutoPilotStage` event: `{ stage: "Scanning"|"Clustering"|"Planning"|"Captioning", progress: f32 }`.
- `platforms/windows/src/engine/src/pipeline/auto_pilot.rs` — emit a stage event at every phase transition + on progress ticks.

**App:**
- `platforms/windows/src/FileID.App/Views/AutoPilot/AutoPilotTracker.xaml(.cs)` — 4-step horizontal tracker (filled/active/empty dots; gold on active).
- Wire `AppViewModel.AutoPilotState` bindable property; subscribe to `AutoPilotStage` events in `EngineClient`.

**Acceptance:** Click AutoPilot in the sidebar; the tracker overlay slides in (spring 0.35/0.78); dots fill left-to-right as the engine progresses; on completion the overlay fades out.

### A7 — Engine-authoritative Restructure classifier + floating ApplyBar

Windows currently derives Anchor/Mixed/Junk in `RestructureViewModel` (C# heuristic: ≥80% homogeneity = Anchor, ≤2 files = Junk, else Mixed). macOS has a real classifier in `Restructure.swift`. Plus macOS has a floating frosted ApplyBar with step chips + apply-as-shortcuts → convert flow that Windows lacks.

**Engine:** port the macOS classifier to `platforms/windows/src/engine/src/pipeline/restructure.rs`. Add `tier: "Anchor"|"Mixed"|"Junk"` to each `RestructurePlan` item; serialize through IPC. Drop the C# heuristic.

**App:** `platforms/windows/src/FileID.App/Views/Restructure/RestructureView.xaml(.cs)` — three tier sections; floating ApplyBar at the bottom (`<Border Background="AcrylicBrush" CornerRadius="16">`) with step chips matching `RestructureApplyBar.swift`.

**Acceptance:** Run a restructure on a mixed-content folder; tiers come from the engine (verifiable via the IPC frame in `app.log`); the floating bar appears bottom-center, animates in via spring, and chains the two-step apply (shortcuts → convert).

### B3 — Per-inference EP-failure recovery

Today an EP that BUILDS its session but FAILS at first `session.run()` (e.g. CUDA + wrong cuDNN at runtime) kills the worker. The fallback chain in `runtime.rs` handles build-time failures; runtime failures are unguarded.

**Files:** `platforms/windows/src/engine/src/models/{arcface,scrfd,mobileclip,clip_text}.rs`.

**Approach:** On the first `session.run()` per worker, wrap in a guard; on failure, rebuild via `create_session` skipping the failed EP (pass the failed EP into `create_session` so `priority_chain` excludes it). Subsequent inferences proceed on the fallback.

**Acceptance:** Build an integration test that mocks an EP-runtime panic; the worker swaps to the next EP and continues without dying.

### A8 — LavaLampBackground visual-fidelity verification

Currently a Composition implementation (three `SpriteVisual`s + `CompositionRadialGradientBrush` + Vector3 keyframe drift + 35% darken). macOS uses Canvas + `.blur(radius: 120)` Gaussian. They may not render identically — radial-gradient falloff isn't a true Gaussian.

**Approach:** Capture a 5-second 60-fps clip on both platforms at matching window sizes (1440×900). Eyeball side-by-side. If the radial-gradient version reads as visually different, swap the brush for `CompositionBackdropBrush` + `GaussianBlurEffect` (radius 120) over solid color discs.

**Acceptance:** Slow-motion playback shows the same "drift + blur" feel; no banding visible on Windows that isn't visible on macOS.

### A9–A11 — Final polish pass (SF Symbols mapping, spring tuning, empty/error states)

Multi-screen visual review pass. Needs the user (or a test runner) to step through every view on both platforms.

- **A9 SF Symbols → Segoe Fluent mapping audit:** enumerate every SF Symbol in `platforms/apple/app/Sources/FileID/**` (≈50 unique), find each Windows glyph in `platforms/windows/src/FileID.App/**/*.xaml`, replace any that read visually different from the SF original.
- **A10 Spring animation tuning:** WinUI `SpringScalarNaturalMotionAnimation`'s `Period` + `DampingRatio` is parameterized differently from SwiftUI's `response` + `dampingFraction`. Build a side-by-side demo screen with a tap-to-grow on both platforms; tune until the decay envelope matches.
- **A11 Empty / error-state parity sweep:** walk every view's empty + error state, fix icon/copy/centering/spacing divergences.

**Acceptance:** No reviewer can tell, from screenshots, which screen is macOS and which is Windows (target = "down to the pixel" stated by the user).

---

## External manual steps (carry-over)

- EV cert purchase + install (so signed binaries skip SmartScreen on first run).
- Real-hardware verification pass per `SHIP.md` Appendix W matrix (six rows, target ≥ 4 green to ship Windows v1.0). Throughput targets reflect DirectML / CPU paths (no packs — see `PACKS.md`).

## Closed-as-intended (no code)

- **Multi-vendor auto-install (OpenVINO / QNN packs).** Performance Packs were policy-removed in V14.8.2 — DirectML is the universal D3D12 GPU path for every vendor. Only NVIDIA's CUDA llama.cpp build is auto-installable today because it's MIT-licensed redistributable from ggml-org; that flow shipped in V14.9 (`CudaAutoInstaller`). See `PACKS.md` + `DECISIONS.md` (2026-05-11).

---

## (Historical entries below — closed in V14.7.2)

## V14.8 — Polish + engine-authoritative classification (2026-05-05) — CLOSED IN V14.7.2

V14.7.1 closed every CRITICAL parity gap, every HIGH security finding, and every HIGH bug from the V14.7 audits. The remaining work is polish + the next layer of correctness:

### Engine-authoritative Restructure classification

V14.7 surfaces Anchor / Mixed / Junk counts in the Restructure tab by deriving them in C# from per-source-folder move ratios (homogeneity proxy: ≥80% of moves to one destination category = Anchor; ≤2 files = Junk; otherwise Mixed). The macOS engine has a real classifier in `Restructure.swift` that carries these tiers explicitly. Port that classifier to `engine/src/pipeline/restructure.rs` and surface the tiers via new fields on `RestructurePlan`. Drop the C# approximation once landed.

### HMAC-signed trash_log entries

V14.7 defends against trash_log forgery by requiring restore destinations to be inside an authorized `scan_sessions.root_path`. That blocks the worst case (write into System32) but a local attacker who knows the user's library root can still forge entries pointing inside it. Sign each entry with HMAC-SHA256 keyed off a DPAPI-stored secret; reject entries whose HMAC doesn't verify. Same shape works for `merge_log.json`.

### Theme primitives wired to surfaces

`ShimmerView`, `CompletionRipple`, `IridescentBorder` are built in `FileID.Theme` but no view consumes them. Wire to:
- `ShimmerView` over Library tile placeholders during scan
- `CompletionRipple` on per-file `BatchSummary` arrival in the scan in-flight panel
- `IridescentBorder` on the empty-state hero ("FileID" rainbow shimmer matches macOS Detail.swift)

### FilePreviewSheet feature surface

macOS preview sheet has: ←/→ sibling navigation, drafted tag input, OCR/face badges in preview, "Analyze with Deep Analyze" button on the toolbar, Esc-to-close. Windows has a basic ContentDialog with limited tooling. Port the macOS shape.

### Engine-side AutoPilot state machine + UI feedback

V14.7 wires the `AutoPilot` button to the existing IPC. The engine does run the full pipeline, but there's no per-stage progress beyond what the manual flow emits. Add stage-tagged progress events (`AutoPilotStage` = Scanning / Clustering / Planning / Captioning) and a sidebar overlay that shows the current stage as a 4-step tracker.

### iterate.ps1 corpus regression harness

Port macOS `iterate.sh` to PowerShell. 11 corpus assertions: scan completes, face clusters, dupe groups, memory bound, throughput target per hardware tier, no crash dumps, FTS5 hits, sidecar tags round-trip, WAL clean. CI gate.

### LavaLamp Composition rewrite

User's favorite. Currently a flat dark backdrop because Win2D's `CanvasAnimatedControl` fast-failed on Windows 11 26200. Rewrite using `Microsoft.UI.Composition` directly: three `SpriteVisual`s with `CompositionRadialGradientBrush`, `CompositionScalarKeyFrameAnimation`, `CompositionBackdropBrush` + `GaussianBlurEffect`. Honor `UISettings.AnimationsEnabled` for reduced-motion.

### Performance Pack ZIP hosting (manual user step)

Engine + Settings UI are wired (V14.6) — the pack URLs point at `huggingface.co/datasets/fileid-app/performance-packs`. The dataset repo + ZIPs need to be uploaded once (CUDA / OpenVINO / QNN packs from each vendor's official redistributable). Until then the buttons surface a friendly "Pack not available yet — see PACKS.md".

### EV cert codesigning (manual user step)

`build/sign.ps1` + `WinVerifyTrustChecker.cs` + `publish-bundle.ps1 -Sign` are wired. Need to actually buy the EV cert (DigiCert / SSL.com / Sectigo, ~$300/year + identity verification). Once installed, set `FILEID_EV_THUMBPRINT` and ship-builds will refuse Unsigned binaries.

---

## V14.7 — Remaining macOS-parity gaps (audit findings, 2026-05-05) — CLOSED IN V14.7.1

Three parallel audits (macOS feature parity / security / bug sweep) surfaced gaps. V14.6 round of fixes closed: ZIP-slip + bytes/entry caps, bounded IPC line read, vlm kill_on_drop, PowerShell ExecutionPolicy Bypass, reserved-name rejection (CON/PRN/AUX/NUL/COM*/LPT*), CloseHandle on snapshot, WinVerifyTrust revocation check, LibraryViewModel CTS race, EngineClient lifecycle (`_isStarting` debounce + `_expectingExit` guard), UndoStack timeout cross-talk, Sidebar pause desync, CompletedPanel duration calc, EngineClient.IsPaused + LastScanDuration tracking. V14.7.1 closed the rest:

### CRITICAL parity gaps

1. **People multi-select bulk merge / mark-as-unknown.** macOS lets you check N cluster cards then "Merge into one" / "Mark all as unknown". Windows only has 1:1 drag-drop. Engine `mergeClusters` already supports many-to-one; UI surfacing is the work.
2. **Cleanup per-group action menu.** macOS exposes per-group: Select all, Select all except keeper, Invert, Skip group, Delete group. Windows only has a global "Trash non-keepers". The keeper concept becomes much more usable with these.
3. **Restructure three-tier Anchor/Mixed/Junk classifier UI.** macOS has `RestructureRecommendationRow`/`StatHero`/`HoverContext` driving a layered recommendation view. Windows shows a flat per-category card list. Visual + workflow gap.
4. **Settings model installer cards.** macOS has CLIP/ArcFace/VLM install/progress cards with per-model rate + ETA. Windows has descriptive text only.
5. **AutoPilot UI.** macOS chains Scan→Group→Caption→Propose with state-machine UI. Windows has the engine handler (`autoPilot` IPC) but no UI invokes it.

### HIGH parity gaps

6. **FilePreviewSheet feature surface.** macOS has ←/→ sibling nav, OCR/face badges in the preview, drafted tag input, Reveal/Open/Analyze toolbar, Esc close. Windows preview is a basic ContentDialog with limited tooling.
7. **DeepAnalyze "Name people first" gate** — required step on macOS that's missing on Windows.
8. **DeepAnalyze status card / RAM badge / pending-mins ETA** — macOS shows these; Windows doesn't.
9. **DeepAnalyze "smart names ready" pending-rename pill** that links to BulkRenameSheet from the tab itself — not just per-tile right-click.
10. **DeepAnalyze startingCard pre-progress** ("Queued / Loading <model>… / Resolving targets…").
11. **Library multi-select-mode toggle / Tag selected pill / Undo last rename header button** — macOS keeps these visible always; Windows hides them until tile selection.
12. **Restructure floating frosted ApplyBar with step chips.** Visual + the two-step "Apply as shortcuts → Convert to real moves" workflow.
13. **GPU EP override actually applied.** XAML comment notes the override is saved to `settings.json` but the engine doesn't yet read it on the next spawn. Wire `runtime.rs` to consume `GpuExecutionProviderOverride`.
14. **Theme primitives unused.** ShimmerView, CompletionRipple, IridescentBorder are built in FileID.Theme but no view consumes them yet (macOS uses them on Library tiles, DeepAnalyze tokens, hero cards).
15. **Library FTS5/CLIP search wiring in the search box.** Search exists at the engine level, but the box is UI-only — needs the actual queries plumbed in.

### Open security findings

- **SEC-3 DLL planting** — `models/runtime.rs::has_dll` searches PATH. Add `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR)` at engine startup so EP DLL loads only from system32 + the engine binary's directory.
- **SEC-5 TOCTOU restructure apply** — between `canonicalize_safely` and `MoveFileExW`, a junction at the destination's parent can redirect outside the library root. Open the parent with `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS` and refuse reparse-point parents, OR require destinations to not exist.
- **SEC-7 trash_log forgery** — any local writer of `%LOCALAPPDATA%\FileID\trash_log.json` can craft a batch entry pointing at arbitrary `original_path` and the engine PowerShell-restores into it. Mitigate via DPAPI-encrypted HMAC over each entry, or restrict restore destinations to library-root descendants.
- **SEC-9 Open handler ext allowlist** — Library Open / Preview Open / RecentScans Open all `ShellExecute` on a path read from the DB without an extension filter. A `.jpg` row whose underlying file was swapped on disk for `.exe`/`.lnk` will execute. Add an extension allowlist for "Open"; "Reveal" stays universal.

### Open bugs

- **BUG-4 backpressure escape** — deep_analyze + scan_session emit progress via unbounded `tokio::spawn`. A token-stream burst creates a growing tail of tasks awaiting the bounded sink. Switch to `try_send` and drop on overflow, or bound with a `JoinSet`.
- **BUG-9 ReadStore concurrent connection** — `Microsoft.Data.Sqlite` connections aren't thread-safe across simultaneous commands; the `_gate` is only used in `OpenAsync`. Either wrap every read in `_gate.WaitAsync` or use ephemeral per-call connections.
- **BUG-12 LibraryView._inflight Dictionary not thread-safe** — switch to `ConcurrentDictionary`.
- **BUG-13 Alt+Decimal accelerator** — `VirtualKey.Decimal` is the numpad `.`, not the comma the comment claims. Likely unintended hotkey on numpad-period.
- **BUG-22 parking_lot mutex held across blocking I/O** — `handle_apply_tags`/`trash_files` lock the writer while iterating + writing sidecars. Blocks concurrent IPC writers (e.g. AutoPilot scan).

### MEDIUM/LOW items

Documented in the audit reports linked from STATE.md V14.7. Address opportunistically.

---

## 1. User-side Phase 1 verification on real Windows hardware

Phase 1 (V11) ships a feature-rich UI shell but I haven't run any of it. The primary blocker is verifying the build is clean on a real Windows host.

**Acceptance:**
- Open `platforms/windows/FileID.sln` in Visual Studio 2022 17.11+. NuGet restore succeeds.
- `dotnet build platforms/windows/FileID.sln -c Debug` succeeds with zero errors. (Warnings expected; we treat warnings as errors in CI but local-build warnings can leak through XAML codegen.)
- `dotnet run --project platforms/windows/src/FileID.App` launches the app:
  - Mica/Acrylic chrome with a dark title bar.
  - LavaLamp animating behind a sidebar + detail layout.
  - First-launch Welcome sheet appears (since no models installed yet); Skip dismisses cleanly.
  - Sidebar shows folder picker placeholder + the 6 disabled tabs + an "Engine starting…" pill at the bottom.
  - The engine pill flips to "Engine ready" within 1–2 seconds (because the Rust engine emits `ready` on stdin/stdout). If it stays "Starting…" or goes "Crashed", check `%LOCALAPPDATA%\FileID\logs\app.log`.
- `dotnet test platforms/windows/Tests/FileID.IpcSchema.Tests` is GREEN.
- Side-by-side LavaLamp video review at 1080p against macOS reference: the three-ellipse drift + 120 px Gaussian + 35 % darken overlay should look indistinguishable. Frame-by-frame ideally, but a 30 s recording is sufficient for sign-off.
- Hit Ctrl+O. Folder picker opens. Pick a folder. Sidebar header switches to "<parent>/leaf" with leaf in gold, Change/Clear/Wipe actions appear, tabs become enabled.
- Hit Ctrl+R after picking a folder. Sidebar processing control flips to in-flight state with progress bar (which will sit at 0 because the engine returns `not_implemented` for startScan in Phase 0 — that's expected; Phase 2 wires the real scan).
- Ctrl+Shift+S toggles the sidebar.
- Alt+1..6 jumps tabs.
- Drag a folder onto the window. Gold-bordered overlay appears. Drop accepts the folder.
- Reduce-motion verification: Settings → Accessibility → Visual effects → Animation effects OFF. LavaLamp halves rate; Shimmer freezes; CompletionRipple becomes inert; IridescentBorder freezes gold.
- Accessibility Insights audit ≥ 0 critical issues. Tab key reaches every interactive element.

**Likely first-run hiccups (in priority order):**
1. WinAppSDK 1.6 runtime not installed: surface error MessageBox at launch. Install via `winget install Microsoft.WindowsAppRuntime.1.6` and relaunch.
2. NuGet restore fails: probably the `nuget.config` carve-out — re-run `dotnet restore platforms/windows/FileID.sln`.
3. XAML compilation errors I missed: most likely candidates are the IridescentBorder template (Win2D namespace), the DetailHostView swap pattern, and the templated control attached property registrations. If a build error references one of those, paste it and I'll fix.
4. Engine doesn't spawn: the C# app expects `FileIDEngine.exe` either alongside `FileID.exe` or under `engine/target/{x86_64,aarch64}-pc-windows-msvc/release/`. Run `pwsh platforms/windows/build/build.ps1` first to produce the engine binary.

## 2. Apply the `startScan` IPC breaking change on the macOS side

Carried over from V10. The Rust engine implements the new payload from day one. The macOS engine + app + iterate.sh still use the legacy `(rootBookmark: Data, rootPathDisplay: String)` payload. One coordinated commit on a Mac:

- Edit `platforms/apple/shared/Sources/FileIDShared/IPCProtocol.swift` — change the case associated values to `(rootPath: String, rootDisplay: String?)`.
- Edit `platforms/apple/engine/Sources/FileIDEngine/FileIDEngineMain.swift` — accept `rootPath` directly (drop the bookmark resolve branch).
- Edit `platforms/apple/app/Sources/FileID/EngineClient.swift` — stop creating a security-scoped bookmark; send the path directly.
- Edit `platforms/apple/scripts/iterate.sh` line 128 — change the IPC frame to `{"startScan":{"rootPath":"$CORPUS"}}`.
- Verify `swift test` passes. Run `bash scripts/iterate.sh`.

After this, both engines speak the same IPC. Nothing else cross-platform-breaking is queued.

## 3. Phase 2 — Library tab end-to-end on Windows

Per `platforms/windows/PHASES.md` Phase 2. Big chunk: scan pipeline (walkdir, EXIF, phash, MobileCLIP scan-time embed, OCR via Windows.Media.Ocr, SCRFD+ArcFace face detect+embed, DBWriter), Library tab UI (search, multi-select, file preview sheet, tag editor, bulk actions). 4–5 weeks of work.

Don't start until item 1 above passes.

## 4. Lingering macOS work (deferred during the port)

Carried over from V9/V10. Pick up after Phase 1 Windows ships, or interleave if scope allows.

- **Soak Restructure tab** on real ~50K library (Sankey, hover bus, drill-down, floating apply bar).
- **Engine perf sweep** — audit `ScanCoordinator`, `JobQueue`, `IPCSink` for strict-concurrency warnings; sustain ≥140 files/s on M1 Pro.
- **v1.0 ship checklist** (`shared/docs/SHIP.md`) — code signing + notarization, app icon, About panel, Sparkle channel.

## 5. Ideas parking lot

- Drag-and-drop a Restructure proposal row to override its destination.
- Per-cluster "merge into existing person" affordance in People.
- Smart Albums backed by saved CLIP queries.
- Export Restructure proposals as a JSON manifest for off-app review.
- Once Phase 4 lands, schedule a recurring agent to verify the privacy CI gate stays green on every release tag.
