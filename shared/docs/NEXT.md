# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

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
